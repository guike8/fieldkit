use clap::Parser;
fn main() {
    let code = match fieldkit_agent::cli::agent(fieldkit_agent::cli::AgentArgs::parse()) {
        Ok((report, code)) => {
            println!("{}", report);
            code
        }
        // Never echo filesystem/provider exception strings: they may contain credentials.
        Err(_) => {
            eprintln!(
                "Agent stopped; protocol, transport or configuration failure. Details omitted; no retry performed. Inspect local state before resuming."
            );
            1
        }
    };
    std::process::exit(code);
}
