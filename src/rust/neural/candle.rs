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
    #[must_use]
    pub const fn new(forward: F) -> Self {
        Self { forward }
    }
}

impl<F> TokenModel for CandleTokenModel<F>
where
    F: FnMut(&[TokenId]) -> candle_core::Result<Tensor> + Send,
{
    fn logits(&mut self, prefix: &[TokenId]) -> Result<Vec<f32>, GenerationError> {
        (self.forward)(prefix)
            .and_then(|tensor| tensor.to_vec1::<f32>())
            .map_err(|error| GenerationError::Model(error.to_string()))
    }
}

/// Apply Puggle's mask to a rank-one Candle logits tensor and return a tensor
/// on the same device. Model evaluation remains outside Puggle's trust claims.
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
