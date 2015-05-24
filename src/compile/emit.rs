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

//! Emit the AST in some format
//!
//! Which formats should be supported:
//! 	* Rust source code ☑
//! 	* Rust AST ☐
//! 	* LLVM something ☐

use ast::*;

pub trait ToRustSrc {
	fn to_rust_src(&self) -> String;
}

impl ToRustSrc for Path {
	fn to_rust_src(&self) -> String {
		format!("{}{}{}",
			if self.is_absolute() { "::" } else { "" },
			self.parts()[0],
			self.parts()[1..].iter().fold(String::new(), |acc, s| format!("{}::{}", acc, s)))
	}
}

impl ToRustSrc for Type {
	fn to_rust_src(&self) -> String {
		match *self {
			Type::Inferred => "_".to_string(),
			Type::Basic(ref ty) => ty.clone(),
			Type::Construct(ref con, ref args) => format!("{}<{}>",
				con,
				args.iter().fold(String::new(), |acc, ty| format!("{}{},", acc, ty.to_rust_src()))),
			Type::Tuple(ref tys) => format!("({})",
				tys.iter().fold(String::new(), |acc, ty| format!("{}{},", acc, ty.to_rust_src()))),
			Type::Poly(ref ty) => ty.clone(),
		}
	}
}

impl ToRustSrc for TypedBinding {
	fn to_rust_src(&self) -> String {
		format!("{}: {}", self.ident, self.type_sig.to_rust_src())
	}
}

impl ToRustSrc for Use {
	fn to_rust_src(&self) -> String {
		self.paths.iter()
			.fold(String::new(), |acc, ident| format!("{}use {};", acc, ident.to_rust_src()))
	}
}

/// Return the different elements of `it`. Each item in returned vec is unique, no doubles.
fn different_elements<'a, I: Iterator<Item=&'a Type>>(it: I) -> Vec<&'a Type> {
	let mut v: Vec<&Type> = Vec::new();

	for ty in it {
		if !v.contains(&ty) {
			v.push(ty)
		}
	}

	v
}

impl ToRustSrc for ConstDef {
	fn to_rust_src(&self) -> String {
		if let Expr::Lambda(ref lambda) = *self.body.value {
			format!("fn {}<{}>({}) -> {} {{ {} }}",
				self.binding.ident,
				different_elements(lambda.arg_bindings.iter()
						.map(|tb| &tb.type_sig)
						.filter(|ty| ty.is_poly()))
					.into_iter()
					.fold(String::new(), |acc, ty| format!("{}{},", acc, ty.to_rust_src())),
				lambda.arg_bindings.first()
					.map(|first| lambda.arg_bindings.tail()
						.iter()
						.fold(first.to_rust_src(), |acc, bnd|
							format!("{}, {}", acc, bnd.to_rust_src())))
					.unwrap_or("".into()),
				lambda.body.type_.to_rust_src(),
				lambda.body.to_rust_src())
		} else {
			format!("const {}: {} = {};",
				self.binding.ident,
				self.binding.type_sig.to_rust_src(),
				self.body.to_rust_src())
		}
	}
}

impl ToRustSrc for SExpr {
	fn to_rust_src(&self) -> String {
		let func = self.func.to_rust_src();
		match func.as_ref() {
			"+" | "-" | "*" | "/" | ">" | "<" => format!("({}{}{})",
				self.args[0].to_rust_src(),
				func,
				self.args[1].to_rust_src()),
			"=" => format!("({} == {})", self.args[0].to_rust_src(), self.args[1].to_rust_src()),
			_ => format!("{}({})",
				self.func.to_rust_src(),
				self.args.first()
					.map(|first| self.args[1..].iter()
						.fold(first.to_rust_src(), |acc, bnd|
							format!("{}, {}", acc, bnd.to_rust_src())))
					.unwrap_or("".into()))
		}
	}
}

impl ToRustSrc for Block {
	fn to_rust_src(&self) -> String {
		self.exprs.first()
			.map(|first| self.exprs[1..].iter()
				.fold(format!("{{ {}", first.to_rust_src()), |acc, expr|
					format!("{}; {}", acc, expr.to_rust_src())) + "}")
			.unwrap_or("{ }".into())
	}
}

impl ToRustSrc for Cond {
	fn to_rust_src(&self) -> String {
		format!("if {} {{ {} }}{}{}",
			self.clauses[0].0.to_rust_src(),
			self.clauses[0].1.to_rust_src(),
			self.clauses.iter().fold(String::new(), |acc, &(ref cond, ref conseq)|
				format!("{} else if {} {{ {} }}", acc, cond.to_rust_src(), conseq.to_rust_src())),
			self.else_clause.as_ref().map(|conseq| format!(" else {{ {} }}", conseq.to_rust_src()))
				.unwrap_or("".into()),
		)
	}
}

impl ToRustSrc for Lambda {
	fn to_rust_src(&self) -> String {
		format!("|{}| -> {} {{ {} }}",
			self.arg_bindings.first()
				.map(|first| self.arg_bindings[1..].iter()
					.fold(first.to_rust_src(), |acc, bnd|
						format!("{}, {}", acc, bnd.to_rust_src())))
				.unwrap_or("".into()),
			self.body.type_.to_rust_src(),
			self.body.to_rust_src()
		)
	}
}

impl ToRustSrc for VarDef {
	fn to_rust_src(&self) -> String {
		format!("let{} {}: {} = {}",
			if self.mutable { " mut" } else { "" },
			self.binding.ident,
			self.binding.type_sig.to_rust_src(),
			if let Expr::Lambda(ref lambda) = *self.body.value {
				format!("|{}| -> {} {{ {} }}",
					lambda.arg_bindings.first()
						.map(|first| lambda.arg_bindings.tail()
							.iter()
							.fold(first.to_rust_src(), |acc, bnd|
								format!("{}, {}", acc, bnd.to_rust_src())))
						.unwrap_or("".into()),
					lambda.body.type_.to_rust_src(),
					lambda.body.to_rust_src())
			} else {
				self.body.to_rust_src()
			})
	}
}

impl ToRustSrc for Assign {
	fn to_rust_src(&self) -> String {
		format!("{} = {}", self.lvalue.ident, self.rvalue.to_rust_src())
	}
}

impl ToRustSrc for ExprMeta {
	fn to_rust_src(&self) -> String {
		match *self.value {
			Expr::Nil => "()".into(),
			Expr::NumLit(ref s) => s.clone(),
			Expr::StrLit(ref s) => s.clone(),
			Expr::Bool(b) => if b { "true" } else { "false" }.into(),
			Expr::Binding(ref ident) => ident.to_rust_src(),
			Expr::SExpr(ref sexpr) => sexpr.to_rust_src(),
			Expr::Block(ref block) => block.to_rust_src(),
			Expr::Cond(ref cond) => cond.to_rust_src(),
			Expr::Lambda(ref λ) => λ.to_rust_src(),
			Expr::VarDef(ref def) => def.to_rust_src(),
			Expr::Assign(ref a) => a.to_rust_src(),
		}
	}
}

impl ToRustSrc for AST {
	fn to_rust_src(&self) -> String {
		format!("{}{}",
			self.uses.iter()
				.fold(String::new(), |acc, u| format!("{}{};\n", acc, u.to_rust_src())),
			self.const_defs.iter()
				.fold(String::new(), |acc, def| format!("{}{}\n", acc, def.to_rust_src())))
	}
}

pub fn generate_rust_src(ast: &AST) -> String {
	ast.to_rust_src()
}
