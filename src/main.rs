use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use fieldkit::{Config, Job, Workbench, collectors, discovery::DiscoveryOptions};
use serde_json::{Value, json};
use std::{
    io::{self, Read},
    path::PathBuf,
    sync::atomic::AtomicBool,
};

#[derive(Parser)]
#[command(
    version,
    about = "Fieldkit · evidence-backed OSINT discovery and investigation",
    after_help = "Start the TUI: fieldkit tui --case cases/my-investigation\nCollect: fieldkit --case cases/example username HANDLE --sites github,gitlab\nAgent jobs: fieldkit --case cases/example run job.json"
)]
struct Cli {
    #[arg(long, global = true)]
    case: Option<PathBuf>,
    #[arg(long,global=true,default_value_t=10,value_parser=clap::value_parser!(u64).range(1..=120))]
    timeout: u64,
    #[arg(long,global=true,default_value_t=64,value_parser=clap::value_parser!(u64).range(1..=128))]
    max_requests: u64,
    #[arg(long,global=true,default_value_t=50,value_parser=clap::value_parser!(u64).range(1..=500))]
    max_results: u64,
    #[arg(long,global=true,default_value_t=4194304,value_parser=clap::value_parser!(u64).range(1024..=8388608))]
    max_bytes: u64,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Open the native terminal investigation workbench.
    Tui,
    /// Serve the local investigation app.
    App {
        #[arg(long, default_value = ".")]
        workspace: PathBuf,
        #[arg(long, default_value_t = 8765)]
        port: u16,
    },
    /// Serve agent tools over newline-delimited JSON-RPC stdio.
    Mcp {
        #[arg(long, default_value = ".")]
        workspace: PathBuf,
    },
    /// Print machine-readable collector and interface capabilities.
    Capabilities,
    /// Run a JSON collector job (use - for stdin).
    Run { file: String },
    /// Run up to 100 JSONL collector jobs.
    Batch {
        file: String,
        #[arg(long,default_value_t=2,value_parser=clap::value_parser!(u64).range(1..=8))]
        jobs: u64,
    },
    /// Read, export, or verify a case.
    Case {
        action: String,
        path: Option<PathBuf>,
        #[arg(long, default_value = "json")]
        format: String,
    },
    /// Plan and run a bounded discovery queue.
    Discover {
        seed: Option<String>,
        #[arg(long, default_value = "username")]
        kind: String,
        #[arg(long, default_value_t = 10)]
        jobs: usize,
        #[arg(long, default_value_t = 2)]
        depth: usize,
        #[arg(long, value_delimiter = ',')]
        domains: Vec<String>,
        #[arg(long, value_delimiter = ',')]
        sites: Vec<String>,
        #[arg(long)]
        web_search: bool,
        #[arg(long)]
        resume: Option<String>,
        #[arg(long)]
        plan_only: bool,
    },
    /// Inspect or rebuild the evidence-grounded case ontology.
    Ontology {
        #[arg(long)]
        sync: bool,
    },
    #[command(external_subcommand)]
    Collect(Vec<String>),
}

fn input(file: &str) -> Result<String> {
    let mut body = String::new();
    if file == "-" {
        io::stdin()
            .take(8 * 1024 * 1024 + 1)
            .read_to_string(&mut body)?;
    } else {
        let f = std::fs::File::open(file)?;
        f.take(8 * 1024 * 1024 + 1).read_to_string(&mut body)?;
    }
    ensure!(body.len() <= 8 * 1024 * 1024, "Job input exceeds 8 MiB");
    Ok(body)
}
fn print(v: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn run_job(job: &Job, config: &Config, case: &Option<PathBuf>) -> Result<Value> {
    if let Some(path) = case {
        Workbench::new(path, config.clone())?.run_tool(job)
    } else {
        Ok(collectors::collect(job, config)?.report)
    }
}

fn collector_job(args: Vec<String>) -> Result<Job> {
    ensure!(!args.is_empty(), "Choose a collector");
    let mut job = Job {
        command: args[0].clone(),
        target: None,
        options: json!({}),
    };
    let mut i = 1;
    while i < args.len() {
        if let Some(key) = args[i].strip_prefix("--") {
            let (key, inline) = key
                .split_once('=')
                .map(|(k, v)| (k, Some(v)))
                .unwrap_or((key, None));
            let key = key.replace('-', "_");
            let boolean = ["include_candidates", "search", "prefix", "download_media"]
                .contains(&key.as_str());
            let value = if let Some(value) = inline {
                value.to_owned()
            } else if boolean
                && args
                    .get(i + 1)
                    .is_none_or(|s| !["true", "false"].contains(&s.as_str()))
            {
                "true".to_owned()
            } else {
                i += 1;
                args.get(i).context("Option needs a value")?.clone()
            };
            let value = if key == "options" {
                serde_json::from_str(&value)?
            } else if ["sites", "types", "modules", "kinds"].contains(&key.as_str()) {
                json!(value.split(',').map(str::to_owned).collect::<Vec<_>>())
            } else if boolean {
                json!(value.parse::<bool>()?)
            } else if ["limit", "port", "before", "max_media"].contains(&key.as_str()) {
                json!(value.parse::<u64>()?)
            } else {
                json!(value)
            };
            if key == "options" {
                ensure!(value.is_object(), "--options expects a JSON object");
                job.options = value;
            } else {
                job.options[&key] = value;
            }
        } else {
            ensure!(
                job.target.is_none(),
                "Only one target is allowed; quote targets containing spaces"
            );
            job.target = Some(args[i].clone());
        }
        i += 1;
    }
    collectors::validate_job(&job)?;
    Ok(job)
}

fn batch(
    body: &str,
    file: &str,
    workers: usize,
    config: &Config,
    case: &Option<PathBuf>,
) -> Result<Value> {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    let lines: Vec<_> = body.lines().filter(|s| !s.trim().is_empty()).collect();
    ensure!(
        !lines.is_empty() && lines.len() <= 100,
        "Supply 1–100 JSONL jobs"
    );
    let jobs: Vec<Job> = lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str(line).unwrap_or_else(|_| Job {
                command: "invalid".into(),
                target: Some(format!("line {}", index + 1)),
                options: json!({}),
            })
        })
        .collect();
    let model = case
        .as_ref()
        .map(|p| Workbench::new(p, config.clone()))
        .transpose()?;
    let next = AtomicUsize::new(0);
    let results = Mutex::new(vec![Value::Null; jobs.len()]);
    std::thread::scope(|scope| {
        for _ in 0..workers.min(jobs.len()) {
            scope.spawn(||loop {
                let index=next.fetch_add(1,Ordering::Relaxed);let Some(job)=jobs.get(index)else{break;};
                let result=if let Some(model)=&model {model.run_tool(job)}else{collectors::collect(job,config).map(|c|c.report)};
                let report=result.unwrap_or_else(|error|json!({"schema_version":"1.0","run_id":fieldkit::model::id("run"),"command":job.command,"target":job.target,"status":"error","data":null,"sources":[],"entities":[],"relationships":[],"errors":[{"code":"BATCH_JOB","message":error.to_string()}],"timing":{"completed_at":fieldkit::model::now()}}));
                results.lock().unwrap()[index]=report;
            });
        }
    });
    let reports = results.into_inner().unwrap();
    let failed = reports.iter().filter(|r| status_code(r) != 0).count();
    Ok(
        json!({"schema_version":"1.0","tool":"fieldkit","version":env!("CARGO_PKG_VERSION"),"run_id":fieldkit::model::id("run"),"command":"batch","target":file,"status":if failed>0{"partial"}else{"ok"},"data":{"results":reports,"total":reports.len(),"complete_count":reports.len()-failed,"unresolved_count":failed},"sources":[],"entities":[],"relationships":[],"errors":[],"timing":{"completed_at":fieldkit::model::now()},"untrusted_content":true}),
    )
}

fn run(cli: Cli) -> Result<i32> {
    let config = Config {
        timeout_secs: cli.timeout,
        max_requests: cli.max_requests as usize,
        max_results: cli.max_results as usize,
        max_bytes: cli.max_bytes as usize,
    };
    let default_case = || {
        cli.case
            .clone()
            .unwrap_or_else(|| PathBuf::from("cases/default"))
    };
    match cli.command {
        Command::Tui => fieldkit::tui::run(Workbench::new(default_case(), config)?)?,
        Command::App { workspace, port } => {
            let case = fieldkit::store::scoped_path(
                &workspace,
                default_case()
                    .to_str()
                    .context("Case path requires UTF-8")?,
                false,
            )?;
            fieldkit::app::serve(Workbench::new(case, config)?, workspace, port)?;
        }
        Command::Mcp { workspace } => fieldkit::mcp::serve(workspace, cli.case, config)?,
        Command::Capabilities => print(&collectors::capabilities())?,
        Command::Run { file } => {
            let job: Job = serde_json::from_str(&input(&file)?)?;
            let report = run_job(&job, &config, &cli.case)?;
            print(&report)?;
            return Ok(status_code(&report));
        }
        Command::Batch { file, jobs } => {
            let report = batch(&input(&file)?, &file, jobs as usize, &config, &cli.case)?;
            print(&report)?;
            return Ok(status_code(&report));
        }
        Command::Case {
            action,
            path,
            format,
        } => {
            let case = path
                .or(cli.case)
                .unwrap_or_else(|| PathBuf::from("cases/default"));
            let w = Workbench::new(case, config)?;
            match action.as_str() {
                "init" | "show" => print(&w.snapshot()?)?,
                "verify" => {
                    let verified = w.store.verify()?;
                    print(&verified)?;
                    return Ok(if verified["ok"] == true { 0 } else { 2 });
                }
                "export" => std::print!("{}", w.export(&format)?),
                _ => bail!("Case operation must be init, show, verify, or export"),
            }
        }
        Command::Discover {
            seed,
            kind,
            jobs,
            depth,
            domains,
            sites,
            web_search,
            resume,
            plan_only,
        } => {
            let w = Workbench::new(default_case(), config)?;
            let session = if let Some(id) = resume {
                w.store.get("discovery", &id)?
            } else {
                w.create_discovery(DiscoveryOptions {
                    kind,
                    seed: seed.context("Supply a seed or --resume ID")?,
                    max_jobs: jobs,
                    max_depth: depth,
                    domains,
                    sites,
                    web_search,
                })?
            };
            if plan_only {
                print(&session)?;
            } else {
                let result = w.run_discovery(
                    fieldkit::model::text(&session, "id"),
                    &AtomicBool::new(false),
                    |v| eprintln!("{}", v),
                )?;
                print(&result)?;
            }
        }
        Command::Ontology { sync } => {
            let w = Workbench::new(default_case(), config)?;
            print(&if sync {
                w.sync_ontology()?
            } else {
                w.ontology_snapshot()?
            })?;
        }
        Command::Collect(args) => {
            let job = collector_job(args)?;
            let report = run_job(&job, &config, &cli.case)?;
            print(&report)?;
            return Ok(status_code(&report));
        }
    }
    Ok(0)
}

fn status_code(report: &Value) -> i32 {
    if ["ok", "not_found"].contains(&fieldkit::model::text(report, "status")) {
        0
    } else {
        2
    }
}
fn main() {
    let code = match run(Cli::parse()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{}", json!({"status":"error","error":error.to_string()}));
            1
        }
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_native_username_job() {
        let j = collector_job(vec![
            "username".into(),
            "example".into(),
            "--sites".into(),
            "github,gitlab".into(),
            "--include-candidates=false".into(),
        ])
        .unwrap();
        assert_eq!(j.options["sites"], json!(["github", "gitlab"]));
        assert_eq!(j.options["include_candidates"], false);
    }
    #[test]
    fn rejects_extra_targets() {
        assert!(collector_job(vec!["username".into(), "a".into(), "b".into()]).is_err());
    }
    #[test]
    fn batch_isolates_bad_lines_preserves_order_and_saves_parallel_evidence() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().canonicalize().unwrap().join("case");
        let report = batch(
            "{\"command\":\"doctor\"}\nbroken json\n{\"command\":\"integrations\"}",
            "-",
            2,
            &Config::default(),
            &Some(path.clone()),
        )
        .unwrap();
        assert_eq!(report["status"], "partial");
        assert_eq!(report["data"]["unresolved_count"], 1);
        assert_eq!(report["data"]["results"][0]["command"], "doctor");
        assert_eq!(report["data"]["results"][1]["status"], "invalid");
        assert_eq!(report["data"]["results"][2]["command"], "integrations");
        let w = Workbench::new(path, Config::default()).unwrap();
        assert_eq!(w.store.reports(10).unwrap().len(), 3);
        assert_eq!(w.store.verify().unwrap()["ok"], true);
    }
}
