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
const VIEWS: [&str; 10] = [
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
        }
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
    Discovery,
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
}

struct Ui {
    workbench: Workbench,
    data: Value,
    view: View,
    selected: [usize; 10],
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
}

fn safe(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
        .collect()
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
    Ok(DiscoveryOptions {
        kind: required(form.value(0), "Seed kind")?,
        seed: required(form.value(1), "Seed")?,
        max_jobs,
        max_depth,
        domains: csv(form.value(4)),
        sites: csv(form.value(5)),
        web_search,
    })
}

impl Ui {
    fn new(workbench: Workbench) -> Result<Self> {
        let data = workbench.snapshot()?;
        Ok(Self { workbench, data, view: View::Discovery, selected: [0; 10], child_selected: 0, child_focus: false,
            filter: String::new(), inspector_scroll: 0, modal: None, worker: None,
            notice: "Press d to start discovery from a username, public URL, domain or search query. ? opens the guide.".into(),
            notice_error: false, quitting: false, refreshed_at: Instant::now() })
    }

    fn run_loop(&mut self, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
        loop {
            self.poll_worker();
            if self.quitting && self.worker.is_none() {
                break;
            }
            if self.worker.is_some() && self.refreshed_at.elapsed() > Duration::from_secs(1) {
                self.refresh();
            }
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
    fn refresh(&mut self) {
        match self.workbench.snapshot() {
            Ok(data) => {
                self.data = data;
                self.refreshed_at = Instant::now();
            }
            Err(error) => self.notice(format!("Could not refresh case: {error:#}"), true),
        }
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
    fn children(&self) -> Vec<Value> {
        match self.view {
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
        let rows = if self.child_focus && matches!(self.view, View::Discovery | View::Flows) {
            self.children()
        } else {
            self.records()
        };
        let index = if self.child_focus {
            self.child_selected
        } else {
            self.selected[self.view.index()]
        };
        rows.get(index.min(rows.len().saturating_sub(1))).cloned()
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
                    if let Some(worker) = worker {
                        let _ = worker.handle.join();
                    }
                    match result {
                        Ok(report) => self.notice(
                            format!(
                                "{label} · {} · observations preserved in this case",
                                text(&report, "status")
                            ),
                            false,
                        ),
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
            KeyCode::Enter if matches!(self.view, View::Discovery | View::Flows) => {
                self.child_focus = !self.child_focus;
                self.inspector_scroll = 0;
            }
            KeyCode::Esc => {
                self.filter.clear();
                self.child_focus = false;
                self.inspector_scroll = 0;
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.refresh();
                self.notice("Case refreshed", false);
            }
            KeyCode::Char('d') => self.discovery_form(),
            KeyCode::Char('g') => self.start_discovery(),
            KeyCode::Char('p') => {
                if let Some(worker) = &self.worker {
                    worker.stop.store(true, Ordering::Relaxed);
                    self.notice(
                        "Pause requested · finishing and preserving the current collector",
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
            self.notice("Closing after the current collector is preserved. The discovery queue can be resumed next time.", false);
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

    fn discovery_form(&mut self) {
        self.modal = Some(Modal::Form(Form::new(
            "Plan discovery · evidence-backed ontology",
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
            ],
        )));
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
            Constraint::Length(2),
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
            "{} evidence · {} entities · {} leads · ontology v{}",
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
        let mut lines = vec![Line::default(), Line::default()];
        for (index, title) in VIEWS.iter().enumerate() {
            let line = usize::from(index >= 5);
            let label = format!(" {}:{} ", (index + 1) % 10, title);
            let style = if index == self.view.index() {
                Style::default()
                    .bg(Color::DarkGray)
                    .fg(ACCENT)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            lines[line].spans.push(Span::styled(label, style));
        }
        frame.render_widget(Paragraph::new(lines), area);
    }
    fn draw_records(&self, frame: &mut Frame, area: Rect) {
        if matches!(self.view, View::Discovery | View::Flows) {
            let panels = Layout::vertical([
                Constraint::Length(7.min(area.height / 2).max(3)),
                Constraint::Min(3),
            ])
            .split(area);
            let records = self.records();
            let title = if self.view == View::Discovery {
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
            let title = if self.view == View::Discovery {
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
                    "completed" | "ok" | "accepted" | "corroborated" => Color::Green,
                    "failed" | "error" | "rejected" | "contested" | "blocked" => Color::Yellow,
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
                Constraint::Length(11),
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
        let text = if let Some(record) = self.current() {
            let references = reference_ids(&record);
            let mut prelude = String::new();
            if !references.is_empty() {
                prelude.push_str(&format!(
                    "{} supporting evidence record(s) · o opens citations\n",
                    references.len()
                ));
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
            prelude.push('\n');
            prelude.push_str(&safe(
                &serde_json::to_string_pretty(&record).unwrap_or_default(),
            ));
            prelude
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
        let keys = if self.view == View::Discovery {
            "d new discovery  g run/resume  p pause  Enter sessions/jobs  o evidence"
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
            Line::from(Span::styled(" Tab/Shift+Tab views · ↑↓ records · Ctrl+R refresh · i question · x export · ? help · q quit", Style::default().fg(DIM))),
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
            Paragraph::new(
                "Tab / Enter next field · Shift+Tab previous · Ctrl+S save · Esc cancel",
            )
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
            return value.chars().take(11).collect();
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
DISCOVERY AND ONTOLOGY\n\
1. Press i to set the investigation question.\n\
2. Press d. Choose username, profile, url, domain or search; enter a seed, job budget and expansion depth. Ctrl+S saves the plan.\n\
3. Press g to run or resume. Collection runs in a background thread. The persistent frontier records each job, its reason, evidence references, result and depth.\n\
4. Press p to pause after the current collector. q saves the current result and closes; g resumes the remaining queue next time.\n\
5. Ontology shows the entity types and relationship signatures learned from observations. Claims retain the captured relationship, evidence IDs, source references, hashes and structured source pointers. y synchronizes old case evidence.\n\
6. Select a claim, derived job or correlation and press o to inspect supporting evidence. PgUp/PgDn scroll the inspector.\n\
7. In Claims or Correlations, press v to accept/reject with a rationale. Correlations are working leads; shared usernames or URLs do not automatically merge identities or prove ownership.\n\n\
EVERYDAY INVESTIGATION\n\
t  Run one collector and capture its result. For username, Target is the handle. Options can restrict providers: {\"sites\":[\"github\",\"bluesky\"]}.\n\
e  Capture an observation with its source URL and optional source time.\n\
v  Assess selected evidence: unreviewed, corroborated, contested or discarded. Blocked/rate-limited results remain unresolved.\n\
n  Add an analyst note linked to selected evidence or claim citations.\n\
a  Add a typed entity. l links existing entities with supporting evidence IDs.\n\
f  Plan a flow with its first step. s adds a step. Enter switches between flows and steps; r runs the selected step.\n\
x  Export the investigation to a new Markdown or JSON file. Existing files are preserved.\n\n\
NAVIGATION AND FORMS\n\
Tab/Shift+Tab switch views; 1–9 and 0 jump directly. Up/Down or j/k select records. Enter switches between parent and child tables in Discovery and Flows.\n\
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
        assert_eq!(ui.view, View::Activity);
        ui.key(press(KeyCode::Tab));
        assert_eq!(ui.view, View::Discovery);
        ui.key(press(KeyCode::Char('4')));
        assert_eq!(ui.view, View::Evidence);
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
