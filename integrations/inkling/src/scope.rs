use crate::{ALLOWED_TOOLS, agent::error_result};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::collections::HashSet;

pub trait ToolBackend {
    fn tools(&self) -> Vec<Value>;
    fn call(&mut self, name: &str, arguments: &Value) -> Result<Value>;
}

pub struct ScopedTools<B> {
    pub backend: B,
    pub tools: Vec<Value>,
    pub investigation_id: Option<String>,
    question: String,
    mode: String,
    seeds: Vec<String>,
    creation_attempted: bool,
    attempted_steps: HashSet<String>,
}
impl<B: ToolBackend> ScopedTools<B> {
    pub fn new(
        backend: B,
        question: &str,
        mode: &str,
        seeds: Vec<String>,
        allow_collection: bool,
    ) -> Self {
        let tools = backend.tools().into_iter().filter_map(|mut t| {
            let name = t["name"].as_str()?;
            if !ALLOWED_TOOLS.contains(&name) || (!allow_collection && name == "fieldkit_investigation_run_step") { return None; }
            if name == "fieldkit_capabilities" {
                t["description"] = json!("Read one collector's input schema; does not collect");
                t["inputSchema"] = json!({"type":"object","properties":{"command":{"type":"string","minLength":1,"maxLength":128}},
                    "required":["command"],"additionalProperties":false});
            }
            Some(t)
        }).collect();
        Self {
            backend,
            tools,
            investigation_id: None,
            question: question.into(),
            mode: mode.into(),
            seeds,
            creation_attempted: false,
            attempted_steps: HashSet::new(),
        }
    }
    pub fn call(&mut self, name: &str, args: &Value) -> Result<Value> {
        if !self.tools.iter().any(|t| t["name"] == name) {
            return Ok(error_result("Tool is not enabled"));
        }
        if !args.is_object() {
            return Ok(error_result("Tool arguments must be an object"));
        }
        if name == "fieldkit_capabilities" {
            let command = args["command"].as_str();
            if args.as_object().unwrap().len() != 1
                || command.is_none_or(|s| s.is_empty() || s.chars().count() > 128)
            {
                return Ok(error_result(
                    "Supply one collector command for schema lookup",
                ));
            }
            let result = self.backend.call(name, &json!({}))?;
            if result["isError"] == true {
                return Ok(result);
            }
            let mut report = result["structuredContent"].clone();
            let commands: Vec<Value> = report["data"]["commands"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|c| c["name"].as_str() == command)
                .cloned()
                .collect();
            if commands.len() != 1 {
                return Ok(error_result(
                    "Unknown collector command; use the planning schema's command enum",
                ));
            }
            let version = report["data"]["version"].clone();
            report["data"] = json!({"commands":commands,"version":version});
            return Ok(json!({"isError":false,"structuredContent":report,
                "content":[{"type":"text","text":serde_json::to_string(&report)?}]}));
        }
        if name == "fieldkit_investigation_create" {
            if self.creation_attempted {
                return Ok(error_result(
                    "Only one investigation may be created; inspect existing state",
                ));
            }
            if args["question"] != self.question
                || args["mode"] != self.mode
                || args["seeds"] != json!(self.seeds)
                || args.get("searches").is_some()
            {
                return Ok(error_result(
                    "Preserve the operator's exact question, mode and seeds",
                ));
            }
            self.creation_attempted = true;
            let result = self.backend.call(name, args)?;
            self.investigation_id = if result["isError"] == true {
                None
            } else {
                result["structuredContent"]["data"]["id"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
            };
            ensure!(
                result["isError"] == true || self.investigation_id.is_some(),
                "Investigation creation returned no id; inspect local state"
            );
            return Ok(result);
        }
        if name.starts_with("fieldkit_investigation_")
            && (self.investigation_id.is_none()
                || args["id"].as_str() != self.investigation_id.as_deref())
        {
            return Ok(error_result("Use only this run's investigation id"));
        }
        if name == "fieldkit_investigation_run_step" {
            let Some(step) = args["step_id"].as_str().filter(|s| !s.is_empty()) else {
                return Ok(error_result(
                    "Missing or already attempted step; no automatic retry",
                ));
            };
            if !self.attempted_steps.insert(step.into()) {
                return Ok(error_result(
                    "Missing or already attempted step; no automatic retry",
                ));
            }
        }
        self.backend.call(name, args)
    }
}
