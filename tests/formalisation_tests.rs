// SPDX-License-Identifier: MPL-2.0
//! Focused behavioral tests for the formalisation pipeline added in PR #2.
//! Run with `cargo test --locked --test formalisation_tests`.

#[path = "formalisation/ast.rs"]
mod ast;
#[cfg(feature = "candle")]
#[path = "formalisation/candle.rs"]
mod candle;
#[path = "formalisation/cli.rs"]
mod cli;
#[path = "formalisation/gatekeeper.rs"]
mod gatekeeper;
#[path = "formalisation/kanren.rs"]
mod kanren;
#[path = "formalisation/neural.rs"]
mod neural;
#[path = "formalisation/paxos.rs"]
mod paxos;
#[path = "formalisation/support.rs"]
mod support;
#[path = "formalisation/wire.rs"]
mod wire;
