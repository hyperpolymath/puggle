// SPDX-License-Identifier: MPL-2.0
//! Generated Cap'n Proto bindings for the handoff schema.
//!
//! Generated code only. It lives in its own crate so that the generated
//! `unsafe` does not touch the `puggle` crate, which forbids `unsafe_code`.

#![allow(clippy::all, clippy::pedantic)]

pub mod puggle_handoff_capnp {
    include!(concat!(env!("OUT_DIR"), "/puggle_handoff_capnp.rs"));
}
