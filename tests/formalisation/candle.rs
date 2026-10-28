// SPDX-License-Identifier: MPL-2.0

use candle_core::{Device, Tensor};
use puggle::{
    ast::TargetProver,
    neural::{
        mask_candle_logits, CandleError, CandleTokenModel, GenerationError, KanrenConstraint,
        MaskError, TokenAction, TokenModel,
    },
};
use std::collections::BTreeMap;

fn constraint() -> KanrenConstraint {
    KanrenConstraint::new(BTreeMap::from([(
        1,
        TokenAction::Start(TargetProver::Lean),
    )]))
}

#[test]
fn candle_mask_preserves_input_shape_device_and_valid_score() {
    let input = Tensor::from_vec(vec![10.0_f32, -2.0, 3.0], 3, &Device::Cpu).unwrap();
    let masked = mask_candle_logits(&constraint(), &input).unwrap();
    assert_eq!(
        masked.to_vec1::<f32>().unwrap(),
        vec![f32::NEG_INFINITY, -2.0, f32::NEG_INFINITY]
    );
    assert_eq!(masked.dims(), input.dims());
    assert!(masked.device().same_device(input.device()));
    assert_eq!(input.to_vec1::<f32>().unwrap(), vec![10.0, -2.0, 3.0]);
}

#[test]
fn candle_mask_propagates_rank_dtype_and_symbolic_errors() {
    let rank_two = Tensor::from_vec(vec![0.0_f32; 4], (2, 2), &Device::Cpu).unwrap();
    assert!(matches!(
        mask_candle_logits(&constraint(), &rank_two),
        Err(CandleError::Candle(_))
    ));
    let wrong_type = Tensor::from_vec(vec![1_u32, 2], 2, &Device::Cpu).unwrap();
    assert!(matches!(
        mask_candle_logits(&constraint(), &wrong_type),
        Err(CandleError::Candle(_))
    ));
    let scores = Tensor::from_vec(vec![0.0_f32; 2], 2, &Device::Cpu).unwrap();
    let empty = KanrenConstraint::new(BTreeMap::new());
    assert!(matches!(
        mask_candle_logits(&empty, &scores),
        Err(CandleError::Mask(MaskError::SymbolicDeadEnd))
    ));
}

#[test]
fn candle_model_forwards_prefix_and_mutable_callback_state() {
    let mut calls = 0;
    let mut model = CandleTokenModel::new(move |prefix: &[u32]| {
        calls += 1;
        assert_eq!(prefix, if calls == 1 { &[7][..] } else { &[7, 8][..] });
        Tensor::from_vec(vec![calls as f32, prefix.len() as f32], 2, &Device::Cpu)
    });
    assert_eq!(model.logits(&[7]).unwrap(), vec![1.0, 1.0]);
    assert_eq!(model.logits(&[7, 8]).unwrap(), vec![2.0, 2.0]);
}

#[test]
fn candle_model_maps_forward_and_tensor_conversion_failures_to_model_errors() {
    let mut failing = CandleTokenModel::new(|_prefix: &[u32]| -> candle_core::Result<Tensor> {
        candle_core::bail!("synthetic forward failure")
    });
    assert!(
        matches!(failing.logits(&[]), Err(GenerationError::Model(message)) if message.contains("synthetic forward failure"))
    );
    let mut bad_shape = CandleTokenModel::new(|_prefix: &[u32]| {
        Tensor::from_vec(vec![0.0_f32; 4], (2, 2), &Device::Cpu)
    });
    assert!(matches!(
        bad_shape.logits(&[]),
        Err(GenerationError::Model(_))
    ));
}
