use clap::Parser;
fn main() {
    let code = match fieldkit_agent::cli::benchmark(fieldkit_agent::cli::BenchmarkArgs::parse()) {
        Ok((report, code)) => {
            if let Some(report) = report {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).expect("JSON report")
                );
            }
            code
        }
        Err(_) => {
            eprintln!("Benchmark could not complete (details omitted to protect credentials).");
            2
        }
    };
    std::process::exit(code);
}
