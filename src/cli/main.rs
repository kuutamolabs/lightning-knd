mod api;

use crate::api::Api;
use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// IP address or hostname of the target machine.
    #[arg(short, long)]
    target: String,
    /// Path to the TLS cert of the target API.
    #[arg(short, long)]
    cert_path: String,
    /// Path to the macaroon for authenticating with the API.
    #[arg(short, long)]
    macaroon_path: String,
    /// Command to run
    #[clap(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Get information about this lightning node.
    GetInfo,
}

fn main() {
    let args = Args::parse();

    match run_command(args) {
        Ok(_) => (),
        Err(e) => println!("Error executing command: {}", e),
    }
}

fn run_command(args: Args) -> Result<()> {
    let api = Api::new(&args.target, &args.cert_path, &args.macaroon_path)?;

    match args.command {
        Command::GetInfo => {
            let info = api.get_info()?;
            println!("{}", info);
        }
    }
    Ok(())
}
