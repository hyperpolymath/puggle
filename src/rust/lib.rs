// SPDX-License-Identifier: MPL-2.0
//! Puggle's untrusted formalisation pipeline.
//!
//! Nothing in this crate is part of ECHIDNA's trusted computing base. A value
//! crosses the boundary only after constrained construction, semantic quorum,
//! and immutable BLAKE3 binding.

#![forbid(unsafe_code)]

pub mod ast;
pub mod gatekeeper;
pub mod kanren;
pub mod neural;
pub mod paxos;
pub mod wire;

pub use ast::{Binding, FormalAst, Sort, TargetProver};
pub use gatekeeper::{FrozenSpec, SemanticGatekeeper};
