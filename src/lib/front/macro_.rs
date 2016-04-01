// The MIT License (MIT)
//
// Copyright (c) 2015 Johan Johansson
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
// THE SOFTWARE.

// TODO: Macro hygiene. Prevent shadowing and such, maybe by letting vars introduced inside the
//       macro be located inside a module generated by the macro.
//       So something like:
//           (def-macro m ()
//               (def-var a (:U64 42))
//               (set a (inc a)))
//       Would expand to:
//           (module m
//               (def-var a (:U64 42)))
//           (unsafe (set m\a (inc m\a)))
// TODO: Add special names that expand to special stuff, like for example $crate in Rust macros.
//       E.g. the name $here could expand to the path to the same crate+module as where the
//       macro is defined.
// TODO: Use visitor pattern. A `MacroExpander` can track recurse depth and provide useful errors
// FIXME: Extending syntax variable maps is not correct. TODO: error or something on existing entry
// TODO: Track recursion level and have a maximum recursion depth

use std::collections::{HashMap, HashSet};
use std::iter::once;
use std::fmt;
use itertools::Itertools;
use super::SrcPos;
use super::lex::CST;

/// Returns the length of the longest list bound to by a syntax var in `tree`.
/// Returns `None` if no syntax var is bound to a list
fn max_syntax_var_len(tree: &CST, syntax_vars: &HashMap<&str, CST>) -> Option<usize> {
    match *tree {
        CST::Ident(ident, _) => {
            syntax_vars.get(ident).and_then(|tree| {
                match *tree {
                    CST::SExpr(ref v, _) | CST::List(ref v, _) => Some(v.len()),
                    _ => None,
                }
            })
        }
        CST::SExpr(ref v, _) | CST::List(ref v, _) => {
            v.iter()
             .filter_map(|e| max_syntax_var_len(e, syntax_vars))
             .max()
        }
        _ => None,
    }
}

/// Substitute sequence syntax vars in `tree` for the element in the sequence at index `i`.
/// If no element exists at index `i`, repeat the last element in the sequence.
/// If the sequence is empty, return `None`
fn subst_syntax_vars_at_iteration<'src>(tree: &CST<'src>,
                                        i: usize,
                                        syntax_vars: &HashMap<&'src str, CST<'src>>)
                                        -> Option<CST<'src>> {
    match *tree {
        CST::Ident(ident, _) => {
            syntax_vars.get(ident)
                       .map(|subst| {
                           match *subst {
                               CST::SExpr(ref substs_list, _) | CST::List(ref substs_list, _) => {
                                   if substs_list.len() < (i + 1) {
                                       substs_list.last().cloned()
                                   } else {
                                       Some(substs_list[i].clone())
                                   }
                               }
                               _ => Some(subst.clone()),
                           }
                       })
                       .unwrap_or(Some(tree.clone()))
        }
        CST::SExpr(ref v, ref pos) | CST::List(ref v, ref pos) => {
            let constructor: fn(_, _) -> _ = if tree.is_sexpr() {
                CST::SExpr
            } else {
                CST::List
            };

            Some(constructor(v.iter()
                              .filter_map(|e| subst_syntax_vars_at_iteration(e, i, syntax_vars))
                              .collect(),
                             pos.clone()))
        }
        _ => Some(tree.clone()),
    }
}

/// Iterate over the zipped elements of the sequence syntax variables in a pattern,
/// repeating the pattern with the new elements of each iteration
///
/// ## Example
/// ```lisp
/// (c1 and c2) ... ; Where c1 is the list `(1 2 3)`, c2 is the list `(a b c)`, and `and` is a syntax literal
/// ; expands to:
/// (1 and a) (2 and b) (3 and c)
/// ```
fn flatten<'src>(pattern: &CST<'src>,
                 syntax_vars: &HashMap<&'src str, CST<'src>>)
                 -> Vec<CST<'src>> {
    match max_syntax_var_len(pattern, syntax_vars) {
        Some(max) => {
            (0..max)
                .filter_map(|i| subst_syntax_vars_at_iteration(pattern, i, syntax_vars))
                .collect()
        }
        None => {
            pattern.pos()
                   .error("Can't flatten a pattern that contains no sequence syntax variables")
        }
    }
}

/// Recursively substitute all syntax variables in `tree` for their definitions
fn subst_syntax_vars<'src>(tree: &CST<'src>,
                           syntax_vars: &HashMap<&'src str, CST<'src>>)
                           -> CST<'src> {
    match *tree {
        CST::Ident(ident, _) => syntax_vars.get(ident).unwrap_or(tree).clone(),
        CST::SExpr(ref v, ref pos) | CST::List(ref v, ref pos) if !v.is_empty() => {
            if let CST::Ident("macro-quote", ref pos) = v[0] {
                // It's an escape
                if v.len() != 2 {
                    pos.error(format!("Arity mismatch. Expected 1, found {}", v.len()));
                }
                v[1].clone()
            } else {
                let mut substituted = Vec::new();

                for i in (0..v.len()).filter(|&i| !v[i].is_ellipsis()) {
                    if v.get(i + 1).map(CST::is_ellipsis).unwrap_or(false) {
                        // syntax var followed by ellipsis, try to flatten it as a sequence

                        substituted.extend(flatten(&v[i], syntax_vars));
                    } else {
                        substituted.push(subst_syntax_vars(&v[i], syntax_vars));
                    }
                }
                if tree.is_sexpr() {
                    CST::SExpr(substituted, pos.clone())
                } else {
                    CST::List(substituted, pos.clone())
                }
            }
        }
        _ => tree.clone(),
    }
}

/// A pattern to be match against `CST` as part of macro expansion.
#[derive(Clone, Debug, PartialEq, Eq)]
enum MacroPattern<'src> {
    Ident(&'src str),
    SExpr(Vec<MacroPattern<'src>>),
    List(Vec<MacroPattern<'src>>),
}
impl<'src> MacroPattern<'src> {
    /// Construct a new `MacroPattern` from a syntax tree
    fn parse(tree: &CST<'src>, literals: &HashSet<&'src str>) -> Self {
        match *tree {
            CST::Ident(ident, _) => MacroPattern::Ident(ident),
            CST::SExpr(ref v, ref pos) | CST::List(ref v, ref pos) => {
                let patts = v.iter()
                             .map(|e| MacroPattern::parse(e, literals))
                             .collect::<Vec<_>>();

                if unambiguous_sequences(&patts, literals) {
                    if tree.is_sexpr() {
                        MacroPattern::SExpr(patts)
                    } else {
                        MacroPattern::List(patts)
                    }

                } else {
                    pos.error("Ambiguous pattern")
                }
            }
            _ => tree.pos().error("Expected list or ident"),
        }
    }

    /// Try to get the pattern as an `Ident`
    fn get_ident(&self) -> Option<&'src str> {
        match *self {
            MacroPattern::Ident(ident) => Some(ident),
            _ => None,
        }
    }

    /// Returns whether this pattern contains any syntax literal
    fn contains_any_literal(&self, literals: &HashSet<&str>) -> bool {
        match *self {
            MacroPattern::Ident(ident) => literals.contains(ident),
            MacroPattern::List(ref v) | MacroPattern::SExpr(ref v) => {
                v.iter().any(|p| p.contains_any_literal(literals))
            }
        }
    }

    /// Return a set of the bindings of all syntax variables that are produced
    /// when this pattern is matched
    fn variable_names(&self, literals: &HashSet<&'src str>) -> HashSet<&'src str> {
        match *self {
            MacroPattern::Ident(id) => {
                if literals.contains(id) {
                    HashSet::new()
                } else {
                    once(id).collect()
                }
            }
            MacroPattern::List(ref v) | MacroPattern::SExpr(ref v) => {
                v.iter().flat_map(|p| p.variable_names(literals)).collect()
            }
        }
    }

    /// Match a syntax tree against this macro pattern, returning a map of the bound syntax items
    /// if the pattern was a match
    fn match_(&self,
              tree: &CST<'src>,
              literals: &HashSet<&'src str>)
              -> Option<HashMap<&'src str, CST<'src>>> {
        match *tree {
            CST::SExpr(ref sexpr, ref pos) => self.match_sexpr(sexpr, pos, literals),
            CST::List(ref list, ref pos) => self.match_list(list, pos, literals),
            CST::Ident(ident, ref pos) => self.match_ident(ident, pos, literals),
            _ => self.match_lit(tree, literals),
        }
    }

    /// Match an identifier against this macro pattern, returning a map of the
    /// binding of the identifier if the pattern was a match
    fn match_ident(&self,
                   ident: &'src str,
                   pos: &SrcPos<'src>,
                   literals: &HashSet<&'src str>)
                   -> Option<HashMap<&'src str, CST<'src>>> {
        self.get_ident().and_then(|p_ident| {
            if literals.contains(p_ident) {
                if p_ident == ident {
                    Some(HashMap::new())
                } else {
                    None
                }
            } else {
                Some(once((p_ident, CST::Ident(ident, pos.clone()))).collect())
            }
        })
    }

    /// Match a numeric or string literal against this macro pattern, returning a map of the binding
    /// of the literal if the pattern was a match
    fn match_lit(&self,
                 lit: &CST<'src>,
                 literals: &HashSet<&str>)
                 -> Option<HashMap<&'src str, CST<'src>>> {
        match *self {
            MacroPattern::Ident(p_ident) if !literals.contains(p_ident) => {
                Some(once((p_ident, lit.clone())).collect())
            }
            _ => None,
        }
    }

    /// Match a parentheses list, i.e. an s-expression, against this macro pattern, returning a map
    /// of the binding of the sexpr if the pattern was a match
    fn match_sexpr(&self,
                   sexpr: &[CST<'src>],
                   pos: &SrcPos<'src>,
                   literals: &HashSet<&'src str>)
                   -> Option<HashMap<&'src str, CST<'src>>> {
        match *self {
            MacroPattern::Ident(p_ident) if !literals.contains(p_ident) => {
                Some(once((p_ident, CST::SExpr(sexpr.into(), pos.clone()))).collect())
            }
            MacroPattern::SExpr(ref p_sexpr) => match_all(p_sexpr, sexpr, pos, literals),
            _ => None,
        }
    }

    /// Match a bracket list, i.e. a syntax list, against this macro pattern, returning a map
    /// of the binding of the list if the pattern was a match
    fn match_list(&self,
                  list: &[CST<'src>],
                  pos: &SrcPos<'src>,
                  literals: &HashSet<&'src str>)
                  -> Option<HashMap<&'src str, CST<'src>>> {
        match *self {
            MacroPattern::Ident(p_ident) if !literals.contains(p_ident) => {
                Some(once((p_ident, CST::List(list.into(), pos.clone()))).collect())
            }
            MacroPattern::List(ref p_list) => match_all(p_list, list, pos, literals),
            _ => None,
        }
    }

    /// Match all syntax trees in `seq` against the same pattern to produce sequence syntax vars
    /// of all the matches
    /// E.g. matching `(1 2 3) (4 5 6) (7 8 9)` against `(a b c)` produces the syntax vars:
    /// `a = [1, 4, 7], b = [2, 5, 8], c = [3, 6, 9]`
    /// Returns a map of the sequence vars if all trees in `seq` matched `self`.
    fn sequence_match<'csts>(&self,
                             seq: &'csts [CST<'src>],
                             seq_pos: &SrcPos<'src>,
                             literals: &HashSet<&'src str>)
                             -> Option<HashMap<&'src str, CST<'src>>> {
        let mut vars = self.variable_names(literals)
                           .into_iter()
                           .map(|v| (v, Vec::new()))
                           .collect::<HashMap<_, _>>();

        for maybe_matched in seq.iter().map(|tree| self.match_(tree, literals)) {
            if let Some(matched) = maybe_matched {
                for (binding, val) in matched {
                    vars.get_mut(&binding)
                        .unwrap_or_else(|| {
                            unreachable!("ICE: sequence_match: Binding not produced by \
                                          `variable_names` occured in matched pattern")
                        })
                        .push(val)
                }
            } else {
                return None;
            }
        }

        Some(vars.into_iter()
                 .map(|(k, v)| {
                     let pos = v.last()
                                .map(|last| v[0].pos().to(last.pos()))
                                .unwrap_or(seq_pos.clone());
                     (k, CST::List(v, pos))
                 })
                 .collect())
    }

    /// Match multiple syntax trees against the same pattern to produce sequence syntax vars
    /// of all the matches.
    /// Keeps matching elements in `seq` until an element matches `delim`.
    /// Returns a map of the sequence vars and the remaining `CST`s if some tree matched `delim` and
    /// all trees in `seq` until then matched `self`.
    fn sequence_match_until_delim<'csts>
        (&self,
         delim: &MacroPattern<'src>,
         seq: &'csts [CST<'src>],
         seq_pos: &SrcPos<'src>,
         literals: &HashSet<&'src str>)
         -> Option<(HashMap<&'src str, CST<'src>>, &'csts [CST<'src>])> {
        let mut seq_vars = HashMap::<_, Vec<CST>>::new();

        for i in 0..seq.len() {
            if let Some(matched_delim_map) = delim.match_(&seq[i], literals) {
                let mut vars = seq_vars.into_iter()
                                       .map(|(k, v)| {
                                           let pos = v.last()
                                                      .map(|last| v[0].pos().to(last.pos()))
                                                      .unwrap_or(seq_pos.clone());
                                           (k, CST::List(v, pos))
                                       })
                                       .collect::<HashMap<_, _>>();
                for (k, v) in matched_delim_map.into_iter() {
                    if vars.insert(k, v).is_some() {
                        return None;
                    }
                }

                return Some((vars, &seq[i + 1..]));
            } else if let Some(matched_self_map) = self.match_(&seq[i], literals) {
                for (binding, val) in matched_self_map {
                    if seq_vars.contains_key(&binding) {
                        seq_vars.get_mut(&binding).unwrap().push(val)
                    } else {
                        seq_vars.insert(binding, vec![val]);
                    }
                }
            } else {
                return None;
            }
        }

        None
    }
}
impl<'src> fmt::Display for MacroPattern<'src> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            MacroPattern::Ident(s) => write!(f, "{}", s),
            MacroPattern::SExpr(ref v) => {
                write!(f,
                       "({})",
                       v.iter().map(|e| e.to_string()).intersperse(" ".into()).collect::<String>())
            }
            MacroPattern::List(ref v) => {
                write!(f,
                       "[{}]",
                       v.iter().map(|e| e.to_string()).intersperse(" ".into()).collect::<String>())
            }
        }
    }
}

/// Whether all sequences in `patts` are unambiguously matchable
fn unambiguous_sequences(patts: &[MacroPattern], literals: &HashSet<&str>) -> bool {
    if patts.get(0) == Some(&MacroPattern::Ident("macro-escape")) {
        // The pattern is a macro escape. Do not attempt to analyze it
        return true;
    }

    let mut ambiguous_sequence = false;

    for patt in patts {
        match *patt {
            MacroPattern::Ident("...") => {
                if ambiguous_sequence {
                    // If the pattern contains two sequences not separated by a literal, there are
                    // multiple matching options for the pattern, which is, as such, ambiguous.
                    return false;
                } else {
                    ambiguous_sequence = true
                }
            }
            // A literal delimits a sequence and allows a following sequence
            // without causing matching ambiguity
            MacroPattern::Ident(ident) if literals.contains(ident) => ambiguous_sequence = false,
            _ => (),
        }
    }
    true
}

/// Match a sequence of syntax trees against a sequence of macro patterns.
/// If the pattern sequence is a match, return a map of the bound syntax vars
fn match_all<'src>(mut patts: &[MacroPattern<'src>],
                   mut args: &[CST<'src>],
                   pos: &SrcPos<'src>,
                   literals: &HashSet<&'src str>)
                   -> Option<HashMap<&'src str, CST<'src>>> {
    let mut map = HashMap::new();

    while patts.len() > 0 {
        if patts.get(1) == Some(&MacroPattern::Ident("...")) {
            if patts.get(2).map(|p| p.contains_any_literal(literals)).unwrap_or(false) {
                // Sequence followed by pattern containing a literal.
                // Match until the at least partially literal pattern is encountered

                if let Some((m, rest)) = patts[0].sequence_match_until_delim(&patts[2],
                                                                             args,
                                                                             pos,
                                                                             literals) {
                    map.extend(m);

                    patts = &patts[3..];
                    args = rest;
                } else {
                    return None;
                }
            } else {
                // Sequence not followed by any kind of literal. Match until there are
                // as many args left as there are patterns following this sequence

                let n_following_patts = patts.len() - 2;

                if args.len() < n_following_patts {
                    // Too few args left
                    return None;
                } else if let Some(m) = patts[0].sequence_match(&args[..args.len() - n_following_patts],
                                                         pos,
                                                         literals) {
                    map.extend(m);

                    patts = &patts[2..];
                    args = &args[args.len() - n_following_patts..];
                } else {
                    return None;
                }
            }
        } else {
            if let Some(m) = args.get(0).and_then(|arg| patts[0].match_(arg, literals)) {
                map.extend(m);

                patts = &patts[1..];
                args = &args[1..];
            } else {
                return None;
            }
        }
    }

    if args.len() == 0 {
        Some(map)
    } else {
        None
    }
}

/// A definition of a macro through a series of rules, which are pattern matching cases.
#[derive(Clone, Debug)]
struct Macro<'src> {
    literals: HashSet<&'src str>,
    rules: Vec<(MacroPattern<'src>, CST<'src>)>,
}
impl<'src> Macro<'src> {
    /// Apply a macro to some argumens
    fn apply_to(&self,
                args: &[CST<'src>],
                pos: &SrcPos<'src>,
                macros: &mut HashMap<&'src str, Macro<'src>>)
                -> Option<CST<'src>> {
        for &(ref pattern, ref template) in &self.rules {
            if let Some(bound) = pattern.match_list(args, &pos, &self.literals) {
                let mut template = subst_syntax_vars(template, &bound);

                template.add_expansion_site(pos);

                return expand_cst_macros(&template, macros);
            }
        }
        pos.error(format!("No rule matched the argument CSTs `{}`, in macro invocation",
                          args.iter()
                              .map(|e| e.to_string())
                              .intersperse(" ".into())
                              .collect::<String>()))
    }
}

/// Parse a syntax tree as a bracket-list of identifiers to interpret as syntax literals
fn parse_syntax_literals<'src>(cst: &CST<'src>) -> HashSet<&'src str> {
    match *cst {
        CST::List(ref lits, _) => {
            lits.iter()
                .map(|item| {
                    match *item {
                        CST::Ident(lit, _) => lit,
                        _ => item.pos().error("Expected literal identifier"),
                    }
                })
                .collect()
        }
        _ => cst.pos().error("Expected list"),
    }
}

/// Parse a list of syntax trees as pairs of macro patterns and templates
fn parse_syntax_rules<'src>(rules_parts: &[CST<'src>],
                            literals: &HashSet<&'src str>)
                            -> Vec<(MacroPattern<'src>, CST<'src>)> {
    let mut rules = Vec::new();

    for rule_parts in rules_parts {
        if let CST::List(ref rule, ref pos) = *rule_parts {
            // TODO: Make this variadic. Capture everything after the pattern as templates
            if let (Some(pattern), Some(template)) = (rule.get(0), rule.get(1)) {
                rules.push((MacroPattern::parse(pattern, literals), template.clone()))
            } else {
                pos.error("Expected pattern and template")
            }
        } else {
            rule_parts.pos().error("Expected list")
        }
    }

    rules
}

/// Parse and define a `Macro` from given `parts`
fn define_macro<'src>(parts: &[CST<'src>],
                      pos: &SrcPos<'src>,
                      macros: &mut HashMap<&'src str, Macro<'src>>) {
    let name = if let Some(name_tree) = parts.get(0) {
        match *name_tree {
            CST::Ident(name, _) => name,
            _ => name_tree.pos().error("Expected identifier"),
        }
    } else {
        pos.error("Name missing in macro definition")
    };

    let literals = parse_syntax_literals(parts.get(1).unwrap_or_else(|| {
        pos.error("Literals list missing in macro definition")
    }));

    let rules = parse_syntax_rules(&parts[2..], &literals);

    if macros.insert(name,
                     Macro {
                         literals: literals,
                         rules: rules,
                     })
             .is_some() {
        pos.error(format!("Duplicate definition of macro `{}`", name))
    }
}

/// Visit a syntax tree, expanding it if it's a macro invocation,
/// defining it if it's a macro definition, and leaving it unchanged otherwise.
/// Returns `None` if expansion resulted in nothing, or `cst` was a macro definition
fn expand_cst_macros<'src>(cst: &CST<'src>,
                           macros: &mut HashMap<&'src str, Macro<'src>>)
                           -> Option<CST<'src>> {
    match *cst {
        CST::SExpr(ref sexpr, ref pos) => {
            if let Some((head, tail)) = sexpr.split_first() {
                match *head {
                    CST::Ident("quote", _) => Some(cst.clone()),
                    CST::Ident("def-macro", _) => {
                        define_macro(tail, pos, macros);
                        None
                    }
                    CST::Ident(name, _) if macros.contains_key(name) => {
                        // The s-expression is a macro invocation
                        let macro_rules = macros[name].clone();

                        macro_rules.apply_to(tail, pos, macros)
                    }
                    _ => {
                        Some(CST::SExpr(sexpr.iter()
                                             .filter_map(|arg| expand_cst_macros(arg, macros))
                                             .collect(),
                                        pos.clone()))
                    }
                }
            } else {
                Some(cst.clone())
            }
        }
        _ => Some(cst.clone()),
    }
}

/// Define and expand all macros in `trees`
pub fn expand_macros<'src>(trees: &[CST<'src>]) -> Vec<CST<'src>> {
    let mut macros = HashMap::new();

    trees.iter().filter_map(|item| expand_cst_macros(item, &mut macros)).collect()
}
