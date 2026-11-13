//! Native terminal investigation workbench. All collection runs off the UI thread.
use crate::{
    Job, Workbench,
    discovery::DiscoveryOptions,
    model::{strings, text},
};
use anyhow::{Context, Result, bail};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap,
    },
};
use serde_json::{Value, json};
use std::{
    fs::OpenOptions,
    io::{self, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const ACCENT: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;
const VIEWS: [&str; 13] = [
    "Discovery",
    "Ontology",
    "Claims",
    "Evidence",
    "Entities",
    "Links",
    "Flows",
    "Correlations",
    "Notes",
    "Activity",
    "Reasoning",
    "Investigations",
    "Graph",
];

/// Restore the terminal on ordinary errors and unwinding, including setup errors.
struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
    }
}

/// Open the same persistent case used by the CLI, app and MCP server.
pub fn run(workbench: Workbench) -> Result<()> {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
        previous(info);
    }));
    let mut ui = Ui::new(workbench)?;
    enable_raw_mode().context("Could not enable terminal raw mode")?;
    let _guard = TerminalGuard;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    let result = ui.run_loop(&mut terminal);
    if let Some(worker) = ui.worker.take() {
        worker.stop.store(true, Ordering::Relaxed);
        // Collector requests have configured timeouts. Finish writing the current
        // observation before closing the case rather than leaving a half-record.
        let _ = worker.handle.join();
    }
    result
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum View {
    Discovery,
    Ontology,
    Claims,
    Evidence,
    Entities,
    Links,
    Flows,
    Correlations,
    Notes,
    Activity,
    Reasoning,
    Investigations,
    Graph,
}
impl View {
    fn index(self) -> usize {
        self as usize
    }
    fn from_index(index: usize) -> Self {
        [
            Self::Discovery,
            Self::Ontology,
            Self::Claims,
            Self::Evidence,
            Self::Entities,
            Self::Links,
            Self::Flows,
            Self::Correlations,
            Self::Notes,
            Self::Activity,
            Self::Reasoning,
            Self::Investigations,
            Self::Graph,
        ][index % VIEWS.len()]
    }
    fn key(self) -> &'static str {
        match self {
            Self::Discovery => "discovery",
            Self::Ontology => "ontology",
            Self::Claims => "claims",
            Self::Evidence => "evidence",
            Self::Entities => "entities",
            Self::Links => "links",
            Self::Flows => "flows",
            Self::Correlations => "correlations",
            Self::Notes => "notes",
            Self::Activity => "activity",
            Self::Reasoning => "reasoning",
            Self::Investigations => "investigations",
            Self::Graph => "graph",
        }
    }
    fn shortcut(self) -> String {
        match self {
            Self::Reasoning => "b".into(),
            Self::Investigations => "w".into(),
            Self::Graph => "G".into(),
            _ => ((self.index() + 1) % 10).to_string(),
        }
    }
    fn has_children(self) -> bool {
        matches!(self, Self::Discovery | Self::Flows | Self::Investigations)
    }
}

#[derive(Clone, Debug)]
struct Field {
    label: String,
    value: String,
    hint: String,
    cursor: usize,
}
impl Field {
    fn new(label: &str, value: impl Into<String>, hint: &str) -> Self {
        let value = value.into();
        let cursor = value.chars().count();
        Self {
            label: label.into(),
            value,
            hint: hint.into(),
            cursor,
        }
    }
    fn insert(&mut self, ch: char) {
        if ch.is_control() || self.value.len() >= 20000 {
            return;
        }
        let at = self.byte_index();
        self.value.insert(at, ch);
        self.cursor += 1;
    }
    fn byte_index(&self) -> usize {
        self.value
            .char_indices()
            .nth(self.cursor)
            .map(|(index, _)| index)
            .unwrap_or(self.value.len())
    }
    fn edit(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.value.clear();
                self.cursor = 0;
            }
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert(ch)
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let end = self.byte_index();
                self.cursor -= 1;
                let start = self.byte_index();
                self.value.replace_range(start..end, "");
            }
            KeyCode::Delete if self.cursor < self.value.chars().count() => {
                let start = self.byte_index();
                let end = self
                    .value
                    .char_indices()
                    .nth(self.cursor + 1)
                    .map(|(i, _)| i)
                    .unwrap_or(self.value.len());
                self.value.replace_range(start..end, "");
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.value.chars().count()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.value.chars().count(),
            _ => {}
        }
    }
}

#[derive(Clone, Debug)]
enum FormAction {
    Investigation,
    InvestigationReview {
        investigation_id: String,
        finding_id: String,
    },
    Discovery,
    Planner(String),
    Collector,
    Evidence,
    Case,
    Note(Vec<String>),
    Entity,
    Link,
    Flow,
    Step(String),
    EvidenceReview(String),
    CorrelationReview(String),
    ClaimReview(String),
    Export,
    Filter,
}
#[derive(Clone, Debug)]
struct Form {
    title: String,
    action: FormAction,
    fields: Vec<Field>,
    focus: usize,
    error: String,
}
impl Form {
    fn new(title: &str, action: FormAction, fields: Vec<Field>) -> Self {
        Self {
            title: title.into(),
            action,
            fields,
            focus: 0,
            error: String::new(),
        }
    }
    fn value(&self, index: usize) -> &str {
        self.fields.get(index).map(|v| v.value.trim()).unwrap_or("")
    }
}

enum Modal {
    Form(Form),
    Help(u16),
    EvidencePicker { rows: Vec<Value>, selected: usize },
}
enum WorkerEvent {
    Progress(Value),
    Done(std::result::Result<Value, String>),
}
struct Worker {
    stop: Arc<AtomicBool>,
    receiver: mpsc::Receiver<WorkerEvent>,
    handle: thread::JoinHandle<()>,
    label: String,
    kind: WorkerKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkerKind {
    Collection,
    InvestigationStep,
    ProviderStatus,
}

#[derive(Default)]
struct GraphNavigation {
    focus: Option<String>,
    history: Vec<(String, usize)>,
    external: Option<(String, String)>,
    present_pending: bool,
    browse_nodes: bool,
}

struct Ui {
    workbench: Workbench,
    data: Value,
    view: View,
    selected: [usize; VIEWS.len()],
    child_selected: usize,
    child_focus: bool,
    filter: String,
    inspector_scroll: u16,
    modal: Option<Modal>,
    worker: Option<Worker>,
    notice: String,
    notice_error: bool,
    quitting: bool,
    refreshed_at: Instant,
    provider_status: Option<Value>,
    activity_revision: u64,
    graph: GraphNavigation,
}

fn safe(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
        .collect()
}
// Graph labels are single-line source data, never terminal control/layout text.
fn graph_inline(value: &str) -> String {
    value.chars().map(|ch| {
        if ch.is_control() || matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
            ' '
        } else {
            ch
        }
    }).collect()
}
fn graph_node_label(node: &Value) -> String {
    let value = node["value"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| node["value"].to_string());
    format!(
        "[{}] {}",
        graph_inline(text(node, "type")),
        graph_inline(&value)
    )
}
// Compact table labels only: preserve exact node values and full inspector URLs.
fn graph_neighbor_label(node: &Value) -> String {
    let archived = || -> Option<String> {
        if text(node, "type") != "url" {
            return None;
        }
        let url = url::Url::parse(text(node, "value")).ok()?;
        if url.host_str() != Some("web.archive.org") {
            return None;
        }
        let (stamp, original) = url.path().strip_prefix("/web/")?.split_once('/')?;
        let stamp = stamp.strip_suffix("id_").unwrap_or(stamp);
        if stamp.len() != 14 || !stamp.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let captured = chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%d%H%M%S").ok()?;
        let original = url::Url::parse(original).ok()?;
        Some(format!(
            "[url] {} {}{} · archive",
            captured.format("%Y-%m-%d"),
            original.host_str()?,
            original.path()
        ))
    };
    archived()
        .map(|label| graph_inline(&label))
        .unwrap_or_else(|| graph_node_label(node))
}
fn array(value: &Value, key: &str) -> Vec<Value> {
    value[key].as_array().cloned().unwrap_or_default()
}
fn csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
fn reference_ids(record: &Value) -> Vec<String> {
    let mut ids = strings(&record["evidence_ids"]);
    if let Some(id) = record["evidence_id"].as_str() {
        ids.push(id.into());
    }
    if let Some(supports) = record["supports"].as_array() {
        for support in supports {
            ids.extend(strings(&support["evidence_ids"]));
            if let Some(id) = support["evidence_id"].as_str() {
                ids.push(id.into());
            }
        }
    }
    for citation in array(record, "citations") {
        if let Some(id) = citation["evidence_id"].as_str() {
            ids.push(id.into());
        }
        ids.extend(strings(&citation["evidence_ids"]));
    }
    ids.sort();
    ids.dedup();
    ids
}
fn required(value: &str, name: &str) -> Result<String> {
    if value.trim().is_empty() {
        bail!("{name} is required");
    }
    Ok(value.trim().to_owned())
}
fn job_fields() -> Vec<Field> {
    vec![
        Field::new(
            "Collector",
            "username",
            "username, profile, http, search, domain, telegram-profile, telegram-messages, ...",
        ),
        Field::new(
            "Target",
            "",
            "A username, public URL, domain, or another collector's target",
        ),
        Field::new(
            "Options JSON",
            "{}",
            "For username: {\"sites\":[\"github\",\"bluesky\"]}",
        ),
    ]
}
fn form_job(form: &Form, start: usize) -> Result<Job> {
    Ok(Job {
        command: required(form.value(start), "Collector")?,
        target: (!form.value(start + 1).is_empty()).then(|| form.value(start + 1).to_owned()),
        options: serde_json::from_str(form.value(start + 2))
            .context("Options must be a JSON object")?,
    })
}
fn investigation_searches(form: &Form) -> Result<Vec<Job>> {
    if form.value(1).is_empty() {
        if !form.value(2).is_empty() || !form.value(3).is_empty() {
            bail!("A search query is required when specifying dates");
        }
        return Ok(vec![]);
    }
    let limit: usize = form
        .value(4)
        .parse()
        .context("Limit must be a whole number")?;
    if !(1..=100).contains(&limit) {
        bail!("Limit must be between 1 and 100");
    }
    let mut options = json!({"limit": limit});
    for (index, key) in [(2, "since"), (3, "until")] {
        if !form.value(index).is_empty() {
            options[key] = json!(form.value(index));
        }
    }
    let job = Job {
        command: "telegram-search".into(),
        target: Some(form.value(1).to_owned()),
        options,
    };
    crate::collectors::validate_job(&job)?;
    Ok(vec![job])
}

fn discovery_options(form: &Form) -> Result<DiscoveryOptions> {
    let max_jobs: usize = form
        .value(2)
        .parse()
        .context("Job budget must be a whole number")?;
    let max_depth: usize = form
        .value(3)
        .parse()
        .context("Depth must be a whole number")?;
    if !(1..=100).contains(&max_jobs) {
        bail!("Job budget must be between 1 and 100");
    }
    if max_depth > 4 {
        bail!("Depth must be between 0 and 4");
    }
    let web_search = match form.value(6).to_ascii_lowercase().as_str() {
        "yes" | "true" => true,
        "no" | "false" => false,
        _ => bail!("Web search must be yes or no"),
    };
    let planner = form.value(7).to_ascii_lowercase();
    if !matches!(planner.as_str(), "local" | "nvidia") {
        bail!("Planner must be local or nvidia");
    }
    let reasoning_budget: usize = form
        .value(8)
        .parse()
        .context("Reasoning budget must be a whole number")?;
    if !(1..=20).contains(&reasoning_budget) {
        bail!("Reasoning budget must be between 1 and 20 calls across this session");
    }
    Ok(DiscoveryOptions {
        kind: required(form.value(0), "Seed kind")?,
        seed: required(form.value(1), "Seed")?,
        max_jobs,
        max_depth,
        domains: csv(form.value(4)),
        sites: csv(form.value(5)),
        web_search,
        planner,
        reasoning_budget,
    })
}

fn provider_notice(status: &Value) -> (String, bool) {
    let ready = status["ready"].as_bool().unwrap_or(false);
    let state = status["status"]
        .as_str()
        .unwrap_or(if ready { "ready" } else { "unverified" });
    let blockers = strings(&status["blockers"]);
    let message = if text(status, "message").is_empty() {
        blockers.into_iter().take(8).collect::<Vec<_>>().join("; ")
    } else {
        text(status, "message").to_owned()
    };
    if ready {
        (format!("NVIDIA Auto Ontology ready · {message}"), false)
    } else {
        (
            format!(
                "NVIDIA Auto Ontology {state} · {message} · setup: integrations/auto-ontology/README.md"
            ),
            true,
        )
    }
}

fn completion_notice(label: &str, report: &Value) -> (String, bool) {
    let status = text(report, "status");
    let reason = report["stop_reason"]
        .as_str()
        .unwrap_or_else(|| text(&report["stop_reason"], "message"));
    let reason_lower = reason.to_ascii_lowercase();
    let paused_provider_failure = status == "paused"
        && report["options"]["planner"] == "nvidia"
        && [
            "unavailable",
            "unconfigured",
            "unreachable",
            "setup",
            "configuration",
            "connection",
            "authentication",
            "compilation",
        ]
        .iter()
        .any(|word| reason_lower.contains(word));
    let planner_failed =
        matches!(status, "needs_setup" | "planner_failed") || paused_provider_failure;
    let failed = planner_failed
        || matches!(
            status,
            "unavailable" | "error" | "invalid" | "blocked" | "rate_limited" | "failed"
        );
    let detail = if text(report, "message").is_empty() {
        reason
    } else {
        text(report, "message")
    };
    if status == "reasoning_budget_exhausted" {
        return (
            format!(
                "{label} · NVIDIA reasoning budget exhausted · m updates the total call budget; g resumes the saved queue"
            ),
            true,
        );
    }
    if planner_failed {
        (
            format!(
                "{label} · {status} · {detail} · b reasoning / z connection check · no local fallback"
            ),
            true,
        )
    } else if !reason.is_empty() {
        (format!("{label} · {status} · {reason}"), failed)
    } else {
        (
            format!("{label} · {status} · observations preserved in this case"),
            failed,
        )
    }
}

fn provider_details(status: &Value) -> String {
    let mut details = format!(
        "NVIDIA Auto Ontology\nStatus: {}\n",
        status["status"].as_str().unwrap_or("unconfigured")
    );
    for (key, label) in [
        ("endpoint", "Endpoint"),
        ("target_db", "Case projection database"),
        ("projection_path", "Projection path"),
    ] {
        if let Some(value) = status[key].as_str().filter(|value| !value.is_empty()) {
            details.push_str(&format!("{label}: {}\n", safe(value)));
        }
    }
    if !text(status, "message").is_empty() {
        details.push_str(&format!("{}\n", safe(text(status, "message"))));
    }
    for key in ["blockers", "unverified"] {
        for message in strings(&status[key]).into_iter().take(8) {
            details.push_str(&format!("{}\n", safe(&message)));
        }
    }
    details.push_str("\nz checks the configured server and compiled ontology in the background.\n\nSetup: integrations/auto-ontology/README.md. Configure the documented environment or provider configuration file, then select planner=nvidia in a new discovery plan.\n\nConnection credentials stay in provider configuration and are never entered in this TUI.\n");
    details
}

impl Ui {
    fn new(workbench: Workbench) -> Result<Self> {
        let activity_revision = workbench.store.activity_revision()?;
        let data = workbench.snapshot()?;
        let mut ui = Self { workbench, data, view: View::Discovery, selected: [0; VIEWS.len()], child_selected: 0, child_focus: false,
            filter: String::new(), inspector_scroll: 0, modal: None, worker: None,
            notice: "Press d to start discovery from a username, public URL, domain or search query. ? opens the guide.".into(),
            notice_error: false, quitting: false, refreshed_at: Instant::now(), provider_status: None,
            activity_revision, graph: GraphNavigation::default() };
        ui.sync_graph();
        Ok(ui)
    }

    fn run_loop(&mut self, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
        loop {
            self.poll_worker();
            if self.quitting && self.worker.is_none() {
                break;
            }
            self.poll_case_changes();
            self.present_graph_focus();
            terminal.draw(|frame| self.draw(frame))?;
            if event::poll(Duration::from_millis(100))? {
                match event::read()? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => self.key(key),
                    Event::Paste(content) => {
                        if let Some(Modal::Form(form)) = &mut self.modal
                            && let Some(field) = form.fields.get_mut(form.focus)
                        {
                            for ch in content.chars() {
                                field.insert(if matches!(ch, '\n' | '\r' | '\t') {
                                    ' '
                                } else {
                                    ch
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn notice(&mut self, message: impl Into<String>, error: bool) {
        self.notice = message.into();
        self.notice_error = error;
    }
    fn poll_case_changes(&mut self) {
        if self.refreshed_at.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.refreshed_at = Instant::now();
        match self.workbench.store.activity_revision() {
            Ok(revision) if revision != self.activity_revision => self.refresh(),
            Ok(_) => {}
            Err(error) => self.notice(format!("Could not check case changes: {error:#}"), true),
        }
    }
    fn refresh(&mut self) {
        // Read the revision first: changes racing with the snapshot are checked
        // again next poll rather than accidentally acknowledged without a read.
        let revision = self.workbench.store.activity_revision().ok();
        match self.workbench.snapshot() {
            Ok(data) => {
                self.data = data;
                if let Some(revision) = revision {
                    self.activity_revision = revision;
                }
                self.refreshed_at = Instant::now();
                self.sync_graph();
            }
            Err(error) => self.notice(format!("Could not refresh case: {error:#}"), true),
        }
    }
    fn graph_node(&self, id: &str) -> Option<&Value> {
        self.data["graph"]["nodes"]
            .as_array()?
            .iter()
            .find(|node| text(node, "id") == id)
    }
    fn graph_focus(&self) -> Option<&Value> {
        self.graph
            .focus
            .as_deref()
            .and_then(|id| self.graph_node(id))
    }
    fn sync_graph(&mut self) {
        let entity_id = text(&self.data["graph_focus"], "entity_id");
        let external = (!entity_id.is_empty()).then(|| {
            (
                entity_id.to_owned(),
                text(&self.data["graph_focus"], "updated_at").to_owned(),
            )
        });
        if external != self.graph.external {
            self.graph.external = external.clone();
            self.graph.present_pending = false;
            if let Some((id, _)) = external {
                if self.graph_node(&id).is_some() {
                    self.graph.focus = Some(id);
                    self.graph.present_pending = true;
                    self.graph.history.clear();
                    self.graph.browse_nodes = false;
                    self.selected[View::Graph.index()] = 0;
                    self.inspector_scroll = 0;
                    if self.modal.is_some() || self.worker.is_some() {
                        self.notice("Agent graph focus is ready; Graph will open after the current editor, picker or worker finishes.", false);
                    }
                } else {
                    self.notice("Requested graph focus is outside source-backed latest-500 coverage; keeping the available local focus.", false);
                }
            }
        }
        if self.graph_focus().is_none() {
            let preferred = text(&self.data["graph"], "focus_id");
            self.graph.focus = self
                .graph_node(preferred)
                .or_else(|| {
                    self.data["graph"]["nodes"]
                        .as_array()
                        .and_then(|nodes| nodes.first())
                })
                .map(|node| text(node, "id").to_owned());
            self.graph.history.clear();
            self.selected[View::Graph.index()] = 0;
        }
        let len = self.graph_records().len();
        self.selected[View::Graph.index()] =
            self.selected[View::Graph.index()].min(len.saturating_sub(1));
        self.present_graph_focus();
    }
    fn present_graph_focus(&mut self) {
        if self.graph.present_pending
            && self.modal.is_none()
            && self.worker.is_none()
            && !self.quitting
        {
            self.graph.present_pending = false;
            self.view = View::Graph;
            self.filter.clear();
            self.child_focus = false;
            self.child_selected = 0;
            self.inspector_scroll = 0;
            self.notice("Agent graph focus received · Enter follows an observation; Backspace returns; o opens original evidence.", false);
        }
    }
    fn graph_neighbor(&self, edge: &Value) -> Option<&Value> {
        let focus = self.graph.focus.as_deref()?;
        let neighbor = if text(edge, "source_id") == focus {
            text(edge, "target_id")
        } else if text(edge, "target_id") == focus {
            text(edge, "source_id")
        } else {
            return None;
        };
        self.graph_node(neighbor)
    }
    fn graph_records(&self) -> Vec<Value> {
        let needle = self.filter.to_lowercase();
        if self.graph.browse_nodes {
            return self.filtered(array(&self.data["graph"], "nodes"));
        }
        self.data["graph"]["edges"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|edge| {
                let Some(neighbor) = self.graph_neighbor(edge) else {
                    return false;
                };
                needle.is_empty()
                    || format!("{} {}", edge, graph_node_label(neighbor))
                        .to_lowercase()
                        .contains(&needle)
            })
            .cloned()
            .collect()
    }
    fn graph_follow(&mut self) {
        let records = self.graph_records();
        let Some(record) =
            records.get(self.selected[View::Graph.index()].min(records.len().saturating_sub(1)))
        else {
            return;
        };
        let next = if self.graph.browse_nodes {
            Some(record.clone())
        } else {
            self.graph_neighbor(record).cloned()
        };
        if let Some(next) = next {
            let next_id = text(&next, "id").to_owned();
            if self.graph.focus.as_deref() != Some(next_id.as_str()) {
                if let Some(previous) = self.graph.focus.take() {
                    self.graph
                        .history
                        .push((previous, self.selected[View::Graph.index()]));
                }
                self.graph.focus = Some(next_id);
            }
            self.graph.browse_nodes = false;
            self.selected[View::Graph.index()] = 0;
            self.inspector_scroll = 0;
        }
    }
    fn graph_back(&mut self) {
        while let Some((id, selected)) = self.graph.history.pop() {
            if self.graph_node(&id).is_some() {
                self.graph.focus = Some(id);
                self.graph.browse_nodes = false;
                self.selected[View::Graph.index()] =
                    selected.min(self.graph_records().len().saturating_sub(1));
                self.inspector_scroll = 0;
                return;
            }
        }
        self.notice("No previous graph focus. Left opens all source-backed nodes, including disconnected nodes.", false);
    }
    fn sessions(&self) -> Vec<Value> {
        let rows = if self.data["discovery"].is_array() {
            array(&self.data, "discovery")
        } else {
            array(&self.data, "discoveries")
        };
        self.filtered(rows)
    }
    fn selected_session(&self) -> Option<Value> {
        let rows = self.sessions();
        rows.get(self.selected[View::Discovery.index()].min(rows.len().saturating_sub(1)))
            .cloned()
    }
    fn selected_flow(&self) -> Option<Value> {
        let rows = self.filtered(array(&self.data, "flows"));
        rows.get(self.selected[View::Flows.index()].min(rows.len().saturating_sub(1)))
            .cloned()
    }
    fn selected_investigation(&self) -> Option<Value> {
        let rows = self.filtered(array(&self.data, "investigations"));
        rows.get(self.selected[View::Investigations.index()].min(rows.len().saturating_sub(1)))
            .cloned()
    }
    fn children(&self) -> Vec<Value> {
        match self.view {
            View::Investigations => self
                .selected_investigation()
                .map(|row| {
                    let mut children = array(&row, "steps");
                    children.extend(array(&row, "findings"));
                    children
                })
                .unwrap_or_default(),
            View::Discovery => {
                let Some(session) = self.selected_session() else {
                    return vec![];
                };
                let id = text(&session, "id");
                let frontier = array(&self.data, "frontier");
                if frontier.is_empty() {
                    if let Some(jobs) = session["jobs"].as_array() {
                        return jobs.clone();
                    }
                    return self.workbench.discovery_jobs(id).unwrap_or_default();
                }
                frontier
                    .into_iter()
                    .filter(|job| {
                        text(job, "discovery_id") == id
                            || text(job, "session_id") == id
                            || text(job, "session") == id
                    })
                    .collect()
            }
            View::Flows => self
                .selected_flow()
                .map(|row| array(&row, "steps"))
                .unwrap_or_default(),
            _ => vec![],
        }
    }
    fn filtered(&self, rows: Vec<Value>) -> Vec<Value> {
        if self.filter.is_empty() {
            return rows;
        }
        let needle = self.filter.to_lowercase();
        rows.into_iter()
            .filter(|row| row.to_string().to_lowercase().contains(&needle))
            .collect()
    }
    fn records(&self) -> Vec<Value> {
        let rows = match self.view {
            View::Graph => return self.graph_records(),
            View::Discovery => return self.sessions(),
            View::Ontology => {
                let mut values = array(&self.data["ontology"], "concepts");
                values.extend(array(&self.data["ontology"], "relations"));
                values
            }
            View::Claims => array(&self.data["ontology"], "claims"),
            _ => array(&self.data, self.view.key()),
        };
        self.filtered(rows)
    }
    fn current(&self) -> Option<Value> {
        let rows = if self.child_focus && self.view.has_children() {
            self.children()
        } else {
            self.records()
        };
        let index = if self.child_focus {
            self.child_selected
        } else {
            self.selected[self.view.index()]
        };
        rows.get(index.min(rows.len().saturating_sub(1)))
            .cloned()
            .or_else(|| {
                (self.view == View::Graph && !self.graph.browse_nodes)
                    .then(|| self.graph_focus().cloned())
                    .flatten()
            })
    }
    fn move_selection(&mut self, delta: isize) {
        let len = if self.child_focus {
            self.children().len()
        } else {
            self.records().len()
        };
        let selected = if self.child_focus {
            &mut self.child_selected
        } else {
            &mut self.selected[self.view.index()]
        };
        *selected = selected
            .saturating_add_signed(delta)
            .min(len.saturating_sub(1));
        if !self.child_focus {
            self.child_selected = 0;
        }
        self.inspector_scroll = 0;
    }
    fn change_view(&mut self, index: usize) {
        self.view = View::from_index(index);
        self.child_focus = false;
        self.child_selected = 0;
        self.inspector_scroll = 0;
        if self.view == View::Graph {
            self.sync_graph();
        }
    }

    fn spawn(
        &mut self,
        label: String,
        task: impl FnOnce(Workbench, Arc<AtomicBool>, mpsc::Sender<WorkerEvent>) -> Result<Value>
        + Send
        + 'static,
    ) {
        if self.worker.is_some() {
            self.notice(
                "A collector is running. Use p to pause discovery after the current collector.",
                true,
            );
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let model = self.workbench.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            let progress = sender.clone();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                task(model, worker_stop, progress)
            }))
            .map_err(|_| {
                "Collector stopped unexpectedly; refresh and resume the investigation.".to_string()
            })
            .and_then(|result| result.map_err(|error| format!("{error:#}")));
            let _ = sender.send(WorkerEvent::Done(result));
        });
        self.notice(
            format!("Running {label} · evidence and ontology update as results arrive"),
            false,
        );
        self.worker = Some(Worker {
            stop,
            receiver,
            handle,
            label,
            kind: WorkerKind::Collection,
        });
    }
    fn poll_worker(&mut self) {
        let events: Vec<_> = self
            .worker
            .as_ref()
            .map(|w| w.receiver.try_iter().collect())
            .unwrap_or_default();
        for event in events {
            match event {
                WorkerEvent::Progress(value) => {
                    let message = if !text(&value, "message").is_empty() {
                        text(&value, "message").to_owned()
                    } else if text(&value, "status") == "reasoning" {
                        "NVIDIA Auto Ontology is reasoning over preserved case evidence and selecting a next step…".into()
                    } else {
                        format!(
                            "{} {} · {}",
                            text(&value, "command"),
                            text(&value, "target"),
                            text(&value, "status")
                        )
                    };
                    self.notice(message, false);
                    self.refresh();
                }
                WorkerEvent::Done(result) => {
                    let worker = self.worker.take();
                    let label = worker.as_ref().map(|w| w.label.clone()).unwrap_or_default();
                    let kind = worker
                        .as_ref()
                        .map(|w| w.kind)
                        .unwrap_or(WorkerKind::Collection);
                    if let Some(worker) = worker {
                        let _ = worker.handle.join();
                    }
                    match result {
                        Ok(report) if kind == WorkerKind::ProviderStatus => {
                            let (message, warning) = provider_notice(&report);
                            self.provider_status = Some(report);
                            self.notice(message, warning);
                        }
                        Ok(report) if kind == WorkerKind::InvestigationStep => {
                            let (message, warning) = completion_notice(&label, &report);
                            self.notice(format!("{message} · findings require human review; r reviews a selected finding"), warning);
                        }
                        Ok(report) => {
                            let (message, warning) = completion_notice(&label, &report);
                            self.notice(message, warning);
                        }
                        Err(error) if kind == WorkerKind::ProviderStatus => self.notice(format!("NVIDIA connection check failed · {error} · setup: integrations/auto-ontology/README.md"), true),
                        Err(error) => self.notice(error, true),
                    }
                    self.refresh();
                }
            }
        }
    }

    fn key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit();
            return;
        }
        if self.modal.is_some() {
            self.modal_key(key);
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.quit(),
            KeyCode::Tab => self.change_view(self.view.index() + 1),
            KeyCode::BackTab => self.change_view(self.view.index() + VIEWS.len() - 1),
            KeyCode::Char(ch @ '1'..='9') => self.change_view(ch as usize - '1' as usize),
            KeyCode::Char('0') => self.change_view(9),
            KeyCode::Char('b') => self.change_view(View::Reasoning.index()),
            KeyCode::Char('w') => self.change_view(View::Investigations.index()),
            KeyCode::Char('G') => self.change_view(View::Graph.index()),
            KeyCode::Left | KeyCode::Right if self.view == View::Graph => {
                self.graph.browse_nodes = key.code == KeyCode::Left;
                self.selected[View::Graph.index()] = 0;
                self.inspector_scroll = 0;
            }
            KeyCode::Enter if self.view == View::Graph => self.graph_follow(),
            KeyCode::Backspace if self.view == View::Graph => self.graph_back(),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Home => {
                self.selected[self.view.index()] = 0;
                self.child_selected = 0;
                self.inspector_scroll = 0;
            }
            KeyCode::End => self.move_selection(isize::MAX),
            KeyCode::PageDown => self.inspector_scroll = self.inspector_scroll.saturating_add(8),
            KeyCode::PageUp => self.inspector_scroll = self.inspector_scroll.saturating_sub(8),
            KeyCode::Enter if self.view.has_children() => {
                self.child_focus = !self.child_focus;
                self.inspector_scroll = 0;
            }
            KeyCode::Enter if self.view == View::Reasoning => self.open_recommended_job(),
            KeyCode::Esc => {
                self.filter.clear();
                self.child_focus = false;
                self.inspector_scroll = 0;
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.refresh();
                self.notice("Case refreshed", false);
            }
            KeyCode::Char('d') if self.view == View::Investigations => self.investigation_form(),
            KeyCode::Char('d') => self.discovery_form(),
            KeyCode::Char('m') => self.planner_form(),
            KeyCode::Char('g') if self.view == View::Investigations => {
                self.start_investigation_step()
            }
            KeyCode::Char('g') => self.start_discovery(),
            KeyCode::Char('z') => self.check_provider(),
            KeyCode::Char('p')
                if self
                    .worker
                    .as_ref()
                    .is_some_and(|worker| worker.kind == WorkerKind::InvestigationStep) =>
            {
                self.notice("One bounded investigation step is running; it finishes without scheduling further steps.", false);
            }
            KeyCode::Char('p') => {
                if let Some(worker) = &self.worker {
                    worker.stop.store(true, Ordering::Relaxed);
                    self.notice(
                        "Pause requested · stopping reasoning or preserving the current collector",
                        false,
                    );
                } else {
                    self.notice(
                        "No discovery worker is running. Select a session and press g to resume.",
                        false,
                    );
                }
            }
            KeyCode::Char('t') => {
                self.modal = Some(Modal::Form(Form::new(
                    "Run collector · save evidence",
                    FormAction::Collector,
                    job_fields(),
                )))
            }
            KeyCode::Char('e') => self.evidence_form(),
            KeyCode::Char('i') => self.case_form(),
            KeyCode::Char('a') => self.entity_form(),
            KeyCode::Char('l') => self.link_form(),
            KeyCode::Char('n') => self.note_form(),
            KeyCode::Char('f') => self.flow_form(),
            KeyCode::Char('s') => self.step_form(),
            KeyCode::Char('r') if self.view == View::Investigations => {
                self.investigation_review_form()
            }
            KeyCode::Char('r') => self.start_step(),
            KeyCode::Char('v') => self.review_form(),
            KeyCode::Char('c') => self.correlation_form(),
            KeyCode::Char('o') => self.open_evidence(),
            KeyCode::Char('y') => match self.workbench.sync_ontology() {
                Ok(_) => {
                    self.refresh();
                    self.notice("Ontology synchronized from preserved evidence", false);
                }
                Err(error) => self.notice(format!("{error:#}"), true),
            },
            KeyCode::Char('x') => {
                self.modal = Some(Modal::Form(Form::new(
                    "Export investigation · create a new file",
                    FormAction::Export,
                    vec![
                        Field::new("Format", "markdown", "markdown or json"),
                        Field::new(
                            "New output filename",
                            "fieldkit-investigation.md",
                            "An existing file will never be overwritten",
                        ),
                    ],
                )))
            }
            KeyCode::Char('/') => {
                self.modal = Some(Modal::Form(Form::new(
                    "Filter the current case records",
                    FormAction::Filter,
                    vec![Field::new(
                        "Contains",
                        self.filter.clone(),
                        "Case-insensitive match; Escape from the main view clears this filter",
                    )],
                )))
            }
            KeyCode::Char('?') => self.modal = Some(Modal::Help(0)),
            _ => {}
        }
    }

    fn quit(&mut self) {
        self.quitting = true;
        if let Some(worker) = &self.worker {
            worker.stop.store(true, Ordering::Relaxed);
            self.notice("Stopping reasoning or preserving the current collector before closing. The discovery queue can be resumed next time.", false);
        }
    }
    fn modal_key(&mut self, key: KeyEvent) {
        let Some(modal) = self.modal.take() else {
            return;
        };
        match modal {
            Modal::Help(mut scroll) => match key.code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Enter => {}
                KeyCode::Down | KeyCode::PageDown => {
                    scroll = scroll.saturating_add(3);
                    self.modal = Some(Modal::Help(scroll));
                }
                KeyCode::Up | KeyCode::PageUp => {
                    scroll = scroll.saturating_sub(3);
                    self.modal = Some(Modal::Help(scroll));
                }
                _ => self.modal = Some(Modal::Help(scroll)),
            },
            Modal::EvidencePicker { rows, mut selected } => match key.code {
                KeyCode::Esc => {}
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = (selected + 1).min(rows.len().saturating_sub(1));
                    self.modal = Some(Modal::EvidencePicker { rows, selected });
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    selected = selected.saturating_sub(1);
                    self.modal = Some(Modal::EvidencePicker { rows, selected });
                }
                KeyCode::Enter => {
                    if let Some(row) = rows.get(selected) {
                        self.jump_evidence(text(row, "id"));
                    }
                }
                _ => self.modal = Some(Modal::EvidencePicker { rows, selected }),
            },
            Modal::Form(mut form) => {
                let save =
                    key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL);
                if key.code == KeyCode::Esc {
                    return;
                }
                if save {
                    match self.submit(&form) {
                        Ok(()) => {
                            self.refresh();
                        }
                        Err(error) => {
                            form.error = format!("{error:#}");
                            self.modal = Some(Modal::Form(form));
                        }
                    }
                    return;
                }
                match key.code {
                    KeyCode::Tab | KeyCode::Down | KeyCode::Enter => {
                        form.focus = (form.focus + 1) % form.fields.len()
                    }
                    KeyCode::BackTab | KeyCode::Up => {
                        form.focus = (form.focus + form.fields.len() - 1) % form.fields.len()
                    }
                    _ => {
                        if let Some(field) = form.fields.get_mut(form.focus) {
                            field.edit(key);
                        }
                    }
                }
                self.modal = Some(Modal::Form(form));
            }
        }
    }

    fn investigation_form(&mut self) {
        self.modal = Some(Modal::Form(Form::new(
            "New Telegram investigation · save locally; no collection",
            FormAction::Investigation,
            vec![
                Field::new(
                    "Question",
                    "",
                    "What should the cited evidence support or challenge?",
                ),
                Field::new(
                    "Search query (optional)",
                    "",
                    "Blank saves only the question; a query plans one bounded telegram-search step",
                ),
                Field::new(
                    "Since (optional)",
                    "",
                    "YYYY-MM-DD or RFC3339; blank uses the archive default (about 12 months ago)",
                ),
                Field::new(
                    "Until (optional)",
                    "",
                    "Exclusive YYYY-MM-DD or RFC3339; archive data must be at least six months old",
                ),
                Field::new(
                    "Result limit",
                    "20",
                    "1–100 archive results; saving never sends a network request",
                ),
            ],
        )));
    }
    fn start_investigation_step(&mut self) {
        let Some(investigation) = self.selected_investigation() else {
            self.notice(
                "Press d to save an investigation question and optional search first.",
                true,
            );
            return;
        };
        let steps = array(&investigation, "steps");
        let Some(step) = steps.get(self.child_selected).filter(|_| self.child_focus) else {
            self.notice("Enter switches to steps/findings. Select a search step and press g; findings use r for human review.", true);
            return;
        };
        if self.worker.is_some() {
            self.notice("A worker is already active. Wait for it before running another investigation step.", true);
            return;
        }
        let id = text(&investigation, "id").to_owned();
        let step_id = text(step, "id").to_owned();
        self.spawn(
            format!("Telegram investigation step · {}", text(step, "target")),
            move |model, _, _| model.run_investigation_step(&id, &step_id),
        );
        if let Some(worker) = &mut self.worker {
            worker.kind = WorkerKind::InvestigationStep;
        }
        self.notice("Running one bounded Telegram search step · no automatic follow-ups or finding acceptance", false);
    }
    fn investigation_review_form(&mut self) {
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.kind == WorkerKind::InvestigationStep)
        {
            self.notice(
                "Wait for the bounded investigation step to finish before saving human review.",
                true,
            );
            return;
        }
        let Some(investigation) = self.selected_investigation() else {
            self.notice("Select an investigation finding to review.", true);
            return;
        };
        let step_count = array(&investigation, "steps").len();
        let finding = self
            .child_selected
            .checked_sub(step_count)
            .and_then(|index| array(&investigation, "findings").get(index).cloned())
            .filter(|_| self.child_focus);
        let Some(finding) = finding else {
            self.notice("Select a finding in the child table, then press r. Running a step never accepts its findings.", true);
            return;
        };
        self.modal = Some(Modal::Form(Form::new(
            "Human review · only accepted findings enter the summary",
            FormAction::InvestigationReview {
                investigation_id: text(&investigation, "id").to_owned(),
                finding_id: text(&finding, "id").to_owned(),
            },
            vec![
                Field::new(
                    "Decision",
                    "",
                    "Type accepted or rejected explicitly; inspect citations with o before reviewing",
                ),
                Field::new(
                    "Review rationale",
                    "",
                    "Required: explain the evidence, limitations and alternative explanations; identity-sensitive findings need human judgment",
                ),
            ],
        )));
    }
    fn discovery_form(&mut self) {
        self.modal = Some(Modal::Form(Form::new(
            "Plan discovery · local / NVIDIA reasoning",
            FormAction::Discovery,
            vec![
                Field::new(
                    "Seed kind",
                    "username",
                    "username, profile, url, domain or search",
                ),
                Field::new(
                    "Seed",
                    "",
                    "Enter the username, public URL, domain or query to investigate",
                ),
                Field::new(
                    "Collector job budget",
                    "12",
                    "Maximum jobs across this session, including resumed runs; 1–100",
                ),
                Field::new(
                    "Maximum expansion depth",
                    "2",
                    "0 means seed only; 1–4 follows evidence-backed leads",
                ),
                Field::new(
                    "Scope domains (optional)",
                    "",
                    "Comma-separated domains; blank follows public references within the job/depth budget",
                ),
                Field::new(
                    "Username providers (optional)",
                    "",
                    "Comma-separated: github, gitlab, devto, bluesky, youtube, ...",
                ),
                Field::new(
                    "Include web search",
                    "no",
                    "yes uses BRAVE_SEARCH_API_KEY; no uses collector observations and explicit public links",
                ),
                Field::new(
                    "Planner",
                    "local",
                    "local follows observed links; nvidia asks NVIDIA Auto Ontology to select evidence-backed next steps",
                ),
                Field::new(
                    "Reasoning call budget",
                    "5",
                    "1–20 NVIDIA reasoning calls total across resumed runs; separate from collector job budget",
                ),
            ],
        )));
    }
    fn check_provider(&mut self) {
        if self.worker.is_some() {
            self.notice(
                "A worker is active. Pause discovery before checking the NVIDIA connection.",
                true,
            );
            return;
        }
        self.spawn(
            "NVIDIA Auto Ontology connection check".into(),
            |model, _, _| model.nvidia_status(),
        );
        if let Some(worker) = &mut self.worker {
            worker.kind = WorkerKind::ProviderStatus;
        }
        self.notice(
            "Checking NVIDIA Auto Ontology readiness in the background…",
            false,
        );
    }
    fn planner_form(&mut self) {
        if self.worker.is_some() {
            self.notice(
                "Pause discovery before changing its planner or reasoning budget.",
                true,
            );
            return;
        }
        let session = if self.view == View::Reasoning {
            self.current()
                .and_then(|record| {
                    array(&self.data, "discovery")
                        .into_iter()
                        .find(|session| session["id"] == record["discovery_id"])
                })
                .or_else(|| self.selected_session())
        } else {
            self.selected_session()
        };
        let Some(session) = session else {
            self.notice(
                "Select a discovery session first, or press d to plan one.",
                true,
            );
            return;
        };
        let used = session["reasoning_used"].as_u64().unwrap_or(0);
        let hint = format!(
            "1–20 calls total across resumed runs; {used} already used. The budget must cover these existing calls."
        );
        self.modal = Some(Modal::Form(Form::new(
            "Update selected discovery planner",
            FormAction::Planner(text(&session, "id").into()),
            vec![
                Field::new(
                    "Planner",
                    session["options"]["planner"].as_str().unwrap_or("local"),
                    "local or nvidia; changes the planner for the existing queue without repeating collection",
                ),
                Field::new(
                    "Total reasoning call budget",
                    session["options"]["reasoning_budget"]
                        .as_u64()
                        .unwrap_or(5)
                        .to_string(),
                    &hint,
                ),
            ],
        )));
    }
    fn open_recommended_job(&mut self) {
        let Some(record) = self.current() else {
            return;
        };
        let job_id = text(&record, "selected_job_id");
        if job_id.is_empty() {
            self.notice("This reasoning decision did not select a collector job. Inspect its rationale and error context.", false);
            return;
        }
        let discovery_id = text(&record, "discovery_id");
        self.filter.clear();
        let Some(session_index) = self
            .sessions()
            .iter()
            .position(|session| text(session, "id") == discovery_id)
        else {
            self.notice(
                "The recommendation's discovery session is outside the current view window.",
                true,
            );
            return;
        };
        self.change_view(View::Discovery.index());
        self.selected[View::Discovery.index()] = session_index;
        if let Some(index) = self
            .children()
            .iter()
            .position(|job| text(job, "id") == job_id)
        {
            self.child_selected = index;
            self.child_focus = true;
            self.notice(
                "Showing the recommended collector job and its captured supporting evidence.",
                false,
            );
        } else {
            self.notice("The selected job is outside the current frontier window. Inspect the reasoning record's anchors.", true);
        }
    }
    fn start_discovery(&mut self) {
        let Some(session) = self.selected_session() else {
            self.discovery_form();
            return;
        };
        let id = text(&session, "id").to_owned();
        let label = format!("discovery · {}", text(&session, "seed"));
        self.spawn(label, move |model, stop, sender| {
            model.run_discovery(&id, &stop, move |progress| {
                let _ = sender.send(WorkerEvent::Progress(progress));
            })
        });
    }
    fn evidence_form(&mut self) {
        self.modal = Some(Modal::Form(Form::new(
            "Capture evidence with source context",
            FormAction::Evidence,
            vec![
                Field::new("Title", "", "Describe the observation"),
                Field::new(
                    "Original observation",
                    "",
                    "Source content; keep your interpretation in a separate note",
                ),
                Field::new("Public source URL", "", "Optional exact source address"),
                Field::new(
                    "Source time",
                    "",
                    "Optional ISO-8601 publication/observation time; collection time is preserved separately",
                ),
            ],
        )));
    }
    fn case_form(&mut self) {
        self.modal = Some(Modal::Form(Form::new(
            "Investigation question",
            FormAction::Case,
            vec![
                Field::new(
                    "Case title",
                    text(&self.data["metadata"], "title"),
                    "A concise investigation name",
                ),
                Field::new(
                    "Research question",
                    text(&self.data["metadata"], "question"),
                    "What evidence would support or challenge the question?",
                ),
            ],
        )));
    }
    fn entity_form(&mut self) {
        self.modal = Some(Modal::Form(Form::new(
            "Add a typed entity",
            FormAction::Entity,
            vec![
                Field::new(
                    "Entity type",
                    "profile_url",
                    "Examples: profile_url, username, domain, organization; accounts stay distinct",
                ),
                Field::new("Value", "", "The entity's exact identifier"),
                Field::new("Display label", "", "Optional analyst label"),
            ],
        )));
    }
    fn link_form(&mut self) {
        let current = self.current().unwrap_or(Value::Null);
        let source = if self.view == View::Entities {
            text(&current, "id")
        } else {
            ""
        };
        self.modal = Some(Modal::Form(Form::new(
            "Link entities with supporting evidence",
            FormAction::Link,
            vec![
                Field::new("Source entity ID", source, "Use the exact ID from Entities"),
                Field::new("Target entity ID", "", "A distinct existing entity"),
                Field::new(
                    "Relationship",
                    "links_to",
                    "Describe only what the cited evidence supports",
                ),
                Field::new(
                    "Supporting evidence IDs",
                    reference_ids(&current).join(","),
                    "Comma-separated; at least one existing case evidence ID is required",
                ),
                Field::new(
                    "Rationale / limitations",
                    "",
                    "Explain the relationship and any uncertainty",
                ),
            ],
        )));
    }
    fn note_form(&mut self) {
        let current = self.current().unwrap_or(Value::Null);
        let ids = if self.view == View::Evidence {
            vec![text(&current, "id").to_owned()]
                .into_iter()
                .filter(|id| !id.is_empty())
                .collect()
        } else {
            reference_ids(&current)
        };
        self.modal = Some(Modal::Form(Form::new(
            "Add an analyst note",
            FormAction::Note(ids),
            vec![Field::new(
                "Note",
                "",
                "Interpretation, alternative explanations, and next questions; selected evidence is linked",
            )],
        )));
    }
    fn flow_form(&mut self) {
        let mut fields = vec![Field::new(
            "Research flow title",
            "",
            "The plan contains a first collector step; run it with r",
        )];
        fields.extend(job_fields());
        self.modal = Some(Modal::Form(Form::new(
            "Plan a research flow",
            FormAction::Flow,
            fields,
        )));
    }
    fn step_form(&mut self) {
        let Some(flow) = self.selected_flow() else {
            self.notice("Create a flow with f, then add steps with s.", true);
            return;
        };
        self.modal = Some(Modal::Form(Form::new(
            "Add a collector step",
            FormAction::Step(text(&flow, "id").into()),
            job_fields(),
        )));
    }
    fn start_step(&mut self) {
        let Some(flow) = self.selected_flow() else {
            self.notice(
                "Select a research flow first. Use g to run discovery sessions.",
                true,
            );
            return;
        };
        let steps = array(&flow, "steps");
        let Some(step) = steps.get(self.child_selected.min(steps.len().saturating_sub(1))) else {
            self.notice("Add a flow step with s.", true);
            return;
        };
        let flow_id = text(&flow, "id").to_owned();
        let step_id = text(step, "id").to_owned();
        self.spawn(
            format!("flow step · {}", text(step, "command")),
            move |model, _, _| model.run_step(&flow_id, &step_id),
        );
    }
    fn review_form(&mut self) {
        if self.view == View::Investigations {
            self.investigation_review_form();
            return;
        }
        if self.view == View::Correlations {
            self.correlation_form();
            return;
        }
        let Some(current) = self.current() else {
            self.notice("Select evidence, a claim or a correlation to review.", true);
            return;
        };
        if self.view == View::Claims {
            self.modal = Some(Modal::Form(Form::new(
                "Review a source-grounded claim",
                FormAction::ClaimReview(text(&current, "id").into()),
                vec![
                    Field::new(
                        "Decision",
                        "accepted",
                        "accepted or rejected; this does not merge identities",
                    ),
                    Field::new(
                        "Rationale",
                        "",
                        "Required: explain what the cited source supports and its limitations",
                    ),
                ],
            )));
        } else if self.view == View::Evidence {
            self.modal = Some(Modal::Form(Form::new(
                "Assess evidence · preserve the original capture",
                FormAction::EvidenceReview(text(&current, "id").into()),
                vec![
                    Field::new(
                        "Assessment",
                        text(&current, "status"),
                        "unreviewed, corroborated, contested or discarded",
                    ),
                    Field::new(
                        "Reason / limitations",
                        text(&current, "review_note"),
                        "Account existence alone does not establish who owns it",
                    ),
                ],
            )));
        } else {
            self.notice("Review evidence with v, claims with v, and correlations with c. Use o to open cited evidence.", true);
        }
    }
    fn correlation_form(&mut self) {
        let rows = array(&self.data, "correlations");
        let selected = if self.view == View::Correlations {
            self.current()
        } else {
            rows.get(self.selected[View::Correlations.index()]).cloned()
        };
        let Some(row) = selected else {
            self.notice("No correlation leads yet. Discovery builds them from captured handles and explicit shared references.", false);
            return;
        };
        self.modal = Some(Modal::Form(Form::new(
            "Review a correlation lead · identities stay distinct",
            FormAction::CorrelationReview(text(&row, "id").into()),
            vec![
                Field::new("Decision", "accepted", "accepted or rejected"),
                Field::new(
                    "Rationale",
                    "",
                    "Required: explain the evidence and alternative explanations; o opens source evidence",
                ),
            ],
        )));
    }
    fn open_evidence(&mut self) {
        let Some(current) = self.current() else {
            return;
        };
        let ids = reference_ids(&current);
        if ids.is_empty() {
            self.notice("This record has no evidence references. Seed jobs record your starting instruction; derived jobs must cite observations.", false);
            return;
        }
        let mut rows: Vec<Value> = ids.iter().map(|id| {
            array(&self.data, "evidence").into_iter().find(|row| text(row, "id") == id)
                .unwrap_or_else(|| json!({"id": id, "title": "Evidence outside the current view window; inspect the full case export"}))
        }).collect();
        if rows.len() == 1 {
            self.jump_evidence(text(&rows.remove(0), "id"));
        } else {
            self.modal = Some(Modal::EvidencePicker { rows, selected: 0 });
        }
    }
    fn jump_evidence(&mut self, id: &str) {
        self.filter.clear();
        if let Some(index) = array(&self.data, "evidence")
            .iter()
            .position(|row| text(row, "id") == id)
        {
            self.change_view(View::Evidence.index());
            self.selected[View::Evidence.index()] = index;
            self.notice("Showing the preserved observation and its source/hash context. PgUp/PgDn scroll the inspector.", false);
        } else {
            self.notice("Evidence is outside the bounded view window. Export the complete case to inspect its reference.", true);
        }
    }

    fn submit(&mut self, form: &Form) -> Result<()> {
        match &form.action {
            FormAction::Investigation => {
                let question = required(form.value(0), "Question")?;
                let searches = investigation_searches(form)?;
                let record = self.workbench.create_investigation(&question, searches)?;
                self.filter.clear();
                self.change_view(View::Investigations.index());
                self.refresh();
                if let Some(index) = self
                    .records()
                    .iter()
                    .position(|row| row["id"] == record["id"])
                {
                    self.selected[View::Investigations.index()] = index;
                }
                self.notice("Investigation saved locally; no search has run. Enter selects steps/findings; g runs one selected step; r reviews a finding.", false);
            }
            FormAction::InvestigationReview {
                investigation_id,
                finding_id,
            } => {
                let decision = form.value(0);
                if !matches!(decision, "accepted" | "rejected") {
                    bail!("Decision must be accepted or rejected");
                }
                let note = required(form.value(1), "Review rationale")?;
                self.workbench.review_investigation_finding(
                    investigation_id,
                    finding_id,
                    decision,
                    &note,
                )?;
                self.notice("Human review saved locally. The summary contains accepted findings only; source observations remain unchanged.", false);
            }
            FormAction::Discovery => {
                let record = self.workbench.create_discovery(discovery_options(form)?)?;
                self.filter.clear();
                self.change_view(View::Discovery.index());
                self.selected[0] = 0;
                self.refresh();
                if let Some(index) = self
                    .sessions()
                    .iter()
                    .position(|row| row["id"] == record["id"])
                {
                    self.selected[0] = index;
                }
                self.notice("Discovery planned. Press g to run; p pauses after the current collector. Inspect evidence with o.", false);
            }
            FormAction::Planner(id) => {
                let planner = form.value(0).to_ascii_lowercase();
                if !matches!(planner.as_str(), "local" | "nvidia") {
                    bail!("Planner must be local or nvidia");
                }
                let budget: usize = form
                    .value(1)
                    .parse()
                    .context("Reasoning budget must be a whole number")?;
                if !(1..=20).contains(&budget) {
                    bail!("Reasoning budget must be between 1 and 20 total calls");
                }
                self.workbench
                    .configure_discovery_planner(id, &planner, budget)?;
                self.notice("Discovery planner updated. Existing collector results and reasoning usage are preserved; g resumes the queue.", false);
            }
            FormAction::Collector => {
                let job = form_job(form, 0)?;
                crate::collectors::validate_job(&job)?;
                self.spawn(job.command.clone(), move |model, _, _| model.run_tool(&job));
            }
            FormAction::Evidence => {
                self.workbench.add_evidence(
                    &required(form.value(0), "Title")?,
                    form.value(1),
                    form.value(2),
                    "note",
                    (!form.value(3).is_empty()).then(|| form.value(3)),
                )?;
                self.change_view(View::Evidence.index());
                self.notice("Evidence captured; the source observation and your later assessment are separate records.", false);
            }
            FormAction::Case => {
                self.workbench.update_case(form.value(0), form.value(1))?;
                self.notice("Investigation question saved", false);
            }
            FormAction::Note(ids) => {
                self.workbench.add_note(form.value(0), ids.clone())?;
                self.notice("Analyst note saved with its evidence references", false);
            }
            FormAction::Entity => {
                self.workbench
                    .add_entity(form.value(0), form.value(1), form.value(2))?;
                self.notice("Typed entity added", false);
            }
            FormAction::Link => {
                self.workbench.add_link(
                    form.value(0),
                    form.value(1),
                    form.value(2),
                    csv(form.value(3)),
                    form.value(4),
                )?;
                self.notice("Evidence-backed relationship saved", false);
            }
            FormAction::Flow => {
                self.workbench
                    .create_flow(form.value(0), vec![form_job(form, 1)?])?;
                self.change_view(View::Flows.index());
                self.notice(
                    "Flow planned. Enter selects its steps; r runs the selected step.",
                    false,
                );
            }
            FormAction::Step(id) => {
                self.workbench.add_step(id, form_job(form, 0)?)?;
                self.notice("Collector step added to the selected flow", false);
            }
            FormAction::EvidenceReview(id) => {
                self.workbench
                    .review_evidence(id, form.value(0), form.value(1))?;
                self.notice("Evidence assessment recorded", false);
            }
            FormAction::CorrelationReview(id) => {
                required(form.value(1), "Review rationale")?;
                self.workbench
                    .review_correlation(id, form.value(0), form.value(1))?;
                self.notice(
                    "Correlation review recorded; source identities remain distinct",
                    false,
                );
            }
            FormAction::ClaimReview(id) => {
                required(form.value(1), "Review rationale")?;
                self.workbench
                    .review_claim(id, form.value(0), form.value(1))?;
                self.notice(
                    "Claim review recorded alongside its captured support",
                    false,
                );
            }
            FormAction::Export => {
                let format = form.value(0);
                if !matches!(format, "json" | "markdown") {
                    bail!("Format must be json or markdown");
                }
                let content = self.workbench.export(format)?;
                let filename = required(form.value(1), "Output filename")?;
                let mut options = OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut file = options
                    .open(&filename)
                    .context("Choose a new writable filename; existing files are preserved")?;
                file.write_all(content.as_bytes())?;
                file.sync_all()?;
                self.notice(format!("Export saved to {filename}"), false);
            }
            FormAction::Filter => {
                self.filter = form.value(0).into();
                self.selected[self.view.index()] = 0;
                self.child_selected = 0;
            }
        }
        Ok(())
    }

    fn draw(&self, frame: &mut Frame) {
        let area = frame.area();
        if area.width < 55 || area.height < 15 {
            frame.render_widget(
                Paragraph::new(
                    "Fieldkit needs at least 55 columns × 15 rows.\nResize the terminal; q quits.",
                )
                .block(Block::bordered().title(" Fieldkit ")),
                area,
            );
            return;
        }
        let layout = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(self.navigation_lines(area.width).len() as u16),
            Constraint::Min(5),
            Constraint::Length(3),
        ])
        .split(area);
        self.draw_header(frame, layout[0]);
        self.draw_navigation(frame, layout[1]);
        let panels = if area.width >= 100 {
            Layout::horizontal([Constraint::Percentage(46), Constraint::Percentage(54)])
                .split(layout[2])
        } else {
            Layout::vertical([Constraint::Percentage(52), Constraint::Percentage(48)])
                .split(layout[2])
        };
        self.draw_records(frame, panels[0]);
        self.draw_inspector(frame, panels[1]);
        self.draw_footer(frame, layout[3]);
        if let Some(modal) = &self.modal {
            match modal {
                Modal::Form(form) => self.draw_form(frame, form),
                Modal::Help(scroll) => self.draw_help(frame, *scroll),
                Modal::EvidencePicker { rows, selected } => {
                    self.draw_evidence_picker(frame, rows, *selected)
                }
            }
        }
    }
    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let title = text(&self.data["metadata"], "title");
        let ontology_version = self.data["ontology"]["version"].as_u64().unwrap_or(0);
        let counts = format!(
            "{} · {} evidence · {} entities · {} leads · ontology v{}",
            self.planner_summary(),
            array(&self.data, "evidence").len(),
            array(&self.data, "entities").len(),
            array(&self.data, "correlations").len(),
            ontology_version
        );
        let running = self
            .worker
            .as_ref()
            .map(|w| {
                if w.stop.load(Ordering::Relaxed) {
                    "  ◌ PAUSING"
                } else {
                    "  ◉ RUNNING"
                }
            })
            .unwrap_or("");
        let lines = vec![
            Line::from(vec![
                Span::styled(
                    " FIELDKIT ",
                    Style::default()
                        .bg(ACCENT)
                        .fg(Color::Black)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {}{running}", safe(title)),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(Span::styled(format!(" {counts}"), Style::default().fg(DIM))),
        ];
        frame.render_widget(Paragraph::new(lines), area);
    }
    fn draw_navigation(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Paragraph::new(self.navigation_lines(area.width)), area);
    }
    fn navigation_lines(&self, available: u16) -> Vec<Line<'static>> {
        let mut lines = vec![Line::default()];
        let mut width = 0;
        for (index, title) in VIEWS.iter().enumerate() {
            let shortcut = View::from_index(index).shortcut();
            let label = format!(" {shortcut}:{title} ");
            let label_width = label.chars().count();
            if width > 0 && width + label_width > available as usize {
                lines.push(Line::default());
                width = 0;
            }
            width += label_width;
            let style = if index == self.view.index() {
                Style::default()
                    .bg(Color::DarkGray)
                    .fg(ACCENT)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            lines
                .last_mut()
                .unwrap()
                .spans
                .push(Span::styled(label, style));
        }
        lines
    }
    fn planner_summary(&self) -> String {
        if self.view == View::Graph {
            return "Source observations · latest 500 reports · not verified identity".into();
        }
        if self.view == View::Investigations {
            return "Shared investigation · human-reviewed findings".into();
        }
        let session = if self.view == View::Reasoning {
            self.current()
                .and_then(|record| {
                    array(&self.data, "discovery")
                        .into_iter()
                        .find(|session| session["id"] == record["discovery_id"])
                })
                .or_else(|| self.selected_session())
        } else {
            self.selected_session()
        };
        let Some(session) = session else {
            return "planner: local".into();
        };
        let planner = session["options"]["planner"].as_str().unwrap_or("local");
        if planner != "nvidia" {
            return format!("planner: {planner}");
        }
        let used = ["reasoning_used", "reasoning_calls", "reasoning_attempted"]
            .iter()
            .find_map(|key| session[*key].as_u64())
            .unwrap_or_else(|| {
                array(&self.data, "reasoning")
                    .iter()
                    .filter(|record| record["discovery_id"] == session["id"])
                    .count() as u64
            });
        let budget = session["options"]["reasoning_budget"].as_u64().unwrap_or(5);
        format!("planner: NVIDIA · reasoning {used}/{budget}")
    }
    fn draw_records(&self, frame: &mut Frame, area: Rect) {
        if self.view == View::Graph {
            self.draw_graph(frame, area);
            return;
        }
        if self.view.has_children() {
            let panels = Layout::vertical([
                Constraint::Length(7.min(area.height / 2).max(3)),
                Constraint::Min(3),
            ])
            .split(area);
            let records = self.records();
            let title = if self.view == View::Investigations {
                "Investigations · d new · Enter children"
            } else if self.view == View::Discovery {
                "Discovery sessions · g run/resume"
            } else {
                "Research flows · Enter selects steps"
            };
            self.table(
                frame,
                panels[0],
                &records,
                self.selected[self.view.index()],
                title,
                !self.child_focus,
            );
            let children = self.children();
            let title = if self.view == View::Investigations {
                "Steps then findings · g run · r review"
            } else if self.view == View::Discovery {
                "Frontier · Enter switches focus"
            } else {
                "Collector steps · r runs selection"
            };
            self.table(
                frame,
                panels[1],
                &children,
                self.child_selected,
                title,
                self.child_focus,
            );
        } else {
            let title = if self.filter.is_empty() {
                VIEWS[self.view.index()].to_owned()
            } else {
                format!(
                    "{} · filter: {}",
                    VIEWS[self.view.index()],
                    safe(&self.filter)
                )
            };
            self.table(
                frame,
                area,
                &self.records(),
                self.selected[self.view.index()],
                &title,
                true,
            );
        }
    }
    fn graph_diagram(&self) -> String {
        let Some(focus) = self.graph_focus() else {
            return "No source-backed nodes in latest 500 reports.\nManual/model assessments and findings do not populate this graph.".into();
        };
        let focus_label = format!("FOCUS {}", graph_node_label(focus));
        let records = self.graph_records();
        if self.graph.browse_nodes {
            return format!(
                "{focus_label}\n\nAll source-backed nodes, including disconnected ones.\nEnter focuses a typed node; Right returns to its edges."
            );
        }
        let Some(edge) =
            records.get(self.selected[View::Graph.index()].min(records.len().saturating_sub(1)))
        else {
            return format!(
                "{focus_label}\n\nNo matching one-hop source observations.\nLeft: browse all nodes · Esc: clear search"
            );
        };
        let neighbor = self
            .graph_neighbor(edge)
            .map(graph_node_label)
            .unwrap_or_default();
        let relation = graph_inline(text(edge, "relation"));
        if text(edge, "source_id") == text(focus, "id") {
            format!(
                "{focus_label}\n  │ {relation} (attributed)\n  └──→ {neighbor}\n→ outgoing · ← incoming relative to focus"
            )
        } else {
            format!(
                "{neighbor}\n  │ {relation} (attributed)\n  └──→ {focus_label}\n→ outgoing · ← incoming relative to focus"
            )
        }
    }
    fn draw_graph(&self, frame: &mut Frame, area: Rect) {
        let compact = area.height < 11;
        let panels = Layout::vertical([
            Constraint::Length(if compact { 2.min(area.height) } else { 8 }),
            Constraint::Min(0),
        ])
        .split(area);
        let diagram = if compact {
            self.graph_focus()
                .map(|node| format!("FOCUS {}", graph_node_label(node)))
                .unwrap_or_else(|| "No source-backed nodes · latest 500 reports".into())
        } else {
            self.graph_diagram()
        };
        let diagram = Paragraph::new(diagram).wrap(Wrap { trim: false });
        if compact {
            frame.render_widget(diagram, panels[0]);
        } else {
            frame.render_widget(
                diagram.block(
                    Block::bordered()
                        .border_style(Style::default().fg(DIM))
                        .title(" One-hop observation · latest 500 reports "),
                ),
                panels[0],
            );
        }
        let records = self.graph_records();
        let rows: Vec<Row> = records
            .iter()
            .map(|record| {
                if self.graph.browse_nodes {
                    Row::new(vec![
                        graph_inline(text(record, "type")),
                        graph_node_label(record),
                        format!("{} ev", reference_ids(record).len()),
                    ])
                } else {
                    let direction =
                        if text(record, "source_id") == self.graph.focus.as_deref().unwrap_or("") {
                            "→ out"
                        } else {
                            "← in"
                        };
                    let neighbor = self
                        .graph_neighbor(record)
                        .map(graph_neighbor_label)
                        .unwrap_or_default();
                    Row::new(vec![
                        direction.into(),
                        graph_inline(text(record, "relation")),
                        neighbor,
                        format!("{} ev", reference_ids(record).len()),
                    ])
                }
            })
            .collect();
        // Relations are nominal labels, not proportional marks: reserve their
        // actual display width rather than truncating them at 30% of a split
        // pane. Keep room for direction, citations and a readable neighbor;
        // unusually long relations remain fully available in the inspector.
        let relation_width = records
            .iter()
            .map(|record| Line::from(graph_inline(text(record, "relation"))).width())
            .max()
            .unwrap_or(8)
            .max(8)
            .min(area.width.saturating_sub(29) as usize)
            .max(1) as u16;
        let (header, widths) = if self.graph.browse_nodes {
            (
                vec!["Type", "Typed value (not identity)", "Cites"],
                vec![
                    Constraint::Length(12),
                    Constraint::Min(8),
                    Constraint::Length(5),
                ],
            )
        } else {
            (
                vec!["Dir", "Relation", "Typed neighbor", "Cites"],
                vec![
                    Constraint::Length(5),
                    Constraint::Length(relation_width),
                    Constraint::Min(8),
                    Constraint::Length(5),
                ],
            )
        };
        let mode = if self.graph.browse_nodes {
            "Nodes"
        } else {
            "Edges"
        };
        let filter = if self.filter.is_empty() {
            String::new()
        } else {
            format!(" · /{}", graph_inline(&self.filter))
        };
        let title = format!(
            " {mode} {}{filter} · Left nodes / Right edges ",
            records.len()
        );
        let mut state = TableState::default();
        if !records.is_empty() {
            state.select(Some(
                self.selected[View::Graph.index()].min(records.len() - 1),
            ));
        }
        frame.render_stateful_widget(
            Table::new(rows, widths)
                .header(Row::new(header).style(Style::default().fg(DIM)))
                .block(
                    Block::bordered()
                        .border_style(Style::default().fg(DIM))
                        .title(title),
                )
                .row_highlight_style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("› "),
            panels[1],
            &mut state,
        );
        if records.is_empty() && panels[1].height > 4 {
            frame.render_widget(
                Paragraph::new("No matching records. Left browses nodes; Esc clears search.")
                    .wrap(Wrap { trim: false })
                    .style(Style::default().fg(DIM)),
                Rect {
                    x: panels[1].x + 1,
                    y: panels[1].y + 3,
                    width: panels[1].width.saturating_sub(2),
                    height: panels[1].height.saturating_sub(4),
                },
            );
        }
    }
    fn graph_inspector(&self) -> String {
        let mut output = String::from(
            "Latest 500 reports · source observations only.\nAttributed relations are NOT identity/ownership verification.\n\n",
        );
        if let Some(record) = self.current() {
            if record.get("relation").is_some() {
                let source = self
                    .graph_node(text(&record, "source_id"))
                    .map(graph_node_label)
                    .unwrap_or_default();
                let target = self
                    .graph_node(text(&record, "target_id"))
                    .map(graph_node_label)
                    .unwrap_or_default();
                output.push_str(&format!(
                    "{source}\n  ── {} ──→\n{target}\nStatus: source_observation (not confirmed)\n",
                    graph_inline(text(&record, "relation"))
                ));
                if record["review"].is_object() {
                    output.push_str(&format!("Legacy claim review: {}\nSeparate from trusted finding review; never confirms this edge.\n{}\n",
                        graph_inline(text(&record["review"], "status")), graph_inline(text(&record["review"], "review_note"))));
                }
            } else {
                output.push_str(&format!(
                    "{}\nTyped value, not a verified identity.\n",
                    graph_node_label(&record)
                ));
            }
            output.push_str("\nPROVENANCE · o opens original evidence\n");
            let source_ids = strings(&record["source_ids"]);
            for id in reference_ids(&record) {
                output.push_str(&format!("Evidence: {}\n", graph_inline(&id)));
                if let Some(evidence) = self.data["evidence"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|evidence| text(evidence, "id") == id)
                {
                    for (key, label) in [
                        ("collector_status", "Collector"),
                        ("run_id", "Run"),
                        ("collected_at", "Collected"),
                        ("report_sha256", "Report SHA256"),
                        ("hash_scope", "Hash scope"),
                    ] {
                        if !text(evidence, key).is_empty() {
                            output.push_str(&format!(
                                "{label}: {}\n",
                                graph_inline(text(evidence, key))
                            ));
                        }
                    }
                    for source in array(evidence, "sources").iter().filter(|source| {
                        source_ids.iter().any(|id| id == text(source, "source_id"))
                    }) {
                        for (key, label) in [
                            ("source_id", "Source"),
                            ("url", "URL"),
                            ("sha256", "Source SHA256"),
                            ("hash_scope", "Source hash scope"),
                            ("observed_at", "Source time"),
                        ] {
                            if !text(source, key).is_empty() {
                                output.push_str(&format!(
                                    "{label}: {}\n",
                                    graph_inline(text(source, key))
                                ));
                            }
                        }
                    }
                    output.push_str(
                        "Analyst evidence assessments are separate from collector status.\n",
                    );
                } else {
                    output.push_str(
                        "Evidence outside the current bounded view; inspect the case export.\n",
                    );
                }
                output.push('\n');
            }
            output.push_str("RECORD\n");
            output.push_str(&graph_inline(
                &serde_json::to_string(&record).unwrap_or_default(),
            ));
        } else {
            output.push_str("No matching source-backed records. Left browses all typed nodes, including disconnected nodes. Esc clears search.\n");
        }
        output.push_str("\n\nLIMITATIONS\n");
        for limitation in strings(&self.data["graph"]["limitations"]) {
            output.push_str(&format!("{}\n\n", graph_inline(&limitation)));
        }
        output
    }
    fn table(
        &self,
        frame: &mut Frame,
        area: Rect,
        records: &[Value],
        index: usize,
        title: &str,
        focused: bool,
    ) {
        let rows: Vec<Row> = records
            .iter()
            .map(|record| {
                let status = record_status(record);
                let color = match status.as_str() {
                    "running" => ACCENT,
                    "pending_review" => Color::Yellow,
                    "completed" | "ok" | "accepted" | "corroborated" => Color::Green,
                    "failed" | "error" | "rejected" | "contested" | "blocked" | "unavailable"
                    | "unreachable" | "needs_setup" | "planner_fai" => Color::Yellow,
                    _ => Color::Gray,
                };
                let evidence = reference_ids(record).len();
                Row::new(vec![
                    safe(&status),
                    safe(&record_summary(record)),
                    if evidence == 0 {
                        String::new()
                    } else {
                        format!("{evidence} ev")
                    },
                ])
                .style(Style::default().fg(color))
            })
            .collect();
        let mut state = TableState::default();
        if !records.is_empty() {
            state.select(Some(index.min(records.len() - 1)));
        }
        let border_style = Style::default().fg(if focused { ACCENT } else { DIM });
        let widget = Table::new(
            rows,
            [
                Constraint::Length(if self.view == View::Investigations {
                    14
                } else {
                    11
                }),
                Constraint::Min(10),
                Constraint::Length(5),
            ],
        )
        .header(Row::new(["Status", "Record", "Cites"]).style(Style::default().fg(DIM)))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(format!(" {title} ")),
        )
        .row_highlight_style(
            Style::default()
                .bg(if focused {
                    Color::DarkGray
                } else {
                    Color::Black
                })
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(if focused { "› " } else { "  " });
        frame.render_stateful_widget(widget, area, &mut state);
        if records.is_empty() && area.height > 4 {
            let message = match self.view {
                View::Discovery if !focused => {
                    "Collected public links become evidence-backed follow-up jobs here."
                }
                View::Discovery => "d  New discovery\ng  Run / resume\n?  Investigation guide",
                View::Ontology => {
                    "Collector results build entity types and relationship signatures as discovery progresses. y synchronizes existing evidence."
                }
                View::Claims => {
                    "Captured relationships appear with source pointers, hashes and evidence references."
                }
                View::Correlations => {
                    "Shared handles and explicit shared references create leads. Each lead must retain supporting evidence."
                }
                View::Investigations => {
                    "d saves a question and optional Telegram search locally. Enter selects steps/findings; g runs one step; r reviews a finding. Only accepted findings enter the summary."
                }
                View::Reasoning => {
                    "NVIDIA planner decisions appear here with selected jobs, rationale and source anchors. z checks the provider; d selects planner=nvidia."
                }
                _ => {
                    "No matching records. t runs a collector; e captures evidence. Escape clears filters."
                }
            };
            frame.render_widget(
                Paragraph::new(message)
                    .wrap(Wrap { trim: false })
                    .style(Style::default().fg(DIM)),
                Rect {
                    x: area.x + 2,
                    y: area.y + 3,
                    width: area.width.saturating_sub(4),
                    height: area.height.saturating_sub(4),
                },
            );
        }
    }
    fn draw_inspector(&self, frame: &mut Frame, area: Rect) {
        let text = if self.view == View::Graph {
            self.graph_inspector()
        } else if let Some(record) = self.current() {
            let references = reference_ids(&record);
            let mut prelude = String::new();
            if !references.is_empty() {
                prelude.push_str(&format!(
                    "{} supporting evidence record(s) · o opens citations\n",
                    references.len()
                ));
            }
            if self.view == View::Investigations {
                if self.child_focus && record.get("content").is_some() {
                    prelude.push_str(&format!(
                        "Finding: {} · {}\n{}\n",
                        text(&record, "status"),
                        text(&record, "kind"),
                        safe(text(&record, "content")),
                    ));
                    prelude.push_str("Analytical proposal, not source evidence.\nr opens local human review; pending/rejected findings are excluded from the summary. Identity-sensitive claims require explicit human judgment.\n");
                } else if self.child_focus {
                    prelude.push_str("Collection step · g runs this one bounded call. No automatic follow-ups or finding acceptance. Inspect run_id, options and errors below.\n");
                } else {
                    prelude.push_str(&format!(
                        "Question: {}\n\nAccepted findings only\n",
                        safe(text(&record, "question"))
                    ));
                    let summary = array(&record, "summary");
                    if summary.is_empty() {
                        prelude.push_str("None accepted. Pending proposals are not conclusions.\n");
                    }
                    for finding in summary {
                        prelude.push_str(&format!("- {}\n", safe(text(&finding, "content"))));
                    }
                    prelude.push_str(&format!("\nCoverage: {}\nEnter selects steps/findings; g runs a step; r reviews a finding.\n", safe(&record["coverage"].as_str().map(str::to_owned).unwrap_or_else(|| record["coverage"].to_string()))));
                }
            }
            if self.view == View::Correlations {
                prelude.push_str(
                    "Lead only. A shared signal does not establish identity or ownership.\n",
                );
            }
            if self.view == View::Evidence {
                prelude.push_str("Original capture + separate analyst assessment. Check hash_scope and collector_status.\n");
            }
            if self.view == View::Ontology {
                prelude.push_str("Schema vocabulary learned from recorded observations. Individual claims remain separately cited.\n");
            }
            if self.view == View::Reasoning {
                prelude.push_str(&format!(
                    "Provider: {} · status: {}\nDecision: {}\n",
                    text(&record, "provider"),
                    text(&record, "status"),
                    record["decision"]
                        .as_str()
                        .or_else(|| record["decision"]["action"].as_str())
                        .unwrap_or("No collector selected")
                ));
                let job_id = text(&record, "selected_job_id");
                if !job_id.is_empty() {
                    if let Some(job) = array(&self.data, "frontier")
                        .iter()
                        .find(|job| text(job, "id") == job_id)
                    {
                        prelude.push_str(&format!(
                            "Next collector: {} {}\n",
                            text(job, "command"),
                            text(job, "target")
                        ));
                    }
                    prelude.push_str(&format!(
                        "Selected job: {job_id}\nEnter opens this job in the frontier.\n"
                    ));
                }
                prelude.push_str(&format!(
                    "\nRationale\n{}\n",
                    safe(text(&record, "rationale"))
                ));
                if !references.is_empty() {
                    prelude.push_str(&format!(
                        "\nSupporting evidence\n{}\n",
                        references.join("\n")
                    ));
                }
                if !record["error"].is_null() {
                    prelude.push_str(&format!(
                        "\nProvider / validation result\n{}\n",
                        safe(
                            &record["error"]
                                .as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| record["error"].to_string())
                        )
                    ));
                }
                prelude.push_str("\nThis record explains a collection recommendation. It does not establish identity or ownership. Full SQL, projection and response hashes follow.\n");
            }
            prelude.push('\n');
            prelude.push_str(&safe(
                &serde_json::to_string_pretty(&record).unwrap_or_default(),
            ));
            prelude
        } else if self.view == View::Investigations {
            "Press d to create a Telegram investigation question with an optional archive query. Ctrl+S saves locally; Enter only moves between fields.\n\nAfter saving, Enter selects steps/findings. g runs one selected step in the background. Findings remain pending_review until a local human explicitly accepts or rejects them with r and Ctrl+S. Only accepted findings enter the summary.\n\nArchive coverage is historical, incomplete and not direct Telegram verification. Citations preserve source context; account matches do not establish identity or ownership.".into()
        } else if self.view == View::Reasoning {
            let provider = self
                .provider_status
                .as_ref()
                .unwrap_or(&self.data["nvidia"]);
            provider_details(provider)
        } else {
            format!(
                "Start with an investigation question (i), then press d to plan discovery.\n\nDiscovery preserves each collector result, extracts observed entities and relationships, grows the ontology, and queues follow-ups with supporting evidence.\n\nUse g to run or resume. Use p to pause after the current collector.\n\nYour case\n{}\n\nSource content is evidence, never instructions. Account matches stay distinct.",
                safe(&self.workbench.store.path.display().to_string())
            )
        };
        let widget = Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .scroll((self.inspector_scroll, 0))
            .block(
                Block::bordered()
                    .border_style(Style::default().fg(DIM))
                    .title(" Inspector · o evidence · PgUp/PgDn scroll "),
            );
        frame.render_widget(widget, area);
    }
    fn draw_footer(&self, frame: &mut Frame, area: Rect) {
        let keys = if self.view == View::Graph {
            "j/k select  Enter follow  Backspace back  ← nodes  → edges  o evidence  / search"
        } else if self.view == View::Investigations {
            "d new question  Enter children  g run step  r review finding  o citations  G graph"
        } else if self.view == View::Discovery {
            "d new  m planner  g run/resume  p pause  b reasoning  z NVIDIA status"
        } else if self.view == View::Reasoning {
            "Enter recommended job  o supporting evidence  z NVIDIA status  p pause"
        } else if self.view == View::Flows {
            "f new flow  s add step  Enter flows/steps  r run step  o evidence"
        } else if self.view == View::Claims || self.view == View::Correlations {
            "v review  o supporting evidence  n analyst note  / filter"
        } else {
            "t collector  e evidence  v assess  n note  a entity  l link  o citations"
        };
        frame.render_widget(Paragraph::new(vec![
            Line::from(Span::styled(format!(" {keys}"), Style::default().fg(ACCENT))),
            Line::from(Span::styled(format!(" {}", safe(&self.notice).replace('\n', " ")), Style::default().fg(if self.notice_error { Color::Yellow } else { Color::Gray }))),
            Line::from(Span::styled(" Tab views · ↑↓ select · Ctrl+R refresh · i question · x export · ? help · q quit", Style::default().fg(DIM))),
        ]), area);
    }
    fn draw_form(&self, frame: &mut Frame, form: &Form) {
        let height = (form.fields.len() as u16 * 3 + 7).min(frame.area().height.saturating_sub(2));
        let area = popup(frame.area(), 90, height);
        frame.render_widget(Clear, area);
        frame.render_widget(
            Block::bordered()
                .border_style(Style::default().fg(ACCENT))
                .title(format!(" {} ", form.title)),
            area,
        );
        let inner = Rect {
            x: area.x + 2,
            y: area.y + 1,
            width: area.width.saturating_sub(4),
            height: area.height.saturating_sub(2),
        };
        frame.render_widget(
            Paragraph::new(format!(
                "Field {}/{} · Tab/Enter next · Shift+Tab back · Ctrl+S save · Esc cancel",
                form.focus + 1,
                form.fields.len(),
            ))
            .style(Style::default().fg(ACCENT)),
            Rect { height: 1, ..inner },
        );
        let available = inner.height.saturating_sub(4) as usize / 3;
        let start = form.focus.saturating_sub(available.saturating_sub(1));
        for (offset, (index, field)) in form
            .fields
            .iter()
            .enumerate()
            .skip(start)
            .take(available)
            .enumerate()
        {
            let y = inner.y + 2 + offset as u16 * 3;
            let active = index == form.focus;
            let label = format!("{}{}", if active { "› " } else { "  " }, field.label);
            frame.render_widget(
                Paragraph::new(label).style(Style::default().fg(if active {
                    ACCENT
                } else {
                    Color::Gray
                })),
                Rect {
                    y,
                    height: 1,
                    ..inner
                },
            );
            let width = inner.width.saturating_sub(2) as usize;
            let start = field.cursor.saturating_sub(width.saturating_sub(1));
            let visible: String = field.value.chars().skip(start).take(width).collect();
            frame.render_widget(
                Paragraph::new(format!("  {}", safe(&visible))).style(
                    Style::default()
                        .bg(if active {
                            Color::DarkGray
                        } else {
                            Color::Black
                        })
                        .fg(Color::White),
                ),
                Rect {
                    y: y + 1,
                    height: 1,
                    ..inner
                },
            );
            if active {
                frame.set_cursor_position((
                    inner.x + 2 + (field.cursor - start).min(width.saturating_sub(1)) as u16,
                    y + 1,
                ));
            }
        }
        let hint = form
            .fields
            .get(form.focus)
            .map(|f| f.hint.as_str())
            .unwrap_or("");
        let footer = if form.error.is_empty() {
            hint
        } else {
            &form.error
        };
        frame.render_widget(
            Paragraph::new(safe(footer))
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(if form.error.is_empty() {
                    DIM
                } else {
                    Color::Yellow
                })),
            Rect {
                x: inner.x,
                y: area.y + area.height.saturating_sub(3),
                width: inner.width,
                height: 2,
            },
        );
    }
    fn draw_help(&self, frame: &mut Frame, scroll: u16) {
        let area = popup(frame.area(), 94, frame.area().height.saturating_sub(2));
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(HELP)
                .wrap(Wrap { trim: false })
                .scroll((scroll, 0))
                .block(
                    Block::bordered()
                        .border_style(Style::default().fg(ACCENT))
                        .title(" Fieldkit investigation guide · ↑↓ scroll · Esc close "),
                ),
            area,
        );
    }
    fn draw_evidence_picker(&self, frame: &mut Frame, rows: &[Value], selected: usize) {
        let area = popup(
            frame.area(),
            85,
            (rows.len() as u16 + 5).min(frame.area().height.saturating_sub(2)),
        );
        frame.render_widget(Clear, area);
        let items: Vec<ListItem> = rows
            .iter()
            .map(|row| {
                ListItem::new(format!(
                    "{}\n{}",
                    safe(text(row, "title")),
                    safe(text(row, "id"))
                ))
            })
            .collect();
        let mut state = ListState::default().with_selected(Some(selected));
        frame.render_stateful_widget(
            List::new(items)
                .block(
                    Block::bordered()
                        .title(" Supporting evidence · ↑↓ choose · Enter open · Esc cancel "),
                )
                .highlight_style(Style::default().bg(Color::DarkGray).fg(ACCENT))
                .highlight_symbol("› "),
            area,
            &mut state,
        );
    }
}

fn popup(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2));
    let height = height.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}
fn record_status(record: &Value) -> String {
    for key in ["status", "collector_status", "kind", "type"] {
        if let Some(value) = record[key].as_str().filter(|v| !v.is_empty()) {
            return if value == "pending_review" {
                value.into()
            } else {
                value.chars().take(11).collect()
            };
        }
    }
    if record.get("relation").is_some() {
        "relation".into()
    } else {
        "record".into()
    }
}
fn record_summary(record: &Value) -> String {
    for key in [
        "title",
        "label",
        "reason",
        "rationale",
        "definition",
        "name",
        "content",
        "seed",
    ] {
        if let Some(value) = record[key].as_str().filter(|v| !v.is_empty()) {
            return value.replace('\n', " ");
        }
    }
    if record["command"].is_string() {
        return format!("{} {}", text(record, "command"), text(record, "target"));
    }
    if record["relation"].is_string() {
        return format!(
            "{} → {}",
            text(record, "relation"),
            text(record, "target_type")
        );
    }
    if record["value"].is_string() {
        return text(record, "value").into();
    }
    if record["type"].is_string() {
        return text(record, "type").into();
    }
    text(record, "id").into()
}

const HELP: &str = "\
TELEGRAM INVESTIGATIONS\n\
w  Open Investigations. d creates a question with an optional archive query, since/until dates and result limit. No model or provider selection is required. Ctrl+S saves locally without network access; Enter only changes fields. Blank query saves a question without collection steps.\n\
Enter switches between investigations and their steps, followed by findings. g runs only the selected step in the background, never the whole plan. Archive results are historical third-party observations, not complete/live coverage or direct Telegram verification.\n\
Findings remain pending_review until a human selects a finding, presses r, explicitly enters accepted/rejected and a rationale, then saves with Ctrl+S. Only accepted findings enter the summary. Review is local and never rewrites captured source evidence. Identity-sensitive conclusions require human judgment. o opens preserved citations through the Evidence view.\n\n\
RELATIONSHIP GRAPH\n\
G (uppercase) opens Graph from any view, including Investigations. The focused typed node and selected one-hop incoming/outgoing observation are shown with direct labels and a keyboard-selectable table. Up/Down or j/k select; Enter follows the selected neighbor; Backspace returns to the previous focus. Left browses all source-backed nodes, including disconnected nodes; Enter focuses one; Right returns to edges. / searches typed neighbors or relations; Esc clears search. o opens original evidence; PgUp/PgDn scroll provenance.\n\
Coverage is the latest 500 reports, not complete history. Source relations are attributed observations, never verified ownership or identity. Matching handles, names or domains do not merge typed IDs. Manual/model assessments, analyst assertions, correlation leads and findings (even accepted findings) do not create edges. Legacy claim reviews remain separate from trusted finding review and never confirm an edge. New agent focus is checked while idle and never dismisses an editor, picker or active worker.\n\n\
DISCOVERY AND ONTOLOGY\n\
1. Press i to set the investigation question.\n\
2. Press d. Choose username, profile, url, domain or search; enter a seed, job budget and expansion depth. Planner=local follows observed public references. Planner=nvidia asks NVIDIA Auto Ontology to reason over preserved evidence and choose next steps. Ctrl+S saves the plan.\n\
3. Press g to run or resume. Collection runs in a background thread. The persistent frontier records each job, its reason, evidence references, result and depth.\n\
4. Press p to pause after the current collector. q saves the current result and closes; g resumes the remaining queue next time.\n\
5. Ontology shows the entity types and relationship signatures learned from observations. Claims retain the captured relationship, evidence IDs, source references, hashes and structured source pointers. y synchronizes old case evidence.\n\
6. Select a claim, derived job or correlation and press o to inspect supporting evidence. PgUp/PgDn scroll the inspector.\n\
7. In Claims or Correlations, press v to accept/reject with a rationale. Correlations are working leads; shared usernames or URLs do not automatically merge identities or prove ownership.\n\n\
NVIDIA AUTO ONTOLOGY\n\
Follow integrations/auto-ontology/README.md to configure the real service and its case projection. Provider credentials belong in the documented environment or configuration file, never a case note or form.\n\
z  Check server connection and compiled ontology readiness in a background worker.\n\
b  Open Reasoning. Each decision records its provider, selected job, rationale, evidence references, source anchors, SQL and request/response hashes. o opens the supporting evidence; Enter opens the selected job in the frontier.\n\
m  Change the selected discovery's planner or increase its total reasoning-call budget. Existing collector results and reasoning usage are preserved. g resumes its queue.\n\
The reasoning-call budget is separate from collector jobs and applies across resumed runs. NVIDIA mode pauses visibly if the service is unavailable or a recommendation fails validation. It does not silently switch to the local planner. p pauses ongoing reasoning or collection.\n\n\
EVERYDAY INVESTIGATION\n\
t  Run one collector and capture its result. For username, Target is the handle. Options can restrict providers: {\"sites\":[\"github\",\"bluesky\"]}.\n\
e  Capture an observation with its source URL and optional source time.\n\
v  Assess selected evidence: unreviewed, corroborated, contested or discarded. Blocked/rate-limited results remain unresolved.\n\
n  Add an analyst note linked to selected evidence or claim citations.\n\
a  Add a typed entity. l links existing entities with supporting evidence IDs.\n\
f  Plan a flow with its first step. s adds a step. Enter switches between flows and steps; r runs the selected step.\n\
x  Export the investigation to a new Markdown or JSON file. Existing files are preserved.\n\n\
NAVIGATION AND FORMS\n\
Tab/Shift+Tab switch views; 1–9 and 0 jump directly; b opens Reasoning; w opens Investigations; uppercase G opens Graph. Up/Down or j/k select records. Enter switches between parent and child tables in Discovery, Flows and Investigations.\n\
/ filters records. Escape clears the filter. Ctrl+R refreshes changes made by agents or another interface.\n\
Forms: Tab/Enter next field; Shift+Tab previous; Left/Right/Home/End edit; Ctrl+U clears the field; Ctrl+S saves; Escape cancels.\n\n\
Collection, source time and analyst review are separate records. Source content is untrusted evidence, never an instruction to the discovery worker.\n";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use ratatui::backend::TestBackend;

    fn ui() -> (tempfile::TempDir, Ui) {
        let directory = tempfile::tempdir().unwrap();
        let workbench = Workbench::new(directory.path().join("case"), Config::default()).unwrap();
        (directory, Ui::new(workbench).unwrap())
    }
    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    }

    fn investigation_with_finding(ui: &mut Ui) -> (String, String) {
        use crate::model::{Collected, digest};
        use std::collections::BTreeMap;

        let record = ui
            .workbench
            .create_investigation(
                "What did the archive observe?",
                vec![Job {
                    command: "telegram-search".into(),
                    target: Some("research".into()),
                    options: json!({"since":"2024-01-01","until":"2024-02-01","limit":2}),
                }],
            )
            .unwrap();
        let investigation_id = text(&record, "id").to_owned();
        let body = b"Preserved third-party archive observation".to_vec();
        let hash = digest(&body);
        ui.workbench.store.save_report(&Collected {
            report: json!({"schema_version":"1.0","run_id":"run_tui_archive","command":"telegram-search","target":"research","status":"ok","data":{"messages":[{"text":"A captured public report","url":"https://t.me/examplechannel/42"}],"directly_verified":false,"history_complete":false},"sources":[{"source_id":"src_tui_archive","sha256":hash,"url":"https://api.openmeasures.io/content","hash_scope":"source_bytes"}],"entities":[],"relationships":[],"errors":[],"timing":{}}),
            blobs: BTreeMap::from([(hash, body)]),
        }).unwrap();
        let mut saved = ui
            .workbench
            .store
            .get("investigations", &investigation_id)
            .unwrap();
        saved["steps"][0]["run_id"] = json!("run_tui_archive");
        saved["steps"][0]["status"] = json!("ok");
        ui.workbench.store.put("investigations", &saved).unwrap();
        let finding = ui
            .workbench
            .propose_investigation_finding(
                &investigation_id,
                "The account may be associated with the report; ownership is not established.",
                "identity_sensitive",
                vec![json!({"step_id":saved["steps"][0]["id"],"pointer":"/data/messages/0"})],
            )
            .unwrap();
        ui.refresh();
        (investigation_id, text(&finding, "id").to_owned())
    }

    #[test]
    fn unicode_editing_uses_character_boundaries() {
        let mut field = Field::new("name", "é猫", "");
        field.edit(press(KeyCode::Left));
        field.edit(press(KeyCode::Backspace));
        assert_eq!(field.value, "猫");
        assert_eq!(field.cursor, 0);
        field.insert('界');
        assert_eq!(field.value, "界猫");
        field.edit(press(KeyCode::Delete));
        assert_eq!(field.value, "界");
    }
    #[test]
    fn terminal_control_sequences_never_reach_widgets() {
        assert_eq!(safe("a\x1b[2J\x00b\nline"), "a[2Jb\nline");
    }
    #[test]
    fn discovery_form_checks_budget_and_boolean_before_execution() {
        let (_directory, mut ui) = ui();
        ui.discovery_form();
        let Some(Modal::Form(mut form)) = ui.modal.take() else {
            panic!("form missing")
        };
        form.fields[1].value = "public_example".into();
        form.fields[2].value = "0".into();
        assert!(
            discovery_options(&form)
                .unwrap_err()
                .to_string()
                .contains("budget")
        );
        form.fields[2].value = "8".into();
        form.fields[6].value = "sometimes".into();
        assert!(discovery_options(&form).is_err());
        form.fields[6].value = "yes".into();
        let options = discovery_options(&form).unwrap();
        assert_eq!(options.seed, "public_example");
        assert!(options.web_search);
    }
    #[test]
    fn discovery_is_initial_and_keyboard_navigation_wraps() {
        let (_directory, mut ui) = ui();
        assert_eq!(ui.view, View::Discovery);
        ui.key(press(KeyCode::BackTab));
        assert_eq!(ui.view, View::Graph);
        ui.key(press(KeyCode::Tab));
        assert_eq!(ui.view, View::Discovery);
        ui.key(press(KeyCode::Char('4')));
        assert_eq!(ui.view, View::Evidence);
        ui.key(press(KeyCode::Char('0')));
        assert_eq!(ui.view, View::Activity);
        ui.key(press(KeyCode::Char('b')));
        assert_eq!(ui.view, View::Reasoning);
    }
    #[test]
    fn forms_require_explicit_save_and_cancel_without_mutation() {
        let (_directory, mut ui) = ui();
        ui.key(press(KeyCode::Char('d')));
        ui.key(press(KeyCode::Enter));
        let Some(Modal::Form(form)) = &ui.modal else {
            panic!("expected form")
        };
        assert_eq!(form.focus, 1);
        ui.key(press(KeyCode::Esc));
        assert!(ui.modal.is_none());
        assert!(ui.workbench.discoveries().unwrap().is_empty());
    }
    #[test]
    fn saving_discovery_form_creates_a_resumable_persistent_queue() {
        let (directory, mut ui) = ui();
        ui.key(press(KeyCode::Char('d')));
        let Some(Modal::Form(form)) = &mut ui.modal else {
            panic!("discovery form")
        };
        form.fields[1].value = "public_example".into();
        form.fields[2].value = "6".into();
        form.fields[5].value = "github,bluesky".into();
        ui.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(ui.modal.is_none());
        let session = ui
            .selected_session()
            .expect("selected newly planned session");
        assert_eq!(session["seed"], "public_example");
        let jobs = ui.children();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0]["command"], "username");
        assert_eq!(jobs[0]["options"]["sites"], json!(["github", "bluesky"]));
        let reopened = Workbench::new(directory.path().join("case"), Config::default()).unwrap();
        assert_eq!(
            reopened.discovery_jobs(text(&session, "id")).unwrap()[0]["status"],
            "pending"
        );
        assert!(
            reopened.store.reports(10).unwrap().is_empty(),
            "planning must not run collection"
        );
    }
    #[test]
    fn nvidia_planner_and_budget_are_explicit_discovery_settings() {
        let (_directory, mut ui) = ui();
        ui.discovery_form();
        let Some(Modal::Form(mut form)) = ui.modal.take() else {
            panic!("discovery form")
        };
        form.fields[1].value = "public_example".into();
        form.fields[7].value = "nvidia".into();
        form.fields[8].value = "7".into();
        let options = discovery_options(&form).unwrap();
        assert_eq!(options.planner, "nvidia");
        assert_eq!(options.reasoning_budget, 7);
        form.fields[8].value = "21".into();
        assert!(discovery_options(&form).is_err());
        form.fields[8].value = "5".into();
        form.fields[7].value = "anything".into();
        assert!(discovery_options(&form).is_err());
    }
    #[test]
    fn existing_queue_can_switch_planner_without_duplicate_collector_jobs() {
        let (_directory, mut ui) = ui();
        ui.discovery_form();
        let Some(Modal::Form(form)) = &mut ui.modal else {
            panic!("discovery form")
        };
        form.fields[1].value = "public_example".into();
        ui.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        let before = ui.children();
        ui.key(press(KeyCode::Char('m')));
        let Some(Modal::Form(form)) = &mut ui.modal else {
            panic!("planner form")
        };
        form.fields[0].value = "nvidia".into();
        form.fields[1].value = "8".into();
        ui.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(ui.modal.is_none(), "planner update should save");
        let session = ui.selected_session().unwrap();
        assert_eq!(session["options"]["planner"], "nvidia");
        assert_eq!(session["options"]["reasoning_budget"], 8);
        assert_eq!(session["reasoning_used"].as_u64().unwrap_or(0), 0);
        assert_eq!(ui.children(), before);
    }
    #[test]
    fn reasoning_recommendation_citations_open_original_evidence() {
        let (_directory, mut ui) = ui();
        ui.data["evidence"] =
            json!([{"id":"ev_reasoning","title":"Captured public profile","status":"unreviewed"}]);
        ui.data["reasoning"] = json!([{"id":"decision_1","provider":"nvidia_auto_ontology","status":"accepted","decision":"collect","rationale":"Inspect the profile's explicitly linked project site","evidence_ids":["ev_reasoning"]}]);
        ui.key(press(KeyCode::Char('b')));
        assert_eq!(ui.view, View::Reasoning);
        ui.key(press(KeyCode::Char('o')));
        assert_eq!(ui.view, View::Evidence);
        assert_eq!(text(&ui.current().unwrap(), "id"), "ev_reasoning");
    }
    #[test]
    fn reasoning_recommendation_opens_selected_job_in_its_session() {
        let (_directory, mut ui) = ui();
        ui.data["discovery"] = json!([{"id":"discovery_1","seed":"public_example","options":{"planner":"nvidia","reasoning_budget":5},"reasoning_used":2}]);
        ui.data["frontier"] = json!([{"id":"job_1","discovery_id":"discovery_1","command":"http","target":"https://example.org/","status":"pending","evidence_ids":["ev_source"]}]);
        ui.data["reasoning"] = json!([{"id":"decision_1","discovery_id":"discovery_1","provider":"nvidia_auto_ontology","status":"accepted","selected_job_id":"job_1","rationale":"Follow the captured explicit website link","evidence_ids":["ev_source"]}]);
        ui.key(press(KeyCode::Char('b')));
        assert!(ui.planner_summary().contains("2/5"));
        ui.key(press(KeyCode::Enter));
        assert_eq!(ui.view, View::Discovery);
        assert!(ui.child_focus);
        assert_eq!(text(&ui.current().unwrap(), "id"), "job_1");
    }
    #[test]
    fn unavailable_provider_is_visible_and_never_looks_like_success() {
        let (message, warning) = provider_notice(
            &json!({"provider":"nvidia_auto_ontology","configured":false,"ready":false,"status":"unconfigured","message":"Configure the Auto Ontology endpoint"}),
        );
        assert!(warning);
        assert!(message.contains("unconfigured"));
        assert!(message.contains("integrations/auto-ontology/README.md"));
        let (message, warning) = completion_notice(
            "discovery",
            &json!({"status":"needs_setup","stop_reason":"NVIDIA server is unavailable"}),
        );
        assert!(warning);
        assert!(message.contains("no local fallback"));
        assert!(!message.contains("observations preserved"));
        let (_, warning) = provider_notice(
            &json!({"ready":true,"status":"ready","message":"Compiled ontology is available"}),
        );
        assert!(!warning);
    }
    #[test]
    fn reasoning_view_renders_small_and_filters_status_credentials() {
        let (_directory, mut ui) = ui();
        ui.data["nvidia"] = json!({"provider":"nvidia_auto_ontology","configured":false,"status":"unconfigured","message":"Configure the real provider","token":"secret_not_for_display"});
        ui.key(press(KeyCode::Char('b')));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| ui.draw(frame)).unwrap();
        let output = buffer_text(&terminal);
        assert!(output.contains("Reasoning"));
        assert!(output.contains("unconfigured"));
        assert!(!output.contains("secret_not_for_display"));
        ui.data["reasoning"] = json!([{"id":"decision_1","provider":"NVIDIA","status":"unavailable","decision":"stop","rationale":"Server could not be reached","evidence_ids":[],"error":"Connection unavailable"}]);
        terminal.draw(|frame| ui.draw(frame)).unwrap();
        let output = buffer_text(&terminal);
        assert!(output.contains("unavailable"));
        assert!(output.contains("Reasoning"));
    }
    #[test]
    fn claim_citations_open_the_original_evidence_record() {
        let (_directory, mut ui) = ui();
        ui.data["evidence"] =
            json!([{"id":"ev_a","title":"Original account capture","status":"unreviewed"}]);
        ui.data["ontology"]["claims"] = json!([{"id":"claim_a","evidence_ids":["ev_a"]}]);
        ui.change_view(View::Claims.index());
        ui.key(press(KeyCode::Char('o')));
        assert_eq!(ui.view, View::Evidence);
        assert_eq!(text(&ui.current().unwrap(), "id"), "ev_a");
    }
    #[test]
    fn multiple_citations_offer_a_picker() {
        let (_directory, mut ui) = ui();
        ui.data["evidence"] = json!([{"id":"ev_a","title":"A"},{"id":"ev_b","title":"B"}]);
        ui.data["correlations"] = json!([{"id":"lead","evidence_ids":["ev_a","ev_b"]}]);
        ui.change_view(View::Correlations.index());
        ui.open_evidence();
        assert!(matches!(ui.modal, Some(Modal::EvidencePicker { .. })));
        ui.key(press(KeyCode::Down));
        ui.key(press(KeyCode::Enter));
        assert_eq!(text(&ui.current().unwrap(), "id"), "ev_b");
    }
    #[test]
    fn discovery_and_form_render_at_supported_terminal_sizes() {
        let (_directory, mut ui) = ui();
        for (width, height) in [(120, 36), (100, 30), (80, 24)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| ui.draw(frame)).unwrap();
            assert!(buffer_text(&terminal).contains("FIELDKIT"));
            assert!(buffer_text(&terminal).contains("Discovery"));
            ui.discovery_form();
            terminal.draw(|frame| ui.draw(frame)).unwrap();
            assert!(buffer_text(&terminal).contains("Ctrl+S save"));
            ui.modal = None;
        }
    }
    #[test]
    fn investigation_shortcut_is_unique_and_preserves_numbered_views() {
        let (_directory, mut ui) = ui();
        let shortcuts = (0..VIEWS.len())
            .map(|index| View::from_index(index).shortcut())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(shortcuts.len(), VIEWS.len());
        ui.key(press(KeyCode::Char('w')));
        assert_eq!(ui.view, View::Investigations);
        assert_eq!(
            ui.planner_summary(),
            "Shared investigation · human-reviewed findings"
        );
        for (key, expected) in [
            ('1', View::Discovery),
            ('7', View::Flows),
            ('0', View::Activity),
            ('b', View::Reasoning),
        ] {
            ui.key(press(KeyCode::Char(key)));
            assert_eq!(ui.view, expected);
        }
    }
    #[test]
    fn investigation_creation_requires_explicit_save_and_never_starts_collection() {
        let (directory, mut ui) = ui();
        ui.key(press(KeyCode::Char('w')));
        ui.key(press(KeyCode::Char('d')));
        let Some(Modal::Form(form)) = &mut ui.modal else {
            panic!("investigation form")
        };
        assert_eq!(form.fields.len(), 5, "no model/provider selector");
        form.fields[0].value = "Which reports were archived?".into();
        form.fields[1].value = "public research".into();
        form.fields[2].value = "2024-01-01".into();
        form.fields[3].value = "2024-02-01".into();
        form.fields[4].value = "3".into();
        ui.key(press(KeyCode::Enter));
        assert!(ui.workbench.investigations().unwrap().is_empty());
        ui.key(press(KeyCode::Esc));
        assert!(ui.workbench.investigations().unwrap().is_empty());
        ui.key(press(KeyCode::Char('d')));
        let Some(Modal::Form(form)) = &mut ui.modal else {
            panic!("investigation form")
        };
        form.fields[0].value = "Which reports were archived?".into();
        form.fields[1].value = "public research".into();
        form.fields[2].value = "2024-01-01".into();
        form.fields[3].value = "2024-02-01".into();
        form.fields[4].value = "3".into();
        ui.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(ui.modal.is_none());
        assert!(ui.worker.is_none());
        let record = ui.selected_investigation().unwrap();
        assert_eq!(record["question"], "Which reports were archived?");
        let children = ui.children();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0]["command"], "telegram-search");
        assert_eq!(children[0]["status"], "pending");
        assert!(children[0]["run_id"].is_null());
        assert_eq!(
            children[0]["options"],
            json!({"since":"2024-01-01","until":"2024-02-01","limit":3})
        );
        let reopened = Workbench::new(directory.path().join("case"), Config::default()).unwrap();
        assert_eq!(reopened.investigations().unwrap().len(), 1);
        assert!(reopened.store.reports(10).unwrap().is_empty());
    }
    #[test]
    fn investigation_question_only_and_search_validation_are_local() {
        let (_directory, mut ui) = ui();
        ui.investigation_form();
        let Some(Modal::Form(mut form)) = ui.modal.take() else {
            panic!("investigation form")
        };
        assert!(ui.submit(&form).is_err(), "question is required");
        form.fields[0].value = "What evidence is still missing?".into();
        ui.submit(&form).unwrap();
        assert!(ui.children().is_empty());
        form.fields[1].value = "research".into();
        form.fields[4].value = "101".into();
        assert!(investigation_searches(&form).is_err());
        form.fields[4].value = "2".into();
        form.fields[2].value = "not-a-date".into();
        assert!(investigation_searches(&form).is_err());
        form.fields[2].value = "2024-02-01".into();
        form.fields[3].value = "2024-01-01".into();
        assert!(investigation_searches(&form).is_err());
        assert!(ui.worker.is_none());
        assert!(ui.workbench.store.reports(10).unwrap().is_empty());
    }
    #[test]
    fn investigation_step_dispatch_is_bounded_and_off_the_ui_thread() {
        let (_directory, mut ui) = ui();
        investigation_with_finding(&mut ui);
        ui.key(press(KeyCode::Char('w')));
        ui.key(press(KeyCode::Char('g')));
        assert!(
            ui.worker.is_none(),
            "parent selection cannot start collection"
        );
        ui.key(press(KeyCode::Enter));
        // This preserved step was already attempted, so the bounded backend call
        // fails locally without network access or automatically retrying it.
        ui.key(press(KeyCode::Char('g')));
        assert!(
            ui.worker
                .as_ref()
                .is_some_and(|worker| worker.kind == WorkerKind::InvestigationStep)
        );
        ui.key(press(KeyCode::Char('p')));
        assert!(!ui.worker.as_ref().unwrap().stop.load(Ordering::Relaxed));
        let worker = ui.worker.take().unwrap();
        let result = worker
            .receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        worker.handle.join().unwrap();
        assert!(matches!(result, WorkerEvent::Done(Err(_))));
        assert_eq!(ui.workbench.store.reports(10).unwrap().len(), 1);
    }
    #[test]
    fn investigation_findings_require_explicit_local_human_review() {
        let (_directory, mut ui) = ui();
        let (id, finding_id) = investigation_with_finding(&mut ui);
        let original = ui
            .workbench
            .store
            .get("investigation_findings", &finding_id)
            .unwrap();
        ui.key(press(KeyCode::Char('w')));
        ui.key(press(KeyCode::Enter));
        ui.key(press(KeyCode::Char('r')));
        assert!(ui.modal.is_none(), "steps cannot be reviewed as findings");
        ui.key(press(KeyCode::Down));
        ui.key(press(KeyCode::Char('g')));
        assert!(ui.worker.is_none(), "findings must not trigger collection");
        assert_eq!(ui.current().unwrap()["status"], "pending_review");
        ui.key(press(KeyCode::Char('r')));
        let Some(Modal::Form(form)) = &ui.modal else {
            panic!("human review form")
        };
        assert!(
            form.value(0).is_empty(),
            "acceptance must not be preselected"
        );
        ui.key(press(KeyCode::Enter));
        ui.key(press(KeyCode::Esc));
        assert!(array(&ui.workbench.investigation(&id).unwrap(), "summary").is_empty());
        ui.key(press(KeyCode::Char('r')));
        let Some(Modal::Form(form)) = &mut ui.modal else {
            panic!("human review form")
        };
        form.fields[0].value = "accepted".into();
        ui.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        let Some(Modal::Form(form)) = &mut ui.modal else {
            panic!("rationale is required")
        };
        assert!(form.error.contains("rationale"));
        form.fields[1].value =
            "Accept only as a cited hypothesis; this archive does not establish ownership.".into();
        ui.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(ui.modal.is_none());
        let record = ui.workbench.investigation(&id).unwrap();
        assert_eq!(record["summary"][0]["id"], finding_id);
        assert_eq!(record["findings"][0]["status"], "accepted");
        assert_eq!(
            record["findings"][0]["review"]["authority"],
            "local_human_review"
        );
        assert!(ui.worker.is_none());
        ui.key(press(KeyCode::Char('r')));
        let Some(Modal::Form(form)) = &mut ui.modal else {
            panic!("human review form")
        };
        form.fields[0].value = "rejected".into();
        form.fields[1].value = "The preserved report does not support this association.".into();
        ui.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(ui.modal.is_none());
        let record = ui.workbench.investigation(&id).unwrap();
        assert!(array(&record, "summary").is_empty());
        assert_eq!(record["findings"][0]["status"], "rejected");
        assert_eq!(
            ui.workbench
                .store
                .get("investigation_findings", &finding_id)
                .unwrap(),
            original,
            "reviews preserve the original proposal and citations"
        );
    }
    #[test]
    fn investigation_pending_findings_render_and_citations_open_evidence() {
        let (_directory, mut ui) = ui();
        investigation_with_finding(&mut ui);
        ui.key(press(KeyCode::Char('w')));
        let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
        terminal.draw(|frame| ui.draw(frame)).unwrap();
        assert!(buffer_text(&terminal).contains("None accepted"));
        ui.key(press(KeyCode::Enter));
        ui.key(press(KeyCode::Down));
        terminal.draw(|frame| ui.draw(frame)).unwrap();
        let output = buffer_text(&terminal);
        assert!(output.contains("pending_review"));
        assert!(output.contains("identity_sensitive"));
        assert!(output.contains("human review"));
        for (width, height) in [(80, 24), (55, 15)] {
            let mut small = Terminal::new(TestBackend::new(width, height)).unwrap();
            small.draw(|frame| ui.draw(frame)).unwrap();
            assert!(buffer_text(&small).contains("w:Investigations"));
        }
        assert_eq!(
            reference_ids(&ui.current().unwrap()),
            vec!["evidence_run_tui_archive"]
        );
        ui.key(press(KeyCode::Char('o')));
        assert_eq!(ui.view, View::Evidence);
        assert_eq!(
            text(&ui.current().unwrap(), "id"),
            "evidence_run_tui_archive"
        );
    }
    #[test]
    fn investigation_citation_only_references_use_the_existing_picker() {
        let (_directory, mut ui) = ui();
        ui.data["investigations"] = json!([{"id":"inv","question":"Question","steps":[],"findings":[{"id":"finding","content":"Pending claim","status":"pending_review","citations":[{"evidence_id":"ev_b"},{"evidence_id":"ev_a"}]}],"summary":[]}]);
        ui.data["evidence"] = json!([{"id":"ev_a","title":"A"},{"id":"ev_b","title":"B"}]);
        ui.key(press(KeyCode::Char('w')));
        ui.key(press(KeyCode::Enter));
        ui.key(press(KeyCode::Char('o')));
        assert!(matches!(ui.modal, Some(Modal::EvidencePicker { .. })));
        ui.key(press(KeyCode::Down));
        ui.key(press(KeyCode::Enter));
        assert_eq!(text(&ui.current().unwrap(), "id"), "ev_b");
    }

    fn graph_fixture(ui: &mut Ui) {
        ui.data["evidence"] = json!([
            {"id":"ev_a","title":"Original A","collection_method":"collector","collector_status":"ok","source_ids":["src_a"],"sources":[{"source_id":"src_a","url":"https://source.example/a","sha256":"source_hash_a","hash_scope":"source_bytes"}],"run_id":"run_a","report_sha256":"report_hash_a","hash_scope":"source_bytes"},
            {"id":"ev_b","title":"Original B","collection_method":"collector","collector_status":"partial","source_ids":["src_b"],"sources":[{"source_id":"src_b","url":"https://source.example/b"}]}
        ]);
        ui.data["entities"] = json!([
            {"id":"a","type":"username","value":"same","label":"VERIFIED OWNER","evidence_ids":["ev_a","ev_b"],"source_ids":["src_a","src_b"]},
            {"id":"b","type":"profile_url","value":"https://b.example/same","evidence_ids":["ev_a"],"source_ids":["src_a"]},
            {"id":"c","type":"profile_url","value":"https://c.example/same","evidence_ids":["ev_b"],"source_ids":["src_b"]},
            {"id":"z","type":"domain","value":"disconnected.example","evidence_ids":["ev_a"],"source_ids":["src_a"]}
        ]);
        ui.data["links"] = json!([
            {"id":"link_a","source_id":"a","target_id":"b","relation":"public_profile_at","status":"observed","evidence_ids":["ev_a"],"source_ids":["src_a"]},
            {"id":"link_b","source_id":"c","target_id":"a","relation":"declares_username","status":"observed","evidence_ids":["ev_b"],"source_ids":["src_b"]}
        ]);
        ui.data["graph"] = crate::graph::project(&ui.data);
        ui.sync_graph();
    }

    #[test]
    fn graph_shortcut_is_appended_and_existing_keys_keep_their_destinations() {
        let (_directory, mut ui) = ui();
        ui.key(press(KeyCode::Char('w')));
        ui.key(press(KeyCode::Char('g')));
        assert_eq!(ui.view, View::Investigations);
        assert!(ui.worker.is_none());
        ui.key(press(KeyCode::Char('G')));
        assert_eq!(ui.view, View::Graph);
        assert_eq!(View::Graph.index(), 12);
        assert_eq!(View::Graph.shortcut(), "G");
        ui.key(press(KeyCode::BackTab));
        assert_eq!(ui.view, View::Investigations);
        for (key, view) in [
            ('4', View::Evidence),
            ('5', View::Entities),
            ('6', View::Links),
            ('b', View::Reasoning),
            ('w', View::Investigations),
        ] {
            ui.key(press(KeyCode::Char(key)));
            assert_eq!(ui.view, view);
        }
        assert_eq!(
            (0..VIEWS.len())
                .map(|index| View::from_index(index).shortcut())
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            VIEWS.len()
        );
    }
    #[test]
    fn graph_keyboard_follows_outgoing_and_incoming_edges_and_goes_back() {
        let (_directory, mut ui) = ui();
        graph_fixture(&mut ui);
        ui.key(press(KeyCode::Char('G')));
        assert_eq!(ui.graph.focus.as_deref(), Some("a"));
        assert_eq!(ui.records().len(), 2);
        assert!(
            ui.graph_diagram()
                .contains("public_profile_at (attributed)")
        );
        assert!(
            ui.graph_diagram()
                .contains("└──→ [profile_url] https://b.example/same")
        );
        ui.key(press(KeyCode::Enter));
        assert_eq!(ui.graph.focus.as_deref(), Some("b"));
        assert_eq!(ui.graph.history.len(), 1);
        ui.key(press(KeyCode::Backspace));
        assert_eq!(ui.graph.focus.as_deref(), Some("a"));
        ui.key(press(KeyCode::Char('j')));
        assert_eq!(ui.current().unwrap()["id"], "link_b");
        assert!(
            ui.graph_diagram()
                .starts_with("[profile_url] https://c.example/same")
        );
        assert!(ui.graph_diagram().contains("└──→ FOCUS [username] same"));
        ui.key(press(KeyCode::Enter));
        assert_eq!(ui.graph.focus.as_deref(), Some("c"));
        ui.key(press(KeyCode::Backspace));
        assert_eq!(ui.graph.focus.as_deref(), Some("a"));
        assert_eq!(ui.current().unwrap()["id"], "link_b");
        ui.key(press(KeyCode::Char('k')));
        assert_eq!(ui.current().unwrap()["id"], "link_a");
    }
    #[test]
    fn graph_browses_disconnected_nodes_and_searches_typed_neighbor_values() {
        let (_directory, mut ui) = ui();
        graph_fixture(&mut ui);
        ui.key(press(KeyCode::Char('G')));
        ui.filter = "c.example".into();
        assert_eq!(ui.records().len(), 1);
        assert_eq!(ui.current().unwrap()["id"], "link_b");
        ui.key(press(KeyCode::Esc));
        assert_eq!(ui.records().len(), 2);
        ui.key(press(KeyCode::Left));
        assert_eq!(ui.records().len(), 4);
        ui.key(press(KeyCode::End));
        assert_eq!(ui.current().unwrap()["id"], "z");
        ui.key(press(KeyCode::Enter));
        assert_eq!(ui.graph.focus.as_deref(), Some("z"));
        assert!(ui.records().is_empty());
        assert_eq!(ui.current().unwrap()["id"], "z");
        assert!(ui.graph_diagram().contains("No matching one-hop"));
        ui.key(press(KeyCode::Enter));
        assert_eq!(ui.graph.focus.as_deref(), Some("z"));
        ui.key(press(KeyCode::Backspace));
        assert_eq!(ui.graph.focus.as_deref(), Some("a"));
        ui.key(press(KeyCode::Char('/')));
        assert!(matches!(
            ui.modal,
            Some(Modal::Form(Form {
                action: FormAction::Filter,
                ..
            }))
        ));
        ui.key(press(KeyCode::Esc));
    }
    #[test]
    fn graph_evidence_uses_original_evidence_and_the_existing_multi_citation_picker() {
        let (_directory, mut ui) = ui();
        graph_fixture(&mut ui);
        ui.key(press(KeyCode::Char('G')));
        ui.key(press(KeyCode::Char('o')));
        assert_eq!(ui.view, View::Evidence);
        assert_eq!(ui.current().unwrap()["id"], "ev_a");
        ui.key(press(KeyCode::Char('G')));
        ui.key(press(KeyCode::Left));
        ui.key(press(KeyCode::Char('o')));
        assert!(matches!(ui.modal, Some(Modal::EvidencePicker { .. })));
        ui.key(press(KeyCode::Char('j')));
        ui.key(press(KeyCode::Enter));
        assert_eq!(ui.view, View::Evidence);
        assert_eq!(ui.current().unwrap()["id"], "ev_b");
        ui.key(press(KeyCode::Char('G')));
        ui.key(press(KeyCode::End));
        ui.key(press(KeyCode::Enter));
        assert_eq!(ui.graph.focus.as_deref(), Some("z"));
        ui.key(press(KeyCode::Char('o')));
        assert_eq!(ui.current().unwrap()["id"], "ev_a");
    }
    #[test]
    fn graph_external_focus_changes_override_once_but_do_not_reset_local_navigation() {
        let (_directory, mut ui) = ui();
        graph_fixture(&mut ui);
        ui.data["graph_focus"] = json!({"id":"graph","entity_id":"a","updated_at":"one"});
        ui.sync_graph();
        ui.key(press(KeyCode::Char('G')));
        ui.key(press(KeyCode::Enter));
        assert_eq!(ui.graph.focus.as_deref(), Some("b"));
        ui.sync_graph();
        assert_eq!(ui.graph.focus.as_deref(), Some("b"));
        assert_eq!(ui.graph.history.len(), 1);
        ui.data["graph_focus"]["updated_at"] = json!("two");
        ui.sync_graph();
        assert_eq!(ui.graph.focus.as_deref(), Some("a"));
        assert!(ui.graph.history.is_empty());
        ui.data["graph_focus"]["entity_id"] = json!("c");
        ui.sync_graph();
        assert_eq!(ui.graph.focus.as_deref(), Some("c"));
        ui.data["graph_focus"]["entity_id"] = json!("missing");
        ui.sync_graph();
        assert_eq!(ui.graph.focus.as_deref(), Some("c"));
        assert!(ui.notice.contains("outside source-backed"));
        ui.data["graph"]["nodes"] = json!([]);
        ui.sync_graph();
        assert!(ui.graph.focus.is_none());
    }
    #[test]
    fn agent_focus_does_not_dismiss_an_editor_or_picker_and_is_presented_once_idle() {
        let (_directory, mut ui) = ui();
        graph_fixture(&mut ui);
        ui.key(press(KeyCode::Char('w')));
        ui.key(press(KeyCode::Char('d')));
        let Some(Modal::Form(form)) = &mut ui.modal else {
            panic!("question editor");
        };
        form.fields[0].value = "Unsaved analyst question".into();
        ui.data["graph_focus"] = json!({"id":"graph","entity_id":"c","updated_at":"one"});
        ui.sync_graph();
        assert_eq!(ui.view, View::Investigations);
        let Some(Modal::Form(form)) = &ui.modal else {
            panic!("editor was dismissed");
        };
        assert_eq!(form.fields[0].value, "Unsaved analyst question");
        assert!(ui.graph.present_pending);
        ui.key(press(KeyCode::Char('!')));
        let Some(Modal::Form(form)) = &ui.modal else {
            panic!("editor missing");
        };
        assert!(form.fields[0].value.contains('!'));
        ui.key(press(KeyCode::Esc));
        ui.present_graph_focus();
        assert_eq!(ui.view, View::Graph);
        assert_eq!(ui.graph.focus.as_deref(), Some("c"));
        ui.key(press(KeyCode::Char('w')));
        ui.sync_graph();
        assert_eq!(
            ui.view,
            View::Investigations,
            "unchanged focus must not steal navigation"
        );
        ui.modal = Some(Modal::EvidencePicker {
            rows: vec![],
            selected: 0,
        });
        ui.data["graph_focus"]["entity_id"] = json!("b");
        ui.sync_graph();
        assert_eq!(ui.view, View::Investigations);
        assert!(matches!(ui.modal, Some(Modal::EvidencePicker { .. })));
        ui.key(press(KeyCode::Esc));
        ui.present_graph_focus();
        assert_eq!(ui.view, View::Graph);
        assert_eq!(ui.graph.focus.as_deref(), Some("b"));
    }

    #[test]
    fn graph_inspector_exposes_provenance_and_keeps_legacy_review_distinct() {
        let (_directory, mut ui) = ui();
        graph_fixture(&mut ui);
        ui.data["graph"]["edges"][0]["review"] =
            json!({"scope":"legacy_claim_review","status":"accepted","review_note":"Legacy only"});
        ui.key(press(KeyCode::Char('G')));
        let output = ui.graph_inspector();
        for expected in [
            "source_observation (not confirmed)",
            "Legacy claim review: accepted",
            "Separate from trusted finding review",
            "Source: src_a",
            "URL: https://source.example/a",
            "Source SHA256: source_hash_a",
            "Report SHA256: report_hash_a",
            "Hash scope: source_bytes",
            "Evidence: ev_a",
            "Collector: ok",
            "latest 500",
            "ownership verification",
        ] {
            assert!(output.contains(expected), "missing {expected}");
        }
        assert!(!output.contains("VERIFIED OWNER"));
        assert!(output.contains("[username] same"));
    }
    #[test]
    fn graph_source_labels_cannot_inject_terminal_or_direction_controls() {
        let (_directory, mut ui) = ui();
        graph_fixture(&mut ui);
        ui.data["graph"]["nodes"][0]["value"] = json!("same\u{1b}[2J\n\t\u{202e}fake");
        ui.data["graph"]["edges"][0]["relation"] = json!("links\r\u{7}\u{2066}to");
        ui.data["evidence"][0]["sources"][0]["url"] =
            json!("https://source.example/\u{1b}]52;evil\u{7}");
        ui.key(press(KeyCode::Char('G')));
        for output in [ui.graph_diagram(), ui.graph_inspector()] {
            assert!(!output.contains('\u{1b}'));
            assert!(!output.contains('\u{7}'));
            assert!(!output.contains('\u{202e}'));
            assert!(!output.contains('\u{2066}'));
            assert!(!output.contains('\r'));
        }
        assert!(!graph_node_label(ui.graph_focus().unwrap()).contains('\n'));
        let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
        terminal.draw(|frame| ui.draw(frame)).unwrap();
        assert!(!buffer_text(&terminal).contains('\u{1b}'));
    }
    #[test]
    fn graph_archive_neighbors_show_date_and_domain_without_changing_typed_values() {
        let node = json!({"type":"url","value":"https://web.archive.org/web/20170618170502id_/http://www.wmc-scams.com:80/"});
        assert_eq!(
            graph_neighbor_label(&node),
            "[url] 2017-06-18 www.wmc-scams.com/ · archive"
        );
        assert!(graph_node_label(&node).contains("https://web.archive.org/web/20170618170502id_/"));
        let later = json!({"type":"url","value":"https://web.archive.org/web/20170724074639id_/http://www.publicopinion.co.za:80/"});
        assert!(
            graph_neighbor_label(&later).starts_with("[url] 2017-07-24 www.publicopinion.co.za/")
        );
        for value in [
            "https://web.archive.org.evil.invalid/web/20170618170502id_/http://example.org/",
            "https://web.archive.org/web/20179999170502id_/http://example.org/",
            "https://web.archive.org/web/20170618170502id_/not-an-original-url",
        ] {
            let node = json!({"type":"url","value":value});
            assert_eq!(graph_neighbor_label(&node), graph_node_label(&node));
        }
    }
    #[test]
    fn graph_renders_focused_empty_disconnected_and_small_terminal_states() {
        let (_directory, mut ui) = ui();
        graph_fixture(&mut ui);
        ui.key(press(KeyCode::Char('G')));
        for (width, height) in [
            (160, 48),
            (120, 36),
            (100, 30),
            (80, 24),
            (55, 15),
            (30, 8),
            (1, 1),
        ] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| ui.draw(frame)).unwrap();
            if width >= 55 {
                let output = buffer_text(&terminal);
                assert!(output.contains("G:Graph"));
                assert!(output.contains("FOCUS"));
                if width >= 80 {
                    for relation in ["public_profile_at", "declares_username"] {
                        assert!(
                            output.contains(relation),
                            "relation {relation} clipped at {width}x{height}"
                        );
                    }
                }
            }
        }
        let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
        terminal.draw(|frame| ui.draw(frame)).unwrap();
        let output = buffer_text(&terminal);
        for expected in [
            "One-hop observation",
            "public_profile_at",
            "declares_username",
            "→ out",
            "← in",
            "PROVENANCE",
        ] {
            assert!(output.contains(expected), "missing {expected}");
        }
        ui.data["graph"] = crate::graph::project(&json!({}));
        ui.sync_graph();
        ui.key(press(KeyCode::Enter));
        ui.key(press(KeyCode::Backspace));
        ui.key(press(KeyCode::Down));
        ui.key(press(KeyCode::Char('o')));
        for (width, height) in [(120, 36), (80, 24), (55, 15)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| ui.draw(frame)).unwrap();
            assert!(buffer_text(&terminal).contains("G:Graph"));
        }
    }
    #[test]
    fn idle_case_poll_refreshes_only_when_activity_revision_changes() {
        let (_directory, mut ui) = ui();
        ui.data["sentinel"] = json!("kept without full snapshot");
        ui.refreshed_at = Instant::now() - Duration::from_secs(2);
        ui.poll_case_changes();
        assert_eq!(ui.data["sentinel"], "kept without full snapshot");
        ui.workbench
            .store
            .event("test.changed", "Synthetic local update", json!({}))
            .unwrap();
        ui.refreshed_at = Instant::now() - Duration::from_secs(2);
        ui.poll_case_changes();
        assert!(ui.data["sentinel"].is_null());
        assert_eq!(
            ui.activity_revision,
            ui.workbench.store.activity_revision().unwrap()
        );
    }

    #[test]
    fn review_requires_rationale_and_retains_form_on_error() {
        let (_directory, mut ui) = ui();
        ui.data["ontology"]["claims"] =
            json!([{"id":"missing","status":"proposed","evidence_ids":["ev"]}]);
        ui.change_view(View::Claims.index());
        ui.review_form();
        ui.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        let Some(Modal::Form(form)) = ui.modal else {
            panic!("review form must stay open")
        };
        assert!(form.error.contains("rationale"));
    }
}
