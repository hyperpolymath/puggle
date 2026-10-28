// SPDX-License-Identifier: MPL-2.0

use super::support::{gatekeeper, ScriptedFormaliser};
use capnp::{message::ReaderOptions, serialize};
use puggle::{
    ast::{Binding, FormalAst, Sort, TargetProver},
    gatekeeper::AgentError,
    wire::{encode, puggle_handoff_capnp},
};

#[tokio::test]
async fn wire_round_trip_preserves_all_targets_dependent_structure_and_receipt_fields() {
    let original = "  group operation\n";
    for (target, wire_target) in [
        (TargetProver::Lean, puggle_handoff_capnp::TargetProver::Lean),
        (TargetProver::Agda, puggle_handoff_capnp::TargetProver::Agda),
        (TargetProver::Smt2, puggle_handoff_capnp::TargetProver::Smt2),
    ] {
        let result = if target == TargetProver::Smt2 {
            Sort::named("Int")
        } else {
            Sort::variable("A")
        };
        let candidate = FormalAst::new(
            target,
            "operation",
            vec![Binding::new("A", Sort::named("Type"))],
            result,
        )
        .unwrap();
        let keeper = gatekeeper(&["group operation"; 5], 2);
        let formaliser =
            ScriptedFormaliser::new([Err(AgentError::new("retry")), Ok(candidate.clone())]);
        let frozen = keeper.run(original, &formaliser).await.unwrap();
        let encoded = encode(&frozen).unwrap();
        assert_eq!(
            encoded,
            encode(&frozen).unwrap(),
            "encoding an immutable value is deterministic"
        );
        let mut input = encoded.as_slice();
        let message = serialize::read_message(&mut input, ReaderOptions::new()).unwrap();
        assert!(input.is_empty(), "exactly one complete message is written");
        let root = message
            .get_root::<puggle_handoff_capnp::formal_signature::Reader<'_>>()
            .unwrap();
        assert_eq!(root.get_target().unwrap(), wire_target);
        assert_eq!(
            root.get_canonical_ast().unwrap().to_str().unwrap(),
            candidate.canonical()
        );
        assert_eq!(root.get_ast_hash().unwrap(), candidate.digest());
        let receipt = root.get_receipt().unwrap();
        assert_eq!(
            receipt.get_ast_hash().unwrap(),
            root.get_ast_hash().unwrap()
        );
        assert_eq!(
            receipt.get_original_hash().unwrap(),
            blake3::hash(original.as_bytes()).as_bytes()
        );
        assert_eq!(receipt.get_distance_millionths(), 0);
        assert_eq!(receipt.get_threshold_millionths(), 50_000);
        assert_eq!(receipt.get_ballot_round(), 0);
        assert_eq!(receipt.get_ballot_proposer(), 0);
        assert_eq!(receipt.get_acceptor_count(), 5);
        assert_eq!(receipt.get_node_count(), 5);
        assert_eq!(receipt.get_quorum(), 3);
        assert_eq!(receipt.get_attempt(), 1);
    }
}

#[tokio::test]
async fn wire_retains_nonzero_accepted_distance_in_integer_millionths() {
    let original = (0..21)
        .map(|i| format!("concept{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    let english = (0..20)
        .map(|i| format!("concept{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    let frozen = gatekeeper(&[english.as_str(); 3], 1)
        .audit_candidate(&original, super::support::ast("f"))
        .await
        .unwrap();
    let encoded = encode(&frozen).unwrap();
    let message = serialize::read_message(&mut encoded.as_slice(), ReaderOptions::new()).unwrap();
    let root = message
        .get_root::<puggle_handoff_capnp::formal_signature::Reader<'_>>()
        .unwrap();
    let receipt = root.get_receipt().unwrap();
    assert_eq!(receipt.get_distance_millionths(), 47_619);
    assert_eq!(receipt.get_attempt(), 0);
    assert_eq!(receipt.get_quorum(), 2);
    assert_eq!(receipt.get_node_count(), 3);
    assert_eq!(receipt.get_acceptor_count(), 3);
}
