// SPDX-License-Identifier: MPL-2.0

use puggle::ast::{AstError, Sort, TargetProver};
use puggle::neural::{
    ConstrainedDecoder, ConstraintError, GenerationError, KanrenConstraint, LogitProcessor,
    MaskError, Phase, SymbolicConstraint, TokenAction, TokenId, TokenModel,
};
use std::collections::BTreeMap;

fn constraint(target: TargetProver) -> KanrenConstraint {
    KanrenConstraint::new(BTreeMap::from([
        (0, TokenAction::Start(target)),
        (1, TokenAction::DeclarationName("identity".into())),
        (2, TokenAction::BeginBinder),
        (3, TokenAction::BinderName("A".into())),
        (4, TokenAction::Sort(Sort::named("Type"))),
        (5, TokenAction::Sort(Sort::variable("A"))),
        (6, TokenAction::EndBinders),
        (7, TokenAction::Finish),
        (8, TokenAction::BinderName("x".into())),
        (9, TokenAction::Sort(Sort::variable("missing"))),
        (
            10,
            TokenAction::Sort(Sort::function(Sort::named("Nat"), Sort::named("Nat"))),
        ),
        (11, TokenAction::DeclarationName("axiom".into())),
        (12, TokenAction::BinderName("bad name".into())),
    ]))
}

fn commit(constraint: &mut KanrenConstraint, tokens: &[TokenId]) {
    for &token in tokens {
        constraint.commit(token).expect("valid fixture prefix");
    }
}

#[test]
fn complete_result_requires_finish_and_finished_state_rejects_every_token() {
    let mut state = constraint(TargetProver::Agda);
    for token in [0, 1, 6, 4] {
        assert!(state.ast().unwrap().is_none());
        state.commit(token).unwrap();
    }
    assert_eq!(state.phase(), Phase::Complete);
    assert!(!state.is_complete());
    assert!(state.ast().unwrap().is_none());
    assert_eq!(state.allowed_tokens(), vec![7]);
    state.commit(7).unwrap();
    assert!(state.is_complete());
    assert_eq!(
        state.ast().unwrap().unwrap().render(),
        "postulate\n  identity : Type"
    );
    assert!(state.allowed_tokens().is_empty());
    assert!(matches!(
        state.commit(7),
        Err(ConstraintError::Syntax {
            phase: Phase::Finished,
            ..
        })
    ));
}

#[test]
fn invalid_commits_are_atomic_and_valid_tokens_remain_available() {
    let mut state = constraint(TargetProver::Lean);
    commit(&mut state, &[0, 1, 2, 3]);
    let before = state.logic().clone();
    let allowed = state.allowed_tokens();
    for (token, expected) in [
        (99, ConstraintError::UnknownToken(99)),
        (
            5,
            ConstraintError::Ast(AstError::UnboundVariable("A".into())),
        ),
        (
            9,
            ConstraintError::Ast(AstError::UnboundVariable("missing".into())),
        ),
    ] {
        assert_eq!(state.commit(token), Err(expected));
        assert_eq!(state.phase(), Phase::BinderSort);
        assert_eq!(state.logic(), &before);
        assert_eq!(state.allowed_tokens(), allowed);
    }
    commit(&mut state, &[4, 6, 5, 7]);
    assert_eq!(
        state.ast().unwrap().unwrap().render(),
        "axiom identity (A : Type) : A"
    );
}

#[test]
fn repeated_speculation_never_allocates_variables_or_changes_phase() {
    let mut state = constraint(TargetProver::Lean);
    commit(&mut state, &[0, 1, 2]);
    let before = state.logic().clone();
    for _ in 0..5 {
        assert_eq!(state.allowed_tokens(), vec![3, 8]);
        state.validate(3).unwrap();
        assert!(state.validate(12).is_err());
        assert_eq!(state.logic(), &before);
        assert_eq!(state.phase(), Phase::BinderName);
    }
    commit(&mut state, &[3, 4]);
    assert_eq!(state.logic().substitution.len(), 1);
}

#[test]
fn duplicate_and_invalid_binder_names_and_reserved_declarations_fail() {
    let mut state = constraint(TargetProver::Lean);
    state.commit(0).unwrap();
    assert!(matches!(
        state.commit(11),
        Err(ConstraintError::Ast(AstError::InvalidIdentifier {
            kind: "declaration",
            ..
        }))
    ));
    commit(&mut state, &[1, 2, 3, 4, 2]);
    assert_eq!(
        state.commit(3),
        Err(ConstraintError::Ast(AstError::DuplicateBinder("A".into())))
    );
    assert!(matches!(
        state.commit(12),
        Err(ConstraintError::Ast(AstError::InvalidIdentifier {
            kind: "binder",
            ..
        }))
    ));
    assert_eq!(state.phase(), Phase::BinderName);
    commit(&mut state, &[8, 5, 6, 5, 7]);
    assert_eq!(state.ast().unwrap().unwrap().bindings().len(), 2);
}

#[test]
fn smt_masks_dependent_and_function_sorts_even_when_variable_is_bound() {
    let mut state = constraint(TargetProver::Smt2);
    commit(&mut state, &[0, 1, 2, 3, 4, 6]);
    assert_eq!(state.allowed_tokens(), vec![4]);
    assert_eq!(
        state.validate(5),
        Err(ConstraintError::Ast(AstError::UnsupportedDependentSort))
    );
    assert_eq!(
        state.validate(10),
        Err(ConstraintError::Ast(AstError::UnsupportedFunctionSort))
    );
}

#[test]
fn masking_preserves_valid_scores_counts_nonfinite_values_and_masks_unknown_tokens() {
    let state = constraint(TargetProver::Lean);
    for allowed_score in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -3.5] {
        let mut logits = vec![100.0; 15];
        logits[0] = allowed_score;
        let before = state.logic().clone();
        let stats = LogitProcessor.apply(&state, &mut logits).unwrap();
        assert_eq!((stats.allowed, stats.masked), (1, 14));
        assert_eq!(logits[0].to_bits(), allowed_score.to_bits());
        assert!(logits[1..].iter().all(|value| *value == f32::NEG_INFINITY));
        assert_eq!(state.logic(), &before);
    }
    assert!(matches!(
        LogitProcessor.apply(&state, &mut []),
        Err(MaskError::SymbolicDeadEnd)
    ));
}

struct Scores(Vec<f32>);
impl TokenModel for Scores {
    fn logits(&mut self, _prefix: &[TokenId]) -> Result<Vec<f32>, GenerationError> {
        Ok(self.0.clone())
    }
}

#[derive(Debug)]
struct OneStep {
    complete: bool,
    fail_commit: bool,
}
impl SymbolicConstraint for OneStep {
    fn validate(&self, _token: TokenId) -> Result<(), ConstraintError> {
        Ok(())
    }
    fn commit(&mut self, token: TokenId) -> Result<(), ConstraintError> {
        if self.fail_commit {
            return Err(ConstraintError::UnknownToken(token));
        }
        self.complete = true;
        Ok(())
    }
    fn is_complete(&self) -> bool {
        self.complete
    }
}
fn one_step() -> OneStep {
    OneStep {
        complete: false,
        fail_commit: false,
    }
}

#[test]
fn greedy_selection_ignores_all_nonfinite_scores_and_breaks_ties_by_lowest_id() {
    let result = ConstrainedDecoder::new(
        Scores(vec![f32::NAN, f32::INFINITY, -2.0, -2.0, f32::NEG_INFINITY]),
        one_step(),
        1,
    )
    .generate()
    .unwrap();
    assert_eq!(result.tokens, vec![2]);
    assert!(result.constraint.is_complete());
}

#[test]
fn symbolically_allowed_but_nonfinite_scores_are_a_generation_error() {
    let result = ConstrainedDecoder::new(
        Scores(vec![f32::NAN, f32::INFINITY, f32::NEG_INFINITY]),
        one_step(),
        1,
    )
    .generate();
    assert!(matches!(result, Err(GenerationError::NoFiniteLogit)));
}

#[test]
fn model_and_commit_failures_propagate_with_their_original_error() {
    struct FailingModel;
    impl TokenModel for FailingModel {
        fn logits(&mut self, _prefix: &[TokenId]) -> Result<Vec<f32>, GenerationError> {
            Err(GenerationError::Model("offline".into()))
        }
    }
    assert!(
        matches!(ConstrainedDecoder::new(FailingModel, one_step(), 1).generate(), Err(GenerationError::Model(message)) if message == "offline")
    );
    let rejecting = OneStep {
        complete: false,
        fail_commit: true,
    };
    assert!(matches!(
        ConstrainedDecoder::new(Scores(vec![1.0]), rejecting, 1).generate(),
        Err(GenerationError::Constraint(ConstraintError::UnknownToken(
            0
        )))
    ));
}

#[test]
fn zero_budget_is_rejected_even_if_initial_state_is_complete() {
    let complete = OneStep {
        complete: true,
        fail_commit: false,
    };
    assert!(matches!(
        ConstrainedDecoder::new(Scores(vec![]), complete, 0).generate(),
        Err(GenerationError::StepBudgetExhausted { budget: 0 })
    ));
}

#[test]
fn positive_budget_returns_already_complete_constraint_without_querying_model() {
    let complete = OneStep {
        complete: true,
        fail_commit: false,
    };
    let result = ConstrainedDecoder::new(Scores(vec![]), complete, 1)
        .generate()
        .unwrap();
    assert!(result.tokens.is_empty());
}

#[test]
fn decoder_masks_higher_scoring_invalid_tokens_and_supplies_exact_prefixes() {
    struct SequenceModel;
    impl TokenModel for SequenceModel {
        fn logits(&mut self, prefix: &[TokenId]) -> Result<Vec<f32>, GenerationError> {
            let sequence = [0, 1, 6, 4, 7];
            assert_eq!(prefix, &sequence[..prefix.len()]);
            let mut scores = vec![-10.0; 13];
            scores[sequence[prefix.len()] as usize] = 1.0;
            scores[9] = 1000.0; // Always unbound, despite being the model's favorite.
            Ok(scores)
        }
    }
    let generation = ConstrainedDecoder::new(SequenceModel, constraint(TargetProver::Lean), 5)
        .generate()
        .unwrap();
    assert_eq!(generation.tokens, vec![0, 1, 6, 4, 7]);
    assert!(generation.constraint.ast().unwrap().is_some());
    assert!(matches!(
        ConstrainedDecoder::new(SequenceModel, constraint(TargetProver::Lean), 4).generate(),
        Err(GenerationError::StepBudgetExhausted { budget: 4 })
    ));
}

#[test]
fn decoder_propagates_symbolic_dead_end_for_an_unrepresented_vocabulary() {
    assert!(matches!(
        ConstrainedDecoder::new(Scores(vec![1.0]), KanrenConstraint::new(BTreeMap::new()), 1)
            .generate(),
        Err(GenerationError::Mask(MaskError::SymbolicDeadEnd))
    ));
}
