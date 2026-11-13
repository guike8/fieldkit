//! Nightjar: an optional native interface over shared cases, with explicit sidecar execution.
mod agent;
mod graph;
mod worker;

use eframe::egui::{self, RichText};
use graph::{BLUE, GraphAction, GraphView, INK, SECONDARY, SURFACE};
use serde_json::Value;
use std::{path::PathBuf, sync::Arc};
use worker::{Reply, Request, Worker};

pub(crate) fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
pub(crate) fn rows<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value[key].as_array().map(Vec::as_slice).unwrap_or(&[])
}
/// Source text is never interpreted. Invisible controls remain inspectable.
pub(crate) fn safe_text(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if (c.is_control() && c != '\n' && c != '\t')
                || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            {
                format!("\\u{{{:04x}}}", c as u32)
            } else {
                c.to_string()
            }
        })
        .collect()
}
pub(crate) fn clip(value: &str, maximum: usize) -> String {
    let value = safe_text(value);
    let mut chars = value.chars();
    let mut result: String = chars.by_ref().take(maximum).collect();
    if chars.next().is_some() {
        result.push('…');
    }
    result
}
fn has_recorded_error(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
        Value::String(v) => !v.is_empty(),
        _ => true,
    }
}
fn case_name(state: &Value) -> String {
    let title = text(&state["metadata"], "title");
    if !title.is_empty() && title != "Untitled investigation" {
        return safe_text(title);
    }
    std::path::Path::new(text(state, "case_path"))
        .file_name()
        .map(|s| safe_text(&s.to_string_lossy()))
        .unwrap_or_else(|| "Untitled case".into())
}
fn chip(ui: &mut egui::Ui, label: impl Into<String>) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(label.into()).small().color(SECONDARY));
        });
}
fn primary(label: &str) -> egui::Button<'_> {
    egui::Button::new(RichText::new(label).strong()).stroke(egui::Stroke::new(1.0_f32, BLUE))
}
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[derive(Default)]
struct PlanForm {
    question: String,
    mode: String,
    seeds: String,
}
impl PlanForm {
    fn validate(&self) -> Result<Vec<String>, String> {
        if self.question.trim().is_empty()
            || self.question.len() > 4000
            || self.question.contains('\0')
        {
            return Err("Add a question (up to 4,000 bytes).".into());
        }
        if !["person", "organization", "infrastructure", "crypto"].contains(&self.mode.as_str()) {
            return Err("Choose a scope.".into());
        }
        let seeds: Vec<String> = self
            .seeds
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        if !(1..=5).contains(&seeds.len())
            || seeds
                .iter()
                .any(|s| s.len() > 2048 || s.chars().any(char::is_control))
        {
            return Err(
                "Add 1–5 starting points, one per line (up to 2,048 bytes each; no controls)."
                    .into(),
            );
        }
        if self.mode == "crypto"
            && seeds.iter().any(|seed| {
                !seed.strip_prefix("ethereum:0x").is_some_and(|hex| {
                    matches!(hex.len(), 40 | 64) && hex.bytes().all(|b| b.is_ascii_hexdigit())
                })
            })
        {
            return Err(
                "Use ethereum:0x followed by a 40-digit address or 64-digit transaction hash."
                    .into(),
            );
        }
        Ok(seeds)
    }
}
struct Document {
    title: String,
    record: Arc<Value>,
    raw: String,
    page: usize,
    page_starts: Vec<usize>,
}
impl Document {
    fn new(title: impl Into<String>, record: Value) -> Self {
        let raw = serde_json::to_string_pretty(&record).unwrap_or_default();
        let mut page_starts: Vec<usize> =
            raw.char_indices().step_by(8000).map(|(i, _)| i).collect();
        if page_starts.is_empty() {
            page_starts.push(0);
        }
        page_starts.push(raw.len());
        Self {
            title: title.into(),
            record: Arc::new(record),
            raw,
            page: 0,
            page_starts,
        }
    }
    fn pages(&self) -> usize {
        self.page_starts.len() - 1
    }
    fn page_text(&self) -> String {
        safe_text(&self.raw[self.page_starts[self.page]..self.page_starts[self.page + 1]])
    }
}

pub struct Nightjar {
    worker: Worker,
    snapshot: Option<Arc<Value>>,
    path: String,
    form: PlanForm,
    graph: GraphView,
    selected_investigation: String,
    document: Option<Document>,
    busy: bool,
    notice: String,
    error: Option<String>,
    show_create: bool,
    show_open: bool,
    show_case_details: bool,
    show_settings: bool,
    show_inspector: bool,
    workspace_tab: usize,
    drafting: bool,
    review_findings: bool,
    evidence_search: String,
    agent_config: agent::AgentConfig,
    agent_available: Option<Result<(), String>>,
    show_run: bool,
    allow_collection: bool,
    confirm_provider: bool,
    agent_running: bool,
    agent_result: Option<Arc<Value>>,
}
impl Nightjar {
    pub fn new(context: &eframe::CreationContext<'_>, initial_case: Option<PathBuf>) -> Self {
        // Setup only: shared by the app and native smoke driver, never a render frame.
        #[cfg(target_os = "macos")]
        if let Ok(bytes) = std::fs::read("/System/Library/Fonts/SFNS.ttf") {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert(
                "system-sans".into(),
                egui::FontData::from_owned(bytes).into(),
            );
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .insert(0, "system-sans".into());
            context.egui_ctx.set_fonts(fonts);
        }
        configure(&context.egui_ctx);
        Self::with_context(context.egui_ctx.clone(), initial_case)
    }
    fn with_context(context: egui::Context, initial_case: Option<PathBuf>) -> Self {
        let mut app = Self {
            worker: Worker::spawn(context),
            snapshot: None,
            path: initial_case
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            form: PlanForm::default(),
            graph: GraphView::default(),
            selected_investigation: String::new(),
            document: None,
            busy: false,
            notice: "Local workspace".into(),
            error: None,
            show_create: false,
            show_open: false,
            show_case_details: false,
            show_settings: false,
            show_inspector: false,
            workspace_tab: 0,
            drafting: true,
            review_findings: false,
            evidence_search: String::new(),
            agent_config: agent::AgentConfig::default(),
            agent_available: None,
            show_run: false,
            allow_collection: false,
            confirm_provider: false,
            agent_running: false,
            agent_result: None,
        };
        if let Some(path) = initial_case {
            app.send(Request::Open {
                path,
                create: false,
            });
        }
        app
    }
    fn send(&mut self, request: Request) {
        if self.busy {
            return;
        }
        let running = matches!(request, Request::RunAgent { .. });
        self.worker
            .cancel
            .store(false, std::sync::atomic::Ordering::Relaxed);
        match self.worker.requests.try_send(request) {
            Ok(()) => {
                self.agent_running = running;
                self.busy = true;
                self.error = None;
                self.notice = if running {
                    "Agent running"
                } else {
                    "Working…"
                }
                .into();
            }
            Err(error) => self.error = Some(format!("Case worker unavailable: {error}")),
        }
    }
    fn receive(&mut self) {
        while let Ok(reply) = self.worker.replies.try_recv() {
            self.busy = false;
            self.agent_running = false;
            match reply {
                Reply::Snapshot {
                    state,
                    notice,
                    investigation,
                } => {
                    let switched = self
                        .snapshot
                        .as_ref()
                        .is_none_or(|old| old["case_path"] != state["case_path"]);
                    if switched {
                        self.document = None;
                        self.show_inspector = false;
                        self.agent_result = None;
                        self.graph.search.clear();
                        self.selected_investigation.clear();
                        self.workspace_tab = 0;
                    }
                    if let Some(id) = investigation {
                        self.selected_investigation = id;
                        self.drafting = false;
                        self.workspace_tab = 0;
                    }
                    if !rows(&state, "investigations")
                        .iter()
                        .any(|i| text(i, "id") == self.selected_investigation)
                    {
                        self.selected_investigation = rows(&state, "investigations")
                            .first()
                            .map(|i| text(i, "id").into())
                            .unwrap_or_default();
                    }
                    if switched {
                        self.drafting = self.selected_investigation.is_empty();
                    }
                    self.path = text(&state, "case_path").into();
                    self.snapshot = Some(Arc::new(state));
                    self.notice = notice;
                }
                Reply::Document { title, value } => {
                    self.inspect(title, value);
                    self.notice = "Record loaded".into();
                }
                Reply::Availability(result) => {
                    self.notice = if result.is_ok() {
                        "Connection configured · not network-tested"
                    } else {
                        "Agent configuration needs attention"
                    }
                    .into();
                    self.agent_available = Some(result);
                }
                Reply::AgentFinished { state, output } => {
                    if let Some(state) = state {
                        self.snapshot = Some(Arc::new(state));
                    } else {
                        self.error = Some(
                            "Agent exited; case refresh failed. Refresh before continuing.".into(),
                        );
                    }
                    self.selected_investigation =
                        text(&output["report"], "investigation_id").into();
                    self.drafting = false;
                    self.notice =
                        format!("Agent stopped · {}", text(&output["report"], "stop_reason"));
                    if output["process_success"] != true {
                        self.error = Some(
                            "Agent did not complete. Inspect its stop reason and partial results."
                                .into(),
                        );
                    }
                    self.agent_result = Some(Arc::new(output));
                    self.workspace_tab = 0;
                }
                Reply::Error(error) => {
                    self.error = Some(error);
                    self.notice = "Operation failed · previous snapshot retained".into();
                }
            }
        }
    }
    fn inspect(&mut self, title: impl Into<String>, record: Value) {
        self.document = Some(Document::new(title, record));
        self.show_inspector = true;
    }
    fn start_question(&mut self) {
        self.drafting = true;
        self.workspace_tab = 0;
        self.form = PlanForm::default();
    }

    fn sidebar(&mut self, ui: &mut egui::Ui, snapshot: Option<&Value>) {
        ui.add_space(12.0);
        ui.label(RichText::new("Nightjar").size(24.0).strong());
        ui.add_space(24.0);
        if let Some(state) = snapshot {
            ui.label(RichText::new(clip(&case_name(state), 32)).strong())
                .on_hover_text(case_name(state));
            if state["metadata"]["is_demo"] == true {
                chip(ui, "SYNTHETIC");
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!self.busy, egui::Button::new("Refresh"))
                    .on_hover_text("Read case changes; no collection or model call")
                    .clicked()
                {
                    self.send(Request::Refresh);
                }
                if ui.button("Case details").clicked() {
                    self.show_case_details = true;
                }
            });
        } else {
            ui.label(RichText::new("No case open").color(SECONDARY));
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.busy, egui::Button::new("Open…"))
                .clicked()
            {
                self.show_open = true;
            }
            if ui
                .add_enabled(!self.busy, egui::Button::new("New case…"))
                .clicked()
            {
                self.path.clear();
                self.show_create = true;
            }
        });
        ui.add_space(28.0);
        for (index, label, hint) in [
            (0, "1   Ask", "Question and scoped starting points"),
            (1, "2   Explore", "Navigate source relationships"),
            (2, "3   Review", "Inspect sources and analytical findings"),
        ] {
            if ui
                .add_sized(
                    [ui.available_width(), 38.0],
                    egui::Button::new(label).selected(self.workspace_tab == index),
                )
                .on_hover_text(hint)
                .clicked()
            {
                self.workspace_tab = index;
            }
        }
        ui.add_space(28.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("QUESTIONS").small().color(SECONDARY));
            if ui.small_button("+ New").clicked() {
                self.start_question();
            }
        });
        if let Some(state) = snapshot {
            if rows(state, "investigations").is_empty() {
                ui.small("Your saved questions appear here.");
            }
            for investigation in rows(state, "investigations") {
                let id = text(investigation, "id");
                if ui
                    .add(
                        egui::Button::new(clip(text(investigation, "question"), 58))
                            .wrap()
                            .selected(!self.drafting && self.selected_investigation == id),
                    )
                    .on_hover_text(safe_text(text(investigation, "question")))
                    .clicked()
                {
                    self.selected_investigation = id.into();
                    self.drafting = false;
                    self.workspace_tab = 0;
                }
            }
        }
        ui.add_space(28.0);
        ui.separator();
        if ui.button("Agent settings…").clicked() {
            self.show_settings = true;
        }
    }

    fn plan(&mut self, ui: &mut egui::Ui, loaded: bool) {
        ui.label(
            RichText::new("What would you like to find out?")
                .size(23.0)
                .strong(),
        );
        ui.add_space(12.0);
        ui.add(
            egui::TextEdit::multiline(&mut self.form.question)
                .font(egui::TextStyle::Heading)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text("Ask a question in your own words…"),
        );
        ui.add_space(20.0);
        ui.label(RichText::new("Scope").strong());
        ui.horizontal_wrapped(|ui| {
            for (mode, label) in [
                ("person", "Person"),
                ("organization", "Organization"),
                ("infrastructure", "Infrastructure"),
                ("crypto", "Crypto"),
            ] {
                ui.selectable_value(&mut self.form.mode, mode.into(), label);
            }
        });
        ui.add_space(12.0);
        ui.label("Starting points");
        if self.form.mode == "crypto" {
            ui.horizontal_wrapped(|ui| {
                if ui.button("Trace an address").on_hover_text("Fill a local question template; nothing runs").clicked() { self.form.question = "What public Ethereum activity and source-attributed relationships are visible for this address?".into(); }
                if ui.button("Inspect a transaction").on_hover_text("Fill a local question template; nothing runs").clicked() { self.form.question = "What does this Ethereum transaction show, and which observed addresses does it connect?".into(); }
            });
        }
        ui.add(
            egui::TextEdit::multiline(&mut self.form.seeds)
                .desired_rows(2)
                .desired_width(f32::INFINITY)
                .hint_text(if self.form.mode == "crypto" {
                    "ethereum:0x… — address or transaction hash"
                } else {
                    "A domain, public profile or scoped handle — one per line"
                }),
        );
        ui.small("1–5 starting points").on_hover_text("Only the explicit seeds you provide define the starting scope. Each accepts up to 2,048 bytes.");
        ui.add_space(20.0);
        let validation = self.form.validate();
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    loaded && !self.busy && validation.is_ok(),
                    primary("Save question locally"),
                )
                .on_hover_text(
                    "Save the original question, mode and seeds. No model call or collection.",
                )
                .clicked()
            {
                self.send(Request::Plan {
                    question: self.form.question.clone(),
                    mode: self.form.mode.clone(),
                    seeds: validation.clone().unwrap(),
                });
            }
            chip(ui, "Local only · nothing runs");
        });
        if !loaded {
            ui.small("Open or create a case to save this question.");
        } else if !self.form.question.is_empty()
            && let Err(error) = validation
        {
            ui.small(error);
        }
    }

    fn ask(&mut self, ui: &mut egui::Ui, state: Option<&Value>) {
        let selected = state.and_then(|s| {
            rows(s, "investigations")
                .iter()
                .find(|i| text(i, "id") == self.selected_investigation)
        });
        if self.drafting || selected.is_none() {
            self.plan(ui, state.is_some());
            return;
        }
        let plan = selected.unwrap();
        ui.add(
            egui::Label::new(
                RichText::new(safe_text(text(plan, "question")))
                    .size(23.0)
                    .strong(),
            )
            .wrap()
            .selectable(true),
        );
        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            chip(
                ui,
                if text(plan, "mode").is_empty() {
                    "Archive"
                } else {
                    text(plan, "mode")
                },
            );
            chip(ui, format!("{} starting points", rows(plan, "seeds").len()));
            chip(
                ui,
                if self.agent_running {
                    "Agent running"
                } else {
                    "Saved locally"
                },
            );
        });
        ui.add_space(20.0);
        self.agent_action(ui, plan);
        ui.add_space(24.0);
        if let Some(result) = self
            .agent_result
            .clone()
            .filter(|r| text(&r["report"], "investigation_id") == self.selected_investigation)
        {
            egui::Frame::group(ui.style())
                .inner_margin(16)
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Agent response").strong());
                        chip(ui, text(&result["report"], "stop_reason"));
                        chip(ui, "Analysis · not source evidence");
                    });
                    ui.add_space(8.0);
                    ui.add(
                        egui::Label::new(clip(text(&result["report"], "final"), 8000))
                            .wrap()
                            .selectable(true),
                    );
                    if ui.button("Response details & tool trace").clicked() {
                        self.inspect("Agent response", (*result).clone());
                    }
                });
        }
        let steps = rows(plan, "steps");
        if !steps.is_empty() {
            ui.collapsing(format!("{} recorded steps", steps.len()), |ui| {
                for step in steps {
                    ui.horizontal_wrapped(|ui| {
                        chip(ui, text(step, "status"));
                        if ui
                            .button(clip(
                                &format!("{} · {}", text(step, "command"), text(step, "target")),
                                72,
                            ))
                            .clicked()
                        {
                            self.inspect("Investigation step", step.clone());
                        }
                        if !text(step, "run_id").is_empty()
                            && ui
                                .add_enabled(!self.busy, egui::Button::new("Report"))
                                .clicked()
                        {
                            self.send(Request::Report(text(step, "run_id").into()));
                        }
                    });
                    if has_recorded_error(&step["error"]) {
                        ui.label(format!("Error: {}", clip(&step["error"].to_string(), 300)));
                    }
                }
            });
        }
        ui.collapsing("Question details", |ui| {
            for seed in rows(plan, "seeds") {
                ui.add(
                    egui::Label::new(safe_text(seed.as_str().unwrap_or_default())).selectable(true),
                );
            }
            ui.small(safe_text(text(plan, "coverage")));
            ui.horizontal(|ui| {
                if ui.button("Exact plan").clicked() {
                    self.inspect("Investigation plan", plan.clone());
                }
                if ui.button("Use as a new question").clicked() {
                    self.form = PlanForm {
                        question: text(plan, "question").into(),
                        mode: text(plan, "mode").into(),
                        seeds: rows(plan, "seeds")
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("\n"),
                    };
                    self.drafting = true;
                }
            });
        });
    }
    fn agent_action(&mut self, ui: &mut egui::Ui, plan: &Value) {
        let scoped = ["person", "organization", "infrastructure", "crypto"]
            .contains(&text(plan, "mode"))
            && !rows(plan, "seeds").is_empty();
        if self.agent_running {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Working on your question…");
            });
            return;
        }
        ui.horizontal_wrapped(|ui| {
            if self.agent_available.as_ref().is_some_and(Result::is_ok) && scoped {
                if ui.add_enabled(!self.busy, primary("Run agent…")).on_hover_text("Review the provider and collection consent before anything runs. Starts a new scoped sidecar investigation.").clicked() { self.show_run = true; self.allow_collection = false; self.confirm_provider = false; }
            } else if ui.add_enabled(!self.busy, primary("Set up agent…")).on_hover_text("Configure the sidecar and check key presence. No provider call during setup.").clicked() { self.show_settings = true; }
            if ui.button("Explore sources →").clicked() { self.workspace_tab = 1; }
        });
        if !scoped {
            ui.small("Agent execution needs an explicitly scoped plan.");
        }
    }

    fn review(&mut self, ui: &mut egui::Ui, state: &Value) {
        let findings: Vec<_> = rows(state, "investigations")
            .iter()
            .flat_map(|i| rows(i, "findings"))
            .collect();
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(
                &mut self.review_findings,
                false,
                format!("Sources ({})", rows(state, "evidence").len()),
            );
            ui.selectable_value(
                &mut self.review_findings,
                true,
                format!("Findings ({})", findings.len()),
            );
        });
        ui.add_space(16.0);
        if self.review_findings {
            ui.horizontal_wrapped(|ui| {
                chip(ui, "Analysis · not source evidence");
                chip(ui, "Read-only here");
            });
            ui.collapsing("Review decisions in the terminal", |ui| {
                ui.label("Finding decisions require deliberate local human review in the TUI.");
                let command = format!(
                    "fieldkit --case {} tui",
                    shell_quote(text(state, "case_path"))
                );
                if ui.button("Copy TUI command").clicked() {
                    ui.ctx().copy_text(command.clone());
                }
                ui.add(
                    egui::Label::new(RichText::new(command).monospace())
                        .wrap()
                        .selectable(true),
                );
            });
            if findings.is_empty() {
                ui.add_space(24.0);
                ui.label("No findings proposed yet.");
            }
            for finding in findings {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        chip(ui, text(finding, "status"));
                        chip(ui, text(finding, "kind"));
                    });
                    if ui
                        .add(egui::Button::new(clip(text(finding, "content"), 220)).wrap())
                        .clicked()
                    {
                        self.inspect("Analytical finding", finding.clone());
                    }
                });
            }
        } else {
            ui.add(
                egui::TextEdit::singleline(&mut self.evidence_search)
                    .hint_text("Find a source…")
                    .desired_width(f32::INFINITY),
            );
            let query = self.evidence_search.to_lowercase();
            let records: Vec<_> = rows(state, "evidence")
                .iter()
                .filter(|e| {
                    text(e, "title").to_lowercase().contains(&query)
                        || text(e, "id").to_lowercase().contains(&query)
                })
                .collect();
            if records.is_empty() {
                ui.add_space(24.0);
                ui.label("No sources to review yet.");
            }
            egui::ScrollArea::vertical()
                .id_salt("source-review")
                .max_height(600.0)
                .show_rows(ui, 64.0, records.len(), |ui, range| {
                    for index in range {
                        let record = records[index];
                        ui.horizontal_wrapped(|ui| {
                            if ui
                                .add(egui::Button::new(clip(text(record, "title"), 90)).wrap())
                                .clicked()
                            {
                                self.inspect("Source record", record.clone());
                            }
                            chip(ui, text(record, "status"));
                            if !text(record, "collector_status").is_empty() {
                                chip(ui, text(record, "collector_status"));
                            }
                        });
                    }
                });
        }
    }

    fn inspector(&mut self, ui: &mut egui::Ui, state: Option<&Value>) {
        ui.horizontal(|ui| {
            ui.heading("Details");
            if ui.button("Close").clicked() {
                self.show_inspector = false;
            }
        });
        ui.separator();
        let Some(document) = self.document.as_mut() else {
            return;
        };
        ui.label(RichText::new(safe_text(&document.title)).strong());
        let record = document.record.clone();
        for key in ["title", "value", "relation", "content"] {
            if let Some(value) = record.get(key).filter(|v| !v.is_null()) {
                ui.add(
                    egui::Label::new(clip(value.as_str().unwrap_or(&value.to_string()), 3000))
                        .wrap()
                        .selectable(true),
                );
            }
        }
        ui.horizontal_wrapped(|ui| {
            for key in ["type", "status", "collector_status"] {
                if !text(&record, key).is_empty() {
                    chip(ui, safe_text(text(&record, key)));
                }
            }
        });
        ui.collapsing("Provenance & hashes", |ui| {
            for key in ["id", "source_url", "source_ids", "evidence_ids", "sources", "hash_scope", "sha256", "artifact_sha256", "report_sha256", "proposal_sha256", "run_id"] {
                if let Some(value) = record.get(key).filter(|v| !v.is_null()) {
                    ui.label(RichText::new(key).small().color(SECONDARY));
                    ui.add(egui::Label::new(clip(value.as_str().unwrap_or(&value.to_string()), 6000)).wrap().selectable(true));
                }
            }
            ui.small("Loading is not verification. Source text is untrusted; links never open automatically.");
        });
        ui.collapsing("Full record JSON", |ui| {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(document.page > 0, egui::Button::new("Previous"))
                    .clicked()
                {
                    document.page -= 1;
                }
                ui.label(format!("{} / {}", document.page + 1, document.pages()));
                if ui
                    .add_enabled(
                        document.page + 1 < document.pages(),
                        egui::Button::new("Next"),
                    )
                    .clicked()
                {
                    document.page += 1;
                }
            });
            ui.add(
                egui::Label::new(RichText::new(document.page_text()).monospace())
                    .wrap()
                    .selectable(true),
            );
        });
        if !text(&record, "run_id").is_empty()
            && ui
                .add_enabled(!self.busy, egui::Button::new("Open stored report"))
                .clicked()
        {
            self.send(Request::Report(text(&record, "run_id").into()));
        }
        if record.get("value").is_some()
            && !text(&record, "id").is_empty()
            && ui
                .add_enabled(!self.busy, egui::Button::new("Focus in Explore"))
                .clicked()
        {
            self.workspace_tab = 1;
            self.send(Request::Focus(text(&record, "id").into()));
        }
        if let Some(state) = state {
            for id in rows(&record, "evidence_ids") {
                if let Some(evidence) = rows(state, "evidence").iter().find(|e| e["id"] == *id)
                    && ui
                        .button(clip(text(evidence, "title"), 55))
                        .on_hover_text("Supporting source record")
                        .clicked()
                {
                    self.inspect("Supporting source", evidence.clone());
                }
            }
        }
    }
    fn source_graph(&mut self, ui: &mut egui::Ui, state: &Value) {
        if let Some(action) = self.graph.show(ui, &state["graph"]) {
            match action {
                GraphAction::Focus(id) => {
                    if let Some(node) = rows(&state["graph"], "nodes")
                        .iter()
                        .find(|n| text(n, "id") == id)
                    {
                        self.inspect("Source node", node.clone());
                    }
                    self.send(Request::Focus(id));
                }
                GraphAction::Inspect(record) => self.inspect("Source observation", record),
            }
        }
    }

    pub fn frame(&mut self, context: &egui::Context) {
        self.receive();
        let snapshot = self.snapshot.clone();
        egui::TopBottomPanel::bottom("status").show(context, |ui| {
            ui.horizontal_wrapped(|ui| {
                if self.busy {
                    ui.spinner();
                }
                ui.label(if self.busy {
                    if self.agent_running {
                        "Agent running"
                    } else {
                        "Working…"
                    }
                } else if self.error.is_some() {
                    "Needs attention"
                } else {
                    "Local case"
                })
                .on_hover_text(safe_text(&self.notice));
                if self.agent_running && ui.button("Cancel agent").clicked() {
                    self.worker
                        .cancel
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    self.notice = "Cancelling; persisted observations remain.".into();
                }
                if let Some(error) = &self.error {
                    ui.label(RichText::new(clip(error, 160)).strong())
                        .on_hover_text(safe_text(error));
                }
            });
        });
        egui::SidePanel::left("case-sidebar")
            .default_width(224.0)
            .min_width(200.0)
            .max_width(320.0)
            .show(context, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.sidebar(ui, snapshot.as_deref()));
            });
        if self.show_inspector && self.document.is_some() {
            egui::SidePanel::right("record-inspector")
                .default_width(330.0)
                .min_width(260.0)
                .max_width(500.0)
                .show(context, |ui| {
                    egui::ScrollArea::vertical()
                        .show(ui, |ui| self.inspector(ui, snapshot.as_deref()));
                });
        }
        egui::CentralPanel::default().show(context, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("workspace")
                .show(ui, |ui| {
                    ui.add_space(14.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.heading(match self.workspace_tab {
                            1 => "Explore",
                            2 => "Review",
                            3 => "Case activity",
                            _ => "Ask",
                        });
                        if snapshot.as_ref().is_some_and(|s| s["truncated"] == true) {
                            chip(ui, "Latest 500 reports");
                        }
                    });
                    ui.add_space(24.0);
                    if let Some(state) = snapshot.as_deref() {
                        match self.workspace_tab {
                            1 => self.source_graph(ui, state),
                            2 => self.review(ui, state),
                            3 => {
                                for event in rows(state, "activity").iter().take(100) {
                                    if ui
                                        .button(clip(
                                            &format!(
                                                "{} · {}",
                                                text(event, "created_at"),
                                                text(event, "title")
                                            ),
                                            100,
                                        ))
                                        .clicked()
                                    {
                                        self.inspect("Case activity", event.clone());
                                    }
                                }
                                if rows(state, "activity").is_empty() {
                                    ui.label("No case activity yet.");
                                }
                            }
                            _ => self.ask(ui, Some(state)),
                        }
                    } else {
                        if self.workspace_tab == 0 {
                            self.ask(ui, None);
                        } else {
                            ui.label("Open a case to explore its sources.");
                        }
                    }
                });
        });
        self.dialogs(context, snapshot.as_deref());
    }
    fn dialogs(&mut self, context: &egui::Context, state: Option<&Value>) {
        if self.show_open || self.show_create {
            let create = self.show_create;
            egui::Modal::new(egui::Id::new("case-dialog")).show(context, |ui| {
                ui.set_max_width(480.0);
                ui.heading(if create { "A new case" } else { "Open a case" });
                ui.label(if create {
                    "Choose a new folder for your investigation."
                } else {
                    "Choose the folder containing your case.sqlite3 file."
                });
                ui.add(
                    egui::TextEdit::singleline(&mut self.path)
                        .desired_width(440.0)
                        .hint_text("/path/to/case-folder"),
                );
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        self.show_create = false;
                        self.show_open = false;
                    }
                    if ui
                        .add_enabled(
                            !self.busy && !self.path.trim().is_empty(),
                            primary(if create {
                                "Create empty case"
                            } else {
                                "Open case"
                            }),
                        )
                        .clicked()
                    {
                        self.show_create = false;
                        self.show_open = false;
                        self.send(Request::Open {
                            path: PathBuf::from(self.path.trim()),
                            create,
                        });
                    }
                });
                chip(ui, "Local only · no collection");
            });
        }
        if self.show_case_details {
            egui::Modal::new(egui::Id::new("case-details")).show(context, |ui| {
                ui.set_max_width(520.0); ui.heading("Case details");
                if let Some(state) = state {
                    ui.label(RichText::new(case_name(state)).strong());
                    ui.add(egui::Label::new(safe_text(text(state, "case_path"))).wrap().selectable(true));
                    ui.horizontal(|ui| {
                        if ui.add_enabled(!self.busy, egui::Button::new("Verify integrity")).clicked() { self.show_case_details = false; self.send(Request::Verify); }
                        if ui.button("Activity").clicked() { self.workspace_tab = 3; self.show_case_details = false; }
                    });
                    ui.collapsing("Coverage", |ui| { ui.label("The source projection covers the latest 500 reports, not the complete case. Missing records are not evidence of absence."); });
                }
                if ui.button("Done").clicked() { self.show_case_details = false; }
            });
        }
        if self.show_settings {
            egui::Modal::new(egui::Id::new("agent-settings")).show(context, |ui| {
                ui.set_max_width(560.0); ui.heading("Agent settings");
                let mut changed = false;
                for (label, value) in [("Agent executable", &mut self.agent_config.sidecar), ("Fieldkit executable", &mut self.agent_config.binary), ("HTTPS provider endpoint", &mut self.agent_config.base_url), ("Model", &mut self.agent_config.model), ("Key environment variable name — not the key", &mut self.agent_config.key_env)] {
                    ui.label(label); changed |= ui.add(egui::TextEdit::singleline(value).desired_width(520.0)).changed();
                }
                if changed { self.agent_available = None; }
                ui.collapsing("Credentials & privacy", |ui| { ui.label("The key stays in the app process environment. Never paste it here. Finder may not inherit shell variables; launch from your configured terminal environment. Settings checks make no network call."); });
                match &self.agent_available { Some(Ok(())) => { ui.label("Ready to request a run · connection not network-tested"); }, Some(Err(error)) => { ui.label(safe_text(error)); }, None => {} }
                ui.horizontal(|ui| {
                    if ui.add_enabled(!self.busy, primary("Check setup")).clicked() { self.send(Request::Probe(self.agent_config.clone())); }
                    if ui.button("Done").clicked() { self.show_settings = false; }
                });
            });
        }
        if self.show_run {
            egui::Modal::new(egui::Id::new("run-agent-consent")).show(context, |ui| {
                ui.set_max_width(560.0); ui.heading("Run this investigation?");
                ui.label(format!("Provider: {}\nModel: {}", safe_text(&self.agent_config.base_url), safe_text(&self.agent_config.model)));
                let selected = state.and_then(|s| rows(s, "investigations").iter().find(|i| text(i, "id") == self.selected_investigation));
                if let Some(plan) = selected {
                    ui.separator(); ui.label(RichText::new(safe_text(text(plan, "question"))).strong());
                    ui.label(format!("Scope: {}\nStarting points: {}", text(plan, "mode"), safe_text(&plan["seeds"].to_string())));
                    ui.label("Your question, scope and case context may be sent to this provider and incur charges. Plan-only still calls the provider.");
                    ui.checkbox(&mut self.confirm_provider, "I authorize this provider request and possible charges");
                    ui.checkbox(&mut self.allow_collection, "Allow bounded public-source collection").on_hover_text("OFF by default. No identity findings are approved.");
                    ui.collapsing("Run limits & cancellation", |ui| { ui.label("8 turns · 16 tools · 16,384 total tokens · 2,048 tokens/response · 60s/request · 10-minute deadline. A new scoped investigation is created. Cancel stops the process group; already transmitted requests and persisted observations remain. No identity approval."); });
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() { self.show_run = false; }
                        if ui.add_enabled(!self.busy && self.confirm_provider, primary(if self.allow_collection { "Run with collection" } else { "Run plan-only" })).clicked() {
                            self.show_run = false; self.agent_result = None;
                            self.send(Request::RunAgent { config: self.agent_config.clone(), question: text(plan, "question").into(), mode: text(plan, "mode").into(), seeds: rows(plan, "seeds").iter().filter_map(Value::as_str).map(str::to_owned).collect(), collection: self.allow_collection });
                        }
                    });
                } else { ui.label("This question is no longer available. Refresh the case."); if ui.button("Close").clicked() { self.show_run = false; } }
            });
        }
    }
}
impl eframe::App for Nightjar {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.frame(context);
    }
}
pub fn configure(context: &egui::Context) {
    let mut style = (*context.style()).clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.panel_fill = SURFACE;
    style.visuals.window_fill = SURFACE;
    style.visuals.override_text_color = Some(INK);
    style.visuals.selection.bg_fill = BLUE.gamma_multiply(0.35);
    style.visuals.selection.stroke = egui::Stroke::new(1.0_f32, BLUE);
    style.visuals.hyperlink_color = BLUE;
    style.spacing.item_spacing = egui::vec2(10.0, 10.0);
    style.spacing.button_padding = egui::vec2(12.0, 8.0);
    style.spacing.interact_size.y = 34.0;
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
    context.set_style(style);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn default_workflow_is_local_ask_with_no_blank_inspector() {
        let mut app = Nightjar::with_context(egui::Context::default(), None);
        assert_eq!(app.workspace_tab, 0);
        assert!(app.drafting);
        assert!(!app.show_inspector);
        assert!(app.document.is_none());
        assert!(!app.show_run);
        assert!(!app.allow_collection);
        assert!(!app.confirm_provider);
        assert!(!app.busy);
        assert!(matches!(
            app.worker.replies.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        app.inspect("Source", json!({"id":"source"}));
        assert!(app.show_inspector);
        app.start_question();
        assert!(app.drafting);
        assert!(!app.show_run);
        assert!(!app.busy);
    }
    #[test]
    fn plan_requires_explicit_scope_and_bounded_seeds() {
        let mut form = PlanForm {
            question: "Who references this public profile?".into(),
            ..Default::default()
        };
        assert!(form.validate().is_err());
        form.mode = "person".into();
        form.seeds = "  alice  \n\nhttps://example.org/profile\n".into();
        assert_eq!(
            form.validate().unwrap(),
            vec!["alice", "https://example.org/profile"]
        );
        form.seeds = (0..6).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
        assert!(form.validate().is_err());
    }
    #[test]
    fn crypto_scope_requires_explicit_chain_and_never_starts_a_run() {
        let mut form = PlanForm {
            question: "Trace this address".into(),
            mode: "crypto".into(),
            seeds: "0x123".into(),
        };
        assert!(form.validate().is_err());
        form.seeds = format!("ethereum:0x{}", "a".repeat(40));
        assert_eq!(form.validate().unwrap(), vec![form.seeds.clone()]);
        form.seeds = format!("ethereum:0x{}", "b".repeat(64));
        assert!(form.validate().is_ok());
    }
    #[test]
    fn untrusted_text_is_plain_and_controls_visible() {
        assert_eq!(
            safe_text("<script>x</script>\u{202e}\0"),
            "<script>x</script>\\u{202e}\\u{0000}"
        );
        assert_eq!(clip("夜鷹abc", 3), "夜鷹a…");
    }
    #[test]
    fn empty_error_containers_are_not_failures() {
        for value in [Value::Null, json!([]), json!({}), json!("")] {
            assert!(!has_recorded_error(&value));
        }
        for value in [
            json!(["blocked"]),
            json!("timeout"),
            json!({"message":"failed"}),
        ] {
            assert!(has_recorded_error(&value));
        }
    }
    #[test]
    fn case_name_hides_technical_paths_and_terminal_command_is_quoted() {
        assert_eq!(
            case_name(
                &json!({"metadata":{"title":"Untitled investigation"},"case_path":"/private/cases/Harbor"})
            ),
            "Harbor"
        );
        assert_eq!(
            case_name(
                &json!({"metadata":{"title":"Harbor review"},"case_path":"/private/cases/Harbor"})
            ),
            "Harbor review"
        );
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }
    #[test]
    fn document_paging_retains_original_unicode() {
        let document = Document::new("raw", json!({"value":"夜".repeat(18000)}));
        assert!(document.pages() >= 3);
        assert_eq!(
            serde_json::from_str::<Value>(&document.raw).unwrap()["value"],
            "夜".repeat(18000)
        );
    }
    #[test]
    fn source_graph_frames_use_a_persisted_synthetic_collector_fixture() {
        use std::collections::BTreeMap;
        let temp = tempfile::tempdir().unwrap();
        let wb = fieldkit::Workbench::new(temp.path().join("case"), fieldkit::Config::default())
            .unwrap();
        let bytes = b"SYNTHETIC source fixture, no network".to_vec();
        let hash = fieldkit::model::digest(&bytes);
        let report = json!({"run_id":"run_synthetic", "command":"dns", "target":"example.test", "status":"ok", "data":{"fixture":"SYNTHETIC"},
            "sources":[{"source_id":"src_synthetic","sha256":hash,"kind":"dns_response","url":"https://example.test"}],
            "entities":[{"type":"domain","value":"example.test","source_ids":["src_synthetic"]},{"type":"ip","value":"192.0.2.1","source_ids":["src_synthetic"]}],
            "relationships":[{"source":{"type":"domain","value":"example.test"},"target":{"type":"ip","value":"192.0.2.1"},"relation":"resolves_to","source_ids":["src_synthetic"]}], "timing":{"completed_at":"2026-01-01T00:00:00Z"}});
        wb.store
            .save_report(&fieldkit::Collected {
                report,
                blobs: BTreeMap::from([(hash, bytes)]),
            })
            .unwrap();
        let state = wb.snapshot().unwrap();
        assert_eq!(rows(&state["graph"], "nodes").len(), 2);
        assert_eq!(rows(&state["graph"], "edges").len(), 1);
        let context = egui::Context::default();
        configure(&context);
        let mut app = Nightjar::with_context(context.clone(), None);
        app.snapshot = Some(Arc::new(state));
        app.workspace_tab = 1;
        for table in [false, true] {
            app.graph.table = table;
            let output = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1480.0, 1000.0),
                    )),
                    ..Default::default()
                },
                |ctx| app.frame(ctx),
            );
            assert!(!output.shapes.is_empty());
        }
    }
    #[test]
    fn headless_frames_cover_local_workflow_without_collection() {
        let context = egui::Context::default();
        configure(&context);
        let mut app = Nightjar::with_context(context.clone(), None);
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1440.0, 1000.0),
            )),
            ..Default::default()
        };
        assert!(!context.run(input(), |ctx| app.frame(ctx)).shapes.is_empty());
        let temp = tempfile::tempdir().unwrap();
        let wb = fieldkit::Workbench::new(temp.path().join("case"), fieldkit::Config::default())
            .unwrap();
        wb.create_recon_investigation(
            "What public sources reference example.org?",
            "infrastructure",
            vec!["example.org".into()],
        )
        .unwrap();
        app.snapshot = Some(Arc::new(wb.snapshot().unwrap()));
        app.selected_investigation =
            text(&app.snapshot.as_ref().unwrap()["investigations"][0], "id").into();
        app.drafting = false;
        for tab in 0..4 {
            app.workspace_tab = tab;
            assert!(!context.run(input(), |ctx| app.frame(ctx)).shapes.is_empty());
            assert!(!app.busy);
            assert!(!app.agent_running);
        }
        assert_eq!(wb.snapshot().unwrap()["summary"]["run_count"], 0);
        assert!(!app.show_inspector);
    }
}
