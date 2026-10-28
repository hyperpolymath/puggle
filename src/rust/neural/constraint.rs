// SPDX-License-Identifier: MPL-2.0
//! A relational constraint state for Puggle's signature token events.
//!
//! Tokenizers adapt concrete model tokens to these semantic actions. Every
//! action corresponds to a deterministic AST transition; free-form text never
//! enters the AST without identifier and scope validation.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::ast::{
    validate_declaration_identifier, AstError, Binding, FormalAst, Sort, TargetProver,
};
use crate::kanren::{LVar, State, Term};

use super::TokenId;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TokenAction {
    Start(TargetProver),
    DeclarationName(String),
    BeginBinder,
    BinderName(String),
    Sort(Sort),
    EndBinders,
    Finish,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    Start,
    DeclarationName,
    Binders,
    BinderName,
    BinderSort,
    ResultSort,
    Complete,
    Finished,
}

pub trait SymbolicConstraint: Send {
    /// Speculatively validate a token without changing this state.
    fn validate(&self, token: TokenId) -> Result<(), ConstraintError>;
    /// Commit one token after it has been selected from the masked logits.
    fn commit(&mut self, token: TokenId) -> Result<(), ConstraintError>;
    fn is_complete(&self) -> bool;
}

#[derive(Clone, Debug)]
struct PendingBinder {
    name: String,
    variable: LVar,
}

#[derive(Clone, Debug)]
pub struct KanrenConstraint {
    vocabulary: BTreeMap<TokenId, TokenAction>,
    phase: Phase,
    logic: State,
    target: Option<TargetProver>,
    name: Option<String>,
    bindings: Vec<Binding>,
    bound_variables: BTreeMap<String, LVar>,
    pending: Option<PendingBinder>,
    result: Option<Sort>,
}

impl KanrenConstraint {
    #[must_use]
    pub fn new(vocabulary: BTreeMap<TokenId, TokenAction>) -> Self {
        Self {
            vocabulary,
            phase: Phase::Start,
            logic: State::new(),
            target: None,
            name: None,
            bindings: Vec::new(),
            bound_variables: BTreeMap::new(),
            pending: None,
            result: None,
        }
    }

    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    #[must_use]
    pub const fn logic(&self) -> &State {
        &self.logic
    }

    #[must_use]
    pub fn allowed_tokens(&self) -> Vec<TokenId> {
        self.vocabulary
            .keys()
            .copied()
            .filter(|token| self.validate(*token).is_ok())
            .collect()
    }

    pub fn ast(&self) -> Result<Option<FormalAst>, AstError> {
        if self.phase != Phase::Finished {
            return Ok(None);
        }
        let Some(target) = self.target else {
            return Ok(None);
        };
        let (Some(name), Some(result)) = (self.name.clone(), self.result.clone()) else {
            return Ok(None);
        };
        FormalAst::new(target, name, self.bindings.clone(), result).map(Some)
    }

    fn advance(&self, token: TokenId) -> Result<Self, ConstraintError> {
        let action = self
            .vocabulary
            .get(&token)
            .cloned()
            .ok_or(ConstraintError::UnknownToken(token))?;
        let mut next = self.clone();

        match (self.phase, action) {
            (Phase::Start, TokenAction::Start(target)) => {
                next.target = Some(target);
                next.phase = Phase::DeclarationName;
            }
            (Phase::DeclarationName, TokenAction::DeclarationName(name)) => {
                validate_declaration_identifier(&name, "declaration")?;
                next.name = Some(name);
                next.phase = Phase::Binders;
            }
            (Phase::Binders, TokenAction::BeginBinder) => {
                next.phase = Phase::BinderName;
            }
            (Phase::BinderName, TokenAction::BinderName(name)) => {
                validate_declaration_identifier(&name, "binder")?;
                if next.bound_variables.contains_key(&name) {
                    return Err(AstError::DuplicateBinder(name).into());
                }
                let variable = next.logic.fresh();
                next.pending = Some(PendingBinder { name, variable });
                next.phase = Phase::BinderSort;
            }
            (Phase::BinderSort, TokenAction::Sort(sort)) => {
                let pending = next.pending.clone().ok_or(ConstraintError::Invariant(
                    "binder-sort phase has no pending binder",
                ))?;
                let target = next
                    .target
                    .ok_or(ConstraintError::Invariant("binder arrived before a target"))?;
                let scope = next.scope();
                sort.validate(target, &scope)?;
                let relational_sort = sort.relational_term(&next.bound_variables)?;
                let typed = Term::node("typed", [Term::atom(&pending.name), relational_sort]);
                next.logic.substitution = next
                    .logic
                    .substitution
                    .unify(&Term::var(pending.variable), &typed)
                    .ok_or(ConstraintError::LogicConflict)?;
                next.bound_variables
                    .insert(pending.name.clone(), pending.variable);
                next.bindings.push(Binding::new(pending.name, sort));
                next.pending = None;
                next.phase = Phase::Binders;
            }
            (Phase::Binders, TokenAction::EndBinders) => {
                next.phase = Phase::ResultSort;
            }
            (Phase::ResultSort, TokenAction::Sort(sort)) => {
                let target = next
                    .target
                    .ok_or(ConstraintError::Invariant("result arrived before a target"))?;
                sort.validate(target, &next.scope())?;
                let relational_sort = sort.relational_term(&next.bound_variables)?;
                let result_variable = next.logic.fresh();
                next.logic.substitution = next
                    .logic
                    .substitution
                    .unify(
                        &Term::var(result_variable),
                        &Term::node("result", [relational_sort]),
                    )
                    .ok_or(ConstraintError::LogicConflict)?;
                next.result = Some(sort);
                next.phase = Phase::Complete;
            }
            (Phase::Complete, TokenAction::Finish) => {
                // Re-run the independent AST validator at the boundary rather
                // than assuming the token machine and AST invariants coincide.
                let target = next
                    .target
                    .ok_or(ConstraintError::Invariant("completion has no target"))?;
                let name = next
                    .name
                    .clone()
                    .ok_or(ConstraintError::Invariant("completion has no name"))?;
                let result = next
                    .result
                    .clone()
                    .ok_or(ConstraintError::Invariant("completion has no result"))?;
                FormalAst::new(target, name, next.bindings.clone(), result)?;
                next.phase = Phase::Finished;
            }
            (phase, action) => {
                return Err(ConstraintError::Syntax {
                    phase,
                    action: format!("{action:?}"),
                });
            }
        }
        Ok(next)
    }

    fn scope(&self) -> BTreeSet<String> {
        self.bindings
            .iter()
            .map(|binding| binding.name().to_owned())
            .collect()
    }
}

impl SymbolicConstraint for KanrenConstraint {
    fn validate(&self, token: TokenId) -> Result<(), ConstraintError> {
        self.advance(token).map(|_| ())
    }

    fn commit(&mut self, token: TokenId) -> Result<(), ConstraintError> {
        *self = self.advance(token)?;
        Ok(())
    }

    fn is_complete(&self) -> bool {
        self.phase == Phase::Finished
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ConstraintError {
    #[error("token {0} is absent from the semantic vocabulary")]
    UnknownToken(TokenId),
    #[error("token action {action} is invalid in phase {phase:?}")]
    Syntax { phase: Phase, action: String },
    #[error("relational unification rejected the continuation")]
    LogicConflict,
    #[error("internal constraint invariant violated: {0}")]
    Invariant(&'static str),
    #[error(transparent)]
    Ast(#[from] AstError),
}
