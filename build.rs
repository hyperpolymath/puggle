// SPDX-License-Identifier: MPL-2.0

fn main() {
    println!("cargo:rerun-if-changed=schemas/puggle_handoff.capnp");
    capnpc::CompilerCommand::new()
        .file("schemas/puggle_handoff.capnp")
        .run()
        .expect("Cap'n Proto schema compilation requires the capnp executable");
}
