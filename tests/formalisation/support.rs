// SPDX-License-Identifier: MPL-2.0

use async_trait::async_trait;
use puggle::{
    ast::{FormalAst, Sort, TargetProver},
    gatekeeper::{AgentError, Deformaliser, Formaliser, JaccardMetric, SemanticGatekeeper},
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

pub fn ast(name: &str) -> FormalAst {
    FormalAst::new(TargetProver::Lean, name, vec![], Sort::named("Nat")).unwrap()
}

pub struct TextAgent(pub Result<String, AgentError>);
#[async_trait]
impl Deformaliser for TextAgent {
    async fn deformalise(&self, _ast: &FormalAst) -> Result<String, AgentError> {
        self.0.clone()
    }
}

pub fn gatekeeper(texts: &[&str], attempts: usize) -> SemanticGatekeeper {
    let agents = texts
        .iter()
        .map(|text| Arc::new(TextAgent(Ok((*text).into()))) as Arc<dyn Deformaliser>)
        .collect();
    SemanticGatekeeper::new(agents, Arc::new(JaccardMetric::lexical()), attempts).unwrap()
}

pub struct ScriptedFormaliser {
    pub prompts: Mutex<Vec<String>>,
    replies: Mutex<VecDeque<Result<FormalAst, AgentError>>>,
}
impl ScriptedFormaliser {
    pub fn new(replies: impl IntoIterator<Item = Result<FormalAst, AgentError>>) -> Self {
        Self {
            prompts: Mutex::new(vec![]),
            replies: Mutex::new(replies.into_iter().collect()),
        }
    }
}
#[async_trait]
impl Formaliser for ScriptedFormaliser {
    async fn formalise(&self, prompt: &str) -> Result<FormalAst, AgentError> {
        self.prompts.lock().unwrap().push(prompt.into());
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("formaliser exceeded the expected call count")
    }
}
