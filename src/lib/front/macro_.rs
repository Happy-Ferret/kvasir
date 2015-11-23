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

use std::collections::{HashMap, HashSet};
use std::iter::once;

use super::SrcPos;
use super::lex::{TokenTree, TokenTreeMeta};

type Macros<'src> = HashMap<&'src str, MacroRules<'src>>;
type SyntaxVars<'src> = HashMap<&'src str, TokenTreeMeta<'src>>;

impl<'src> TokenTreeMeta<'src> {
    /// Returns the length of the longest list pointed to by a syntax var in the tree `self`.
    /// Returns `None` if no syntax var points to a list
    fn max_syntax_var_len(&self, syntax_vars: &SyntaxVars<'src>) -> Option<usize> {
        match self.tt {
            TokenTree::Ident(ident) => syntax_vars.get(ident).and_then(|t| t.list_len()),
            TokenTree::List(ref list) => {
                list.iter()
                    .filter_map(|li| li.max_syntax_var_len(syntax_vars))
                    .max()
            }
            _ => None,
        }
    }

    /// Substitute sequence syntax vars in tree for the element in the sequence at index `i`
    ///
    /// If no element exists at index `i`, use the last element in the sequence instead
    fn subst_syntax_vars_iteration(&self,
                                   i: usize,
                                   syntax_vars: &SyntaxVars<'src>)
                                   -> Option<Self> {
        match self.tt {
            TokenTree::Ident(ident) => {
                syntax_vars.get(ident)
                           .map(|subst| {
                               match subst.tt {
                                   TokenTree::List(ref substs_list) if substs_list.len() <
                                                                       (i + 1) => {
                                       substs_list.last().cloned()
                                   }
                                   TokenTree::List(ref substs_list) => Some(substs_list[i].clone()),
                                   _ => Some(subst.clone()),
                               }
                           })
                           .unwrap_or(Some(self.clone()))
            }
            TokenTree::List(ref list) => {
                Some(TokenTreeMeta::new_list(list.iter()
                                                 .filter_map(|li| {
                                                     li.subst_syntax_vars_iteration(i, syntax_vars)
                                                 })
                                                 .collect(),
                                             self.pos.clone()))
            }
            _ => Some(self.clone()),
        }
    }

    /// Flatten and expression containing items matched against sequences
    ///
    /// ```
    /// (c1 and c2) ... // where c1 is the list `(1 2 3)` and c2 is the list `(a b c)`
    /// // expands to:
    /// (1 and a) (2 and b) (3 and c)
    /// ```
    fn flatten(&self, syntax_vars: &SyntaxVars<'src>) -> Vec<Self> {
        match self.max_syntax_var_len(syntax_vars) {
            Some(max) => {
                (0..max)
                    .filter_map(|i| self.subst_syntax_vars_iteration(i, syntax_vars))
                    .collect()
            }
            None => self.pos.error("Token tree contained no sequence syntax variables"),
        }
    }

    fn subst_syntax_vars(&self, syntax_vars: &SyntaxVars<'src>) -> Self {
        match self.tt {
            TokenTree::Ident(ident) => syntax_vars.get(ident).unwrap_or(self).clone(),
            TokenTree::List(ref list) if !list.is_empty() => {
                if let TokenTree::Ident("...") = list[0].tt {
                    // It's an escape
                    if list.len() != 2 {
                        self.pos.error(format!("Arity mismatch. Expected 1, found {}", list.len()));
                    }
                    list[1].clone()
                } else {
                    let mut substituted = Vec::new();

                    for i in (0..list.len()).filter(|&i| list[i].tt != TokenTree::Ident("...")) {
                        if i + 1 < list.len() && list[i + 1].tt == TokenTree::Ident("...") {
                            // syntax var followed by ellipsis, try to flatten it as a sequence
                            substituted.extend(list[i].flatten(syntax_vars));
                        } else {
                            substituted.push(list[i].subst_syntax_vars(syntax_vars));
                        }
                    }
                    TokenTreeMeta::new_list(substituted, self.pos.clone())
                }
            }
            _ => self.clone(),
        }
    }
}

fn unambiguous_sequences(patts: &[MacroPattern], literals: &HashSet<&str>) -> bool {
    if !patts.is_empty() && patts[0] == MacroPattern::Ident("...") {
        // Escape
        return true;
    }

    let mut ambiguous_sequence = false;

    for patt in patts {
        match *patt {
            MacroPattern::Ident("...") if !ambiguous_sequence => ambiguous_sequence = true,
            MacroPattern::Ident("...") => return false,
            MacroPattern::Ident(ident) if literals.contains(ident) => ambiguous_sequence = false,
            _ => (),
        }
    }
    true
}

/// A pattern to be matched against a `TokenTree` as part of macro expansion.
///
/// A `MacroPattern` created as part of a macro definition is guaranteed to be valid
#[derive(Clone, Debug, PartialEq, Eq)]
enum MacroPattern<'src> {
    Ident(&'src str),
    List(Vec<MacroPattern<'src>>),
}
impl<'src> MacroPattern<'src> {
    /// Construct a new, valid `MacroPattern` corresponding to a `TokenTree`
    fn new(ttm: &TokenTreeMeta<'src>, literals: &HashSet<&'src str>) -> Self {
        match ttm.tt {
            TokenTree::Ident(ident) => MacroPattern::Ident(ident),
            TokenTree::List(ref list) => {
                let patts = list.iter()
                                .map(|li| MacroPattern::new(li, literals))
                                .collect::<Vec<_>>();

                if unambiguous_sequences(&patts, literals) {
                    MacroPattern::List(patts)
                } else {
                    ttm.pos.error("Ambiguous pattern")
                }
            }
            _ => ttm.pos.error("Expected list or ident"),
        }
    }

    fn get_ident(&self) -> Option<&str> {
        match *self {
            MacroPattern::Ident(ident) => Some(ident),
            _ => None,
        }
    }

    /// Bind the `TokenTree`, `arg`, to the pattern `self`
    ///
    /// If pattern matched, return the bound pattern
    fn bind(&self,
            arg: &TokenTreeMeta<'src>,
            literals: &HashSet<&'src str>)
            -> Option<SyntaxVars<'src>> {
        match arg.tt {
            TokenTree::List(ref args) => self.bind_sequence(args, &arg.pos, literals),
            TokenTree::Ident(ident) => {
                match *self {
                    MacroPattern::Ident(pi) if literals.contains(pi) && pi == ident => {
                        Some(HashMap::new())
                    }
                    MacroPattern::Ident(pi) if literals.contains(pi) => None,
                    MacroPattern::Ident(pi) => Some(once((pi, arg.clone())).collect()),
                    _ => None,
                }
            }
            _ => {
                match *self {
                    MacroPattern::Ident(pi) if literals.contains(pi) => None,
                    MacroPattern::Ident(pi) => Some(once((pi, arg.clone())).collect()),
                    _ => None,
                }
            }
        }
    }

    // TODO: Make this and some other stuff take references/slices
    // `pos` is a fallback in case `args` is empty
    fn bind_sequence(&self,
                     args: &[TokenTreeMeta<'src>],
                     pos: &SrcPos<'src>,
                     literals: &HashSet<&'src str>)
                     -> Option<SyntaxVars<'src>> {
        let mut map = HashMap::new();

        let pos_interval = if args.is_empty() {
            pos.clone()
        } else {
            let mut pos_interval = args[0].pos.clone();
            pos_interval.end = args.last().unwrap().pos.end.clone();
            pos_interval
        };

        match *self {
            MacroPattern::Ident(pi) => {
                if literals.contains(pi) {
                    return None;
                } else {
                    map.insert(pi, TokenTreeMeta::new_list(args.into(), pos_interval));
                }
            }
            MacroPattern::List(ref patts) => {
                let mut args: Vec<_> = args.iter().collect();
                let mut args_i = 0;

                for i in (0..patts.len()).filter(|&i| patts[i] != MacroPattern::Ident("...")) {
                    if patts.get(i + 1) == Some(&MacroPattern::Ident("...")) {
                        // Followed by ellipsis. It's a repeating sequence
                        let len_til_end = patts[i + 2..]
                                              .iter()
                                              .position(|pat| {
                                                  pat.get_ident()
                                                     .map(|id| literals.contains(id))
                                                     .unwrap_or(false)
                                              })
                                              .map(|j| j + i + 2)
                                              .unwrap_or(patts.len());
                        let keep = len_til_end - (i + 2);

                        let mut to_bind = Vec::new();

                        while let Some(arg) = args.get(args_i) {
                            if len_til_end != patts.len() &&
                               arg.tt.get_ident() == patts[len_til_end].get_ident() {
                                break;
                            } else {
                                to_bind.push(args[args_i]);
                                args_i += 1;
                            }
                        }
                        if keep > to_bind.len() {
                            return None;
                        }

                        let keep_split_pos = to_bind.len() - keep;
                        args = to_bind.split_off(keep_split_pos)
                                      .into_iter()
                                      .chain(args[args_i..].iter().cloned())
                                      .collect();
                        args_i = 0;

                        match patts[i].bind_sequence(&to_bind.iter()
                                                             .map(|&e| e.clone())
                                                             .collect::<Vec<_>>(),
                                                     pos,
                                                     literals) {
                            Some(bound) => map.extend(bound),
                            None => return None,
                        }
                    } else {
                        match args.get(args_i).and_then(|arg| patts[i].bind(arg, literals)) {
                            Some(bound) => map.extend(bound),
                            None => return None,
                        }
                        args_i += 1;
                    }
                }
                if args.len() > args_i {
                    return None;
                }
            }
        }
        Some(map)
    }
}

/// A definition of a macro through a series of rules, which are pattern matching cases.
#[derive(Clone, Debug)]
struct MacroRules<'src> {
    literals: HashSet<&'src str>,
    rules: Vec<(MacroPattern<'src>, TokenTreeMeta<'src>)>,
}
impl<'src> MacroRules<'src> {
    /// Construct a new `MacroRules` structure from token trees representing literals and rules
    fn new(maybe_literals: &TokenTreeMeta<'src>,
           maybe_rules: &[TokenTreeMeta<'src>])
           -> MacroRules<'src> {
        let literals = match maybe_literals.tt {
            TokenTree::List(ref lits) => {
                lits.iter()
                    .map(|item| {
                        match item.tt {
                            TokenTree::Ident(lit) => lit,
                            _ => item.pos.error("Expected literal identifier"),
                        }
                    })
                    .collect()
            }
            _ => maybe_literals.pos.error("Expected list"),
        };

        let mut rules = Vec::new();

        for maybe_rule in maybe_rules {
            if let TokenTree::List(ref rule) = maybe_rule.tt {
                // TODO: Make this variadic. Capture everything after the pattern as templates
                if rule.len() != 2 {
                    maybe_rule.pos.error("Expected pattern and template")
                }

                rules.push((MacroPattern::new(&rule[0], &literals), rule[1].clone()))
            } else {
                maybe_rule.pos.error("Expected list")
            }
        }

        MacroRules {
            literals: literals,
            rules: rules,
        }
    }

    /// Apply a macro to some arguments.
    fn apply_to(&self,
                args: &[TokenTreeMeta<'src>],
                pos: &SrcPos<'src>,
                macros: &mut Macros<'src>)
                -> Option<TokenTreeMeta<'src>> {
        for &(ref pattern, ref template) in &self.rules {
            if let Some(bound) = pattern.bind_sequence(args, &pos, &self.literals) {
                let mut template = template.subst_syntax_vars(&bound);

                template.add_expansion_site(pos);

                return template.expand_macros(macros);
            }
        }
        pos.error("No rule matched in macro invocation")
    }
}

fn define_macro<'src>(parts: &[TokenTreeMeta<'src>],
                      pos: &SrcPos<'src>,
                      macros: &mut HashMap<&'src str, MacroRules<'src>>) {
    let name = if let Some(name_tree) = parts.get(0) {
        match name_tree.tt {
            TokenTree::Ident(name) => name,
            _ => name_tree.pos.error("Expected identifier"),
        }
    } else {
        pos.error("Name missing in macro definition")
    };

    let literals = parts.get(1)
                        .unwrap_or_else(|| pos.error("Literals list missing in macro definition"));

    if macros.insert(name, MacroRules::new(literals, &parts[2..])).is_some() {
        pos.error(format!("Duplicate definition of macro `{}`", name))
    }
}

impl<'src> TokenTreeMeta<'src> {
    /// Returns `None` if expansion resulted in nothing, or token tree was a macro definition
    fn expand_macros(&self, macros: &mut Macros<'src>) -> Option<TokenTreeMeta<'src>> {
        match self.tt {
            TokenTree::List(ref sexpr) => {
                if let Some((head, tail)) = sexpr.split_first() {
                    match head.tt {
                        TokenTree::Ident("quote") => Some(self.clone()),
                        TokenTree::Ident("def-macro") => {
                            define_macro(tail, &self.pos, macros);
                            None
                        }
                        TokenTree::Ident(macro_name) if macros.contains_key(macro_name) => {
                            // The s-expression is a macro call
                            let macro_rules = macros[macro_name].clone();

                            macro_rules.apply_to(tail, &self.pos, macros)
                        }
                        _ => {
                            Some(TokenTreeMeta::new_list(sexpr.iter()
                                                              .filter_map(|arg| {
                                                                  arg.expand_macros(macros)
                                                              })
                                                              .collect(),
                                                         self.pos.clone()))
                        }
                    }
                } else {
                    Some(self.clone())
                }
            }
            _ => Some(self.clone()),
        }
    }
}

pub fn expand_macros<'src>(tts: &[TokenTreeMeta<'src>]) -> Vec<TokenTreeMeta<'src>> {
    let mut macros = Macros::new();
    tts.iter().filter_map(|item| item.expand_macros(&mut macros)).collect()
}
