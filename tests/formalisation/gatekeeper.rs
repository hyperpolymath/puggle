// SPDX-License-Identifier: MPL-2.0

use super::support::{ast, gatekeeper, ScriptedFormaliser, TextAgent};
use async_trait::async_trait;
use puggle::{
    ast::FormalAst,
    gatekeeper::{
        AgentError, CandidateRejection, ConceptExtractor, Deformaliser, EnglishDeformaliser,
        GatekeeperError, JaccardMetric, LexicalConceptExtractor, SemanticError, SemanticGatekeeper,
        SemanticMetric,
    },
    paxos::DistanceError,
};
use std::{collections::BTreeSet, sync::Arc, time::Duration};

fn concepts(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).into()).collect()
}

#[test]
fn lexical_extraction_normalises_plurals_case_punctuation_and_duplicates() {
    assert_eq!(
        LexicalConceptExtractor
            .extract("The GROUPS, groups; categories! class ._Ring_-. and wn:Group.n.01 MSC:20-XX"),
        concepts(&[
            "lex:group",
            "lex:category",
            "lex:class",
            "lex:ring",
            "wn:group.n.01",
            "msc:20-xx"
        ])
    );
    assert!(LexicalConceptExtractor
        .extract("... ___ --- the and wn: msc:")
        .is_empty());
    assert_ne!(
        LexicalConceptExtractor.extract("wn:group.n.01"),
        LexicalConceptExtractor.extract("group.n.01")
    );
}

#[test]
fn jaccard_reports_exact_missing_extraneous_sets_and_rounded_distance() {
    let comparison = JaccardMetric::lexical()
        .compare("group ring", "group field")
        .unwrap();
    assert_eq!(comparison.distance().millionths(), 666_667);
    assert_eq!(comparison.missing(), &concepts(&["lex:ring"]));
    assert_eq!(comparison.extraneous(), &concepts(&["lex:field"]));
    let reverse = JaccardMetric::lexical()
        .compare("group field", "group ring")
        .unwrap();
    assert_eq!(reverse.distance(), comparison.distance());
    assert_eq!(reverse.missing(), comparison.extraneous());
}

#[test]
fn empty_concept_sets_fail_closed_and_retain_directional_feedback() {
    for (left, right, missing, extra) in [
        ("the and", "a or", vec![], vec![]),
        ("group", "", vec!["lex:group"], vec![]),
        ("", "group", vec![], vec!["lex:group"]),
    ] {
        let comparison = JaccardMetric::lexical().compare(left, right).unwrap();
        assert_eq!(comparison.distance().millionths(), 1_000_000);
        assert_eq!(comparison.missing(), &concepts(&missing));
        assert_eq!(comparison.extraneous(), &concepts(&extra));
    }
}

#[test]
fn custom_concept_extractor_is_used_on_both_sides() {
    struct ExactTokens;
    impl ConceptExtractor for ExactTokens {
        fn extract(&self, text: &str) -> BTreeSet<String> {
            text.split_whitespace().map(str::to_owned).collect()
        }
    }
    let comparison = JaccardMetric::new(Arc::new(ExactTokens))
        .compare("the GROUP", "the group")
        .unwrap();
    assert_eq!(comparison.distance().millionths(), 666_667);
    assert_eq!(comparison.missing(), &concepts(&["GROUP"]));
    assert_eq!(comparison.extraneous(), &concepts(&["group"]));
}

#[test]
fn constructor_rejects_invalid_agent_counts_and_zero_attempt_budget() {
    for count in [0, 1, 2, 4, 6] {
        let agents = (0..count)
            .map(|_| Arc::new(TextAgent(Ok("group".into()))) as Arc<dyn Deformaliser>)
            .collect();
        assert!(
            matches!(SemanticGatekeeper::new(agents, Arc::new(JaccardMetric::lexical()), 1), Err(GatekeeperError::InvalidAgentCount(actual)) if actual == count)
        );
    }
    let agents = (0..3)
        .map(|_| Arc::new(TextAgent(Ok("group".into()))) as Arc<dyn Deformaliser>)
        .collect();
    assert!(matches!(
        SemanticGatekeeper::new(agents, Arc::new(JaccardMetric::lexical()), 0),
        Err(GatekeeperError::InvalidAttemptBudget)
    ));
}

#[tokio::test]
async fn blank_and_overlong_originals_are_rejected_before_formalisation() {
    let keeper = gatekeeper(&["group"; 3], 1);
    let formaliser = ScriptedFormaliser::new([]);
    assert!(matches!(
        keeper.run(" \n\t", &formaliser).await,
        Err(GatekeeperError::EmptyInput)
    ));
    let oversized = "é".repeat(32_769);
    assert!(matches!(
        keeper.run(&oversized, &formaliser).await,
        Err(GatekeeperError::InputTooLong {
            actual: 65_538,
            limit: 65_536
        })
    ));
    assert!(formaliser.prompts.lock().unwrap().is_empty());
    assert!(matches!(
        keeper.audit_candidate("", ast("f")).await,
        Err(CandidateRejection::Input(_))
    ));
}

#[tokio::test]
async fn original_and_deformalisation_limits_are_inclusive_utf8_byte_limits() {
    let exact = "é".repeat(32_768);
    let frozen = gatekeeper(&[exact.as_str(); 3], 1)
        .audit_candidate(&exact, ast("boundary"))
        .await
        .unwrap();
    assert_eq!(
        frozen.original_hash(),
        blake3::hash(exact.as_bytes()).as_bytes()
    );
    let too_long = format!("{exact}a");
    let keeper = gatekeeper(&["group", &too_long, "group"], 1);
    assert!(matches!(
        keeper.audit_candidate("group", ast("f")).await,
        Err(CandidateRejection::DeformalisationTooLong { agent: 1 })
    ));
}

#[tokio::test]
async fn five_agent_median_requires_three_matching_observations() {
    for (texts, accepting) in [
        (["ring", "group", "group", "field", "group"], true),
        (["ring", "group", "group", "field", "vector"], false),
    ] {
        let result = gatekeeper(&texts, 1)
            .audit_candidate("group", ast("operation"))
            .await;
        if accepting {
            let frozen = result.unwrap();
            assert_eq!(frozen.consensus().node_count(), 5);
            assert_eq!(frozen.consensus().quorum(), 3);
            assert_eq!(frozen.consensus().proposal().distance().millionths(), 0);
        } else {
            match result.unwrap_err() {
                CandidateRejection::SemanticDrift {
                    distance,
                    observations,
                } => {
                    assert_eq!(distance.millionths(), 1_000_000);
                    assert_eq!(
                        observations
                            .iter()
                            .map(|item| item.agent())
                            .collect::<Vec<_>>(),
                        vec![0, 1, 2, 3, 4]
                    );
                    assert_eq!(observations[0].english(), "ring");
                    assert_eq!(observations[1].comparison().distance().millionths(), 0);
                }
                error => panic!("unexpected rejection: {error}"),
            }
        }
    }
}

#[tokio::test]
async fn exact_five_percent_semantic_distance_is_rejected() {
    let original = (0..20)
        .map(|index| format!("concept{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    let translation = (0..19)
        .map(|index| format!("concept{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    let error = gatekeeper(&[translation.as_str(); 3], 1)
        .audit_candidate(&original, ast("threshold"))
        .await
        .unwrap_err();
    assert!(
        matches!(error, CandidateRejection::SemanticDrift { distance, .. } if distance.millionths() == 50_000)
    );
}

#[tokio::test]
async fn one_failed_agent_rejects_even_when_two_agents_agree() {
    let agents: Vec<Arc<dyn Deformaliser>> = vec![
        Arc::new(TextAgent(Ok("group".into()))),
        Arc::new(TextAgent(Err(AgentError::new("agent offline")))),
        Arc::new(TextAgent(Ok("group".into()))),
    ];
    let keeper = SemanticGatekeeper::new(agents, Arc::new(JaccardMetric::lexical()), 1).unwrap();
    assert!(
        matches!(keeper.audit_candidate("group", ast("f")).await, Err(CandidateRejection::Agent(error)) if error == AgentError::new("agent offline"))
    );
}

#[tokio::test]
async fn panicking_agent_and_metric_errors_reject_candidates() {
    struct PanicAgent;
    #[async_trait]
    impl Deformaliser for PanicAgent {
        async fn deformalise(&self, _ast: &FormalAst) -> Result<String, AgentError> {
            panic!("synthetic agent panic")
        }
    }
    let agents = (0..3)
        .map(|_| Arc::new(PanicAgent) as Arc<dyn Deformaliser>)
        .collect();
    let keeper = SemanticGatekeeper::new(agents, Arc::new(JaccardMetric::lexical()), 1).unwrap();
    assert!(
        matches!(keeper.audit_candidate("group", ast("f")).await, Err(CandidateRejection::AgentTask(error)) if error.is_panic())
    );

    struct FailingMetric;
    impl SemanticMetric for FailingMetric {
        fn compare(
            &self,
            _original: &str,
            _english: &str,
        ) -> Result<puggle::gatekeeper::SemanticComparison, SemanticError> {
            Err(SemanticError::Distance(DistanceError::OutOfRange))
        }
    }
    let agents = (0..3)
        .map(|_| Arc::new(TextAgent(Ok("group".into()))) as Arc<dyn Deformaliser>)
        .collect();
    let keeper = SemanticGatekeeper::new(agents, Arc::new(FailingMetric), 1).unwrap();
    assert!(matches!(
        keeper.audit_candidate("group", ast("f")).await,
        Err(CandidateRejection::Semantic(SemanticError::Distance(
            DistanceError::OutOfRange
        )))
    ));
}

#[tokio::test]
async fn back_translators_run_concurrently() {
    struct BarrierAgent(Arc<tokio::sync::Barrier>);
    #[async_trait]
    impl Deformaliser for BarrierAgent {
        async fn deformalise(&self, _ast: &FormalAst) -> Result<String, AgentError> {
            self.0.wait().await;
            Ok("group".into())
        }
    }
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let agents = (0..3)
        .map(|_| Arc::new(BarrierAgent(barrier.clone())) as Arc<dyn Deformaliser>)
        .collect();
    let keeper = SemanticGatekeeper::new(agents, Arc::new(JaccardMetric::lexical()), 1).unwrap();
    tokio::time::timeout(
        Duration::from_secs(2),
        keeper.audit_candidate("group", ast("f")),
    )
    .await
    .expect("all agents must be started before awaiting their results")
    .unwrap();
}

#[tokio::test]
async fn formaliser_failure_can_recover_on_the_last_permitted_attempt() {
    let original = "  GROUPS\n";
    let formaliser = ScriptedFormaliser::new([
        Err(AgentError::new("temporary model failure")),
        Ok(ast("recovered")),
    ]);
    let frozen = gatekeeper(&["group"; 3], 2)
        .run(original, &formaliser)
        .await
        .unwrap();
    assert_eq!(frozen.attempt(), 1);
    assert_eq!(frozen.ast().name(), "recovered");
    assert_eq!(
        frozen.original_hash(),
        blake3::hash(original.as_bytes()).as_bytes()
    );
    assert_ne!(frozen.original_hash(), blake3::hash(b"group").as_bytes());
    assert_eq!(frozen.ast_hash(), &frozen.ast().digest());
    assert_eq!(frozen.consensus().proposal().ast_hash(), frozen.ast_hash());
    let prompts = formaliser.prompts.lock().unwrap();
    assert_eq!(prompts[0], original);
    assert!(prompts[1].starts_with(original));
    assert!(prompts[1].contains("Repair attempt 1."));
    assert!(prompts[1].contains("temporary model failure"));
}

#[tokio::test]
async fn retries_stop_at_budget_and_do_not_accumulate_prior_repair_prompts() {
    let original = "group";
    let formaliser = ScriptedFormaliser::new([
        Ok(ast("first")),
        Ok(ast("second")),
        Ok(ast("third")),
        Ok(ast("unused")),
    ]);
    let error = gatekeeper(&["ring"; 3], 3)
        .run(original, &formaliser)
        .await
        .unwrap_err();
    assert!(
        matches!(error, GatekeeperError::AttemptsExhausted { attempts: 3, last_rejection: Some(message) } if message.contains("semantic drift"))
    );
    let prompts = formaliser.prompts.lock().unwrap();
    assert_eq!(prompts.len(), 3);
    assert_eq!(prompts[0], original);
    assert!(prompts[1].contains("Restore omitted concepts: lex:group"));
    assert!(prompts[2].contains("Remove introduced concepts: lex:ring"));
    assert!(prompts[2].contains("Repair attempt 2."));
    assert!(!prompts[2].contains("Repair attempt 1."));
    assert_eq!(prompts[2].matches("Puggle semantic repair").count(), 1);
}

#[tokio::test]
async fn repair_diagnostic_truncation_respects_multibyte_character_boundaries() {
    let translation = "界".repeat(1000);
    let formaliser = ScriptedFormaliser::new([Ok(ast("first")), Ok(ast("second"))]);
    let result = gatekeeper(&[translation.as_str(); 3], 2)
        .run("group", &formaliser)
        .await;
    assert!(matches!(
        result,
        Err(GatekeeperError::AttemptsExhausted { attempts: 2, .. })
    ));
    let prompts = formaliser.prompts.lock().unwrap();
    let diagnostic = prompts[1]
        .split("Worst back-translation (untrusted diagnostic only):\n")
        .nth(1)
        .unwrap()
        .lines()
        .next()
        .unwrap();
    assert_eq!(diagnostic, format!("{}…", "界".repeat(682)));
}

#[tokio::test]
async fn formaliser_failures_exhaust_budget_with_last_rejection() {
    let formaliser = ScriptedFormaliser::new([
        Err(AgentError::new("first failure")),
        Err(AgentError::new("last failure")),
    ]);
    let error = gatekeeper(&["group"; 3], 2)
        .run("group", &formaliser)
        .await
        .unwrap_err();
    assert!(
        matches!(error, GatekeeperError::AttemptsExhausted { attempts: 2, last_rejection: Some(message) } if message == "last failure")
    );
    assert_eq!(formaliser.prompts.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn english_reference_styles_include_binders_target_result_and_wrap_modulo_three() {
    let candidate = ast("constant");
    for style in 0..3 {
        let text = EnglishDeformaliser::new(style)
            .deformalise(&candidate)
            .await
            .unwrap();
        assert!(text.contains("constant"));
        assert!(text.contains("lean"));
        assert!(text.contains("no parameters"));
        assert!(text.contains("n3:Nat;"));
        assert_eq!(
            text,
            EnglishDeformaliser::new(style + 3)
                .deformalise(&candidate)
                .await
                .unwrap()
        );
    }
}
