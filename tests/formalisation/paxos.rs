// SPDX-License-Identifier: MPL-2.0

use puggle::paxos::{
    quorum_for, Ballot, Distance, DistanceError, PaxosCluster, PaxosError, Proposal,
};

#[test]
fn distance_conversion_checks_endpoints_rounding_and_all_nonfinite_values() {
    for (input, expected) in [
        (0.0, 0),
        (-0.0, 0),
        (1.0, 1_000_000),
        (0.000_000_49, 0),
        (0.000_000_51, 1),
        (0.049_999_6, 50_000),
    ] {
        let distance = Distance::from_f64(input).unwrap();
        assert_eq!(distance.millionths(), expected);
        assert_eq!(distance.as_f64(), f64::from(expected) / 1_000_000.0);
    }
    for input in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        -f64::EPSILON,
        1.0 + f64::EPSILON,
    ] {
        assert_eq!(Distance::from_f64(input), Err(DistanceError::OutOfRange));
    }
    assert!(Distance::from_millionths(1_000_000).is_ok());
    assert_eq!(
        Distance::from_millionths(1_000_001),
        Err(DistanceError::OutOfRange)
    );
    assert_eq!(
        Distance::from_millionths(u32::MAX),
        Err(DistanceError::OutOfRange)
    );
}

#[test]
fn ballots_order_by_round_before_proposer_and_quorum_is_strict_majority() {
    assert!(Ballot::new(1, 0) > Ballot::new(0, u32::MAX));
    assert!(Ballot::new(1, 2) > Ballot::new(1, 1));
    assert_eq!(
        Ballot::new(1, 2).partial_cmp(&Ballot::new(1, 2)),
        Some(std::cmp::Ordering::Equal)
    );
    for (nodes, expected) in [
        (0, 1),
        (1, 1),
        (2, 2),
        (3, 2),
        (4, 3),
        (5, 3),
        (usize::MAX, usize::MAX / 2 + 1),
    ] {
        assert_eq!(quorum_for(nodes), expected);
    }
}

#[test]
fn invalid_cluster_sizes_fail_before_trying_to_spawn_actors() {
    for count in [0, 1, 2, 4, 6, usize::MAX] {
        assert!(
            matches!(PaxosCluster::new(count), Err(PaxosError::InvalidNodeCount(actual)) if actual == count)
        );
    }
}

#[tokio::test]
async fn cluster_requires_exactly_one_proposal_per_node() {
    let proposal = Proposal::new([1; 32], Distance::from_millionths(0).unwrap());
    for (nodes, actual) in [(3, 0), (3, 2), (3, 4), (5, 4), (5, 6)] {
        let error = PaxosCluster::new(nodes)
            .unwrap()
            .decide(vec![proposal.clone(); actual])
            .await
            .unwrap_err();
        assert_eq!(
            error,
            PaxosError::ProposalCount {
                expected: nodes,
                actual
            }
        );
    }
}

#[tokio::test]
async fn paxos_can_reach_consensus_on_values_that_fail_the_semantic_gate() {
    for nodes in [3, 5] {
        for (distance, accepting) in [(49_999, true), (50_000, false), (1_000_000, false)] {
            let first = Proposal::new([7; 32], Distance::from_millionths(distance).unwrap());
            let competing = Proposal::new([8; 32], Distance::from_millionths(0).unwrap());
            let mut proposals = vec![competing; nodes];
            proposals[0] = first.clone();
            let consensus = PaxosCluster::new(nodes)
                .unwrap()
                .decide(proposals)
                .await
                .unwrap();
            assert_eq!(consensus.proposal(), &first);
            assert_eq!(consensus.proposal().ast_hash(), &[7; 32]);
            assert_eq!(consensus.ballot(), Ballot::new(0, 0));
            assert_eq!(consensus.acceptor_count(), nodes);
            assert_eq!(consensus.node_count(), nodes);
            assert_eq!(consensus.quorum(), nodes / 2 + 1);
            assert_eq!(consensus.is_semantically_accepting(), accepting);
        }
    }
}
