//! The only boundary allowed to touch case files. No collectors or model calls.
use crate::agent::{self, AgentConfig};
use anyhow::{Context, Result, ensure};
use fieldkit::{Config, Workbench};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

#[derive(Debug)]
pub enum Request {
    Open {
        path: PathBuf,
        create: bool,
    },
    Refresh,
    Plan {
        question: String,
        mode: String,
        seeds: Vec<String>,
    },
    Focus(String),
    Report(String),
    Verify,
    Probe(AgentConfig),
    RunAgent {
        config: AgentConfig,
        question: String,
        mode: String,
        seeds: Vec<String>,
        collection: bool,
    },
}

#[derive(Debug)]
pub enum Reply {
    Snapshot {
        state: Value,
        notice: String,
        investigation: Option<String>,
    },
    Document {
        title: String,
        value: Value,
    },
    Error(String),
    Availability(Result<(), String>),
    AgentFinished {
        state: Option<Value>,
        output: Value,
    },
}

pub struct Worker {
    pub requests: mpsc::SyncSender<Request>,
    pub replies: mpsc::Receiver<Reply>,
    pub cancel: Arc<AtomicBool>,
}

impl Worker {
    pub fn spawn(context: eframe::egui::Context) -> Self {
        let (requests, incoming) = mpsc::sync_channel(1);
        let (outgoing, replies) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let engine_cancel = cancel.clone();
        thread::Builder::new()
            .name("nightjar-case".into())
            .spawn(move || {
                let mut engine = Engine {
                    cancel: engine_cancel,
                    ..Default::default()
                };
                while let Ok(request) = incoming.recv() {
                    let reply = engine
                        .execute(request)
                        .unwrap_or_else(|e| Reply::Error(format!("{e:#}")));
                    if outgoing.send(reply).is_err() {
                        break;
                    }
                    context.request_repaint();
                }
            })
            .expect("create case worker thread");
        Self {
            requests,
            replies,
            cancel,
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
struct Engine {
    workbench: Option<Workbench>,
    cancel: Arc<AtomicBool>,
}

impl Engine {
    fn execute(&mut self, request: Request) -> Result<Reply> {
        if let Request::Probe(config) = request {
            return Ok(Reply::Availability(
                config.availability().map_err(|e| e.to_string()),
            ));
        }
        if let Request::Open { path, create } = request {
            if create {
                ensure!(
                    !path.exists(),
                    "New case requires a path that does not exist; use Open for an existing case"
                );
            } else {
                ensure!(
                    path.join("case.sqlite3").is_file(),
                    "Not an existing case: expected case.sqlite3. Use New case deliberately to create one"
                );
            }
            let workbench = Workbench::new(path, Config::default())?;
            let state = workbench.snapshot()?;
            self.workbench = Some(workbench);
            return Ok(Reply::Snapshot {
                state,
                notice: if create {
                    "Empty case created. Nothing collected."
                } else {
                    "Case opened. Nothing collected."
                }
                .into(),
                investigation: None,
            });
        }
        let workbench = self.workbench.as_ref().context("Open a case first")?;
        let mut investigation = None;
        let notice = match request {
            Request::Refresh => "Snapshot refreshed from the shared case.",
            Request::Plan {
                question,
                mode,
                seeds,
            } => {
                let record = workbench.create_recon_investigation(&question, &mode, seeds)?;
                investigation = record["id"].as_str().map(str::to_owned);
                "Investigation plan persisted. No collection or model call was made."
            }
            Request::Focus(id) => {
                workbench.focus_graph(&id)?;
                "Shared source graph focus saved."
            }
            Request::Report(run_id) => {
                let report = workbench.store.report(&run_id)?;
                let sha256 = workbench.store.report_hash(&run_id)?;
                return Ok(Reply::Document {
                    title: format!("Stored report · {run_id}"),
                    value: json!({"report_sha256":sha256,"report":report}),
                });
            }
            Request::Verify => {
                return Ok(Reply::Document {
                    title: "Case integrity verification".into(),
                    value: workbench.store.verify()?,
                });
            }
            Request::RunAgent {
                config,
                question,
                mode,
                seeds,
                collection,
            } => {
                let output = agent::run(
                    &config,
                    &workbench.store.path,
                    &question,
                    &mode,
                    &seeds,
                    collection,
                    &self.cancel,
                )?;
                let state = workbench.snapshot().ok();
                return Ok(Reply::AgentFinished { state, output });
            }
            Request::Open { .. } | Request::Probe(_) => unreachable!(),
        };
        // Report a failed refresh honestly, including if the preceding mutation persisted.
        let state = workbench.snapshot().context(
            "Operation completed, but snapshot refresh failed; refresh before continuing",
        )?;
        Ok(Reply::Snapshot {
            state,
            notice: notice.into(),
            investigation,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn open_never_creates_and_create_is_deliberate() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("case");
        let mut engine = Engine::default();
        assert!(
            engine
                .execute(Request::Open {
                    path: path.clone(),
                    create: false
                })
                .is_err()
        );
        assert!(!path.exists());
        assert!(matches!(
            engine
                .execute(Request::Open {
                    path: path.clone(),
                    create: true
                })
                .unwrap(),
            Reply::Snapshot { .. }
        ));
        assert!(
            engine
                .execute(Request::Open { path, create: true })
                .is_err()
        );
    }
    #[test]
    fn planning_persists_original_question_and_seeds_without_collection() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("case");
        let mut engine = Engine::default();
        engine
            .execute(Request::Open {
                path: path.clone(),
                create: true,
            })
            .unwrap();
        let question = "What public references mention this organization?";
        let reply = engine
            .execute(Request::Plan {
                question: question.into(),
                mode: "organization".into(),
                seeds: vec!["example.org".into()],
            })
            .unwrap();
        let Reply::Snapshot {
            state,
            investigation,
            ..
        } = reply
        else {
            panic!("snapshot expected")
        };
        assert!(investigation.is_some());
        assert_eq!(state["investigations"][0]["question"], question);
        assert_eq!(state["investigations"][0]["seeds"], json!(["example.org"]));
        assert_eq!(state["summary"]["run_count"], 0);
        assert_eq!(state["graph"]["nodes"], json!([]));
        let reopened = Workbench::new(path, Config::default()).unwrap();
        assert_eq!(reopened.investigations().unwrap().len(), 1);
    }
    #[test]
    fn malformed_plan_and_unknown_report_are_errors_not_success() {
        let temp = tempfile::tempdir().unwrap();
        let mut engine = Engine::default();
        engine
            .execute(Request::Open {
                path: temp.path().join("case"),
                create: true,
            })
            .unwrap();
        assert!(
            engine
                .execute(Request::Plan {
                    question: "Why?".into(),
                    mode: "unknown".into(),
                    seeds: vec!["x".into()]
                })
                .is_err()
        );
        assert!(engine.execute(Request::Report("missing".into())).is_err());
        assert!(engine.execute(Request::Focus("missing".into())).is_err());
    }
}
