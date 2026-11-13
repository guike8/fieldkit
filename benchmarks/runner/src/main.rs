use anyhow::{Result, ensure};
use clap::{Parser, Subcommand};
use fieldkit_benchmarks::{
    common::{Output, read_json},
    event_stream, south_africa,
    transport::Client,
};
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Deterministic source/provenance benchmarks; never model-performance scores")]
struct Cli {
    #[command(subcommand)]
    benchmark: Benchmark,
}
#[derive(Subcommand)]
enum Benchmark {
    /// Five supplied dated captures; no one-seed discovery credit.
    SouthAfrica {
        #[command(subcommand)]
        action: SouthAction,
    },
    /// One commit and three exact observed source-file pivots; never executes code.
    EventStream {
        #[command(subcommand)]
        action: EventAction,
    },
}
#[derive(Subcommand)]
enum SouthAction {
    Capture(Args),
    Score(Args),
}
#[derive(Subcommand)]
enum EventAction {
    Capture(Args),
    Verify(Args),
}
#[derive(clap::Args)]
struct Args {
    /// Explicit server argv JSON array, never a shell command.
    #[arg(long)]
    command_json: String,
    #[arg(long)]
    receipt: PathBuf,
    #[arg(long)]
    case_path: Option<PathBuf>,
    #[arg(long)]
    wire_log: Option<PathBuf>,
    #[arg(long)]
    output: Option<PathBuf>,
    /// Relocate task.json/oracle.json; capture opens only task.json.
    #[arg(long)]
    task_dir: Option<PathBuf>,
}
fn run() -> Result<bool> {
    let (south, capture, args) = match Cli::parse().benchmark {
        Benchmark::SouthAfrica {
            action: SouthAction::Capture(a),
        } => (true, true, a),
        Benchmark::SouthAfrica {
            action: SouthAction::Score(a),
        } => (true, false, a),
        Benchmark::EventStream {
            action: EventAction::Capture(a),
        } => (false, true, a),
        Benchmark::EventStream {
            action: EventAction::Verify(a),
        } => (false, false, a),
    };
    ensure!(
        (south && capture) || args.case_path.is_some(),
        "--case-path is required for scoring, verification and event-stream capture"
    );
    let command: Vec<String> = serde_json::from_str(&args.command_json)?;
    ensure!(
        !command.is_empty() && !command[0].is_empty(),
        "--command-json requires nonempty explicit argv"
    );
    let dir = args.task_dir.unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join(if south {
                "south-africa-2017"
            } else {
                "event-stream-2018"
            })
    });
    let task = read_json(&dir.join("task.json"))?;
    // Reserve all output paths BEFORE starting a process or performing any tool call.
    let mut output = args.output.as_deref().map(Output::new).transpose()?;
    let mut receipt_output = if capture {
        Some(Output::new(&args.receipt)?)
    } else {
        None
    };
    let wire = args.wire_log.as_deref().map(Output::new).transpose()?;
    let mut client = Client::new(command, wire)?;
    let result = (|| -> Result<serde_json::Value> {
        if capture {
            if south {
                south_africa::capture(&mut client, &task, receipt_output.as_mut().unwrap())
            } else {
                event_stream::capture(
                    &mut client,
                    &task,
                    receipt_output.as_mut().unwrap(),
                    args.case_path.as_deref().unwrap(),
                )
            }
        } else {
            let oracle = read_json(&dir.join("oracle.json"))?;
            let receipt = read_json(&args.receipt)?;
            if south {
                south_africa::score(
                    &mut client,
                    &task,
                    &oracle,
                    &receipt,
                    args.case_path.as_deref().unwrap(),
                )
            } else {
                event_stream::verify(
                    &mut client,
                    &task,
                    &oracle,
                    &receipt,
                    args.case_path.as_deref().unwrap(),
                )
            }
        }
    })();
    let closed = client.close();
    let result = result?;
    closed?;
    if let Some(out) = &mut output {
        out.checkpoint(&result)?;
    }
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(capture
        || result[if south {
            "core_reconstruction"
        } else {
            "technical_reconstruction"
        }] == "passed")
}
fn main() {
    match run() {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(error) => {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
    }
}
