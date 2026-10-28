// SPDX-License-Identifier: MPL-2.0
//! Symbolically constrained autoregressive decoding.

mod constraint;
mod processor;

#[cfg(feature = "candle")]
mod candle;

pub use constraint::{ConstraintError, KanrenConstraint, Phase, SymbolicConstraint, TokenAction};
pub use processor::{
    ConstrainedDecoder, Generation, GenerationError, LogitProcessor, MaskError, MaskStats, TokenId,
    TokenModel,
};

#[cfg(feature = "candle")]
pub use candle::{mask_candle_logits, CandleError, CandleTokenModel};
