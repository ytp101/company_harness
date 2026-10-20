mod settings;
use crate::{
    agent::Agent,
    message::{MessageKind, MessageStatus, Role},
    ollama::Ollama,
    orchestrator::{self, EventSink, Mode, RunConfig},
    state::{AgentStatus, Event, EventKind, RunOutcome},
    tools,
};
use eframe::egui::{self, Align2, Color32, FontId, Pos2, RichText, Stroke, Vec2};
use std::{
    collections::BTreeMap,
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};
use tokio::{runtime::Runtime, sync::watch};
use uuid::Uuid;
const ACCENT: Color32 = Color32::from_rgb(93, 222, 200);
const MUTED: Color32 = Color32::from_rgb(142, 157, 180);
const TOOL: Color32 = Color32::from_rgb(190, 157, 255);
const DEFAULT_GOAL: &str = "Evaluate whether our fictional SaaS should launch next month";

struct Pulse {
    from: Role,
    to: Role,
    at: Instant,
    delivered: bool,
}
#[derive(Default)]
pub struct ViewState {
    pub run_id: Option<Uuid>,
    pub events: Vec<Event>,
    pub agents: BTreeMap<Role, Agent>,
    pub statuses: BTreeMap<Role, AgentStatus>,
    pub final_report: String,
    pub active: bool,
    pub outcome: Option<RunOutcome>,
}
impl ViewState {
    pub fn fresh(run_id: Option<Uuid>) -> Self {
        Self {
            run_id,
            agents: Role::ALL.into_iter().map(|r| (r, Agent::new(r))).collect(),
            statuses: Role::ALL
                .into_iter()
                .map(|r| (r, AgentStatus::Idle))
                .collect(),
            ..Default::default()
        }
    }
    pub fn apply(&mut self, event: Event) -> bool {
        if self.run_id != Some(event.run_id) || !self.active {
            return false;
        }
        match &event.kind {
            EventKind::State { role, status } => {
                self.statuses.insert(*role, *status);
            }
            EventKind::History { role, message } => {
                self.agents
                    .get_mut(role)
                    .expect("known agent")
                    .history
                    .push(message.clone());
            }
            EventKind::Message(m)
                if m.kind == MessageKind::Final && m.status == MessageStatus::Delivered =>
            {
                if self.final_report.is_empty() {
                    self.final_report = m.body.clone();
                }
            }
            EventKind::Finished { outcome } => {
                self.active = false;
                self.outcome = Some(*outcome);
                for status in self.statuses.values_mut() {
                    if matches!(status, AgentStatus::Working | AgentStatus::Waiting) {
                        *status = if *outcome == RunOutcome::Completed {
                            AgentStatus::Completed
                        } else if *outcome == RunOutcome::Cancelled {
                            AgentStatus::Idle
                        } else {
                            AgentStatus::Failed
                        };
                    }
                }
                if *outcome == RunOutcome::Failed {
                    self.statuses.insert(Role::Ceo, AgentStatus::Failed);
                }
            }
            _ => {}
        }
        self.events.push(event);
        true
    }
}
pub struct CompanyApp {
    runtime: Option<Runtime>,
    rx: Receiver<Event>,
    tx: mpsc::Sender<Event>,
    models_rx: Receiver<Result<Vec<String>, String>>,
    models_tx: mpsc::Sender<Result<Vec<String>, String>>,
    cancel: Option<watch::Sender<bool>>,
    view: ViewState,
    mode: Mode,
    endpoint: String,
    model: String,
    models: Vec<String>,
    connection: String,
    discovering: bool,
    goal: String,
    options: crate::config::RunOptions,
    selected_role: Role,
    selected_event: Option<usize>,
    inspector: bool,
    pulses: Vec<Pulse>,
    started: Option<Instant>,
    elapsed: Duration,
    settings_open: bool,
    model_lab_open: bool,
    eval_rx: Receiver<crate::evaluation::Update>,
    eval_tx: mpsc::Sender<crate::evaluation::Update>,
    eval_cancel: Option<watch::Sender<bool>>,
    evaluating: bool,
    eval_models: std::collections::BTreeSet<String>,
    evaluations: Vec<crate::evaluation::Evaluation>,
    eval_status: String,
}
impl CompanyApp {
    pub fn new(cc: &eframe::CreationContext<'_>, runtime: Runtime) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let mut style = (*cc.egui_ctx.style()).clone();
        style.spacing.item_spacing = Vec2::new(10.0, 8.0);
        style.visuals.panel_fill = Color32::from_rgb(17, 23, 33);
        style.visuals.window_fill = Color32::from_rgb(23, 31, 44);
        style.visuals.selection.bg_fill = Color32::from_rgb(34, 85, 84);
        cc.egui_ctx.set_style(style);
        let (tx, rx) = mpsc::channel();
        let (models_tx, models_rx) = mpsc::channel();
        let (eval_tx, eval_rx) = mpsc::channel();
        let mut app = Self {
            runtime: Some(runtime),
            rx,
            tx,
            models_rx,
            models_tx,
            cancel: None,
            view: ViewState::fresh(None),
            mode: if std::env::args().any(|s| s == "--demo") {
                Mode::Demo
            } else {
                Mode::Ollama
            },
            endpoint: "http://localhost:11434".into(),
            model: String::new(),
            models: vec![],
            connection: "Not checked".into(),
            discovering: false,
            goal: DEFAULT_GOAL.into(),
            options: crate::config::RunOptions {
                concurrency: 2,
                peer_consultation: true,
                ..Default::default()
            },
            selected_role: Role::Ceo,
            selected_event: None,
            inspector: false,
            pulses: vec![],
            started: None,
            elapsed: Duration::ZERO,
            settings_open: std::env::args().any(|a| a == "--setup"),
            model_lab_open: std::env::args().any(|a| a == "--model-lab"),
            eval_rx,
            eval_tx,
            eval_cancel: None,
            evaluating: false,
            eval_models: Default::default(),
            evaluations: vec![],
            eval_status: "No evaluation results in this session".into(),
        };
        app.discover(&cc.egui_ctx);
        if app.mode == Mode::Demo && std::env::args().any(|s| s == "--autorun") {
            app.start(&cc.egui_ctx);
        }
        app
    }
    fn discover(&mut self, ctx: &egui::Context) {
        self.discovering = true;
        self.connection = "Connecting to local Ollama…".into();
        let endpoint = self.endpoint.clone();
        let tx = self.models_tx.clone();
        let ctx = ctx.clone();
        self.runtime.as_ref().unwrap().spawn(async move {
            let result = match Ollama::new(&endpoint) {
                Ok(client) => client.models().await,
                Err(e) => Err(e),
            };
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }
    fn start(&mut self, ctx: &egui::Context) {
        let id = Uuid::new_v4();
        self.view = ViewState::fresh(Some(id));
        self.view.agents = self.options.agents();
        self.view.active = true;
        self.selected_event = None;
        self.pulses.clear();
        self.started = Some(Instant::now());
        self.elapsed = Duration::ZERO;
        let (cancel, receiver) = watch::channel(false);
        self.cancel = Some(cancel);
        let config = RunConfig {
            id,
            mode: self.mode,
            endpoint: self.endpoint.clone(),
            model: self.model.trim().into(),
            goal: self.goal.trim().into(),
            options: self.options.clone(),
        };
        let sink = EventSink {
            run_id: id,
            tx: self.tx.clone(),
            repaint: Some(ctx.clone()),
        };
        self.runtime
            .as_ref()
            .unwrap()
            .spawn(orchestrator::run(config, sink, receiver));
    }
    fn clear(&mut self) {
        self.view = ViewState::fresh(None);
        self.pulses.clear();
        self.selected_event = None;
        self.started = None;
        self.elapsed = Duration::ZERO;
    }
    fn poll(&mut self) {
        for update in self.eval_rx.try_iter() {
            match update {
                crate::evaluation::Update::Result(result) => self.evaluations.push(result),
                crate::evaluation::Update::Finished(status) => {
                    self.evaluating = false;
                    self.eval_cancel = None;
                    self.eval_status = status;
                }
            }
        }
        for result in self.models_rx.try_iter() {
            self.discovering = false;
            match result {
                Ok(models) => {
                    self.connection = format!("Connected • {} installed models", models.len());
                    if self.model.is_empty() {
                        self.model = models.first().cloned().unwrap_or_default();
                    }
                    if self.eval_models.is_empty()
                        && let Some(first) = models.first()
                    {
                        self.eval_models.insert(first.clone());
                    }
                    self.models = models;
                }
                Err(error) => {
                    self.connection = error;
                    self.models.clear();
                }
            }
        }
        for event in self.rx.try_iter().take(128) {
            let pulse = match &event.kind {
                EventKind::Message(m) => Some((m.sender, m.recipient, m.status)),
                _ => None,
            };
            if self.view.apply(event) {
                if let Some((from, to, status)) = pulse {
                    self.pulses.push(Pulse {
                        from,
                        to,
                        at: Instant::now(),
                        delivered: status == MessageStatus::Delivered,
                    });
                }
                if !self.view.active {
                    self.cancel = None;
                    self.elapsed = self.started.map(|t| t.elapsed()).unwrap_or_default();
                }
            }
        }
        self.pulses.retain(|p| p.at.elapsed().as_secs_f32() < 1.5);
    }
    fn controls(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("N / COMPANY HARNESS")
                    .size(21.0)
                    .strong()
                    .color(ACCENT),
            );
            ui.label(RichText::new("LOCAL AGENT LAB").color(MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(if self.mode == Mode::Demo {
                        "DEMO • SCRIPTED"
                    } else {
                        "OLLAMA • LIVE INFERENCE"
                    })
                    .color(if self.mode == Mode::Demo {
                        TOOL
                    } else {
                        ACCENT
                    }),
                );
            });
        });
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Company context & agents").clicked() {
                self.settings_open = true;
            }
            if ui.button("Model lab & recommendations").clicked() {
                self.model_lab_open = true;
            }
            ui.label(format!(
                "{} | {} concurrent | peers {}",
                if self.options.context.use_owner_data {
                    "Owner context"
                } else {
                    "Fictional fixtures"
                },
                self.options.concurrency,
                if self.options.peer_consultation {
                    "on"
                } else {
                    "off"
                }
            ));
        });
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(!self.view.active && !self.evaluating, |ui| {
                ui.selectable_value(&mut self.mode, Mode::Ollama, "Ollama Mode");
                ui.selectable_value(&mut self.mode, Mode::Demo, "Demo Mode");
                ui.separator();
                ui.label("Model");
                egui::ComboBox::from_id_salt("model_picker")
                    .selected_text("Installed models")
                    .show_ui(ui, |ui| {
                        for model in &self.models {
                            ui.selectable_value(&mut self.model, model.clone(), model);
                        }
                    });
                ui.add(
                    egui::TextEdit::singleline(&mut self.model)
                        .desired_width(180.0)
                        .hint_text("Editable model name"),
                );
            });
            if ui
                .add_enabled(
                    !self.discovering && !self.view.active && !self.evaluating,
                    egui::Button::new("Refresh models"),
                )
                .clicked()
            {
                self.discover(ctx);
            }
        });
        egui::CollapsingHeader::new("Connection settings & status").show(ui, |ui| {
            ui.add_enabled_ui(!self.view.active && !self.evaluating && !self.discovering, |ui| { ui.horizontal(|ui| { ui.label("Local endpoint"); ui.text_edit_singleline(&mut self.endpoint); }); });
            ui.label(&self.connection);
            ui.small("Only local HTTP endpoints. Requests: 120 seconds; run: 15 minutes. Context is kept in memory.");
        });
        ui.horizontal(|ui| {
            ui.add_enabled(
                !self.view.active && !self.evaluating,
                egui::TextEdit::multiline(&mut self.goal)
                    .desired_rows(2)
                    .desired_width((ui.available_width() - 290.0).max(250.0))
                    .hint_text("What should the company evaluate?"),
            );
            let valid = self.options.validate().is_ok()
                && !(self.mode == Mode::Demo && self.options.context.use_owner_data)
                && !self.goal.trim().is_empty()
                && self.goal.len() <= 16000
                && (self.mode == Mode::Demo || !self.model.trim().is_empty());
            if ui
                .add_enabled(
                    !self.view.active && !self.evaluating && valid,
                    egui::Button::new(RichText::new("Start").color(ACCENT)),
                )
                .clicked()
            {
                self.start(ctx);
            }
            if ui
                .add_enabled(self.view.active, egui::Button::new("Cancel"))
                .clicked()
                && let Some(cancel) = &self.cancel
            {
                let _ = cancel.send(true);
            }
            if ui
                .add_enabled(
                    !self.view.active && !self.evaluating,
                    egui::Button::new("Clear"),
                )
                .on_hover_text("Clear the completed run and its events")
                .clicked()
            {
                self.clear();
            }
            if ui
                .add_enabled(
                    !self.view.active && !self.evaluating && !self.discovering,
                    egui::Button::new("Reset"),
                )
                .on_hover_text("Clear the run and restore input and connection defaults")
                .clicked()
            {
                self.clear();
                self.goal = DEFAULT_GOAL.into();
                self.endpoint = "http://localhost:11434".into();
                self.model.clear();
                self.mode = Mode::Ollama;
                self.options = crate::config::RunOptions {
                    concurrency: 2,
                    peer_consultation: true,
                    ..Default::default()
                };
                self.discover(ctx);
            }
        });
        if let Err(error) = self.options.validate() {
            ui.colored_label(Color32::LIGHT_RED, error);
        }
        if self.mode == Mode::Demo && self.options.context.use_owner_data {
            ui.colored_label(
                Color32::LIGHT_RED,
                "Owner context requires Ollama Mode. Demo uses fictional scripts.",
            );
        }
        if self.goal.len() > 16000 {
            ui.colored_label(Color32::LIGHT_RED, "Task exceeds 16,000-byte limit.");
        }
        if self.mode == Mode::Demo {
            ui.label(RichText::new("Offline scripted scenario • demonstrates launch review regardless of task • real read-only fixture tools").color(TOOL));
        } else {
            ui.label(RichText::new(&self.connection).small().color(MUTED));
        }
    }
    fn graph(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Company network");
            ui.label(RichText::new("Click an agent to inspect").color(MUTED));
        });
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 270.0), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        let positions: BTreeMap<_, _> = Role::ALL
            .into_iter()
            .zip([
                (0.5, 0.12),
                (0.5, 0.43),
                (0.12, 0.85),
                (0.37, 0.85),
                (0.63, 0.85),
                (0.88, 0.85),
            ])
            .map(|(r, (x, y))| {
                (
                    r,
                    Pos2::new(
                        rect.left() + rect.width() * x,
                        rect.top() + rect.height() * y,
                    ),
                )
            })
            .collect();
        for role in [Role::Owner, Role::Cto, Role::Cfo, Role::Cmo, Role::Legal] {
            for (from, to) in [(role, Role::Ceo), (Role::Ceo, role)] {
                let (a, b) = edge_points(positions[&from], positions[&to]);
                draw_arrow(
                    &painter,
                    a,
                    b,
                    Stroke::new(1.0, Color32::from_rgb(60, 78, 99)),
                );
            }
        }
        if self.options.peer_consultation {
            for a in Role::SPECIALISTS {
                for b in Role::SPECIALISTS {
                    if a == b {
                        continue;
                    }
                    let start = positions[&a] - Vec2::new(0.0, 27.0);
                    let end = positions[&b] - Vec2::new(0.0, 27.0);
                    let height = 30.0 + ((end.x - start.x).abs() * 0.07);
                    let shape = egui::epaint::CubicBezierShape::from_points_stroke(
                        [
                            start,
                            start - Vec2::new(0.0, height),
                            end - Vec2::new(0.0, height),
                            end,
                        ],
                        false,
                        Color32::TRANSPARENT,
                        Stroke::new(0.7, TOOL.gamma_multiply(0.25)),
                    );
                    painter.add(shape);
                    draw_arrow(
                        &painter,
                        end - Vec2::new(0.0, 10.0),
                        end,
                        Stroke::new(0.7, TOOL.gamma_multiply(0.4)),
                    );
                }
            }
        }
        for pulse in &self.pulses {
            let t = (pulse.at.elapsed().as_secs_f32() / 1.5).min(1.0);
            let peer =
                Role::SPECIALISTS.contains(&pulse.from) && Role::SPECIALISTS.contains(&pulse.to);
            let (a, b) = if peer {
                (
                    positions[&pulse.from] - Vec2::new(0.0, 27.0),
                    positions[&pulse.to] - Vec2::new(0.0, 27.0),
                )
            } else {
                edge_points(positions[&pulse.from], positions[&pulse.to])
            };
            let color = if pulse.delivered {
                ACCENT
            } else {
                Color32::from_rgb(245, 191, 95)
            };
            let stroke = Stroke::new(2.0, color.gamma_multiply(1.0 - t * 0.65));
            if peer {
                let height = 30.0 + (b.x - a.x).abs() * 0.07;
                let c1 = a - Vec2::new(0.0, height);
                let c2 = b - Vec2::new(0.0, height);
                painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
                    [a, c1, c2, b],
                    false,
                    Color32::TRANSPARENT,
                    stroke,
                ));
                draw_arrow(&painter, b - Vec2::new(0.0, 10.0), b, stroke);
                let u = 1.0 - t;
                let point = a.to_vec2() * (u * u * u)
                    + c1.to_vec2() * (3.0 * u * u * t)
                    + c2.to_vec2() * (3.0 * u * t * t)
                    + b.to_vec2() * (t * t * t);
                painter.circle_filled(point.to_pos2(), 4.0, color);
            } else {
                draw_arrow(&painter, a, b, stroke);
                painter.circle_filled(a + (b - a) * t, 4.0, color);
            }
        }
        for role in Role::ALL {
            let pos = positions[&role];
            let node = egui::Rect::from_center_size(pos, Vec2::new(86.0, 51.0));
            let response = ui.interact(node, ui.id().with(role), egui::Sense::click());
            if response.clicked() {
                self.selected_role = role;
                self.inspector = true;
                self.selected_event = None;
            }
            let status = self.view.statuses[&role];
            let color = state_color(status);
            painter.rect_filled(node, 8.0, Color32::from_rgb(27, 38, 54));
            painter.rect_stroke(
                node,
                8.0,
                Stroke::new(
                    if self.selected_role == role { 2.0 } else { 1.0 },
                    if self.selected_role == role {
                        ACCENT
                    } else {
                        color.gamma_multiply(0.6)
                    },
                ),
                egui::StrokeKind::Inside,
            );
            painter.text(
                pos - Vec2::new(0.0, 8.0),
                Align2::CENTER_CENTER,
                role.to_string(),
                FontId::proportional(15.0),
                Color32::WHITE,
            );
            painter.text(
                pos + Vec2::new(0.0, 12.0),
                Align2::CENTER_CENTER,
                format!("{status:?}"),
                FontId::proportional(11.0),
                color,
            );
        }
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(ACCENT, " Delivered");
            ui.colored_label(Color32::from_rgb(245, 191, 95), " Sent");
            ui.colored_label(TOOL, "Tools: events below");
            ui.label(
                RichText::new("Arrows show permitted routes")
                    .small()
                    .color(MUTED),
            );
        });
    }
    fn timeline(&mut self, ui: &mut egui::Ui) {
        ui.heading("Execution timeline");
        ui.label(RichText::new("Sent and delivered are separate events. Click any row for its complete original content.").small().color(MUTED));
        egui::ScrollArea::vertical()
            .id_salt("timeline")
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for (i, event) in self.view.events.iter().enumerate() {
                    if matches!(event.kind, EventKind::History { .. }) {
                        continue;
                    }
                    let (title, preview, color) = event_summary(event);
                    let text = format!(
                        "{}  {}\n{}",
                        time_label(event.timestamp),
                        title,
                        preview.chars().take(115).collect::<String>()
                    );
                    if ui
                        .add(
                            egui::Button::selectable(
                                self.selected_event == Some(i),
                                RichText::new(text).color(color),
                            )
                            .wrap()
                            .min_size(Vec2::new(ui.available_width(), 40.0)),
                        )
                        .clicked()
                    {
                        self.selected_event = Some(i);
                        self.inspector = true;
                    }
                }
                if self.view.events.is_empty() {
                    ui.add_space(20.0);
                    ui.label("Submit a goal to begin. Each execution event will appear here.");
                }
            });
    }
    fn detail(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.inspector, false, "CEO final report");
            ui.selectable_value(&mut self.inspector, true, "Inspector");
        });
        ui.separator();
        egui::ScrollArea::vertical().id_salt("details").auto_shrink([false,false]).show(ui,|ui| {
            if !self.inspector {
                ui.heading("Decision desk");
                if self.view.final_report.is_empty() {
                    ui.label("The CEO's synthesis appears after every required specialist has reported or failed.");
                    if let Some(outcome) = self.view.outcome { ui.label(format!("Run outcome: {outcome:?}")); }
                } else { ui.label(RichText::new(&self.view.final_report).size(15.0)); if ui.button("Copy report").clicked() { ui.ctx().copy_text(self.view.final_report.clone()); } }
            } else if let Some(event) = self.selected_event.and_then(|i| self.view.events.get(i)) {
                ui.heading("Event details");
                ui.monospace(format!("Event {}\nRun {}\nUnix ms {}",event.id,event.run_id,event.timestamp));
                ui.separator();
                match &event.kind {
                    EventKind::Message(m) => { ui.label(format!("{} -> {} • {:?} • {:?}",m.sender,m.recipient,m.kind,m.status)); ui.monospace(format!("Message {}",m.id)); if let Some(id)=m.reply_to {ui.monospace(format!("Reply to {id}"));} ui.separator(); ui.label(&m.body); if ui.button("Copy original message").clicked() { ui.ctx().copy_text(m.body.clone()); } }
                    _ => { ui.monospace(serde_json::to_string_pretty(&event.kind).unwrap_or_default()); }
                }
            } else {
                if self.selected_role != Role::Owner {
                    ui.label(RichText::new("EDIT PROMPT FOR NEXT RUN").color(ACCENT));
                    ui.add_enabled_ui(!self.view.active && !self.evaluating,|ui| {
                        let draft=self.options.prompts.entry(self.selected_role).or_insert_with(||Agent::new(self.selected_role).prompt);
                        ui.add(egui::TextEdit::multiline(draft).desired_width(f32::INFINITY).desired_rows(8));
                        if ui.button("Restore role default").clicked() {*draft=Agent::new(self.selected_role).prompt;}
                    });
                    ui.small("Start snapshots these edits. The run history below stays unchanged.");
                    ui.separator();
                }
                let agent = &self.view.agents[&self.selected_role];
                ui.heading(format!("{} / agent context",agent.role));
                ui.label(format!("State: {:?}",self.view.statuses[&agent.role]));
                ui.label(RichText::new("SYSTEM PROMPT").color(ACCENT)); ui.label(&agent.prompt);
                ui.separator(); ui.label(RichText::new("PERMITTED TOOLS").color(TOOL));
                for tool in tools::allowed(agent.role) { ui.monospace(*tool); }
                if tools::allowed(agent.role).is_empty() { ui.label("Human input only"); }
                ui.separator(); ui.label(RichText::new("COMPLETE MESSAGE HISTORY").color(ACCENT));
                for (i,message) in agent.history.iter().enumerate() {
                    egui::CollapsingHeader::new(format!("{} · {} · {}",i+1,message.role,message.content.chars().take(32).collect::<String>())).id_salt((agent.role,i)).show(ui,|ui| { ui.monospace(serde_json::to_string_pretty(message).unwrap_or_default()); });
                }
            }
        });
    }
}
impl eframe::App for CompanyApp {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.poll();
        self.settings_windows(ctx);
        egui::TopBottomPanel::top("controls").show(ctx, |ui| self.controls(ui, ctx));
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(
                    ACCENT,
                    if self.view.active {
                        " RUNNING".into()
                    } else {
                        self.view
                            .outcome
                            .map(|s| format!(" {s:?}"))
                            .unwrap_or(" READY".into())
                    },
                );
                let elapsed = if self.view.active {
                    self.started.map(|t| t.elapsed()).unwrap_or_default()
                } else {
                    self.elapsed
                };
                ui.label(format!(
                    "{} events • {}s",
                    self.view.events.len(),
                    elapsed.as_secs()
                ));
                if let Some(id) = self.view.run_id {
                    ui.label(RichText::new(format!("Run {id}")).small().color(MUTED));
                }
            });
        });
        egui::SidePanel::right("inspector")
            .default_width(390.0)
            .min_width(280.0)
            .resizable(true)
            .show(ctx, |ui| self.detail(ui));
        egui::CentralPanel::default().show(ctx, |ui| {
            self.graph(ui);
            ui.separator();
            self.timeline(ui);
        });
        if self.view.active || self.evaluating || self.discovering || !self.pulses.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
    }
}
impl Drop for CompanyApp {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            let _ = cancel.send(true);
        }
        if let Some(cancel) = &self.eval_cancel {
            let _ = cancel.send(true);
        }
        // Do not wait for network workers while closing a native window.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}
fn state_color(status: AgentStatus) -> Color32 {
    match status {
        AgentStatus::Idle => MUTED,
        AgentStatus::Working => ACCENT,
        AgentStatus::Waiting => Color32::from_rgb(245, 191, 95),
        AgentStatus::Completed => Color32::LIGHT_GREEN,
        AgentStatus::Failed => Color32::LIGHT_RED,
    }
}
fn edge_points(a: Pos2, b: Pos2) -> (Pos2, Pos2) {
    let direction = (b - a).normalized();
    let offset = Vec2::new(-direction.y, direction.x) * 5.0;
    let clearance = (43.0 / direction.x.abs()).min(25.5 / direction.y.abs()) + 9.0;
    (
        a + direction * clearance + offset,
        b - direction * clearance + offset,
    )
}
// egui's built-in arrow scales its head with edge length. Fixed-size heads
// keep long graph edges readable as the window is resized.
fn draw_arrow(painter: &egui::Painter, a: Pos2, b: Pos2, stroke: Stroke) {
    let direction = (b - a).normalized();
    let normal = Vec2::new(-direction.y, direction.x);
    painter.line_segment([a, b], stroke);
    painter.line_segment([b, b - direction * 8.0 + normal * 4.0], stroke);
    painter.line_segment([b, b - direction * 8.0 - normal * 4.0], stroke);
}
fn time_label(ms: u64) -> String {
    let s = ms / 1000;
    format!(
        "{:02}:{:02}:{:02}.{:03} UTC",
        (s / 3600) % 24,
        (s / 60) % 60,
        s % 60,
        ms % 1000
    )
}
fn event_summary(event: &Event) -> (String, String, Color32) {
    match &event.kind {
        EventKind::Message(m) => (
            format!(
                "{} -> {}  {:?} / {:?}",
                m.sender, m.recipient, m.kind, m.status
            ),
            m.body.replace('\n', " "),
            if m.kind == MessageKind::Failure {
                Color32::LIGHT_RED
            } else {
                Color32::WHITE
            },
        ),
        EventKind::Tool {
            role,
            name,
            outcome,
            ..
        } => (
            format!("TOOL {role} / {name}"),
            match outcome {
                None => "Tool called".into(),
                Some(Ok(v)) => format!("Success: {v}"),
                Some(Err(e)) => format!("Failed: {e}"),
            },
            if matches!(outcome, Some(Err(_))) {
                Color32::LIGHT_RED
            } else {
                TOOL
            },
        ),
        EventKind::State { role, status } => (
            format!("{role} -> {status:?}"),
            String::new(),
            state_color(*status),
        ),
        EventKind::Notice(text) => ("Harness".into(), text.clone(), MUTED),
        EventKind::Finished { outcome } => (format!("Run {outcome:?}"), String::new(), ACCENT),
        EventKind::History { .. } => ("Context updated".into(), String::new(), MUTED),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_and_terminal_events_cannot_mutate_view() {
        let id = Uuid::new_v4();
        let mut view = ViewState::fresh(Some(id));
        view.active = true;
        assert!(!view.apply(Event::new(
            Uuid::new_v4(),
            EventKind::State {
                role: Role::Ceo,
                status: AgentStatus::Working
            }
        )));
        view.apply(Event::new(
            id,
            EventKind::State {
                role: Role::Ceo,
                status: AgentStatus::Working,
            },
        ));
        assert_eq!(view.statuses[&Role::Ceo], AgentStatus::Working);
        view.apply(Event::new(
            id,
            EventKind::Finished {
                outcome: RunOutcome::Cancelled,
            },
        ));
        assert!(!view.active);
        assert_eq!(view.statuses[&Role::Ceo], AgentStatus::Idle);
        assert!(!view.apply(Event::new(
            id,
            EventKind::State {
                role: Role::Ceo,
                status: AgentStatus::Completed
            }
        )));
    }
}
