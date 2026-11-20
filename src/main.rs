mod activity;
mod case;
mod cli;
mod collect;
mod diff;
mod export;
mod list;
mod output;
mod refs;
mod scan;
mod usage;
mod watch;
mod win;

use clap::Parser;

fn main() {
    let cli = cli::Cli::parse();
    let pretty = cli.pretty;
    let result = match cli.command {
        cli::Command::Scan(args) => scan::run(args),
        cli::Command::Diff(args) => diff::run(args),
        cli::Command::List(args) => list::run(args),
        cli::Command::Collect(args) => collect::run(args),
        cli::Command::Usage(args) => usage::run(args),
        cli::Command::Refs(args) => refs::run(args),
        cli::Command::Activity(args) => activity::run(args),
        cli::Command::Watch(args) => watch::run(args),
        cli::Command::Export(args) => export::run(args),
    };
    match result {
        Ok(Some(value)) => output::print(&value, pretty),
        Ok(None) => {}
        Err(fail) => {
            eprintln!("zero-surplus: {}", fail.message);
            output::print(&fail.to_json(), pretty);
            std::process::exit(fail.code);
        }
    }
}
