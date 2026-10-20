//! One coordinator owns routing; bounded async workers own individual agent contexts.
//! The GUI receives immutable events; it never borrows an agent across threads.
use crate::{
    agent::Agent,
    message::{Message, MessageKind, MessageStatus, Role},
    ollama::{ChatMessage, Ollama},
    state::{AgentStatus, CeoState, Event, EventKind, RunOutcome},
    tools,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::mpsc::Sender,
    time::Duration,
};
use tokio::sync::watch;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Ollama,
    Demo,
}
#[derive(Clone)]
pub struct RunConfig {
    pub id: Uuid,
    pub mode: Mode,
    pub endpoint: String,
    pub model: String,
    pub goal: String,
    pub options: crate::config::RunOptions,
}
#[derive(Debug, Deserialize)]
struct Delegation {
    role: Role,
    task: String,
}
#[derive(Debug, Deserialize)]
struct Plan {
    assessment: String,
    delegations: Vec<Delegation>,
}
impl Plan {
    fn parse(text: &str) -> Result<Self, String> {
        let plan: Self = serde_json::from_str(text)
            .map_err(|e| format!("CEO returned invalid delegation JSON: {e}"))?;
        let mut roles = BTreeSet::new();
        if plan.assessment.trim().is_empty()
            || plan.delegations.is_empty()
            || plan.delegations.len() > 4
        {
            return Err("CEO must supply an assessment and 1–4 specialist tasks".into());
        }
        for d in &plan.delegations {
            if !Role::SPECIALISTS.contains(&d.role)
                || !roles.insert(d.role)
                || d.task.trim().is_empty()
            {
                return Err(
                    "CEO delegation contains an invalid/duplicate role or empty task".into(),
                );
            }
        }
        Ok(plan)
    }
}
fn plan_schema() -> Value {
    json!({"type":"object","properties":{"assessment":{"type":"string"},"delegations":{"type":"array","minItems":1,"maxItems":4,"items":{"type":"object","properties":{"role":{"type":"string","enum":["Cto","Cfo","Cmo","Legal"]},"task":{"type":"string"}},"required":["role","task"],"additionalProperties":false}}},"required":["assessment","delegations"],"additionalProperties":false})
}
struct Job {
    role: Role,
    input: String,
    format: Option<Value>,
    use_tools: bool,
    demo: String,
}
impl Job {
    fn report(role: Role, input: String) -> Self {
        Self {
            role,
            input,
            format: None,
            use_tools: true,
            demo: demo_response(role, false),
        }
    }
}
#[derive(Deserialize)]
struct PeerQuestion {
    role: Role,
    question: String,
}
#[derive(Deserialize)]
struct PeerPlan {
    reason: String,
    consultations: Vec<PeerQuestion>,
}
impl PeerPlan {
    fn validate(&self, sender: Role, active: &[Role]) -> Result<(), String> {
        let mut seen = BTreeSet::new();
        if self.reason.trim().is_empty() || self.consultations.len() > 2 {
            return Err("Peer plan needs a reason and at most two questions".into());
        }
        for q in &self.consultations {
            if q.role == sender
                || !Role::SPECIALISTS.contains(&q.role)
                || !active.contains(&q.role)
                || !seen.insert(q.role)
                || q.question.trim().is_empty()
                || q.question.len() > 4000
            {
                return Err("Invalid peer target, duplicate peer, or invalid question".into());
            }
        }
        Ok(())
    }
}
#[derive(Deserialize)]
struct PeerAnswer {
    request_id: Uuid,
    answer: String,
}
#[derive(Deserialize)]
struct PeerAnswers {
    answers: Vec<PeerAnswer>,
}
fn peer_schema() -> Value {
    json!({"type":"object","properties":{"reason":{"type":"string"},"consultations":{"type":"array","maxItems":2,"items":{"type":"object","properties":{"role":{"type":"string","enum":["Cto","Cfo","Cmo","Legal"]},"question":{"type":"string"}},"required":["role","question"],"additionalProperties":false}}},"required":["reason","consultations"],"additionalProperties":false})
}
fn answer_schema() -> Value {
    json!({"type":"object","properties":{"answers":{"type":"array","items":{"type":"object","properties":{"request_id":{"type":"string"},"answer":{"type":"string"}},"required":["request_id","answer"],"additionalProperties":false}}},"required":["answers"],"additionalProperties":false})
}
#[derive(Clone)]
pub struct EventSink {
    pub run_id: Uuid,
    pub tx: Sender<Event>,
    pub repaint: Option<eframe::egui::Context>,
}
impl EventSink {
    pub fn emit(&self, kind: EventKind) {
        let event = Event::new(self.run_id, kind);
        tracing::info!(run_id = %self.run_id, event_id = %event.id, "execution event");
        let _ = self.tx.send(event);
        if let Some(ctx) = &self.repaint {
            ctx.request_repaint();
        }
    }
    fn state(&self, role: Role, status: AgentStatus) {
        self.emit(EventKind::State { role, status });
    }
}
struct Engine {
    config: RunConfig,
    sink: EventSink,
    agents: BTreeMap<Role, Agent>,
    client: Option<Ollama>,
    tool_support: bool,
}
impl Engine {
    fn record(&mut self, role: Role, message: ChatMessage) {
        self.sink.emit(EventKind::History {
            role,
            message: message.clone(),
        });
        self.agents
            .get_mut(&role)
            .expect("all roles initialized")
            .history
            .push(message);
    }
    fn send(
        &self,
        queue: &mut VecDeque<Message>,
        sender: Role,
        recipient: Role,
        kind: MessageKind,
        body: String,
    ) {
        let message = Message::new(self.config.id, sender, recipient, kind, body);
        assert!(message.valid_route(), "internal router invariant");
        self.sink.emit(EventKind::Message(message.clone()));
        queue.push_back(message);
    }
    fn deliver(&self, mut message: Message) -> Message {
        message.status = MessageStatus::Delivered;
        self.sink.emit(EventKind::Message(message.clone()));
        message
    }
    fn tool(&mut self, role: Role, name: &str, arguments: Value) -> ChatMessage {
        let call_id = Uuid::new_v4();
        self.sink.emit(EventKind::Tool {
            call_id,
            role,
            name: name.into(),
            arguments: arguments.clone(),
            outcome: None,
        });
        let result =
            tools::dispatch_with_context(role, name, &arguments, &self.config.options.context);
        self.sink.emit(EventKind::Tool {
            call_id,
            role,
            name: name.into(),
            arguments,
            outcome: Some(result.clone()),
        });
        let content = match result {
            Ok(v) => v.to_string(),
            Err(e) => json!({"error":e}).to_string(),
        };
        let mut message = ChatMessage::text("tool", content);
        message.tool_name = Some(name.into());
        message
    }
    async fn turn(&mut self, role: Role, input: String, planning: bool) -> Result<String, String> {
        self.respond(
            role,
            input,
            if planning { Some(plan_schema()) } else { None },
            role != Role::Ceo || planning,
            demo_response(role, planning),
        )
        .await
    }
    async fn respond(
        &mut self,
        role: Role,
        input: String,
        format: Option<Value>,
        use_tools: bool,
        demo: String,
    ) -> Result<String, String> {
        self.sink.state(role, AgentStatus::Working);
        self.record(role, ChatMessage::text("user", input));
        if self.config.mode == Mode::Demo {
            tokio::time::sleep(Duration::from_millis(650)).await;
            for name in tools::allowed(role).iter().filter(|_| use_tools) {
                let mut call = ChatMessage::text("assistant", String::new());
                call.tool_calls.push(crate::ollama::ToolCall {
                    function: crate::ollama::FunctionCall {
                        name: (*name).into(),
                        arguments: json!({}),
                    },
                });
                self.record(role, call);
                let reply = self.tool(role, name, json!({}));
                self.record(role, reply);
            }
            let text = demo;
            self.record(role, ChatMessage::text("assistant", text.clone()));
            return Ok(text);
        }
        // Models without native tools still get real fixture evidence, explicitly logged.
        if !self.tool_support && use_tools {
            self.sink.emit(EventKind::Notice(format!("{role}: native tools unavailable; harness supplies permitted selected company data as context.")));
            for name in tools::allowed(role) {
                let reply = self.tool(role, name, json!({}));
                self.record(
                    role,
                    ChatMessage::text(
                        "user",
                        format!(
                            "Harness-provided read-only evidence from {name}: {}",
                            reply.content
                        ),
                    ),
                );
            }
        }
        // Six responses maximum: at most five tool rounds, eight calls per response.
        for round in 0..6 {
            let agent = &self.agents[&role];
            let response = self
                .client
                .as_ref()
                .expect("Ollama client initialized")
                .chat(
                    self.config.options.model(role, &self.config.model),
                    &agent.history,
                    if self.tool_support && use_tools {
                        tools::schemas(role)
                    } else {
                        vec![]
                    },
                    format.clone(),
                )
                .await?;
            let text = response.content.clone();
            let calls = response.tool_calls.clone();
            self.record(role, response);
            if calls.is_empty() {
                if format.is_none() && looks_like_tool_call(&text) {
                    self.sink.emit(EventKind::Notice(format!("{role}: model returned tool-shaped text instead of a report; requesting a corrected response.")));
                    self.record(role, ChatMessage::text("user", "Your last response was a tool invocation encoded as text, not a report. Return your actual analysis in plain language using the evidence already available. Do not invent or request new functions.".into()));
                    continue;
                }
                return Ok(text);
            }
            if !use_tools {
                return Err("Model requested a tool during a text/structured-only turn".into());
            }
            if calls.len() > 8 || round == 5 {
                return Err(
                    "Agent exceeded bounded tool-call budget (5 rounds, 8 calls per round)".into(),
                );
            }
            for call in calls {
                let reply = self.tool(role, &call.function.name, call.function.arguments);
                self.record(role, reply);
            }
        }
        Err("Agent turn limit reached".into())
    }
    /// JoinSet aborts all owned workers when cancellation drops this future.
    /// Each worker owns exactly one Agent, so two requests never mutate one history.
    async fn batch(
        &mut self,
        jobs: Vec<Job>,
    ) -> Result<BTreeMap<Role, Result<String, String>>, String> {
        let permits =
            std::sync::Arc::new(tokio::sync::Semaphore::new(self.config.options.concurrency));
        let mut workers = tokio::task::JoinSet::new();
        for job in jobs {
            let role = job.role;
            let agent = self
                .agents
                .remove(&role)
                .ok_or("Duplicate agent job in batch")?;
            let mut worker = Engine {
                config: self.config.clone(),
                sink: self.sink.clone(),
                agents: BTreeMap::from([(role, agent)]),
                client: self.client.clone(),
                tool_support: self.tool_support,
            };
            self.sink.state(role, AgentStatus::Waiting);
            let permit = permits
                .clone()
                .acquire_owned()
                .await
                .expect("semaphore remains open");
            workers.spawn(async move {
                let _permit = permit;
                let result = async {
                    if worker.config.mode == Mode::Ollama {
                        let model = worker.config.options.model(role, &worker.config.model);
                        if model != worker.config.options.model(Role::Ceo, &worker.config.model) {
                            worker.tool_support = worker
                                .client
                                .as_ref()
                                .unwrap()
                                .supports_tools(model)
                                .await?;
                        }
                        worker
                            .sink
                            .emit(EventKind::Notice(format!("{role} uses model {model}")));
                    }
                    worker
                        .respond(role, job.input, job.format, job.use_tools, job.demo)
                        .await
                }
                .await;
                worker.sink.state(
                    role,
                    if result.is_ok() {
                        AgentStatus::Waiting
                    } else {
                        AgentStatus::Failed
                    },
                );
                (role, worker.agents.remove(&role).unwrap(), result)
            });
        }
        let mut results = BTreeMap::new();
        while let Some(joined) = workers.join_next().await {
            let (role, agent, result) = joined.map_err(|e| format!("Agent worker failed: {e}"))?;
            self.agents.insert(role, agent);
            results.insert(role, result);
        }
        Ok(results)
    }
    async fn consult(
        &mut self,
        reports: &mut BTreeMap<Role, Result<String, String>>,
    ) -> Result<(), String> {
        let roles: Vec<Role> = reports
            .iter()
            .filter_map(|(role, result)| result.is_ok().then_some(*role))
            .collect();
        if roles.len() < 2 {
            return Ok(());
        }
        self.sink.emit(EventKind::Notice("Peer assessment: each successful specialist may ask up to two other selected specialists. One round; no recursive calls.".into()));
        let jobs = roles.iter().map(|role| {
            let peers: Vec<_> = roles.iter().copied().filter(|p| p != role).collect();
            let demo = json!({"reason":"[DEMO] Check a cross-functional dependency before the final report.","consultations":[{"role":peers[0],"question":"[DEMO] What launch condition in your area changes my recommendation?"}]}).to_string();
            Job { role:*role, input:format!("Review your draft. Do you need peer input before finalizing? Available peers: {peers:?}. Return JSON {{reason: string, consultations: [{{role, question}}]}}. Choose zero to two distinct peers only when needed. Use an empty list when your evidence is sufficient. No tools in this assessment."), format:Some(peer_schema()),use_tools:false,demo }
        }).collect();
        let decisions = self.batch(jobs).await?;
        let mut requests = vec![];
        let mut notes: BTreeMap<Role, Vec<String>> = BTreeMap::new();
        for (role, result) in decisions {
            let decision = result.and_then(|text| {
                let plan: PeerPlan = serde_json::from_str(&text)
                    .map_err(|e| format!("Malformed peer assessment: {e}"))?;
                plan.validate(role, &roles)?;
                self.sink.emit(EventKind::Notice(format!(
                    "{role} peer assessment: {} ({} questions)",
                    plan.reason,
                    plan.consultations.len()
                )));
                Ok(plan)
            });
            match decision {
                Ok(plan) => {
                    for question in plan.consultations {
                        let message = Message::new(
                            self.config.id,
                            role,
                            question.role,
                            MessageKind::Consultation,
                            question.question,
                        );
                        self.sink.emit(EventKind::Message(message.clone()));
                        requests.push(self.deliver(message));
                    }
                }
                Err(error) => {
                    self.sink.emit(EventKind::Notice(format!(
                        "{role}: peer assessment failed: {error}"
                    )));
                    notes.entry(role).or_default().push(format!(
                        "Peer assessment failed: {error}. Disclose this limitation."
                    ));
                }
            }
        }
        // Batch questions per recipient: preserves one mutable history per role.
        let jobs = roles.iter().filter_map(|role| {
            let incoming: Vec<_> = requests.iter().filter(|m| m.recipient == *role).collect();
            if incoming.is_empty() { return None; }
            let questions: Vec<_> = incoming.iter().map(|m|json!({"request_id":m.id,"from":m.sender,"question":m.body})).collect();
            let demo = json!({"answers":incoming.iter().map(|m|json!({"request_id":m.id,"answer":format!("[DEMO peer reply from {role}] {}",reports[role].as_ref().unwrap())})).collect::<Vec<_>>()}).to_string();
            Some(Job {role:*role,input:format!("Answer these peer questions using your existing evidence: {}. Return JSON answers containing request_id and answer for each question, exactly once. Do not initiate more consultations. If evidence is missing say so.",json!(questions)),format:Some(answer_schema()),use_tools:false,demo})
        }).collect();
        let answers = self.batch(jobs).await?;
        for (role, result) in answers {
            let incoming: Vec<_> = requests.iter().filter(|m| m.recipient == role).collect();
            let parsed = result.and_then(|text| {
                let answers: PeerAnswers = serde_json::from_str(&text)
                    .map_err(|e| format!("Malformed peer answer: {e}"))?;
                let expected: BTreeSet<_> = incoming.iter().map(|m| m.id).collect();
                let actual: BTreeSet<_> = answers.answers.iter().map(|a| a.request_id).collect();
                if actual != expected
                    || actual.len() != answers.answers.len()
                    || answers.answers.iter().any(|a| a.answer.trim().is_empty())
                {
                    return Err(
                        "Peer reply IDs missing, duplicated or unexpected; or empty answer".into(),
                    );
                }
                Ok(answers)
            });
            for request in incoming {
                let body = match &parsed {
                    Ok(answers) => answers
                        .answers
                        .iter()
                        .find(|a| a.request_id == request.id)
                        .unwrap()
                        .answer
                        .clone(),
                    Err(error) => format!(
                        "Peer consultation failed for {role}: {error}. No answer is available."
                    ),
                };
                let mut message = Message::new(
                    self.config.id,
                    role,
                    request.sender,
                    MessageKind::ConsultationReply,
                    body.clone(),
                );
                message.reply_to = Some(request.id);
                self.sink.emit(EventKind::Message(message.clone()));
                self.deliver(message);
                notes.entry(request.sender).or_default().push(format!(
                    "Question to {role}: {}\nReply: {body}",
                    request.body
                ));
            }
        }
        let jobs = notes.into_iter().map(|(role,notes)| Job {
            role,input:format!("Finalize your report for CEO after the peer round. Preserve your original evidence, incorporate the following replies, identify disagreements or failed consultations, and do not claim certainty for missing information:\n{}",notes.join("\n\n")),format:None,use_tools:false,
            demo:format!("{}\n\n[DEMO] Peer consultation completed; retain the launch gates and uncertainty in this report.",reports[&role].as_ref().unwrap())
        }).collect();
        for (role, result) in self.batch(jobs).await? {
            reports.insert(role, result);
        }
        Ok(())
    }
    async fn execute(&mut self) -> Result<(), String> {
        self.config.options.validate()?;
        if self.config.mode == Mode::Demo && self.config.options.context.use_owner_data {
            return Err(
                "Demo uses fictional scripts. Switch to Ollama Mode to analyze owner context."
                    .into(),
            );
        }
        self.sink.emit(EventKind::Notice(format!(
            "{} run • {}",
            if self.config.mode == Mode::Demo {
                "DEMO / SCRIPTED"
            } else {
                "OLLAMA / LIVE"
            },
            self.config.model
        )));
        if self.config.mode == Mode::Ollama {
            let client = Ollama::new(&self.config.endpoint)?;
            self.tool_support = client
                .supports_tools(self.config.options.model(Role::Ceo, &self.config.model))
                .await?;
            self.client = Some(client);
        }
        let mut queue = VecDeque::new();
        self.send(
            &mut queue,
            Role::Owner,
            Role::Ceo,
            MessageKind::Task,
            self.config.goal.clone(),
        );
        self.sink.state(Role::Owner, AgentStatus::Waiting);
        let task = self.deliver(queue.pop_front().expect("owner task queued"));
        let raw_plan = self.turn(Role::Ceo, format!("Owner goal: {}\nFirst consult company profile. Return a JSON object with assessment (string) and delegations (array of role and task). Select 1–4 relevant roles from Cto, Cfo, Cmo, Legal, each once. Tasks must be specific to the goal. No Markdown fences.", task.body), true).await?;
        let plan = Plan::parse(&raw_plan)?;
        let mut ceo = CeoState::new(self.config.id, plan.delegations.iter().map(|d| d.role));
        self.send(
            &mut queue,
            Role::Ceo,
            Role::Owner,
            MessageKind::Assessment,
            raw_plan,
        );
        for d in plan.delegations {
            self.sink.state(d.role, AgentStatus::Waiting);
            self.send(&mut queue, Role::Ceo, d.role, MessageKind::Task, d.task);
        }
        self.sink.state(Role::Ceo, AgentStatus::Waiting);
        let mut jobs = vec![];
        while let Some(message) = queue.pop_front() {
            let message = self.deliver(message);
            if message.kind == MessageKind::Task {
                jobs.push(Job::report(message.recipient, format!("Owner goal: {}\nCEO assessment: {}\nYour assignment: {}\nConsult permitted data, then draft evidence, risks and recommendation. A peer consultation round may follow before reporting to CEO.", self.config.goal, plan.assessment, message.body)));
            }
        }
        let mut reports = self.batch(jobs).await?;
        if self.config.options.peer_consultation {
            self.consult(&mut reports).await?;
        }
        for (role, result) in reports {
            let (kind, body, status) = match result {
                Ok(text) => (MessageKind::Report, text, AgentStatus::Completed),
                Err(error) => (
                    MessageKind::Failure,
                    format!("Specialist {role} failed: {error}"),
                    AgentStatus::Failed,
                ),
            };
            self.sink.state(role, status);
            self.send(&mut queue, role, Role::Ceo, kind, body);
        }
        while let Some(message) = queue.pop_front() {
            let message = self.deliver(message);
            ceo.accept(message.run_id, message.sender, message.body);
        }
        if ceo.begin_finalization() {
            let reports = ceo
                .reports
                .iter()
                .map(|(r, s)| format!("{r}:\n{s}"))
                .collect::<Vec<_>>()
                .join("\n\n");
            let final_report = self.turn(Role::Ceo, format!("All required specialist outcomes have arrived:\n{reports}\nProduce the final report for Owner on the original goal. Give a decision, evidence, risks, launch gates, and proposed next steps requiring human approval. Explicitly disclose failed reports and consultations. Never imply failed work succeeded. Return plain language, not JSON or tool calls. No further tools are available or needed."), false).await?;
            self.send(
                &mut queue,
                Role::Ceo,
                Role::Owner,
                MessageKind::Final,
                final_report,
            );
            self.deliver(queue.pop_front().expect("final queued"));
            self.sink.state(Role::Owner, AgentStatus::Completed);
            self.sink.state(Role::Ceo, AgentStatus::Completed);
        }
        if !ceo.decision_sent {
            return Err("Run ended without all required reports".into());
        }
        Ok(())
    }
}

pub async fn run(config: RunConfig, sink: EventSink, mut cancel: watch::Receiver<bool>) {
    let agents = config.options.agents();
    let mut engine = Engine {
        config,
        sink: sink.clone(),
        agents,
        client: None,
        tool_support: false,
    };
    // Dropping execute cancels its current HTTP future or demo delay immediately.
    // A total deadline complements the per-request timeout.
    let outcome = tokio::select! {
        biased;
        _ = async { if !*cancel.borrow() { let _ = cancel.changed().await; } } => {
            sink.emit(EventKind::Notice("Run cancelled by Owner. Pending inference was dropped; Ollama may finish server-side work.".into()));
            RunOutcome::Cancelled
        }
        result = tokio::time::timeout(Duration::from_secs(900), engine.execute()) => {
            match result {
                Ok(Ok(())) => RunOutcome::Completed,
                Ok(Err(error)) => { sink.emit(EventKind::Notice(format!("Run failed: {error}"))); RunOutcome::Failed }
                Err(_) => { sink.emit(EventKind::Notice("Run failed: 15-minute total deadline exceeded".into())); RunOutcome::Failed }
            }
        }
    };
    sink.emit(EventKind::Finished { outcome });
}
fn looks_like_tool_call(text: &str) -> bool {
    serde_json::from_str::<Value>(text).ok().is_some_and(|v| {
        v.get("tool_calls").is_some()
            || v.get("function").is_some()
            || (v.get("name").is_some()
                && (v.get("parameters").is_some() || v.get("arguments").is_some()))
    })
}

fn demo_response(role: Role, planning: bool) -> String {
    if planning {
        return json!({"assessment":"[DEMO / SCRIPTED] Launch readiness requires technical, financial, marketing and policy review.","delegations":Role::SPECIALISTS.map(|r| json!({"role":r,"task":format!("[DEMO / SCRIPTED] Assess launch readiness from the {r} perspective using fictional fixtures.")}))}).to_string();
    }
    format!(
        "[DEMO / SCRIPTED — not LLM output]\n\n{}",
        match role {
            Role::Cto =>
                "Conditional readiness: core beta is ready, but two critical bugs, tenant isolation audit and restore drill block public release. Estimated remediation is three weeks; require evidence before launch.",
            Role::Cfo =>
                "Cash is $240,000, monthly burn $40,000 and revenue $12,000. Net burn is $28,000/month. Runway is 8.57 months before launch costs, or 7.32 months after a $35,000 launch. Keep spending gated; revenue is uncertain.",
            Role::Cmo =>
                "1,800 waitlist entries and 420 qualified leads suggest interest. Only 35 beta customers and 64% weekly activity provide limited retention evidence. Prefer an invitation-only launch with explicit success metrics.",
            Role::Legal =>
                "Privacy notice is draft, DPA needs review and the security audit is pending. Complete and approve these before public launch. This is fictional analysis.",
            _ =>
                "Recommendation: conditional NO-GO for a broad public launch next month; propose a limited beta.\n\nEvidence: engineering estimates three weeks of remediation, two critical bugs remain, and privacy/DPA approval is outstanding. Financial runway after launch cost is approximately 7.32 months. Demand exists but retention evidence is weak.\n\nLaunch gates: pass isolation audit and restore drill; close critical bugs; approve privacy notice and DPA; validate beta retention.\n\nFor human approval: propose a staged beta and capped launch budget, with a readiness review one week before launch. No action has been executed.",
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reject_invalid_delegations() {
        assert!(
            Plan::parse(r#"{"assessment":"a","delegations":[{"role":"Owner","task":"x"}]}"#)
                .is_err()
        );
        assert!(Plan::parse(r#"{"assessment":"a","delegations":[{"role":"Cto","task":"x"},{"role":"Cto","task":"y"}]}"#).is_err());
        assert!(Plan::parse("not json").is_err());
    }
    #[tokio::test]
    async fn demo_routes_in_order_and_finishes_once() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let id = Uuid::new_v4();
        run(
            RunConfig {
                id,
                mode: Mode::Demo,
                endpoint: "unused".into(),
                model: "demo".into(),
                goal: "Launch?".into(),
                options: crate::config::RunOptions::default(),
            },
            EventSink {
                run_id: id,
                tx,
                repaint: None,
            },
            cancel_rx,
        )
        .await;
        let mut sent = BTreeSet::new();
        let mut final_count = 0;
        let mut completed = 0;
        for event in rx.try_iter() {
            if let EventKind::Message(m) = event.kind {
                assert!(m.valid_route());
                match m.status {
                    MessageStatus::Sent => {
                        assert!(sent.insert(m.id));
                    }
                    MessageStatus::Delivered => {
                        assert!(sent.contains(&m.id));
                        if m.kind == MessageKind::Final {
                            final_count += 1;
                        }
                    }
                }
            } else if let EventKind::Finished { outcome } = event.kind {
                assert_eq!(outcome, RunOutcome::Completed);
                completed += 1;
            }
        }
        assert_eq!(final_count, 1);
        assert_eq!(completed, 1);
    }
    #[tokio::test]
    async fn cancellation_prevents_finalization() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (_cancel_tx, cancel_rx) = watch::channel(true);
        let id = Uuid::new_v4();
        run(
            RunConfig {
                id,
                mode: Mode::Demo,
                endpoint: "unused".into(),
                model: "demo".into(),
                goal: "x".into(),
                options: crate::config::RunOptions::default(),
            },
            EventSink {
                run_id: id,
                tx,
                repaint: None,
            },
            cancel_rx,
        )
        .await;
        let events: Vec<_> = rx.try_iter().collect();
        assert!(events.iter().any(|e| matches!(
            e.kind,
            EventKind::Finished {
                outcome: RunOutcome::Cancelled
            }
        )));
        assert!(
            !events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::Message(m) if m.kind == MessageKind::Final))
        );
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::test_support::server;
    fn answer(content: &str) -> (u16, Value) {
        (
            200,
            json!({"done":true,"message":{"role":"assistant","content":content}}),
        )
    }
    fn call(name: &str) -> (u16, Value) {
        (
            200,
            json!({"done":true,"message":{"role":"assistant","content":"","tool_calls":[{"function":{"name":name,"arguments":{}}}]}}),
        )
    }
    fn config(id: Uuid, endpoint: String) -> RunConfig {
        RunConfig {
            id,
            endpoint,
            mode: Mode::Ollama,
            model: "mock".into(),
            goal: "Launch next month?".into(),
            options: crate::config::RunOptions::default(),
        }
    }
    #[tokio::test]
    async fn real_protocol_tools_isolation_and_failed_specialist_synthesis() {
        let plan = r#"{"assessment":"Review engineering and cash","delegations":[{"role":"Cto","task":"Assess engineering"},{"role":"Cfo","task":"Assess cash"}]}"#;
        let (url, server) = server(vec![
            (200, json!({"capabilities":["tools"]})),
            call("get_company_profile"),
            answer(plan),
            call("get_financial_summary"),
            call("get_product_roadmap"),
            answer("CTO original full report: audit pending."),
            answer(""),
            answer("Final: conditional no-go; CFO failed, financial evidence unavailable."),
        ])
        .await;
        let (tx, rx) = std::sync::mpsc::channel();
        let (_cancel, cancel_rx) = watch::channel(false);
        let id = Uuid::new_v4();
        tokio::time::timeout(
            Duration::from_secs(10),
            run(
                config(id, url),
                EventSink {
                    run_id: id,
                    tx,
                    repaint: None,
                },
                cancel_rx,
            ),
        )
        .await
        .unwrap();
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 8);
        assert!(requests[1].0.starts_with("POST /api/chat"));
        assert_eq!(requests[1].1["stream"], false);
        assert!(
            requests[2].1["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["role"] == "tool" && m["tool_name"] == "get_company_profile")
        );
        assert!(
            requests[4].1["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["role"] == "tool"
                    && m["content"]
                        .as_str()
                        .unwrap_or("")
                        .contains("not authorized"))
        );
        assert!(
            !requests[6].1["messages"]
                .to_string()
                .contains("CTO original full report")
        );
        assert!(
            requests[7].1["messages"]
                .to_string()
                .contains("Specialist CFO failed")
        );
        let events: Vec<_> = rx.try_iter().collect();
        assert_eq!(events.iter().filter(|e| matches!(&e.kind,EventKind::Message(m) if m.kind==MessageKind::Final && m.status==MessageStatus::Delivered)).count(),1);
        assert!(events.iter().any(|e| matches!(
            &e.kind,
            EventKind::Tool {
                outcome: Some(Err(_)),
                ..
            }
        )));
        assert!(events.iter().any(|e| matches!(
            e.kind,
            EventKind::State {
                role: Role::Cfo,
                status: AgentStatus::Failed
            }
        )));
        assert!(matches!(
            events.last().unwrap().kind,
            EventKind::Finished {
                outcome: RunOutcome::Completed
            }
        ));
    }
    #[tokio::test]
    async fn repeated_tool_calls_stop_at_budget() {
        let mut responses = vec![(200, json!({"capabilities":["tools"]}))];
        responses.extend((0..6).map(|_| call("get_company_profile")));
        let (url, server) = server(responses).await;
        let (tx, rx) = std::sync::mpsc::channel();
        let (_cancel, cancel_rx) = watch::channel(false);
        let id = Uuid::new_v4();
        tokio::time::timeout(
            Duration::from_secs(10),
            run(
                config(id, url),
                EventSink {
                    run_id: id,
                    tx,
                    repaint: None,
                },
                cancel_rx,
            ),
        )
        .await
        .unwrap();
        assert_eq!(server.await.unwrap().len(), 7);
        let events: Vec<_> = rx.try_iter().collect();
        assert!(
            events
                .iter()
                .any(|e| matches!(&e.kind,EventKind::Notice(s) if s.contains("budget")))
        );
        assert!(matches!(
            events.last().unwrap().kind,
            EventKind::Finished {
                outcome: RunOutcome::Failed
            }
        ));
    }
    #[tokio::test]
    async fn cancellation_interrupts_pending_http() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            let _ = accepted_tx.send(());
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let (cancel, cancel_rx) = watch::channel(false);
        let id = Uuid::new_v4();
        let worker = tokio::spawn(run(
            config(id, url),
            EventSink {
                run_id: id,
                tx,
                repaint: None,
            },
            cancel_rx,
        ));
        tokio::time::timeout(Duration::from_secs(2), accepted_rx)
            .await
            .unwrap()
            .unwrap();
        cancel.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .unwrap()
            .unwrap();
        server.abort();
        assert!(rx.try_iter().any(|e| matches!(
            e.kind,
            EventKind::Finished {
                outcome: RunOutcome::Cancelled
            }
        )));
    }
}

#[cfg(test)]
mod synthesis_tests {
    use super::*;
    #[tokio::test]
    async fn plain_text_tool_invocation_is_corrected_without_final_tools() {
        let reply = |text: &str| {
            (
                200,
                json!({"done":true,"message":{"role":"assistant","content":text}}),
            )
        };
        let (url, server) = crate::test_support::server(vec![
            reply(r#"{"name":"invented_action","parameters":{}}"#),
            reply("Do not launch until the isolation audit passes."),
        ])
        .await;
        let id = Uuid::new_v4();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut engine = Engine {
            config: RunConfig {
                id,
                mode: Mode::Ollama,
                endpoint: url.clone(),
                model: "mock".into(),
                goal: "launch?".into(),
                options: crate::config::RunOptions::default(),
            },
            sink: EventSink {
                run_id: id,
                tx,
                repaint: None,
            },
            agents: Role::ALL.into_iter().map(|r| (r, Agent::new(r))).collect(),
            client: Some(Ollama::new(&url).unwrap()),
            tool_support: true,
        };
        assert_eq!(
            engine
                .turn(Role::Ceo, "Synthesize reports".into(), false)
                .await
                .unwrap(),
            "Do not launch until the isolation audit passes."
        );
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|(_, body)| body["tools"] == json!([])));
        assert!(
            rx.try_iter()
                .any(|e| matches!(e.kind, EventKind::Notice(_)))
        );
    }
    #[tokio::test]
    async fn non_tool_model_receives_real_evidence_without_scripted_output() {
        let (url,server)=crate::test_support::server(vec![(200,json!({"done":true,"message":{"role":"assistant","content":"Model's own financial assessment"}}))]).await;
        let id = Uuid::new_v4();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut engine = Engine {
            config: RunConfig {
                id,
                mode: Mode::Ollama,
                endpoint: url.clone(),
                model: "mock".into(),
                goal: "launch?".into(),
                options: crate::config::RunOptions::default(),
            },
            sink: EventSink {
                run_id: id,
                tx,
                repaint: None,
            },
            agents: Role::ALL.into_iter().map(|r| (r, Agent::new(r))).collect(),
            client: Some(Ollama::new(&url).unwrap()),
            tool_support: false,
        };
        assert_eq!(
            engine
                .turn(Role::Cfo, "Assess cash".into(), false)
                .await
                .unwrap(),
            "Model's own financial assessment"
        );
        let requests = server.await.unwrap();
        assert!(requests[0].1["messages"].to_string().contains("240000"));
        assert_eq!(requests[0].1["tools"], json!([]));
        assert_eq!(
            rx.try_iter()
                .filter(|e| matches!(
                    e.kind,
                    EventKind::Tool {
                        outcome: Some(Ok(_)),
                        ..
                    }
                ))
                .count(),
            2
        );
    }
}

#[cfg(test)]
mod collaboration_tests {
    use super::*;
    #[test]
    fn peer_routes_and_plans_are_bounded() {
        let plan = |role| PeerPlan {
            reason: "Need evidence".into(),
            consultations: vec![PeerQuestion {
                role,
                question: "What is the launch budget?".into(),
            }],
        };
        assert!(
            plan(Role::Cfo)
                .validate(Role::Cto, &[Role::Cto, Role::Cfo])
                .is_ok()
        );
        for role in [Role::Owner, Role::Ceo, Role::Cto, Role::Legal] {
            assert!(
                plan(role)
                    .validate(Role::Cto, &[Role::Cto, Role::Cfo])
                    .is_err()
            );
        }
        for a in Role::ALL {
            for b in Role::ALL {
                assert_eq!(
                    Message::new(
                        Uuid::new_v4(),
                        a,
                        b,
                        MessageKind::Consultation,
                        "question".into()
                    )
                    .valid_route(),
                    a != b && Role::SPECIALISTS.contains(&a) && Role::SPECIALISTS.contains(&b)
                );
            }
        }
    }
    #[tokio::test]
    async fn concurrent_demo_consults_before_exactly_one_final() {
        let id = Uuid::new_v4();
        let (tx, rx) = std::sync::mpsc::channel();
        let (_cancel, cancel_rx) = watch::channel(false);
        let options = crate::config::RunOptions {
            concurrency: 2,
            peer_consultation: true,
            ..Default::default()
        };
        run(
            RunConfig {
                id,
                mode: Mode::Demo,
                endpoint: "unused".into(),
                model: "demo".into(),
                goal: "launch".into(),
                options,
            },
            EventSink {
                run_id: id,
                tx,
                repaint: None,
            },
            cancel_rx,
        )
        .await;
        let events: Vec<_> = rx.try_iter().collect();
        let mut working = BTreeSet::new();
        let mut peak = 0;
        let mut questions = BTreeMap::new();
        let mut answered = BTreeSet::new();
        let mut finals = 0;
        for event in events {
            match event.kind {
                EventKind::State { role, status } if Role::SPECIALISTS.contains(&role) => {
                    if status == AgentStatus::Working {
                        working.insert(role);
                    } else {
                        working.remove(&role);
                    }
                    peak = peak.max(working.len());
                }
                EventKind::Message(m) if m.status == MessageStatus::Delivered => match m.kind {
                    MessageKind::Consultation => {
                        questions.insert(m.id, (m.sender, m.recipient));
                    }
                    MessageKind::ConsultationReply => {
                        let id = m.reply_to.unwrap();
                        assert_eq!(questions[&id], (m.recipient, m.sender));
                        assert!(answered.insert(id));
                    }
                    MessageKind::Final => {
                        assert_eq!(answered.len(), questions.len());
                        assert!(!questions.is_empty());
                        finals += 1;
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        assert_eq!(peak, 2);
        assert_eq!(questions.len(), 4);
        assert_eq!(finals, 1);
    }
    #[tokio::test]
    async fn cancellation_aborts_parallel_workers() {
        let id = Uuid::new_v4();
        let (tx, rx) = std::sync::mpsc::channel();
        let (cancel, cancel_rx) = watch::channel(false);
        let worker = tokio::spawn(run(
            RunConfig {
                id,
                mode: Mode::Demo,
                endpoint: "unused".into(),
                model: "demo".into(),
                goal: "launch".into(),
                options: crate::config::RunOptions {
                    concurrency: 4,
                    peer_consultation: true,
                    ..Default::default()
                },
            },
            EventSink {
                run_id: id,
                tx,
                repaint: None,
            },
            cancel_rx,
        ));
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if rx.try_iter().any(|e| {
                    matches!(
                        e.kind,
                        EventKind::State {
                            role: Role::Cto,
                            status: AgentStatus::Working
                        }
                    )
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        cancel.send(true).unwrap();
        worker.await.unwrap();
        tokio::time::sleep(Duration::from_millis(750)).await;
        let events: Vec<_> = rx.try_iter().collect();
        assert!(matches!(
            events.last().unwrap().kind,
            EventKind::Finished {
                outcome: RunOutcome::Cancelled
            }
        ));
        assert!(
            !events
                .iter()
                .any(|e| matches!(&e.kind,EventKind::Message(m) if m.kind==MessageKind::Final))
        );
    }
}

#[cfg(test)]
mod peer_wire_tests {
    use super::*;
    #[tokio::test]
    async fn peer_questions_use_real_model_answers_and_correlated_ids() {
        let mut index = 0;
        let (url,server)=crate::test_support::server_with(4,move |request| {
            let text=match index {
                0=>json!({"reason":"Need cash constraints","consultations":[{"role":"Cfo","question":"What budget limits apply?"}]}).to_string(),
                1=>json!({"reason":"No missing dependency","consultations":[]}).to_string(),
                2=>{
                    let prompt=request["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap();
                    let questions=prompt.strip_prefix("Answer these peer questions using your existing evidence: ").unwrap().split(". Return JSON").next().unwrap();
                    let questions:Value=serde_json::from_str(questions).unwrap();
                    json!({"answers":[{"request_id":questions[0]["request_id"],"answer":"Actual model answer: cap proposed spend at 35000 USD."}]}).to_string()
                },
                _=>"Revised CTO report: use the peer's 35000 USD spending cap.".into(),
            };index+=1;(200,json!({"done":true,"message":{"role":"assistant","content":text}}))
        }).await;
        let id = Uuid::new_v4();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut engine = Engine {
            config: RunConfig {
                id,
                mode: Mode::Ollama,
                endpoint: url.clone(),
                model: "mock".into(),
                goal: "launch".into(),
                options: Default::default(),
            },
            sink: EventSink {
                run_id: id,
                tx,
                repaint: None,
            },
            agents: Role::ALL.into_iter().map(|r| (r, Agent::new(r))).collect(),
            client: Some(Ollama::new(&url).unwrap()),
            tool_support: true,
        };
        let mut reports = BTreeMap::from([
            (Role::Cto, Ok("Engineering draft".into())),
            (Role::Cfo, Ok("Finance draft".into())),
        ]);
        tokio::time::timeout(Duration::from_secs(5), engine.consult(&mut reports))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(server.await.unwrap().len(), 4);
        assert!(reports[&Role::Cto].as_ref().unwrap().contains("35000"));
        let events: Vec<_> = rx.try_iter().collect();
        let question = events
            .iter()
            .find_map(|e| match &e.kind {
                EventKind::Message(m) if m.kind == MessageKind::Consultation => Some(m),
                _ => None,
            })
            .unwrap();
        let answer = events
            .iter()
            .find_map(|e| match &e.kind {
                EventKind::Message(m) if m.kind == MessageKind::ConsultationReply => Some(m),
                _ => None,
            })
            .unwrap();
        assert_eq!(answer.reply_to, Some(question.id));
        assert_eq!(answer.sender, Role::Cfo);
        assert!(answer.body.starts_with("Actual model answer"));
    }
    #[tokio::test]
    async fn role_model_and_owner_context_reach_the_wire_without_sample_data() {
        let (url, server) = crate::test_support::server(vec![
            (200, json!({"capabilities":[]})),
            (
                200,
                json!({"done":true,"message":{"role":"assistant","content":"Owner analysis"}}),
            ),
        ])
        .await;
        let id = Uuid::new_v4();
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut options = crate::config::RunOptions::default();
        options.context.use_owner_data = true;
        options.context.profile = "Owner bakery".into();
        options.context.finances = "Cash: 99 EUR".into();
        options
            .prompts
            .insert(Role::Cfo, "Custom accountant prompt".into());
        options
            .role_models
            .insert(Role::Cfo, "finance-model".into());
        let agents = options.agents();
        let mut engine = Engine {
            config: RunConfig {
                id,
                mode: Mode::Ollama,
                endpoint: url.clone(),
                model: "default-model".into(),
                goal: "cash".into(),
                options,
            },
            sink: EventSink {
                run_id: id,
                tx,
                repaint: None,
            },
            agents,
            client: Some(Ollama::new(&url).unwrap()),
            tool_support: true,
        };
        let results = engine
            .batch(vec![Job::report(Role::Cfo, "Assess cash".into())])
            .await
            .unwrap();
        assert!(results[&Role::Cfo].is_ok());
        let requests = server.await.unwrap();
        assert_eq!(requests[0].1["model"], "finance-model");
        assert_eq!(requests[1].1["model"], "finance-model");
        let text = requests[1].1.to_string();
        assert!(text.contains("Cash: 99 EUR"));
        assert!(text.contains("Custom accountant prompt"));
        assert!(!text.contains("240000"));
        assert!(!text.contains("Northstar"));
    }
}
