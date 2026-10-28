// SPDX-License-Identifier: MPL-2.0
//! A deliberately small, perfectly bound signature AST.
//!
//! Puggle's decoder targets signatures rather than arbitrary proof terms. The
//! AST is target-independent, but validation is target-specific: dependent
//! variables are legal in Lean and Agda and rejected in SMT-LIB 2.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TargetProver {
    Lean,
    Agda,
    Smt2,
}

impl TargetProver {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lean => "lean",
            Self::Agda => "agda",
            Self::Smt2 => "smt2",
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum Sort {
    /// A previously declared sort constant, such as `Nat`, `Int`, or `Bool`.
    Named(String),
    /// A reference to an earlier binder in this signature.
    Variable(String),
    /// A higher-order sort. SMT-LIB signatures reject this constructor.
    Function(Box<Sort>, Box<Sort>),
}

impl Sort {
    #[must_use]
    pub fn named(name: impl Into<String>) -> Self {
        Self::Named(name.into())
    }

    #[must_use]
    pub fn variable(name: impl Into<String>) -> Self {
        Self::Variable(name.into())
    }

    #[must_use]
    pub fn function(domain: Sort, codomain: Sort) -> Self {
        Self::Function(Box::new(domain), Box::new(codomain))
    }

    pub(crate) fn validate(
        &self,
        target: TargetProver,
        scope: &BTreeSet<String>,
    ) -> Result<(), AstError> {
        match self {
            Self::Named(name) => validate_identifier(name, "sort"),
            Self::Variable(name) => {
                validate_identifier(name, "sort variable")?;
                if target == TargetProver::Smt2 {
                    return Err(AstError::UnsupportedDependentSort);
                }
                if scope.contains(name) {
                    Ok(())
                } else {
                    Err(AstError::UnboundVariable(name.clone()))
                }
            }
            Self::Function(domain, codomain) => {
                if target == TargetProver::Smt2 {
                    return Err(AstError::UnsupportedFunctionSort);
                }
                domain.validate(target, scope)?;
                codomain.validate(target, scope)
            }
        }
    }

    pub(crate) fn relational_term(
        &self,
        bindings: &std::collections::BTreeMap<String, crate::kanren::LVar>,
    ) -> Result<crate::kanren::Term, AstError> {
        use crate::kanren::Term;

        match self {
            Self::Named(name) => Ok(Term::node("named", [Term::atom(name)])),
            Self::Variable(name) => bindings
                .get(name)
                .map(|variable| Term::node("variable", [Term::var(*variable)]))
                .ok_or_else(|| AstError::UnboundVariable(name.clone())),
            Self::Function(domain, codomain) => Ok(Term::node(
                "function",
                [
                    domain.relational_term(bindings)?,
                    codomain.relational_term(bindings)?,
                ],
            )),
        }
    }

    fn canonical_into(&self, output: &mut String) {
        match self {
            Self::Named(name) => {
                output.push('n');
                push_field(output, name);
            }
            Self::Variable(name) => {
                output.push('v');
                push_field(output, name);
            }
            Self::Function(domain, codomain) => {
                output.push_str("f(");
                domain.canonical_into(output);
                output.push(',');
                codomain.canonical_into(output);
                output.push(')');
            }
        }
    }

    #[must_use]
    pub fn canonical(&self) -> String {
        let mut output = String::new();
        self.canonical_into(&mut output);
        output
    }

    fn render(&self, target: TargetProver) -> String {
        match self {
            Self::Named(name) | Self::Variable(name) => name.clone(),
            Self::Function(domain, codomain) => {
                let domain = domain.render(target);
                let codomain = codomain.render(target);
                match target {
                    TargetProver::Lean | TargetProver::Agda => {
                        format!("({domain}) -> ({codomain})")
                    }
                    TargetProver::Smt2 => unreachable!("SMT-LIB rejects function sorts"),
                }
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    name: String,
    sort: Sort,
}

impl Binding {
    #[must_use]
    pub fn new(name: impl Into<String>, sort: Sort) -> Self {
        Self {
            name: name.into(),
            sort,
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn sort(&self) -> &Sort {
        &self.sort
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormalAst {
    target: TargetProver,
    name: String,
    bindings: Vec<Binding>,
    result: Sort,
}

impl FormalAst {
    pub fn new(
        target: TargetProver,
        name: impl Into<String>,
        bindings: Vec<Binding>,
        result: Sort,
    ) -> Result<Self, AstError> {
        let ast = Self {
            target,
            name: name.into(),
            bindings,
            result,
        };
        ast.validate()?;
        Ok(ast)
    }

    pub fn validate(&self) -> Result<(), AstError> {
        validate_declaration_identifier(&self.name, "declaration")?;
        let mut scope = BTreeSet::new();
        for binding in &self.bindings {
            validate_declaration_identifier(&binding.name, "binder")?;
            if !scope.insert(binding.name.clone()) {
                return Err(AstError::DuplicateBinder(binding.name.clone()));
            }
            // A binder is not in scope in its own sort. It is available to all
            // subsequent binders and to the result, which prevents self- and
            // forward-reference scope leaks.
            scope.remove(&binding.name);
            binding.sort.validate(self.target, &scope)?;
            scope.insert(binding.name.clone());
        }
        self.result.validate(self.target, &scope)
    }

    #[must_use]
    pub const fn target(&self) -> TargetProver {
        self.target
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    #[must_use]
    pub const fn result(&self) -> &Sort {
        &self.result
    }

    /// Canonical, unambiguous bytes represented as UTF-8. Every variable-size
    /// field is byte-length-prefixed, so concatenation cannot alias two ASTs.
    #[must_use]
    pub fn canonical(&self) -> String {
        let mut output = String::from("puggle-ast-v1;");
        push_field(&mut output, self.target.as_str());
        push_field(&mut output, &self.name);
        let _ = write!(output, "bindings:{};", self.bindings.len());
        for binding in &self.bindings {
            push_field(&mut output, &binding.name);
            push_field(&mut output, &binding.sort.canonical());
        }
        output.push_str("result:");
        push_field(&mut output, &self.result.canonical());
        output
    }

    #[must_use]
    pub fn digest(&self) -> [u8; 32] {
        *blake3::hash(self.canonical().as_bytes()).as_bytes()
    }

    /// Render a declaration, never an unchecked source fragment supplied by a
    /// model. Identifiers were conservatively validated during construction.
    #[must_use]
    pub fn render(&self) -> String {
        match self.target {
            TargetProver::Lean => {
                let telescope = self
                    .bindings
                    .iter()
                    .map(|binding| {
                        format!("({} : {})", binding.name, binding.sort.render(self.target))
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                let separator = if telescope.is_empty() { "" } else { " " };
                format!(
                    "axiom {}{separator}{} : {}",
                    self.name,
                    telescope,
                    self.result.render(self.target)
                )
            }
            TargetProver::Agda => {
                let mut rendered = format!("postulate\n  {} : ", self.name);
                for binding in &self.bindings {
                    let _ = write!(
                        rendered,
                        "({} : {}) -> ",
                        binding.name,
                        binding.sort.render(self.target)
                    );
                }
                rendered.push_str(&self.result.render(self.target));
                rendered
            }
            TargetProver::Smt2 => {
                let sorts = self
                    .bindings
                    .iter()
                    .map(|binding| binding.sort.render(self.target))
                    .collect::<Vec<_>>()
                    .join(" ");
                format!(
                    "(declare-fun {} ({}) {})",
                    self.name,
                    sorts,
                    self.result.render(self.target)
                )
            }
        }
    }
}

fn push_field(output: &mut String, value: &str) {
    let _ = write!(output, "{}:{};", value.len(), value);
}

pub(crate) fn validate_identifier(value: &str, kind: &'static str) -> Result<(), AstError> {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return Err(AstError::InvalidIdentifier {
            kind,
            value: value.to_owned(),
        });
    };
    if !(first.is_ascii_alphabetic() || first == '_')
        || !characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return Err(AstError::InvalidIdentifier {
            kind,
            value: value.to_owned(),
        });
    }
    Ok(())
}

pub(crate) fn validate_declaration_identifier(
    value: &str,
    kind: &'static str,
) -> Result<(), AstError> {
    validate_identifier(value, kind)?;
    if RESERVED.contains(&value) {
        return Err(AstError::InvalidIdentifier {
            kind,
            value: value.to_owned(),
        });
    }
    Ok(())
}

const RESERVED: &[&str] = &[
    "Type",
    "Prop",
    "Set",
    "axiom",
    "def",
    "theorem",
    "forall",
    "fun",
    "where",
    "data",
    "postulate",
    "module",
    "declare-fun",
    "assert",
    "check-sat",
];

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum AstError {
    #[error("invalid {kind} identifier {value:?}")]
    InvalidIdentifier { kind: &'static str, value: String },
    #[error("duplicate binder {0:?}")]
    DuplicateBinder(String),
    #[error("unbound variable {0:?}")]
    UnboundVariable(String),
    #[error("SMT-LIB 2 does not support dependent sort variables here")]
    UnsupportedDependentSort,
    #[error("SMT-LIB 2 declare-fun does not support higher-order sorts")]
    UnsupportedFunctionSort,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_forward_and_unbound_references() {
        let error = FormalAst::new(
            TargetProver::Lean,
            "bad",
            vec![Binding::new("x", Sort::variable("A"))],
            Sort::named("Nat"),
        )
        .expect_err("A is not bound");
        assert_eq!(error, AstError::UnboundVariable("A".to_owned()));
    }

    #[test]
    fn accepts_a_dependent_lean_signature() {
        let ast = FormalAst::new(
            TargetProver::Lean,
            "identity",
            vec![
                Binding::new("A", Sort::named("Type")),
                Binding::new("x", Sort::variable("A")),
            ],
            Sort::variable("A"),
        )
        .expect("valid dependent signature");
        assert_eq!(ast.render(), "axiom identity (A : Type) (x : A) : A");
        assert_eq!(ast.digest().len(), 32);
    }

    #[test]
    fn canonical_form_distinguishes_field_boundaries() {
        let left = FormalAst::new(
            TargetProver::Agda,
            "ab",
            vec![Binding::new("c", Sort::named("D"))],
            Sort::named("E"),
        )
        .expect("valid AST");
        let right = FormalAst::new(
            TargetProver::Agda,
            "a",
            vec![Binding::new("bc", Sort::named("D"))],
            Sort::named("E"),
        )
        .expect("valid AST");
        assert_ne!(left.canonical(), right.canonical());
    }

    #[test]
    fn smt2_renders_only_first_order_signatures() {
        let ast = FormalAst::new(
            TargetProver::Smt2,
            "plus",
            vec![
                Binding::new("x", Sort::named("Int")),
                Binding::new("y", Sort::named("Int")),
            ],
            Sort::named("Int"),
        )
        .expect("valid SMT-LIB signature");
        assert_eq!(ast.render(), "(declare-fun plus (Int Int) Int)");
    }
}
