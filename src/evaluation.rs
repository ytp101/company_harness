//! Small transparent role probes, not a claim about general model intelligence.
//! Uses public synthetic evidence only; never sends the owner's company context.
use crate::{
    agent::Agent,
    message::Role,
    ollama::{ChatMessage, Ollama},
};
use serde_json::{Value, json};
use std::{
    sync::mpsc::Sender,
    time::{Duration, Instant},
};
use tokio::sync::watch;

#[derive(Debug, Clone)]
pub struct Evaluation {
    pub model: String,
    pub role: Role,
    pub elapsed: Duration,
    pub checks: Vec<(String, bool)>,
    pub response: String,
    pub error: Option<String>,
}
impl Evaluation {
    pub fn score(&self) -> usize {
        self.checks.iter().filter(|(_, pass)| *pass).count()
    }
}
pub enum Update {
    Result(Evaluation),
    Finished(String),
}
fn probe(
    role: Role,
) -> (
    &'static str,
    &'static str,
    &'static [&'static str],
    Option<f64>,
) {
    match role {
        Role::Ceo => (
            "E1: Engineering says a critical tenant-isolation bug is unresolved. E2: Legal says the privacy notice is unapproved. Revenue impact is unknown. Decide public launch readiness. Choose decision hold or launch, cite the evidence, and identify unknowns.",
            "hold",
            &["E1", "E2"],
            None,
        ),
        Role::Cto => (
            "E1: A critical tenant-isolation test fails. E2: Backup restore has never been tested. Traffic growth is unknown. Assess public launch readiness and identify traffic as unknown.",
            "hold",
            &["E1", "E2"],
            None,
        ),
        Role::Cfo => (
            "E1: Cash is 240000 USD and launch costs 35000 USD. E2: Monthly burn is 40000 USD and monthly revenue is 12000 USD. Compute post-launch runway in months as (cash-launch)/(burn-revenue). Future revenue is unknown. Use decision review rather than claiming launch approval.",
            "review",
            &["E1", "E2"],
            Some(205000.0 / 28000.0),
        ),
        Role::Cmo => (
            "E1: 35 beta customers yield a 12 percent trial conversion rate. E2: The waitlist is 1800 but paid retention is unknown. Choose decision pilot or scale based on this limited evidence. Identify retention as unknown.",
            "pilot",
            &["E1", "E2"],
            None,
        ),
        Role::Legal => (
            "E1: The privacy notice is draft. E2: The data processing agreement has not been approved. Jurisdiction is unknown. Assess public launch readiness; identify jurisdiction as unknown. Do not invent a statute or provide legal approval.",
            "hold",
            &["E1", "E2"],
            None,
        ),
        Role::Owner => unreachable!("only AI roles are evaluated"),
    }
}
fn schema() -> Value {
    json!({"type":"object","properties":{"decision":{"type":"string","enum":["hold","launch","review","pilot","scale"]},"evidence_ids":{"type":"array","items":{"type":"string"}},"calculation":{"type":["number","null"]},"unknowns":{"type":"array","items":{"type":"string"}},"next_step":{"type":"string"}},"required":["decision","evidence_ids","calculation","unknowns","next_step"],"additionalProperties":false})
}
pub fn score(role: Role, text: &str) -> Vec<(String, bool)> {
    let value = serde_json::from_str::<Value>(text).ok();
    let v = value.as_ref().unwrap_or(&Value::Null);
    let (_, decision, evidence, number) = probe(role);
    let unknown = match role {
        Role::Ceo | Role::Cfo => "revenue",
        Role::Cto => "traffic",
        Role::Cmo => "retention",
        _ => "jurisdiction",
    };
    let ids: Vec<_> = v["evidence_ids"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    vec![
        (
            "Valid structured result".into(),
            value.is_some()
                && v["decision"].is_string()
                && v["next_step"]
                    .as_str()
                    .is_some_and(|s| !s.trim().is_empty())
                && v["unknowns"].is_array()
                && v.get("calculation").is_some(),
        ),
        (format!("Decision = {decision}"), v["decision"] == decision),
        (
            "Cites exactly E1 and E2".into(),
            ids.len() == evidence.len() && evidence.iter().all(|id| ids.contains(id)),
        ),
        (
            format!("Acknowledges unknown {unknown}"),
            v["unknowns"].as_array().is_some_and(|a| {
                a.iter().any(|s| {
                    s.as_str()
                        .is_some_and(|s| s.to_lowercase().contains(unknown))
                })
            }),
        ),
        (
            if number.is_some() {
                "Runway within 0.02 months"
            } else {
                "No invented numeric calculation"
            }
            .into(),
            match number {
                Some(n) => v["calculation"]
                    .as_f64()
                    .is_some_and(|a| (a - n).abs() < 0.02),
                None => v.get("calculation").is_some_and(Value::is_null),
            },
        ),
    ]
}
pub async fn run(
    endpoint: String,
    models: Vec<String>,
    tx: Sender<Update>,
    mut cancel: watch::Receiver<bool>,
    repaint: Option<eframe::egui::Context>,
) {
    let work = async {
        let client = Ollama::new(&endpoint)?;
        for model in models {
            // Validate that this model is local even though probes do not use tools.
            let capability = client.supports_tools(&model).await;
            for role in [Role::Ceo, Role::Cto, Role::Cfo, Role::Cmo, Role::Legal] {
                let start = Instant::now();
                let input = format!(
                    "Synthetic role evaluation. {}\nReturn JSON with decision, evidence_ids, calculation (null if no calculation requested), unknowns, and a concrete next_step. Use only this evidence. Do not call tools.",
                    probe(role).0
                );
                let result = if let Err(error) = &capability {
                    Err(error.clone())
                } else {
                    client
                        .chat(
                            &model,
                            &[
                                ChatMessage::text("system", Agent::new(role).prompt),
                                ChatMessage::text("user", input),
                            ],
                            vec![],
                            Some(schema()),
                        )
                        .await
                };
                let (response, error) = match result {
                    Ok(m) if m.tool_calls.is_empty() => (m.content, None),
                    Ok(_) => (
                        String::new(),
                        Some("Unexpected tool call during evaluation".into()),
                    ),
                    Err(e) => (String::new(), Some(e)),
                };
                let checks = score(role, &response);
                let _ = tx.send(Update::Result(Evaluation {
                    model: model.clone(),
                    role,
                    elapsed: start.elapsed(),
                    checks,
                    response,
                    error,
                }));
                if let Some(ctx) = &repaint {
                    ctx.request_repaint();
                }
            }
        }
        Ok::<_, String>(())
    };
    let status = tokio::select! {
        biased;
        _=async {if !*cancel.borrow() {let _=cancel.changed().await;}}=>"Evaluation cancelled; partial results retained".into(),
        result=tokio::time::timeout(Duration::from_secs(1800),work)=>match result {Ok(Ok(()))=>"Evaluation complete. Scores are synthetic checks, not general quality rankings.".into(),Ok(Err(e))=>format!("Evaluation failed: {e}"),Err(_)=>"Evaluation stopped at 30-minute deadline".into()},
    };
    let _ = tx.send(Update::Finished(status));
    if let Some(ctx) = repaint {
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn financial_probe_rejects_wrong_math_and_missing_evidence() {
        let good = json!({"decision":"review","evidence_ids":["E1","E2"],"calculation":7.32,"unknowns":["future revenue"],"next_step":"Review budget"});
        assert_eq!(
            score(Role::Cfo, &good.to_string())
                .iter()
                .filter(|(_, pass)| *pass)
                .count(),
            5
        );
        let mut bad = good;
        bad["calculation"] = json!(8.57);
        bad["evidence_ids"] = json!(["E1", "E3"]);
        assert_eq!(
            score(Role::Cfo, &bad.to_string())
                .iter()
                .filter(|(_, pass)| *pass)
                .count(),
            3
        );
        assert!(score(Role::Cfo, "not JSON").iter().all(|(_, pass)| !pass));
    }
}
