# SPDX-License-Identifier: MPL-2.0
@0xd7ad4e6b8f18a932;

# Only values accepted by Puggle's semantic quorum are encoded by wire.rs.
struct FormalSignature {
  astHash @0 :Data;          # Exactly 32 bytes: BLAKE3(canonicalAst).
  canonicalAst @1 :Text;     # Length-prefixed, target-inclusive canonical AST.
  target @2 :TargetProver;
  receipt @3 :ConsensusReceipt;
}

struct ConsensusReceipt {
  astHash @0 :Data;          # Must equal FormalSignature.astHash.
  originalHash @1 :Data;     # BLAKE3 of the exact original UTF-8 input.
  distanceMillionths @2 :UInt32;
  thresholdMillionths @3 :UInt32;
  ballotRound @4 :UInt64;
  ballotProposer @5 :UInt32;
  acceptorCount @6 :UInt32;
  nodeCount @7 :UInt32;
  quorum @8 :UInt32;
  attempt @9 :UInt32;        # Zero-based mutation attempt.
}

enum TargetProver {
  lean @0;
  agda @1;
  smt2 @2;
}
