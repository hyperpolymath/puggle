// SPDX-License-Identifier: MPL-2.0
//! Optional Candle adapters. The relational decoder itself has no ML-runtime
//! dependency, which keeps its tests deterministic and lightweight.

use candle_core::Tensor;
use thiserror::Error;

use super::{GenerationError, LogitProcessor, MaskError, SymbolicConstraint, TokenId, TokenModel};

pub struct CandleTokenModel<F> {
    forward: F,
}

impl<F> CandleTokenModel<F> {
    /// Wrap a forward callback receiving the generated token prefix.
    /// For decoding, it must return rank-one `f32` logits on each invocation.
    #[must_use]
    pub const fn new(forward: F) -> Self {
        Self { forward }
    }
}

impl<F> TokenModel for CandleTokenModel<F>
where
    F: FnMut(&[TokenId]) -> candle_core::Result<Tensor> + Send,
{
    /// Evaluate the callback and copy its rank-one `f32` scores into a vector.
    /// Callback and tensor conversion errors become `GenerationError::Model`.
    fn logits(&mut self, prefix: &[TokenId]) -> Result<Vec<f32>, GenerationError> {
        (self.forward)(prefix)
            .and_then(|tensor| tensor.to_vec1::<f32>())
            .map_err(|error| GenerationError::Model(error.to_string()))
    }
}

/// Apply Puggle's mask to a rank-one Candle logits tensor and return a tensor
/// on the same device. Model evaluation remains outside Puggle's trust claims.
/// The input tensor is unchanged; rejected token scores become negative infinity.
///
/// # Errors
/// Propagates tensor conversion and construction failures as `CandleError::Candle`
/// and symbolic dead ends or oversized vocabularies as `CandleError::Mask`.
pub fn mask_candle_logits<C: SymbolicConstraint>(
    constraint: &C,
    logits: &Tensor,
) -> Result<Tensor, CandleError> {
    let mut values = logits.to_vec1::<f32>()?;
    LogitProcessor.apply(constraint, &mut values)?;
    let vocabulary = values.len();
    Ok(Tensor::from_vec(values, vocabulary, logits.device())?)
}

#[derive(Debug, Error)]
pub enum CandleError {
    #[error(transparent)]
    Candle(#[from] candle_core::Error),
    #[error(transparent)]
    Mask(#[from] MaskError),
}
