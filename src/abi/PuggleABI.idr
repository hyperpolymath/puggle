-- SPDX-License-Identifier: MPL-2.0
||| Formal contract for the Puggle-to-ECHIDNA handoff.
|||
||| This module proves structural prerequisites, not the truth of an informal
||| semantic judgement. Rust supplies the runtime evidence; Idris makes it
||| impossible to omit that evidence when constructing a frozen specification.
module PuggleABI

import Data.Bits
import Data.Nat
import Data.Vect

%default total

public export
data TargetProver = Lean | Agda | SMT2

public export
record FormalAst where
  constructor MkFormalAst
  target : TargetProver
  canonical : String
  digest : Vect 32 Bits8

||| Divide by two, rounding down; zero and one both yield zero.
public export
half : Nat -> Nat
half Z = Z
half (S Z) = Z
half (S (S n)) = S (half n)

||| Return the smallest strict majority, including one for an empty node set.
public export
quorumFor : Nat -> Nat
quorumFor nodes = S (half nodes)

public export
data Odd : Nat -> Type where
  OddOne : Odd 1
  OddStep : Odd n -> Odd (S (S n))

||| Evidence that acceptors form a bounded majority of an odd node set.
||| The indices tie this proof to one particular receipt; it is not a Boolean
||| that can subsequently be paired with different counts.
public export
data QuorumReached : (acceptors : Nat) -> (nodes : Nat) -> Type where
  MkQuorumReached :
    {0 acceptors : Nat} ->
    {0 nodes : Nat} ->
    (0 acceptedEnough : LTE (quorumFor nodes) acceptors) ->
    (0 bounded : LTE acceptors nodes) ->
    (0 odd : Odd nodes) ->
    QuorumReached acceptors nodes

public export
driftThresholdMillionths : Nat
driftThresholdMillionths = 50000

||| Runtime receipt mirrored by schemas/puggle_handoff.capnp. Distances are
||| integers here so the strict threshold is decidable without floating point.
public export
record ConsensusReceipt where
  constructor MkConsensusReceipt
  astHash : Vect 32 Bits8
  originalHash : Vect 32 Bits8
  distanceMillionths : Nat
  ballotRound : Nat
  ballotProposer : Nat
  acceptorCount : Nat
  nodeCount : Nat

||| A frozen value structurally requires all three handoff proofs:
||| * a majority quorum for these exact acceptor/node counts;
||| * a distance strictly below 0.05 for this exact receipt; and
||| * equality between the candidate digest and the receipt's digest.
||| The quorum witness is retained for quorumOf. Quantity-zero semantic and
||| hash-binding fields are erased at runtime but remain mandatory at type
||| checking. There is no constructor which takes only an AST and a receipt.
public export
record FrozenSpec where
  constructor MkFrozenSpec
  ast : FormalAst
  receipt : ConsensusReceipt
  quorum : QuorumReached (acceptorCount receipt) (nodeCount receipt)
  0 semantic : LT (distanceMillionths receipt) driftThresholdMillionths
  0 hashBound : digest ast = astHash receipt

||| The wire name is an alias, not a second, weaker construction path.
public export
FormalSignature : Type
FormalSignature = FrozenSpec

||| Retrieve the receipt bound to this frozen signature.
public export
receiptOf : FormalSignature -> ConsensusReceipt
receiptOf signature = receipt signature

||| Retrieve quorum evidence indexed by this signature's exact receipt counts.
public export
quorumOf : (signature : FormalSignature) ->
  QuorumReached (acceptorCount (receiptOf signature)) (nodeCount (receiptOf signature))
quorumOf (MkFrozenSpec _ _ witness _ _) = witness
