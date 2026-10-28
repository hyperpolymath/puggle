// SPDX-License-Identifier: MPL-2.0
//! Cap'n Proto serialisation for the one-way Puggle → ECHIDNA handoff.

use thiserror::Error;

use crate::ast::TargetProver;
use crate::gatekeeper::FrozenSpec;
use crate::paxos::DRIFT_THRESHOLD_MILLIONTHS;

#[allow(clippy::all, clippy::pedantic)]
pub mod puggle_handoff_capnp {
    include!(concat!(env!("OUT_DIR"), "/puggle_handoff_capnp.rs"));
}

/// Encode an already frozen value. There is intentionally no encoder accepting
/// a bare AST or caller-supplied receipt.
/// Returns an unpacked Cap'n Proto message containing the canonical AST, target,
/// hashes, and consensus receipt, with distances and threshold in millionths.
///
/// # Errors
/// Returns `WireError::CountOverflow` if an acceptor count, node count, or quorum
/// cannot fit in a wire `UInt32` field.
pub fn encode(spec: &FrozenSpec) -> Result<Vec<u8>, WireError> {
    let consensus = spec.consensus();
    let ballot = consensus.ballot();
    let acceptor_count = u32::try_from(consensus.acceptor_count())?;
    let node_count = u32::try_from(consensus.node_count())?;
    let quorum = u32::try_from(consensus.quorum())?;

    let mut message = capnp::message::Builder::new_default();
    {
        let mut root = message.init_root::<puggle_handoff_capnp::formal_signature::Builder<'_>>();
        root.set_ast_hash(spec.ast_hash());
        root.set_canonical_ast(spec.ast().canonical());
        root.set_target(match spec.ast().target() {
            TargetProver::Lean => puggle_handoff_capnp::TargetProver::Lean,
            TargetProver::Agda => puggle_handoff_capnp::TargetProver::Agda,
            TargetProver::Smt2 => puggle_handoff_capnp::TargetProver::Smt2,
        });
        let mut receipt = root.init_receipt();
        receipt.set_ast_hash(spec.ast_hash());
        receipt.set_original_hash(spec.original_hash());
        receipt.set_distance_millionths(consensus.proposal().distance().millionths());
        receipt.set_threshold_millionths(DRIFT_THRESHOLD_MILLIONTHS);
        receipt.set_ballot_round(u64::from(ballot.round));
        receipt.set_ballot_proposer(ballot.proposer);
        receipt.set_acceptor_count(acceptor_count);
        receipt.set_node_count(node_count);
        receipt.set_quorum(quorum);
        receipt.set_attempt(spec.attempt());
    }
    Ok(capnp::serialize::write_message_to_words(&message))
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum WireError {
    #[error("Paxos count does not fit the Cap'n Proto UInt32 fields")]
    CountOverflow(#[from] std::num::TryFromIntError),
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use capnp::message::ReaderOptions;
    use capnp::serialize;

    use super::*;
    use crate::ast::{FormalAst, Sort};
    use crate::gatekeeper::{AgentError, Deformaliser, JaccardMetric, SemanticGatekeeper};

    struct IdentityAgent(String);

    #[async_trait]
    impl Deformaliser for IdentityAgent {
        async fn deformalise(&self, _ast: &FormalAst) -> Result<String, AgentError> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn wire_message_contains_the_frozen_hash_and_receipt() {
        let original = "commutative group operation";
        let agents = (0..3)
            .map(|_| Arc::new(IdentityAgent(original.to_owned())) as Arc<dyn Deformaliser>)
            .collect();
        let gatekeeper = SemanticGatekeeper::new(agents, Arc::new(JaccardMetric::lexical()), 1)
            .expect("valid gatekeeper");
        let frozen = gatekeeper
            .audit_candidate(
                original,
                FormalAst::new(
                    TargetProver::Lean,
                    "operation",
                    Vec::new(),
                    Sort::named("Nat"),
                )
                .expect("valid AST"),
            )
            .await
            .expect("quorum");

        let bytes = encode(&frozen).expect("encoding");
        let mut slice = bytes.as_slice();
        let message = serialize::read_message(&mut slice, ReaderOptions::new())
            .expect("valid Cap'n Proto message");
        let root = message
            .get_root::<puggle_handoff_capnp::formal_signature::Reader<'_>>()
            .expect("root");
        assert_eq!(root.get_ast_hash().expect("hash"), frozen.ast_hash());
        assert_eq!(
            root.get_canonical_ast().expect("canonical AST"),
            frozen.ast().canonical()
        );
        let receipt = root.get_receipt().expect("receipt");
        assert_eq!(receipt.get_ast_hash().expect("hash"), frozen.ast_hash());
        assert_eq!(
            receipt.get_ballot_round(),
            u64::from(frozen.consensus().ballot().round)
        );
        assert_eq!(receipt.get_quorum(), 2);
        assert_eq!(receipt.get_acceptor_count(), 3);
        assert_eq!(receipt.get_threshold_millionths(), 50_000);
        assert_eq!(receipt.get_distance_millionths(), 0);
    }
}
