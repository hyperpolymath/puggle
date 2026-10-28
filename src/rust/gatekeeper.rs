// SPDX-License-Identifier: MPL-2.0
//! Round-trip translation, semantic comparison, prompt repair, and freezing.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;
use tokio::task::JoinSet;
use tokio::time::timeout;

use crate::ast::{AstError, FormalAst};
use crate::paxos::{Consensus, Distance, DistanceError, PaxosCluster, PaxosError, Proposal};

const DEFAULT_INPUT_LIMIT: usize = 64 * 1024;
const DEFORMALISATION_LIMIT: usize = 64 * 1024;
const REPAIR_CONTEXT_LIMIT: usize = 2 * 1024;
const AGENT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("{message}")]
pub struct AgentError {
    message: String,
}

impl AgentError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[async_trait]
pub trait Formaliser: Send + Sync {
    /// Produce an untrusted candidate from an original or repaired prompt.
    /// Return `AgentError` on failure; the gatekeeper counts it as a rejected
    /// attempt and may retry with repair feedback.
    async fn formalise(&self, prompt: &str) -> Result<FormalAst, AgentError>;
}

#[async_trait]
pub trait Deformaliser: Send + Sync {
    /// Translate a candidate into English for comparison with the original.
    /// Return `AgentError` on failure, which rejects the candidate audit.
    async fn deformalise(&self, ast: &FormalAst) -> Result<String, AgentError>;
}

pub trait ConceptExtractor: Send + Sync {
    /// Extract distinct concept identifiers for set-based semantic comparison.
    fn extract(&self, text: &str) -> BTreeSet<String>;
}

/// Deterministic concept extraction with explicit WordNet/MSC2020 support.
///
/// Tokens prefixed `wn:` or `msc:` retain their namespace and are never mixed
/// with fallback lexical concepts. Other content words are normalised into
/// `lex:` concepts. This local extractor is deliberately auditable; a full
/// WordNet or embedding implementation can implement `ConceptExtractor` or
/// `SemanticMetric` without changing the quorum protocol.
#[derive(Clone, Copy, Debug, Default)]
pub struct LexicalConceptExtractor;

impl ConceptExtractor for LexicalConceptExtractor {
    /// Extract lowercase concepts, preserving `wn:` and `msc:` namespaces.
    /// Stop words are omitted; other tokens receive simple plural normalization
    /// and a `lex:` prefix. Punctuation delimits tokens except `:`, `-`, `_`,
    /// and `.`, with leading/trailing periods, hyphens, and underscores removed.
    fn extract(&self, text: &str) -> BTreeSet<String> {
        text.split(|character: char| {
            !(character.is_alphanumeric() || matches!(character, ':' | '-' | '_' | '.'))
        })
        .filter_map(normalise_concept)
        .collect()
    }
}

fn normalise_concept(token: &str) -> Option<String> {
    let token = token.trim_matches(|character| matches!(character, '.' | '-' | '_'));
    if token.is_empty() {
        return None;
    }
    let lower = token.to_lowercase();
    if lower.starts_with("wn:") || lower.starts_with("msc:") {
        return (lower.len() > 4).then_some(lower);
    }
    if STOP_WORDS.contains(&lower.as_str()) {
        return None;
    }
    let stem = if lower.len() > 4 && lower.ends_with("ies") {
        format!("{}y", &lower[..lower.len() - 3])
    } else if lower.len() > 3 && lower.ends_with('s') && !lower.ends_with("ss") {
        lower[..lower.len() - 1].to_owned()
    } else {
        lower
    };
    Some(format!("lex:{stem}"))
}

const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "has", "have", "in", "into",
    "is", "it", "of", "on", "or", "that", "the", "their", "then", "this", "to", "with",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticComparison {
    distance: Distance,
    missing: BTreeSet<String>,
    extraneous: BTreeSet<String>,
}

impl SemanticComparison {
    #[must_use]
    pub const fn distance(&self) -> Distance {
        self.distance
    }

    /// Concepts present in the original but absent from the back-translation.
    #[must_use]
    pub const fn missing(&self) -> &BTreeSet<String> {
        &self.missing
    }

    /// Concepts introduced by the back-translation and absent from the original.
    #[must_use]
    pub const fn extraneous(&self) -> &BTreeSet<String> {
        &self.extraneous
    }
}

pub trait SemanticMetric: Send + Sync {
    /// Compare the exact original input with a candidate's back-translation.
    /// Return distance and repair feedback, or a `SemanticError` that rejects
    /// the candidate audit.
    fn compare(
        &self,
        original: &str,
        deformalised: &str,
    ) -> Result<SemanticComparison, SemanticError>;
}

pub struct JaccardMetric {
    extractor: Arc<dyn ConceptExtractor>,
}

impl JaccardMetric {
    /// Use the supplied extractor for both sides of each Jaccard comparison.
    #[must_use]
    pub fn new(extractor: Arc<dyn ConceptExtractor>) -> Self {
        Self { extractor }
    }

    /// Build a metric using the deterministic [`LexicalConceptExtractor`].
    #[must_use]
    pub fn lexical() -> Self {
        Self::new(Arc::new(LexicalConceptExtractor))
    }
}

impl SemanticMetric for JaccardMetric {
    /// Compute one minus intersection size divided by union size, rounded to
    /// integer millionths, with omitted and introduced concept sets.
    /// If either concept set is empty, return maximum distance, even when both
    /// are empty.
    fn compare(
        &self,
        original: &str,
        deformalised: &str,
    ) -> Result<SemanticComparison, SemanticError> {
        let left = self.extractor.extract(original);
        let right = self.extractor.extract(deformalised);
        // Two vacuous texts are not semantically meaningful formalisations.
        if left.is_empty() || right.is_empty() {
            return Ok(SemanticComparison {
                distance: Distance::from_f64(1.0)?,
                missing: left.difference(&right).cloned().collect(),
                extraneous: right.difference(&left).cloned().collect(),
            });
        }
        let intersection = left.intersection(&right).count();
        let union = left.union(&right).count();
        let similarity = intersection as f64 / union as f64;
        Ok(SemanticComparison {
            distance: Distance::from_f64(1.0 - similarity)?,
            missing: left.difference(&right).cloned().collect(),
            extraneous: right.difference(&left).cloned().collect(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Observation {
    agent: usize,
    english: String,
    comparison: SemanticComparison,
}

impl Observation {
    #[must_use]
    pub const fn agent(&self) -> usize {
        self.agent
    }

    #[must_use]
    pub fn english(&self) -> &str {
        &self.english
    }

    #[must_use]
    pub const fn comparison(&self) -> &SemanticComparison {
        &self.comparison
    }
}

/// An immutable AST together with the exact quorum value that authorised it.
/// Its fields and constructor are private: callers cannot manufacture a frozen
/// spec without running the gatekeeper and Paxos.
#[derive(Clone, Debug)]
pub struct FrozenSpec {
    ast: FormalAst,
    ast_hash: [u8; 32],
    original_hash: [u8; 32],
    consensus: Consensus,
    attempt: u32,
}

impl FrozenSpec {
    /// Freeze an AST whose digest matches an accepting consensus, recording a
    /// BLAKE3 digest of the exact original UTF-8 bytes.
    /// Return `SemanticDrift` if quorum or drift checks fail, or `HashBinding`
    /// if the consensus proposal hash differs from the AST digest.
    fn freeze(
        original: &str,
        ast: FormalAst,
        consensus: Consensus,
        attempt: u32,
    ) -> Result<Self, CandidateRejection> {
        if !consensus.is_semantically_accepting() {
            return Err(CandidateRejection::SemanticDrift {
                distance: consensus.proposal().distance(),
                observations: Vec::new(),
            });
        }
        let ast_hash = ast.digest();
        if consensus.proposal().ast_hash() != &ast_hash {
            return Err(CandidateRejection::HashBinding);
        }
        Ok(Self {
            ast,
            ast_hash,
            original_hash: *blake3::hash(original.as_bytes()).as_bytes(),
            consensus,
            attempt,
        })
    }

    #[must_use]
    pub const fn ast(&self) -> &FormalAst {
        &self.ast
    }

    #[must_use]
    pub const fn ast_hash(&self) -> &[u8; 32] {
        &self.ast_hash
    }

    /// BLAKE3 digest of the exact original UTF-8 bytes, without normalization.
    #[must_use]
    pub const fn original_hash(&self) -> &[u8; 32] {
        &self.original_hash
    }

    #[must_use]
    pub const fn consensus(&self) -> &Consensus {
        &self.consensus
    }

    /// Zero-based generation attempt; a directly audited candidate uses zero.
    #[must_use]
    pub const fn attempt(&self) -> u32 {
        self.attempt
    }
}

pub struct SemanticGatekeeper {
    deformalisers: Vec<Arc<dyn Deformaliser>>,
    metric: Arc<dyn SemanticMetric>,
    max_attempts: usize,
    input_limit: usize,
}

impl SemanticGatekeeper {
    /// Configure three or five concurrent back-translators and a positive
    /// generation attempt budget. Original inputs are limited to 64 KiB of UTF-8.
    ///
    /// # Errors
    /// Returns `InvalidAgentCount` for any other agent count, or
    /// `InvalidAttemptBudget` when `max_attempts` is zero.
    pub fn new(
        deformalisers: Vec<Arc<dyn Deformaliser>>,
        metric: Arc<dyn SemanticMetric>,
        max_attempts: usize,
    ) -> Result<Self, GatekeeperError> {
        if !matches!(deformalisers.len(), 3 | 5) {
            return Err(GatekeeperError::InvalidAgentCount(deformalisers.len()));
        }
        if max_attempts == 0 {
            return Err(GatekeeperError::InvalidAttemptBudget);
        }
        Ok(Self {
            deformalisers,
            metric,
            max_attempts,
            input_limit: DEFAULT_INPUT_LIMIT,
        })
    }

    /// Generate, audit, and repair until a candidate freezes or the bounded
    /// attempt budget is exhausted. Semantic comparisons always use the exact
    /// original input, never a mutated repair prompt.
    /// The attempt budget includes the initial generation; success records a
    /// zero-based attempt index in the frozen specification. Audits require a
    /// Tokio runtime for their agent and acceptor tasks.
    ///
    /// # Errors
    /// Returns `EmptyInput` for whitespace-only input or `InputTooLong` above
    /// 64 KiB. Formaliser and audit failures trigger repair attempts and become
    /// `AttemptsExhausted` with the last rejection message when the budget ends.
    /// Returns `AttemptOverflow` if an attempt index cannot fit in `u32`.
    pub async fn run<F: Formaliser>(
        &self,
        original: &str,
        formaliser: &F,
    ) -> Result<FrozenSpec, GatekeeperError> {
        self.validate_input(original)?;
        let mut prompt = original.to_owned();
        let mut last_rejection = None;

        for attempt in 0..self.max_attempts {
            let attempt = u32::try_from(attempt).map_err(|_| GatekeeperError::AttemptOverflow)?;
            let result = timeout(AGENT_TIMEOUT, formaliser.formalise(&prompt))
                .await
                .unwrap_or_else(|_| Err(AgentError::new("formaliser timed out")));
            let rejection = match result {
                Ok(ast) => match self.audit_candidate_at(original, ast, attempt).await {
                    Ok(frozen) => return Ok(frozen),
                    Err(rejection) => rejection,
                },
                Err(error) => CandidateRejection::Agent(error),
            };
            prompt = mutate_prompt(original, &rejection, attempt);
            last_rejection = Some(rejection.to_string());
        }
        Err(GatekeeperError::AttemptsExhausted {
            attempts: self.max_attempts,
            last_rejection,
        })
    }

    /// Audit an already generated untrusted candidate without retrying it.
    /// Runs all back-translators concurrently on a Tokio runtime and freezes
    /// only when their median distance is strictly below 50,000 millionths
    /// (0.05), Paxos reaches quorum, and the proposal hash matches the AST.
    /// The resulting attempt index is zero.
    ///
    /// # Errors
    /// Returns `Input` for whitespace-only input or input above 64 KiB, and
    /// propagates AST, agent, task-join, metric, and Paxos failures as
    /// `CandidateRejection`. An agent output above 64 KiB is rejected as
    /// `DeformalisationTooLong`; missing observations, excessive drift, and a
    /// mismatched proposal hash are also rejected. Every agent must succeed,
    /// even when a semantic majority would otherwise pass.
    pub async fn audit_candidate(
        &self,
        original: &str,
        ast: FormalAst,
    ) -> Result<FrozenSpec, CandidateRejection> {
        self.audit_candidate_at(original, ast, 0).await
    }

    async fn audit_candidate_at(
        &self,
        original: &str,
        ast: FormalAst,
        attempt: u32,
    ) -> Result<FrozenSpec, CandidateRejection> {
        self.validate_input(original)
            .map_err(|error| CandidateRejection::Input(error.to_string()))?;
        ast.validate()?;
        let ast_hash = ast.digest();

        let mut tasks = JoinSet::new();
        for (agent, deformaliser) in self.deformalisers.iter().cloned().enumerate() {
            let candidate = ast.clone();
            tasks.spawn(async move {
                let result = timeout(AGENT_TIMEOUT, deformaliser.deformalise(&candidate))
                    .await
                    .unwrap_or_else(|_| Err(AgentError::new("deformaliser timed out")));
                (agent, result)
            });
        }

        let mut observations = Vec::with_capacity(self.deformalisers.len());
        while let Some(result) = tasks.join_next().await {
            let (agent, result) = result.map_err(CandidateRejection::AgentTask)?;
            let english = result?;
            if english.len() > DEFORMALISATION_LIMIT {
                return Err(CandidateRejection::DeformalisationTooLong { agent });
            }
            let comparison = self.metric.compare(original, &english)?;
            observations.push(Observation {
                agent,
                english,
                comparison,
            });
        }
        observations.sort_by_key(|observation| observation.agent);
        if observations.len() != self.deformalisers.len() {
            return Err(CandidateRejection::MissingObservation);
        }

        // For an odd number of agents, a median below the threshold is exactly
        // a majority of below-threshold semantic votes. All proposers receive
        // this same canonical value; Paxos binds it immutably to a quorum.
        let mut distances = observations
            .iter()
            .map(|observation| observation.comparison.distance)
            .collect::<Vec<_>>();
        distances.sort_unstable();
        let median = distances[distances.len() / 2];
        let proposal = Proposal::new(ast_hash, median);
        let node_count = observations.len();
        let cluster = PaxosCluster::new(node_count)?;
        let consensus = cluster.decide(vec![proposal; node_count]).await?;

        if !consensus.is_semantically_accepting() {
            return Err(CandidateRejection::SemanticDrift {
                distance: median,
                observations,
            });
        }
        FrozenSpec::freeze(original, ast, consensus, attempt)
    }

    fn validate_input(&self, original: &str) -> Result<(), GatekeeperError> {
        if original.trim().is_empty() {
            return Err(GatekeeperError::EmptyInput);
        }
        if original.len() > self.input_limit {
            return Err(GatekeeperError::InputTooLong {
                actual: original.len(),
                limit: self.input_limit,
            });
        }
        Ok(())
    }
}

fn mutate_prompt(original: &str, rejection: &CandidateRejection, attempt: u32) -> String {
    let mut prompt = String::from(original);
    prompt.push_str("\n\n[Puggle semantic repair — preserve the original intent]\n");
    let _ = writeln!(
        prompt,
        "Repair attempt {}. The previous candidate was not frozen: {}.",
        attempt.saturating_add(1),
        rejection
    );

    if let CandidateRejection::SemanticDrift { observations, .. } = rejection {
        if let Some(worst) = observations
            .iter()
            .max_by_key(|observation| observation.comparison.distance)
        {
            prompt.push_str("Worst back-translation (untrusted diagnostic only):\n");
            push_bounded(&mut prompt, worst.english(), REPAIR_CONTEXT_LIMIT);
            prompt.push('\n');
            match attempt % 3 {
                0 => push_concepts(
                    &mut prompt,
                    "Restore omitted concepts",
                    worst.comparison.missing(),
                ),
                1 => push_concepts(
                    &mut prompt,
                    "Remove introduced concepts",
                    worst.comparison.extraneous(),
                ),
                _ => {
                    push_concepts(
                        &mut prompt,
                        "Restore omitted concepts",
                        worst.comparison.missing(),
                    );
                    push_concepts(
                        &mut prompt,
                        "Remove introduced concepts",
                        worst.comparison.extraneous(),
                    );
                }
            }
        }
    }
    prompt.push_str("Return a freshly constrained signature; do not explain the repair.\n");
    prompt
}

/// Append at most `limit` UTF-8 bytes of `value`, ending on a character boundary.
/// If truncated, append an ellipsis in addition to that byte budget.
fn push_bounded(output: &mut String, value: &str, limit: usize) {
    if value.len() <= limit {
        output.push_str(value);
        return;
    }
    let mut boundary = limit;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    output.push_str(&value[..boundary]);
    output.push('…');
}

fn push_concepts(output: &mut String, label: &str, concepts: &BTreeSet<String>) {
    output.push_str(label);
    output.push_str(": ");
    if concepts.is_empty() {
        output.push_str("none identified; tighten the signature against the original text");
    } else {
        for (index, concept) in concepts.iter().take(16).enumerate() {
            if index > 0 {
                output.push_str(", ");
            }
            output.push_str(concept);
        }
    }
    output.push('\n');
}

/// Small deterministic reference agents for local operation and tests. They
/// render the same validated AST in three different English styles; production
/// deployments can inject independent neural deformalisers through the trait.
#[derive(Clone, Copy, Debug)]
pub struct EnglishDeformaliser {
    style: usize,
}

impl EnglishDeformaliser {
    /// Choose one of three deterministic English templates using `style % 3`.
    #[must_use]
    pub const fn new(style: usize) -> Self {
        Self { style }
    }
}

#[async_trait]
impl Deformaliser for EnglishDeformaliser {
    /// Describe the declaration, target, parameters, and canonical result sort.
    /// This local renderer always succeeds and performs no model calls.
    async fn deformalise(&self, ast: &FormalAst) -> Result<String, AgentError> {
        let bindings = if ast.bindings().is_empty() {
            "no parameters".to_owned()
        } else {
            ast.bindings()
                .iter()
                .map(|binding| format!("{} of sort {}", binding.name(), binding.sort().canonical()))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let result = ast.result().canonical();
        let text = match self.style % 3 {
            0 => format!(
                "Declaration {} in {} accepts {} and returns {}.",
                ast.name(),
                ast.target().as_str(),
                bindings,
                result
            ),
            1 => format!(
                "The {} signature {} maps {} to {}.",
                ast.target().as_str(),
                ast.name(),
                bindings,
                result
            ),
            _ => format!(
                "{} is declared for {} with inputs {}; its output is {}.",
                ast.name(),
                ast.target().as_str(),
                bindings,
                result
            ),
        };
        Ok(text)
    }
}

#[derive(Debug, Error)]
pub enum CandidateRejection {
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error("deformaliser agent task failed: {0}")]
    AgentTask(tokio::task::JoinError),
    #[error(transparent)]
    Semantic(#[from] SemanticError),
    #[error(transparent)]
    Paxos(#[from] PaxosError),
    #[error(transparent)]
    Ast(#[from] AstError),
    #[error("invalid input: {0}")]
    Input(String),
    #[error("deformaliser {agent} exceeded the output limit")]
    DeformalisationTooLong { agent: usize },
    #[error("at least one deformaliser produced no observation")]
    MissingObservation,
    #[error("semantic drift {distance:?} is not below the strict 0.05 threshold")]
    SemanticDrift {
        distance: Distance,
        observations: Vec<Observation>,
    },
    #[error("Paxos proposal is not bound to the candidate AST hash")]
    HashBinding,
}

#[derive(Debug, Error)]
pub enum SemanticError {
    #[error(transparent)]
    Distance(#[from] DistanceError),
}

#[derive(Debug, Error)]
pub enum GatekeeperError {
    #[error("expected exactly 3 or 5 deformaliser agents, got {0}")]
    InvalidAgentCount(usize),
    #[error("the formalisation attempt budget must be positive")]
    InvalidAttemptBudget,
    #[error("input must not be empty")]
    EmptyInput,
    #[error("input is {actual} bytes; the limit is {limit}")]
    InputTooLong { actual: usize, limit: usize },
    #[error("attempt counter overflow")]
    AttemptOverflow,
    #[error("all {attempts} formalisation attempts were rejected: {last_rejection:?}")]
    AttemptsExhausted {
        attempts: usize,
        last_rejection: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use std::future::pending;
    use std::sync::Mutex;

    use super::*;
    use crate::ast::{Sort, TargetProver};

    fn ast(name: &str) -> FormalAst {
        FormalAst::new(TargetProver::Lean, name, Vec::new(), Sort::named("Nat"))
            .expect("valid test AST")
    }

    struct TextAgent(String);

    #[async_trait]
    impl Deformaliser for TextAgent {
        async fn deformalise(&self, _ast: &FormalAst) -> Result<String, AgentError> {
            Ok(self.0.clone())
        }
    }

    fn gatekeeper(texts: &[&str], attempts: usize) -> SemanticGatekeeper {
        let agents = texts
            .iter()
            .map(|text| Arc::new(TextAgent((*text).to_owned())) as Arc<dyn Deformaliser>)
            .collect();
        SemanticGatekeeper::new(agents, Arc::new(JaccardMetric::lexical()), attempts)
            .expect("valid test gatekeeper")
    }

    #[test]
    fn jaccard_uses_namespaced_concepts_and_rejects_vacuity() {
        let metric = JaccardMetric::lexical();
        let same = metric
            .compare("wn:group.n.01 msc:20-XX", "wn:group.n.01 msc:20-XX")
            .expect("comparison");
        assert_eq!(same.distance().millionths(), 0);
        assert_eq!(
            metric
                .compare("the and", "a or")
                .expect("vacuous comparison")
                .distance()
                .millionths(),
            1_000_000
        );
    }

    #[tokio::test]
    async fn semantic_and_paxos_quorums_freeze_the_exact_ast() {
        let original = "a commutative group operation";
        let gatekeeper = gatekeeper(&[original, original, original], 1);
        let candidate = ast("operation");
        let expected_hash = candidate.digest();
        let frozen = gatekeeper
            .audit_candidate(original, candidate)
            .await
            .expect("identical RTT has zero drift");
        assert_eq!(frozen.ast_hash(), &expected_hash);
        assert_eq!(frozen.consensus().acceptor_count(), 3);
        assert_eq!(frozen.consensus().proposal().distance().millionths(), 0);
    }

    #[tokio::test]
    async fn median_below_threshold_requires_a_semantic_majority() {
        let original = "commutative group operation";
        let gatekeeper = gatekeeper(&[original, original, "linear vector space"], 1);
        gatekeeper
            .audit_candidate(original, ast("operation"))
            .await
            .expect("two of three semantic votes pass");
    }

    #[tokio::test]
    async fn drifted_candidate_is_not_frozen() {
        let gatekeeper = gatekeeper(&["alpha beta", "gamma delta", "epsilon zeta"], 1);
        let error = gatekeeper
            .audit_candidate("original mathematical intent", ast("operation"))
            .await
            .expect_err("all agents drift");
        assert!(matches!(error, CandidateRejection::SemanticDrift { .. }));
    }

    struct SequenceFormaliser {
        prompts: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl Formaliser for SequenceFormaliser {
        async fn formalise(&self, prompt: &str) -> Result<FormalAst, AgentError> {
            let mut prompts = self.prompts.lock().map_err(|_| AgentError::new("lock"))?;
            prompts.push(prompt.to_owned());
            if prompts.len() == 1 {
                Ok(ast("drifted"))
            } else {
                Ok(ast("repaired"))
            }
        }
    }

    struct RepairAgent(String);

    #[async_trait]
    impl Deformaliser for RepairAgent {
        async fn deformalise(&self, ast: &FormalAst) -> Result<String, AgentError> {
            if ast.name() == "repaired" {
                Ok(self.0.clone())
            } else {
                Ok("unrelated linear algebra".to_owned())
            }
        }
    }

    struct PendingAgent;

    #[async_trait]
    impl Formaliser for PendingAgent {
        async fn formalise(&self, _prompt: &str) -> Result<FormalAst, AgentError> {
            pending().await
        }
    }

    #[async_trait]
    impl Deformaliser for PendingAgent {
        async fn deformalise(&self, _ast: &FormalAst) -> Result<String, AgentError> {
            pending().await
        }
    }

    struct TimeoutFormaliser {
        prompts: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl Formaliser for TimeoutFormaliser {
        async fn formalise(&self, prompt: &str) -> Result<FormalAst, AgentError> {
            let first = {
                let mut prompts = self.prompts.lock().expect("prompt lock");
                prompts.push(prompt.to_owned());
                prompts.len() == 1
            };
            if first {
                pending().await
            } else {
                Ok(ast("repaired"))
            }
        }
    }

    struct TimeoutRepairAgent(String);

    #[async_trait]
    impl Deformaliser for TimeoutRepairAgent {
        async fn deformalise(&self, ast: &FormalAst) -> Result<String, AgentError> {
            if ast.name() == "repaired" {
                Ok(self.0.clone())
            } else {
                pending().await
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn formaliser_timeouts_exhaust_the_attempt_budget() {
        let gatekeeper = gatekeeper(&["intent"; 3], 2);
        let start = tokio::time::Instant::now();
        let error = timeout(AGENT_TIMEOUT * 3, gatekeeper.run("intent", &PendingAgent))
            .await
            .expect("attempts must finish within the timeout budget")
            .expect_err("a pending formaliser cannot freeze");
        assert!(matches!(
            error,
            GatekeeperError::AttemptsExhausted { attempts: 2, last_rejection: Some(reason) }
                if reason == "formaliser timed out"
        ));
        assert!(start.elapsed() >= AGENT_TIMEOUT * 2);
    }

    #[tokio::test(start_paused = true)]
    async fn formaliser_timeout_repairs_and_retries() {
        let gatekeeper = gatekeeper(&["intent"; 3], 2);
        let formaliser = TimeoutFormaliser {
            prompts: Mutex::new(Vec::new()),
        };
        let frozen = timeout(AGENT_TIMEOUT * 3, gatekeeper.run("intent", &formaliser))
            .await
            .expect("run must finish")
            .expect("second attempt succeeds");
        assert_eq!(frozen.attempt(), 1);
        let prompts = formaliser.prompts.lock().expect("prompt lock");
        assert_eq!(prompts.len(), 2);
        assert!(prompts[1].contains("formaliser timed out"));
        assert!(prompts[1].starts_with("intent"));
    }

    #[tokio::test(start_paused = true)]
    async fn one_deformaliser_timeout_rejects_the_candidate() {
        let mut gatekeeper = gatekeeper(&["intent"; 3], 1);
        gatekeeper.deformalisers[0] = Arc::new(PendingAgent);
        let error = timeout(
            AGENT_TIMEOUT * 2,
            gatekeeper.audit_candidate("intent", ast("operation")),
        )
        .await
        .expect("audit must finish")
        .expect_err("one missing observation prevents freezing");
        assert!(
            matches!(error, CandidateRejection::Agent(error) if error.to_string() == "deformaliser timed out")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn deformaliser_timeout_repairs_and_retries() {
        let mut gatekeeper = gatekeeper(&["intent"; 3], 2);
        gatekeeper.deformalisers[0] = Arc::new(TimeoutRepairAgent("intent".to_owned()));
        let formaliser = SequenceFormaliser {
            prompts: Mutex::new(Vec::new()),
        };
        let frozen = timeout(AGENT_TIMEOUT * 3, gatekeeper.run("intent", &formaliser))
            .await
            .expect("run must finish")
            .expect("second attempt succeeds");
        assert_eq!(frozen.attempt(), 1);
        let prompts = formaliser.prompts.lock().expect("prompt lock");
        assert_eq!(prompts.len(), 2);
        assert!(prompts[1].contains("deformaliser timed out"));
        assert!(prompts[1].starts_with("intent"));
    }

    #[tokio::test]
    async fn rejection_mutates_the_prompt_and_retries() {
        let original = "commutative group operation";
        let agents = (0..3)
            .map(|_| Arc::new(RepairAgent(original.to_owned())) as Arc<dyn Deformaliser>)
            .collect();
        let gatekeeper = SemanticGatekeeper::new(agents, Arc::new(JaccardMetric::lexical()), 2)
            .expect("valid gatekeeper");
        let formaliser = SequenceFormaliser {
            prompts: Mutex::new(Vec::new()),
        };
        let frozen = gatekeeper
            .run(original, &formaliser)
            .await
            .expect("second attempt succeeds");
        assert_eq!(frozen.attempt(), 1);
        let prompts = formaliser.prompts.lock().expect("prompt lock");
        assert_eq!(prompts.len(), 2);
        assert!(prompts[0].starts_with(original));
        assert!(prompts[1].contains("Puggle semantic repair"));
        assert!(prompts[1].contains(original));
    }
}
