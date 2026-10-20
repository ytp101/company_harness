//! Editable drafts are cloned at Start: edits can never change an in-flight run.
use crate::{agent::Agent, message::Role};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default)]
pub struct CompanyContext {
    pub use_owner_data: bool,
    pub profile: String,
    pub roadmap: String,
    pub finances: String,
    pub marketing: String,
    pub policies: String,
}
impl CompanyContext {
    pub fn section(&self, tool: &str) -> &str {
        match tool {
            "get_company_profile" => &self.profile,
            "get_product_roadmap" => &self.roadmap,
            "get_financial_summary" => &self.finances,
            "get_marketing_metrics" => &self.marketing,
            "get_policy_notes" => &self.policies,
            _ => "",
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.use_owner_data && self.profile.trim().is_empty() {
            return Err("Enter your company profile before using owner-provided context.".into());
        }
        if [
            &self.profile,
            &self.roadmap,
            &self.finances,
            &self.marketing,
            &self.policies,
        ]
        .iter()
        .any(|s| s.len() > 12000)
        {
            return Err("Each company context section must be at most 12,000 bytes.".into());
        }
        Ok(())
    }
}
#[derive(Debug, Clone)]
pub struct RunOptions {
    pub context: CompanyContext,
    pub prompts: BTreeMap<Role, String>,
    pub role_models: BTreeMap<Role, String>,
    pub concurrency: usize,
    pub peer_consultation: bool,
}
impl Default for RunOptions {
    fn default() -> Self {
        Self {
            context: CompanyContext::default(),
            prompts: BTreeMap::new(),
            role_models: BTreeMap::new(),
            concurrency: 1,
            peer_consultation: false,
        }
    }
}
impl RunOptions {
    pub fn model<'a>(&'a self, role: Role, fallback: &'a str) -> &'a str {
        self.role_models
            .get(&role)
            .map(String::as_str)
            .filter(|m| !m.trim().is_empty())
            .unwrap_or(fallback)
            .trim()
    }
    pub fn agents(&self) -> BTreeMap<Role, Agent> {
        Role::ALL.into_iter().map(|role| {
            let mut agent = Agent::new(role);
            if let Some(prompt) = self.prompts.get(&role).filter(|p| !p.trim().is_empty()) { agent.prompt = prompt.clone(); }
            // Context is evidence, not editable operating instructions. Tool permissions
            // are enforced in Rust regardless of what the user/model writes here.
            let evidence = if self.context.use_owner_data {
                format!("OWNER-PROVIDED CONTEXT (unverified evidence):\n{}\nOnly owner-provided tool data is available. Missing fields are unknown. Never substitute fictional sample data.", self.context.profile)
            } else { "DATA SOURCE: fictional Northstar Cloud fixtures.".into() };
            agent.history = vec![crate::ollama::ChatMessage::text("system", agent.prompt.clone()), crate::ollama::ChatMessage::text("user", evidence)];
            (role, agent)
        }).collect()
    }
    pub fn validate(&self) -> Result<(), String> {
        self.context.validate()?;
        if !(1..=4).contains(&self.concurrency) {
            return Err("Concurrency must be 1–4.".into());
        }
        if self.prompts.values().any(|p| p.len() > 8000) {
            return Err("Each role prompt must be at most 8,000 bytes.".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_are_snapshots_and_context_is_isolated() {
        let mut draft = RunOptions::default();
        draft.context.use_owner_data = true;
        draft.context.profile = "Owner's café, founded 2020".into();
        draft.context.finances = "Private financial section".into();
        draft
            .prompts
            .insert(Role::Cto, "Custom CTO instructions".into());
        draft
            .role_models
            .insert(Role::Cto, "different-local-model".into());
        let snapshot = draft.clone();
        draft.context.profile = "changed later".into();
        let agents = snapshot.agents();
        assert_eq!(
            agents[&Role::Cto].history[0].content,
            "Custom CTO instructions"
        );
        let history = serde_json::to_string(&agents[&Role::Cto].history).unwrap();
        assert!(history.contains("Owner's café"));
        assert!(!history.contains("changed later"));
        assert!(!history.contains("Private financial section"));
        assert!(!history.contains("Northstar"));
        assert_eq!(
            snapshot.model(Role::Cto, "default"),
            "different-local-model"
        );
        assert_eq!(snapshot.model(Role::Cfo, "default"), "default");
    }
    #[test]
    fn invalid_context_is_rejected() {
        let mut options = RunOptions::default();
        options.context.use_owner_data = true;
        assert!(options.validate().is_err());
        options.context.profile = "A company".into();
        assert!(options.validate().is_ok());
        options.concurrency = 0;
        assert!(options.validate().is_err());
    }
}
