use serde::{Deserialize, Serialize};
use std::{
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Role {
    Owner,
    Ceo,
    Cto,
    Cfo,
    Cmo,
    Legal,
}
impl Role {
    pub const ALL: [Self; 6] = [
        Self::Owner,
        Self::Ceo,
        Self::Cto,
        Self::Cfo,
        Self::Cmo,
        Self::Legal,
    ];
    pub const SPECIALISTS: [Self; 4] = [Self::Cto, Self::Cfo, Self::Cmo, Self::Legal];
}
impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Owner => "Owner",
            Self::Ceo => "CEO",
            Self::Cto => "CTO",
            Self::Cfo => "CFO",
            Self::Cmo => "CMO",
            Self::Legal => "Legal",
        })
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageKind {
    Task,
    Assessment,
    Report,
    Failure,
    Final,
    Consultation,
    ConsultationReply,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageStatus {
    Sent,
    Delivered,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: Uuid,
    pub run_id: Uuid,
    pub sender: Role,
    pub recipient: Role,
    pub kind: MessageKind,
    pub body: String,
    pub timestamp: u64,
    pub status: MessageStatus,
    pub reply_to: Option<Uuid>,
}
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
impl Message {
    pub fn new(
        run_id: Uuid,
        sender: Role,
        recipient: Role,
        kind: MessageKind,
        body: String,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            run_id,
            sender,
            recipient,
            kind,
            body,
            timestamp: now(),
            status: MessageStatus::Sent,
            reply_to: None,
        }
    }
    pub fn valid_route(&self) -> bool {
        use MessageKind::*;
        match (self.sender, self.recipient, self.kind) {
            (Role::Owner, Role::Ceo, Task) | (Role::Ceo, Role::Owner, Assessment | Final) => true,
            (Role::Ceo, role, Task) => Role::SPECIALISTS.contains(&role),
            (role, Role::Ceo, Report | Failure) => Role::SPECIALISTS.contains(&role),
            (a, b, Consultation | ConsultationReply) => {
                a != b && Role::SPECIALISTS.contains(&a) && Role::SPECIALISTS.contains(&b)
            }
            _ => false,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routes_are_role_and_kind_restricted() {
        for a in Role::ALL {
            for b in Role::ALL {
                let m = Message::new(Uuid::new_v4(), a, b, MessageKind::Task, "x".into());
                assert_eq!(
                    m.valid_route(),
                    (a == Role::Owner && b == Role::Ceo)
                        || (a == Role::Ceo && Role::SPECIALISTS.contains(&b))
                );
            }
        }
    }
}
