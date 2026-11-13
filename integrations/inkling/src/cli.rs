use crate::{
    BASE_URL, Budgets, Chat, ChatClient, MODEL, McpClient, ScopedTools, agent::ToolFailure,
    benchmark, redact, run_agent, save_report,
};
use anyhow::{Result, anyhow, ensure};
use clap::Parser;
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};

fn positive(s: &str) -> std::result::Result<u64, String> {
    s.parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| "must be positive".into())
}
fn timeout(s: &str) -> std::result::Result<Duration, String> {
    let n = s
        .parse::<f64>()
        .map_err(|_| "timeout must be finite and positive")?;
    if !n.is_finite() || n <= 0.0 {
        return Err("timeout must be finite and positive".into());
    }
    Duration::try_from_secs_f64(n)
        .map_err(|_| "timeout out of range".into())
        .and_then(|d| {
            if d.is_zero() {
                Err("timeout too small".into())
            } else {
                Ok(d)
            }
        })
}
fn base_url() -> String {
    std::env::var("INKLING_BASE_URL").unwrap_or_else(|_| BASE_URL.into())
}
fn model() -> String {
    std::env::var("INKLING_MODEL").unwrap_or_else(|_| MODEL.into())
}
#[derive(clap::Args, Clone)]
pub struct ModelArgs {
    #[arg(long,default_value_t=base_url())]
    pub base_url: String,
    #[arg(long,default_value_t=model())]
    pub model: String,
    #[arg(long, default_value = "INKLING_API_KEY")]
    pub api_key_env: String,
    #[arg(long,default_value="90",value_parser=timeout)]
    pub timeout: Duration,
    #[arg(long,default_value="2048",value_parser=positive)]
    pub max_tokens: u64,
    #[arg(long,default_value="12",value_parser=positive)]
    pub max_turns: u64,
    #[arg(long,default_value="24",value_parser=positive)]
    pub max_tool_calls: u64,
    #[arg(long,default_value="32768",value_parser=positive)]
    pub max_total_tokens: u64,
    /// New private JSON report path, never overwrites.
    #[arg(long)]
    pub output: Option<PathBuf>,
}
impl ModelArgs {
    pub fn budgets(&self) -> Budgets {
        Budgets {
            max_turns: self.max_turns,
            max_tool_calls: self.max_tool_calls,
            max_total_tokens: self.max_total_tokens,
        }
    }
}
#[derive(Parser)]
#[command(
    about = "Bounded Fieldkit investigator using OpenAI-compatible HTTP and native stdio MCP"
)]
pub struct AgentArgs {
    pub question: String,
    #[arg(long)]
    pub binary: PathBuf,
    #[arg(long)]
    pub workspace: PathBuf,
    #[arg(long)]
    pub case: PathBuf,
    #[arg(long,value_parser=["person","organization","infrastructure","crypto"])]
    pub mode: String,
    #[arg(long = "seed", required = true)]
    pub seeds: Vec<String>,
    /// Enable bounded public collectors; otherwise plan only.
    #[arg(long)]
    pub allow_collection: bool,
    #[command(flatten)]
    pub model: ModelArgs,
}
pub fn agent(args: AgentArgs) -> Result<(Value, i32)> {
    ensure!((1..=5).contains(&args.seeds.len()), "Supply 1–5 seeds");
    ensure!(!args.question.trim().is_empty(), "Question required");
    if let Some(output) = &args.model.output {
        ensure!(
            !output.try_exists()?,
            "Output exists; choose a new report path"
        );
    }
    let key = std::env::var(&args.model.api_key_env)
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "Set the named API key environment variable; never pass secrets on the command line"
            )
        })?;
    let mut client = ChatClient::new(
        &args.model.base_url,
        &args.model.model,
        &key,
        args.model.timeout,
        args.model.max_tokens,
    )?;
    let mcp = McpClient::connect(
        &args.binary,
        &args.workspace,
        &args.case,
        args.model.timeout,
        &args.model.api_key_env,
    )?;
    let mut scoped = ScopedTools::new(
        mcp,
        &args.question,
        &args.mode,
        args.seeds.clone(),
        args.allow_collection,
    );
    let mut context = format!(
        "Operator scope: {}",
        json!({"mode":args.mode,"seeds":args.seeds})
    );
    if !args.allow_collection {
        context.push_str(
            "\nCollection is disabled. Plan only, then explain that nothing was collected.",
        );
    }
    let mut report = run_agent(
        &mut client,
        &scoped.tools.clone(),
        |name, a| scoped.call(name, a).map_err(ToolFailure::Fatal),
        &args.question,
        &context,
        args.model.budgets(),
    )?;
    report["model"] = json!(args.model.model);
    report["investigation_id"] = json!(scoped.investigation_id);
    report["collection_enabled"] = json!(args.allow_collection);
    let report = redact(&report, &key);
    if let Some(output) = &args.model.output {
        save_report(output, &report)?;
    }
    let code = if report["stop_reason"] == "completed" {
        0
    } else {
        2
    };
    let mut summary = report;
    summary.as_object_mut().unwrap().remove("trace");
    Ok((summary, code))
}
#[derive(Parser)]
#[command(
    about = "Synthetic Fieldkit benchmark; default scripted replay is harness validation, not model evaluation"
)]
pub struct BenchmarkArgs {
    #[arg(long,default_value="replay",value_parser=["replay","live"])]
    pub mode: String,
    #[arg(long="scenario",value_parser=["seeded_infrastructure","exact_observed_lead","matching_handles","blocked_stop","source_prompt_injection","citation_fidelity"])]
    pub scenarios: Vec<String>,
    #[command(flatten)]
    pub model: ModelArgs,
}
/// Safe display even in replay, where the supplied endpoint is never used.
pub fn safe_url(base: &str) -> String {
    match url::Url::parse(base) {
        Ok(mut u) => {
            let _ = u.set_username("");
            let _ = u.set_password(None);
            u.set_query(None);
            u.set_fragment(None);
            u.to_string()
        }
        Err(_) => "[invalid URL omitted]".into(),
    }
}
pub fn benchmark(args: BenchmarkArgs) -> Result<(Option<Value>, i32)> {
    if let Some(output) = &args.model.output {
        ensure!(!output.try_exists()?, "Output exists");
    }
    let key = if args.mode == "live" {
        Some(
            std::env::var(&args.model.api_key_env)
                .ok()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    anyhow!("Live mode requires a key in the named environment variable")
                })?,
        )
    } else {
        None
    };
    let mut client = key
        .as_ref()
        .map(|key| {
            ChatClient::new(
                &args.model.base_url,
                &args.model.model,
                key,
                args.model.timeout,
                args.model.max_tokens,
            )
        })
        .transpose()?;
    let names = if args.scenarios.is_empty() {
        benchmark::SCENARIOS
            .iter()
            .map(|s| s.name.to_owned())
            .collect()
    } else {
        args.scenarios
    };
    let mut results = vec![];
    for name in names {
        let chat = client.as_mut().map(|c| c as &mut dyn Chat);
        results.push(benchmark::run_scenario(
            &name,
            chat,
            args.model.budgets(),
            "reference",
        )?);
    }
    let config = json!({"base_url":safe_url(&args.model.base_url),"base_url_used":args.mode=="live","max_turns":args.model.max_turns,
        "max_tool_calls":args.model.max_tool_calls,"max_total_tokens":args.model.max_total_tokens,"temperature":0.0,"max_tokens":args.model.max_tokens});
    let mut report = benchmark::make_report(
        results,
        if args.mode == "replay" {
            benchmark::REPLAY_MODEL
        } else {
            &args.model.model
        },
        &args.mode,
        config,
    );
    if let Some(key) = key {
        report = redact(&report, &key);
    }
    let code = if report["aggregate"]["failed"] == 0 {
        0
    } else {
        1
    };
    if let Some(output) = &args.model.output {
        save_report(output, &report)?;
        Ok((None, code))
    } else {
        for scenario in report["scenarios"].as_array_mut().unwrap() {
            scenario.as_object_mut().unwrap().remove("trace");
        }
        report["trace_output"] = json!("Use --output NEW_PATH.json to retain full tool traces");
        Ok((Some(report), code))
    }
}
