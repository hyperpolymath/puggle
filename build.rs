// SPDX-License-Identifier: MPL-2.0

/// Generate Rust bindings for the handoff schema and rerun when it changes.
///
/// # Panics
/// Panics if schema compilation fails, including when `capnp` is unavailable.
fn main() {
    println!("cargo:rerun-if-changed=schemas/puggle_handoff.capnp");
    capnpc::CompilerCommand::new()
        .file("schemas/puggle_handoff.capnp")
        .run()
        .expect("Cap'n Proto schema compilation requires the capnp executable");
}
