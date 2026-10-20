use super::*;
impl CompanyApp {
    pub(super) fn settings_windows(&mut self, ctx: &egui::Context) {
        let mut open = self.settings_open;
        egui::Window::new("Company context & agents").open(&mut open).default_size([680.0,600.0]).resizable(true).show(ctx,|ui| {
            egui::ScrollArea::vertical().id_salt("setup_scroll").show(ui,|ui| {
                ui.label("Draft settings are captured at Start. Everything stays in memory; tools only read the sections you supply.");
                ui.add_enabled_ui(!self.view.active && !self.evaluating,|ui| {
                    if ui.checkbox(&mut self.options.context.use_owner_data,"Use my own company context (replace fictional data)").changed() && self.options.context.use_owner_data && self.goal==DEFAULT_GOAL {self.goal="Evaluate whether our company should launch next month".into();}
                    if self.options.context.use_owner_data {
                        ui.colored_label(ACCENT,"Owner-provided / unverified • Empty sections remain unknown • No sample-data fallback");
                        for (name,text) in [
                            ("Company profile / shared context",&mut self.options.context.profile),
                            ("Product roadmap / engineering evidence",&mut self.options.context.roadmap),
                            ("Finances / currency / period / assumptions",&mut self.options.context.finances),
                            ("Marketing / customers / metrics",&mut self.options.context.marketing),
                            ("Policies / legal jurisdiction / known constraints",&mut self.options.context.policies),
                        ] {
                            egui::CollapsingHeader::new(name).default_open(name.starts_with("Company")).show(ui,|ui| {
                                ui.add(egui::TextEdit::multiline(text).desired_rows(6).desired_width(f32::INFINITY).hint_text("Paste or type context here. Include dates and sources where available."));
                                ui.small(format!("{} / 12000 bytes",text.len()));
                            });
                        }
                    }
                    ui.separator(); ui.heading("Async orchestration");
                    ui.add(egui::Slider::new(&mut self.options.concurrency,1..=4).text("Concurrent specialists"));
                    ui.checkbox(&mut self.options.peer_consultation,"Let each specialist evaluate whether it needs peer consultation");
                    ui.small("One consultation round, at most two questions per specialist. Each role retains one independent history. Ollama may queue requests depending on its server settings and available memory.");
                    ui.separator(); ui.heading("Per-role models and prompts");
                    ui.small("Blank model uses the main model selector. Start with one shared model on a 16 GB Mac to avoid repeated model swaps.");
                    for role in [Role::Ceo,Role::Cto,Role::Cfo,Role::Cmo,Role::Legal] {
                        egui::CollapsingHeader::new(role.to_string()).id_salt(("role_setup",role)).show(ui,|ui| {
                            let model=self.options.role_models.entry(role).or_default();
                            ui.horizontal(|ui| {
                                ui.add(egui::TextEdit::singleline(model).hint_text("Use shared model"));
                                egui::ComboBox::from_id_salt(("role_model",role)).selected_text("Installed").show_ui(ui,|ui| { ui.selectable_value(model,String::new(),"Shared default"); for installed in &self.models {ui.selectable_value(model,installed.clone(),installed);} });
                            });
                            let prompt=self.options.prompts.entry(role).or_insert_with(||Agent::new(role).prompt);
                            ui.label("System prompt (editable inline)");
                            ui.add(egui::TextEdit::multiline(prompt).desired_rows(6).desired_width(f32::INFINITY));
                            ui.horizontal(|ui| {ui.small(format!("{} / 8000 bytes",prompt.len()));if ui.button("Restore default prompt").clicked(){*prompt=Agent::new(role).prompt;}});
                        });
                    }
                    if let Err(error)=self.options.validate(){ui.colored_label(Color32::LIGHT_RED,error);}
                });
            });
        });
        self.settings_open = open;
        let mut open = self.model_lab_open;
        egui::Window::new("Model lab & recommendations").open(&mut open).default_size([740.0,620.0]).resizable(true).show(ctx,|ui| {
            egui::ScrollArea::vertical().id_salt("model_lab_scroll").show(ui,|ui| {
                ui.heading("Candidates to try — not measured winners");
                ui.label("For this 16 GB Mac: start with one shared 4B model; compare a 9B model at concurrency 1. Separate role contexts do not require separate models. Download size is not total runtime memory.");
                for (model,url,note) in [
                    ("qwen3.5:4b","https://ollama.com/library/qwen3.5:4b","First candidate for all roles; smaller footprint for local iteration."),
                    ("qwen3.5:9b","https://ollama.com/library/qwen3.5:9b","Compare for CEO synthesis, CTO analysis, CFO arithmetic and Legal caution; more memory and latency."),
                    ("qwen3:8b","https://ollama.com/library/qwen3:8b","Alternative tool-capable baseline. Compare CMO clarity and evidence handling as well as other roles."),
                ] {
                    ui.horizontal(|ui| {ui.hyperlink_to(model,url);if ui.button("Copy pull command").clicked(){ui.ctx().copy_text(format!("ollama pull {model}"));}});
                    ui.small(note);
                }
                ui.label("Pull your chosen model in a terminal, then Refresh models. No downloads run automatically. These role mappings are hypotheses, not evidence that a model is best at finance or law.");
                ui.separator();ui.heading("Evaluate installed models");
                ui.label("Five synthetic role probes per model, five transparent checks per probe. Uses default role prompts and public synthetic evidence only, never your company context. Does not measure production/legal correctness or native tool reliability. Inspect answers and assess quality yourself.");
                ui.add_enabled_ui(!self.evaluating && !self.view.active,|ui| {
                    for model in &self.models {let mut selected=self.eval_models.contains(model);if ui.checkbox(&mut selected,model).changed(){if selected {self.eval_models.insert(model.clone());}else{self.eval_models.remove(model);}}}
                    if ui.add_enabled(!self.eval_models.is_empty(),egui::Button::new("Run role evaluation")).clicked() {
                        self.evaluations.clear();self.evaluating=true;self.eval_status="Evaluating sequentially to keep model comparisons predictable…".into();
                        let (tx,rx)=watch::channel(false);self.eval_cancel=Some(tx);
                        self.runtime.as_ref().unwrap().spawn(crate::evaluation::run(self.endpoint.clone(),self.eval_models.iter().cloned().collect(),self.eval_tx.clone(),rx,Some(ctx.clone())));
                    }
                });
                if ui.add_enabled(self.evaluating,egui::Button::new("Cancel evaluation")).clicked() && let Some(cancel)=&self.eval_cancel {let _=cancel.send(true);}
                ui.label(&self.eval_status);
                for (index,result) in self.evaluations.iter().enumerate() {
                    egui::CollapsingHeader::new(format!("{} / {} — {}/5 checks — {:.1}s{}",result.role,result.model,result.score(),result.elapsed.as_secs_f32(),if result.error.is_some(){" — FAILED"}else{""})).id_salt(("eval",index)).show(ui,|ui| {
                        for (name,pass) in &result.checks {ui.colored_label(if *pass {ACCENT}else{Color32::LIGHT_RED},format!("{} {name}",if *pass {"PASS"}else{"FAIL"}));}
                        if let Some(error)=&result.error{ui.colored_label(Color32::LIGHT_RED,error);}
                        ui.label(&result.response);
                        if ui.add_enabled(!self.view.active && !self.evaluating && result.error.is_none(),egui::Button::new("Use this model for this role")).clicked(){self.options.role_models.insert(result.role,result.model.clone());}
                    });
                }
            });
        });
        self.model_lab_open = open;
    }
}
