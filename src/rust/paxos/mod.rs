// SPDX-License-Identifier: MPL-2.0
//! Single-decree Paxos over semantic-distance proposals.
//!
//! Acceptors are independent Tokio actors. Proposers perform genuine prepare
//! and accept rounds, carry forward the highest previously accepted value, and
//! commit only after a majority of acceptors acknowledges the same ballot.

use std::cmp::Ordering;

use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

pub const DRIFT_THRESHOLD_MILLIONTHS: u32 = 50_000;
pub const DRIFT_THRESHOLD: Distance = Distance(DRIFT_THRESHOLD_MILLIONTHS);
const MAX_ROUNDS_PER_PROPOSER: u32 = 8;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Distance(u32);

impl Distance {
    pub fn from_f64(value: f64) -> Result<Self, DistanceError> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(DistanceError::OutOfRange);
        }
        let scaled = (value * 1_000_000.0).round();
        Ok(Self(scaled as u32))
    }

    pub const fn from_millionths(value: u32) -> Result<Self, DistanceError> {
        if value <= 1_000_000 {
            Ok(Self(value))
        } else {
            Err(DistanceError::OutOfRange)
        }
    }

    #[must_use]
    pub const fn millionths(self) -> u32 {
        self.0
    }

    #[must_use]
    pub fn as_f64(self) -> f64 {
        f64::from(self.0) / 1_000_000.0
    }

    #[must_use]
    pub fn is_semantically_equivalent(self) -> bool {
        self < DRIFT_THRESHOLD
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Proposal {
    ast_hash: [u8; 32],
    distance: Distance,
}

impl Proposal {
    #[must_use]
    pub const fn new(ast_hash: [u8; 32], distance: Distance) -> Self {
        Self { ast_hash, distance }
    }

    #[must_use]
    pub const fn ast_hash(&self) -> &[u8; 32] {
        &self.ast_hash
    }

    #[must_use]
    pub const fn distance(&self) -> Distance {
        self.distance
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Ballot {
    pub round: u32,
    pub proposer: u32,
}

impl Ballot {
    #[must_use]
    pub const fn new(round: u32, proposer: u32) -> Self {
        Self { round, proposer }
    }
}

impl Ord for Ballot {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.round, self.proposer).cmp(&(other.round, other.proposer))
    }
}

impl PartialOrd for Ballot {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug)]
struct AcceptedValue {
    ballot: Ballot,
    proposal: Proposal,
}

#[derive(Debug)]
struct Promise {
    accepted: Option<AcceptedValue>,
}

#[derive(Debug)]
enum PrepareOutcome {
    Promised(Promise),
    Rejected(Ballot),
}

#[derive(Debug)]
enum AcceptOutcome {
    Accepted,
    Rejected(Ballot),
}

enum AcceptorCommand {
    Prepare {
        ballot: Ballot,
        reply: oneshot::Sender<PrepareOutcome>,
    },
    Accept {
        ballot: Ballot,
        proposal: Proposal,
        reply: oneshot::Sender<AcceptOutcome>,
    },
}

#[derive(Clone, Debug)]
struct AcceptorHandle {
    outbox: mpsc::Sender<AcceptorCommand>,
}

impl AcceptorHandle {
    async fn prepare(&self, ballot: Ballot) -> Result<PrepareOutcome, ActorError> {
        let (reply, response) = oneshot::channel();
        self.outbox
            .send(AcceptorCommand::Prepare { ballot, reply })
            .await
            .map_err(|_| ActorError::Stopped)?;
        response.await.map_err(|_| ActorError::Stopped)
    }

    async fn accept(
        &self,
        ballot: Ballot,
        proposal: Proposal,
    ) -> Result<AcceptOutcome, ActorError> {
        let (reply, response) = oneshot::channel();
        self.outbox
            .send(AcceptorCommand::Accept {
                ballot,
                proposal,
                reply,
            })
            .await
            .map_err(|_| ActorError::Stopped)?;
        response.await.map_err(|_| ActorError::Stopped)
    }
}

async fn acceptor_actor(mut inbox: mpsc::Receiver<AcceptorCommand>) {
    let mut promised: Option<Ballot> = None;
    let mut accepted: Option<AcceptedValue> = None;

    while let Some(command) = inbox.recv().await {
        match command {
            AcceptorCommand::Prepare { ballot, reply } => {
                let outcome = if promised.is_some_and(|current| current > ballot) {
                    PrepareOutcome::Rejected(promised.expect("checked as present"))
                } else {
                    promised = Some(ballot);
                    PrepareOutcome::Promised(Promise {
                        accepted: accepted.clone(),
                    })
                };
                // A disconnected proposer is not an acceptor failure.
                let _ = reply.send(outcome);
            }
            AcceptorCommand::Accept {
                ballot,
                proposal,
                reply,
            } => {
                let outcome = if promised.is_some_and(|current| current > ballot) {
                    AcceptOutcome::Rejected(promised.expect("checked as present"))
                } else {
                    promised = Some(ballot);
                    accepted = Some(AcceptedValue { ballot, proposal });
                    AcceptOutcome::Accepted
                };
                let _ = reply.send(outcome);
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Consensus {
    proposal: Proposal,
    ballot: Ballot,
    acceptor_count: usize,
    node_count: usize,
}

impl Consensus {
    #[must_use]
    pub const fn proposal(&self) -> &Proposal {
        &self.proposal
    }

    #[must_use]
    pub const fn ballot(&self) -> Ballot {
        self.ballot
    }

    #[must_use]
    pub const fn acceptor_count(&self) -> usize {
        self.acceptor_count
    }

    #[must_use]
    pub const fn node_count(&self) -> usize {
        self.node_count
    }

    #[must_use]
    pub const fn quorum(&self) -> usize {
        quorum_for(self.node_count)
    }

    #[must_use]
    pub fn is_semantically_accepting(&self) -> bool {
        self.acceptor_count >= self.quorum() && self.proposal.distance.is_semantically_equivalent()
    }
}

pub struct PaxosCluster {
    acceptors: Vec<AcceptorHandle>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for PaxosCluster {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl PaxosCluster {
    pub fn new(node_count: usize) -> Result<Self, PaxosError> {
        if !matches!(node_count, 3 | 5) {
            return Err(PaxosError::InvalidNodeCount(node_count));
        }
        let mut acceptors = Vec::with_capacity(node_count);
        let mut tasks = Vec::with_capacity(node_count);
        for _ in 0..node_count {
            let (outbox, inbox) = mpsc::channel(2 * node_count);
            acceptors.push(AcceptorHandle { outbox });
            tasks.push(tokio::spawn(acceptor_actor(inbox)));
        }
        Ok(Self { acceptors, tasks })
    }

    /// Run proposers in a deterministic succession until one reaches quorum.
    /// Their ballots remain globally ordered, and later proposers must adopt a
    /// previously accepted value discovered during prepare.
    pub async fn decide(self, proposals: Vec<Proposal>) -> Result<Consensus, PaxosError> {
        if proposals.len() != self.acceptors.len() {
            return Err(PaxosError::ProposalCount {
                expected: self.acceptors.len(),
                actual: proposals.len(),
            });
        }
        let mut last_error = None;
        for (proposer, proposal) in proposals.into_iter().enumerate() {
            let proposer = u32::try_from(proposer).map_err(|_| PaxosError::TooManyNodes)?;
            match propose(proposer, proposal, &self.acceptors).await {
                Ok(consensus) => return Ok(consensus),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or(PaxosError::NoQuorum))
    }
}

async fn propose(
    proposer: u32,
    own_proposal: Proposal,
    acceptors: &[AcceptorHandle],
) -> Result<Consensus, PaxosError> {
    let node_count = acceptors.len();
    let quorum = quorum_for(node_count);
    let mut round = 0_u32;

    for _ in 0..MAX_ROUNDS_PER_PROPOSER {
        let ballot = Ballot::new(round, proposer);
        let mut promises = 0_usize;
        let mut highest_accepted: Option<AcceptedValue> = None;
        let mut highest_rejection: Option<Ballot> = None;

        for acceptor in acceptors {
            match acceptor.prepare(ballot).await {
                Ok(PrepareOutcome::Promised(promise)) => {
                    promises += 1;
                    if let Some(accepted) = promise.accepted {
                        if highest_accepted
                            .as_ref()
                            .is_none_or(|current| accepted.ballot > current.ballot)
                        {
                            highest_accepted = Some(accepted);
                        }
                    }
                }
                Ok(PrepareOutcome::Rejected(promised)) => {
                    highest_rejection =
                        Some(highest_rejection.map_or(promised, |current| current.max(promised)));
                }
                Err(_) => {
                    // An unavailable acceptor is simply a missing vote. Paxos
                    // remains live while a majority of actors is reachable.
                }
            }
        }

        if promises < quorum {
            round = next_round(round, highest_rejection)?;
            continue;
        }

        // Paxos safety rule: if any promise carried an accepted value, propose
        // the value associated with the highest accepted ballot, not our own.
        let proposal = highest_accepted.map_or(own_proposal.clone(), |accepted| accepted.proposal);
        let mut accepted_count = 0_usize;
        for acceptor in acceptors {
            match acceptor.accept(ballot, proposal.clone()).await {
                Ok(AcceptOutcome::Accepted) => accepted_count += 1,
                Ok(AcceptOutcome::Rejected(promised)) => {
                    highest_rejection =
                        Some(highest_rejection.map_or(promised, |current| current.max(promised)));
                }
                Err(_) => {}
            }
        }

        if accepted_count >= quorum {
            return Ok(Consensus {
                proposal,
                ballot,
                acceptor_count: accepted_count,
                node_count,
            });
        }
        round = next_round(round, highest_rejection)?;
    }
    Err(PaxosError::NoQuorum)
}

fn next_round(current: u32, rejection: Option<Ballot>) -> Result<u32, PaxosError> {
    let minimum = rejection.map_or(current, |ballot| current.max(ballot.round));
    minimum.checked_add(1).ok_or(PaxosError::BallotExhausted)
}

#[must_use]
pub const fn quorum_for(node_count: usize) -> usize {
    node_count / 2 + 1
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum DistanceError {
    #[error("semantic distance must be finite and within [0, 1]")]
    OutOfRange,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
enum ActorError {
    #[error("acceptor actor stopped")]
    Stopped,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum PaxosError {
    #[error("Paxos requires exactly 3 or 5 nodes, got {0}")]
    InvalidNodeCount(usize),
    #[error("expected {expected} proposals, got {actual}")]
    ProposalCount { expected: usize, actual: usize },
    #[error("too many acceptor nodes for 32-bit proposer identifiers")]
    TooManyNodes,
    #[error("ballot round space exhausted")]
    BallotExhausted,
    #[error("no proposer obtained an acceptor quorum")]
    NoQuorum,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal(distance: u32, marker: u8) -> Proposal {
        let mut hash = [0_u8; 32];
        hash[0] = marker;
        Proposal::new(
            hash,
            Distance::from_millionths(distance).expect("valid test distance"),
        )
    }

    #[tokio::test]
    async fn three_nodes_commit_a_low_drift_value() {
        let cluster = PaxosCluster::new(3).expect("valid cluster");
        let proposal = proposal(12_500, 1);
        let consensus = cluster
            .decide(vec![proposal.clone(); 3])
            .await
            .expect("quorum");
        assert_eq!(consensus.proposal(), &proposal);
        assert_eq!(consensus.acceptor_count(), 3);
        assert_eq!(consensus.quorum(), 2);
        assert!(consensus.is_semantically_accepting());
    }

    #[tokio::test]
    async fn a_later_proposer_preserves_the_accepted_value() {
        let cluster = PaxosCluster::new(3).expect("valid cluster");
        let first = proposal(10_000, 1);
        let second = proposal(20_000, 2);
        let chosen = propose(0, first.clone(), &cluster.acceptors)
            .await
            .expect("first quorum");
        assert_eq!(chosen.proposal(), &first);
        let next = propose(1, second, &cluster.acceptors)
            .await
            .expect("second quorum");
        assert_eq!(next.proposal(), &first);
    }

    #[tokio::test]
    async fn minority_actor_failure_does_not_block_quorum() {
        let cluster = PaxosCluster::new(3).expect("valid cluster");
        cluster.tasks[0].abort();
        let proposal = proposal(1_000, 1);
        let consensus = propose(0, proposal.clone(), &cluster.acceptors)
            .await
            .expect("two surviving acceptors form a quorum");
        assert_eq!(consensus.proposal(), &proposal);
        assert_eq!(consensus.acceptor_count(), 2);
    }

    #[tokio::test]
    async fn threshold_is_strict() {
        assert!(Distance::from_millionths(49_999)
            .expect("valid")
            .is_semantically_equivalent());
        assert!(!DRIFT_THRESHOLD.is_semantically_equivalent());
    }

    #[test]
    fn rejects_non_finite_distances() {
        assert!(Distance::from_f64(f64::NAN).is_err());
        assert!(Distance::from_f64(-0.1).is_err());
        assert!(Distance::from_f64(1.1).is_err());
    }
}
