use crate::{
    message::{Message, Role, now},
    ollama::ChatMessage,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentStatus {
    Idle,
    Working,
    Waiting,
    Completed,
    Failed,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventKind {
    Message(Message),
    State {
        role: Role,
        status: AgentStatus,
    },
    History {
        role: Role,
        message: ChatMessage,
    },
    Tool {
        call_id: Uuid,
        role: Role,
        name: String,
        arguments: serde_json::Value,
        outcome: Option<Result<serde_json::Value, String>>,
    },
    Notice(String),
    Finished {
        outcome: RunOutcome,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunOutcome {
    Completed,
    Failed,
    Cancelled,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: Uuid,
    pub run_id: Uuid,
    pub timestamp: u64,
    pub kind: EventKind,
}
impl Event {
    pub fn new(run_id: Uuid, kind: EventKind) -> Self {
        Self {
            id: Uuid::new_v4(),
            run_id,
            timestamp: now(),
            kind,
        }
    }
}

/// Owned by one run, unlike the original state recreated inside the queue loop.
#[derive(Debug)]
pub struct CeoState {
    pub run_id: Uuid,
    pub required: BTreeSet<Role>,
    pub reports: BTreeMap<Role, String>,
    pub decision_sent: bool,
}
impl CeoState {
    pub fn new(run_id: Uuid, required: impl IntoIterator<Item = Role>) -> Self {
        Self {
            run_id,
            required: required.into_iter().collect(),
            reports: BTreeMap::new(),
            decision_sent: false,
        }
    }
    pub fn accept(&mut self, run_id: Uuid, role: Role, report: String) -> bool {
        if run_id != self.run_id
            || self.decision_sent
            || !self.required.contains(&role)
            || self.reports.contains_key(&role)
        {
            return false;
        }
        self.reports.insert(role, report);
        true
    }
    pub fn begin_finalization(&mut self) -> bool {
        if self.decision_sent
            || self.required.is_empty()
            || self.reports.len() != self.required.len()
        {
            return false;
        }
        self.decision_sent = true;
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reports_and_finalization_are_run_scoped_and_exactly_once() {
        let id = Uuid::new_v4();
        let mut s = CeoState::new(id, [Role::Cto, Role::Cfo]);
        assert!(!s.begin_finalization());
        assert!(!s.accept(Uuid::new_v4(), Role::Cto, "stale".into()));
        assert!(!s.accept(id, Role::Legal, "unsolicited".into()));
        assert!(s.accept(id, Role::Cto, "ready".into()));
        assert!(!s.accept(id, Role::Cto, "duplicate".into()));
        assert!(!s.begin_finalization());
        assert!(s.accept(id, Role::Cfo, "FAILED: timeout".into()));
        assert!(s.begin_finalization());
        assert!(!s.begin_finalization());
        assert!(!s.accept(id, Role::Cfo, "late".into()));
    }
}
