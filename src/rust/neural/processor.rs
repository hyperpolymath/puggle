// SPDX-License-Identifier: MPL-2.0

use thiserror::Error;

use super::{ConstraintError, SymbolicConstraint};

pub type TokenId = u32;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MaskStats {
    pub allowed: usize,
    pub masked: usize,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LogitProcessor;

impl LogitProcessor {
    /// Mask every continuation rejected by the current symbolic state.
    ///
    /// Masking means negative infinity, not zero: zero is still a possible
    /// probability after softmax. Validation is performed against speculative
    /// state and therefore cannot partially apply a rejected token.
    /// Slice indices are token identifiers. Returned counts describe symbolic
    /// validity, including allowed tokens with non-finite logits.
    ///
    /// # Errors
    /// Returns `SymbolicDeadEnd` if no token is allowed, or `VocabularyTooLarge`
    /// if an index cannot fit in `TokenId`. The slice may be modified on error;
    /// individual validation errors are converted to masked logits.
    pub fn apply<C: SymbolicConstraint>(
        &self,
        constraint: &C,
        logits: &mut [f32],
    ) -> Result<MaskStats, MaskError> {
        let mut stats = MaskStats::default();
        for (token, logit) in logits.iter_mut().enumerate() {
            let token = TokenId::try_from(token).map_err(|_| MaskError::VocabularyTooLarge)?;
            if constraint.validate(token).is_ok() {
                stats.allowed += 1;
            } else {
                *logit = f32::NEG_INFINITY;
                stats.masked += 1;
            }
        }
        if stats.allowed == 0 {
            return Err(MaskError::SymbolicDeadEnd);
        }
        Ok(stats)
    }

    /// Select the largest finite logit, preferring the lowest index on ties.
    /// Return `NoFiniteLogit` if none exists or `VocabularyTooLarge` if a finite
    /// entry's index cannot fit in `TokenId`.
    fn select(&self, logits: &[f32]) -> Result<TokenId, GenerationError> {
        let mut best: Option<(TokenId, f32)> = None;
        for (index, logit) in logits.iter().copied().enumerate() {
            if !logit.is_finite() {
                continue;
            }
            let token =
                TokenId::try_from(index).map_err(|_| GenerationError::VocabularyTooLarge)?;
            if best.is_none_or(|(_, best_logit)| logit > best_logit) {
                best = Some((token, logit));
            }
        }
        best.map(|(token, _)| token)
            .ok_or(GenerationError::NoFiniteLogit)
    }
}

/// The minimal model boundary needed by the constrained decoder. Implementors
/// may wrap Candle, tch-rs, a remote untrusted model, or a deterministic test
/// model. Logits must cover exactly one vocabulary distribution per call.
pub trait TokenModel: Send {
    /// Evaluate the generated prefix, which is empty on the first step, and
    /// return next-token scores indexed by token identifier. Evaluation errors
    /// are propagated by the decoder as `GenerationError`.
    fn logits(&mut self, prefix: &[TokenId]) -> Result<Vec<f32>, GenerationError>;
}

pub struct ConstrainedDecoder<M, C> {
    model: M,
    constraint: C,
    processor: LogitProcessor,
    max_steps: usize,
}

impl<M, C> ConstrainedDecoder<M, C>
where
    M: TokenModel,
    C: SymbolicConstraint,
{
    #[must_use]
    pub fn new(model: M, constraint: C, max_steps: usize) -> Self {
        Self {
            model,
            constraint,
            processor: LogitProcessor,
            max_steps,
        }
    }

    #[must_use]
    pub const fn constraint(&self) -> &C {
        &self.constraint
    }

    pub fn constraint_mut(&mut self) -> &mut C {
        &mut self.constraint
    }

    /// Greedily decode under the symbolic mask. Sampling can be layered on top
    /// of `LogitProcessor::apply`; it must never sample before masking.
    /// Returns generated tokens and the final constraint, choosing the lowest
    /// token identifier when finite logits tie.
    ///
    /// # Errors
    /// Propagates model, mask, selection, and commit errors. Returns
    /// `StepBudgetExhausted` if `max_steps` is zero, even for a complete initial
    /// constraint, or if generation remains incomplete after that many tokens.
    /// Completion on the final permitted token succeeds.
    pub fn generate(mut self) -> Result<Generation<C>, GenerationError> {
        if self.max_steps == 0 {
            return Err(GenerationError::StepBudgetExhausted { budget: 0 });
        }
        let mut tokens = Vec::new();
        for _ in 0..self.max_steps {
            if self.constraint.is_complete() {
                return Ok(Generation {
                    tokens,
                    constraint: self.constraint,
                });
            }
            let mut logits = self.model.logits(&tokens)?;
            self.processor.apply(&self.constraint, &mut logits)?;
            let token = self.processor.select(&logits)?;
            self.constraint.commit(token)?;
            tokens.push(token);
        }
        if self.constraint.is_complete() {
            Ok(Generation {
                tokens,
                constraint: self.constraint,
            })
        } else {
            Err(GenerationError::StepBudgetExhausted {
                budget: self.max_steps,
            })
        }
    }
}

#[derive(Debug)]
pub struct Generation<C> {
    pub tokens: Vec<TokenId>,
    pub constraint: C,
}

#[derive(Debug, Error)]
pub enum MaskError {
    #[error("the symbolic state rejects every token in the vocabulary")]
    SymbolicDeadEnd,
    #[error("vocabulary contains more tokens than the TokenId representation")]
    VocabularyTooLarge,
}

#[derive(Debug, Error)]
pub enum GenerationError {
    #[error(transparent)]
    Mask(#[from] MaskError),
    #[error(transparent)]
    Constraint(#[from] ConstraintError),
    #[error("model failed: {0}")]
    Model(String),
    #[error("every symbolically valid continuation had a non-finite logit")]
    NoFiniteLogit,
    #[error("vocabulary contains more tokens than the TokenId representation")]
    VocabularyTooLarge,
    #[error("generation exceeded its {budget}-token budget before a complete AST")]
    StepBudgetExhausted { budget: usize },
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::ast::{Sort, TargetProver};
    use crate::neural::{KanrenConstraint, TokenAction};

    fn vocabulary() -> BTreeMap<TokenId, TokenAction> {
        BTreeMap::from([
            (0, TokenAction::Start(TargetProver::Lean)),
            (1, TokenAction::DeclarationName("identity".to_owned())),
            (2, TokenAction::BeginBinder),
            (3, TokenAction::BinderName("A".to_owned())),
            (4, TokenAction::Sort(Sort::named("Type"))),
            (5, TokenAction::BinderName("x".to_owned())),
            (6, TokenAction::Sort(Sort::variable("A"))),
            (7, TokenAction::EndBinders),
            (8, TokenAction::Sort(Sort::variable("missing"))),
            (9, TokenAction::Finish),
        ])
    }

    #[test]
    fn invalid_syntax_and_unbound_variables_are_negative_infinity() {
        let mut constraint = KanrenConstraint::new(vocabulary());
        let processor = LogitProcessor;
        let mut initial = vec![0.0; 10];
        let stats = processor
            .apply(&constraint, &mut initial)
            .expect("one action");
        assert_eq!(stats.allowed, 1);
        assert!(initial[0].is_finite());
        assert!(initial[1..].iter().all(|logit| *logit == f32::NEG_INFINITY));

        for token in [0, 1, 2, 3, 4, 2, 5, 6, 7] {
            constraint.commit(token).expect("valid prefix");
        }
        let mut result_logits = vec![0.0; 10];
        processor
            .apply(&constraint, &mut result_logits)
            .expect("bound result exists");
        assert_eq!(result_logits[8], f32::NEG_INFINITY);
        assert!(result_logits[6].is_finite());
        assert!(result_logits[4].is_finite());
        assert_eq!(result_logits[0], f32::NEG_INFINITY);
    }

    #[test]
    fn speculative_validation_does_not_consume_a_fresh_variable() {
        let mut constraint = KanrenConstraint::new(vocabulary());
        for token in [0, 1, 2] {
            constraint.commit(token).expect("valid prefix");
        }
        let before = constraint.logic().clone();
        constraint.validate(3).expect("valid binder");
        assert_eq!(constraint.logic(), &before);
    }

    struct ExpectedModel {
        sequence: Vec<TokenId>,
    }

    impl TokenModel for ExpectedModel {
        fn logits(&mut self, prefix: &[TokenId]) -> Result<Vec<f32>, GenerationError> {
            let mut logits = vec![0.0; 10];
            let expected = self.sequence.get(prefix.len()).copied().ok_or_else(|| {
                GenerationError::Model("model queried after the expected sequence".to_owned())
            })?;
            logits[expected as usize] = 10.0;
            Ok(logits)
        }
    }

    #[test]
    fn autoregressive_decoder_commits_only_the_selected_valid_token() {
        let sequence = vec![0, 1, 2, 3, 4, 2, 5, 6, 7, 6, 9];
        let model = ExpectedModel {
            sequence: sequence.clone(),
        };
        let constraint = KanrenConstraint::new(vocabulary());
        let generation = ConstrainedDecoder::new(model, constraint, sequence.len())
            .generate()
            .expect("complete constrained AST");
        assert_eq!(generation.tokens, sequence);
        let ast = generation
            .constraint
            .ast()
            .expect("AST validation")
            .expect("finished AST");
        assert_eq!(ast.name(), "identity");
        assert_eq!(ast.bindings().len(), 2);
    }

    #[test]
    fn a_symbolic_dead_end_fails_closed() {
        let constraint = KanrenConstraint::new(BTreeMap::new());
        let error = LogitProcessor
            .apply(&constraint, &mut [0.0, 0.0])
            .expect_err("unknown tokens are all masked");
        assert!(matches!(error, MaskError::SymbolicDeadEnd));
    }
}
