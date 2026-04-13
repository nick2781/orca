use clap::Parser;

#[derive(Parser)]
#[command(name = "orca", version, about = "Multi-agent orchestrator")]
struct Cli {
    /// Subcommand to run
    command: Option<String>,
}

fn main() {
    let _cli = Cli::parse();
    println!("orca v{}", env!("CARGO_PKG_VERSION"));
}
