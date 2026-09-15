//! `siftr`: wrap a command, record its behaviors, and surface what changed since recent runs.

mod cmd;
mod home;
mod output;
mod project;
mod record;
mod sidechannel;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use siftr_store::FeedbackKind;

const AFTER_HELP: &str = "\
Exit codes:
  run      the command's own code; 125 if siftr fails before starting it, 126 if it can't be executed, 127 if not found
  ingest   0 recorded, 2 error
  queries  0 results, 1 nothing found, 2 error
  status   0 healthy, 1 something needs attention, 2 error
  ack      0 recorded, 2 error
  dismiss  0 recorded, 2 error
  gc       0 done, 2 error

Examples:
  siftr run -- bundle exec rspec
  siftr changes
  siftr explain s3
  siftr ack s3 -m 'fixing the N+1'";

#[derive(Parser)]
#[command(
    name = "siftr",
    version,
    about = "Record a command's behaviors and surface what changed since its recent runs.",
    after_help = AFTER_HELP
)]
struct Cli {
    /// Data directory [default: $XDG_DATA_HOME/siftr, else ~/.local/share/siftr]
    #[arg(long, env = "SIFTR_HOME", global = true, value_name = "DIR")]
    home: Option<PathBuf>,

    /// Print JSON to stdout, errors and empty results included
    #[arg(short = 'j', long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a command, passing its output through, and record what it did
    Run(cmd::run::Args),
    /// Record a file or stdin as if it were a command's output
    Ingest(cmd::ingest::Args),
    /// Behavioral changes in a run
    Changes(cmd::changes::Args),
    /// A run's top behaviors by count or time
    Summary(cmd::summary::Args),
    /// Raw lines kept as evidence for a behavior
    Evidence(cmd::evidence::Args),
    /// A signal's current and baseline numbers, and its evidence
    Explain(cmd::explain::Args),
    /// Mark a signal as being acted on
    Ack(cmd::feedback::Args),
    /// Mark a signal as not worth acting on
    Dismiss(cmd::feedback::Args),
    /// Runs recorded in this project
    History(cmd::history::Args),
    /// What the data dir holds and what retention does about it; exits 1 when something needs attention
    Status(cmd::status::Args),
    /// Prune what's past the retention limits now, and reclaim the space
    Gc(cmd::gc::Args),
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        // Help and --version aren't errors; an argument error under -j is still one JSON document.
        Err(error) if error.use_stderr() && json_requested(std::env::args_os().skip(1)) => {
            output::report_error(output::ErrorCode::Usage, &clap_message(&error), true);
            return ExitCode::from(2);
        }
        Err(error) => error.exit(),
    };
    let globals = cmd::Globals {
        home: cli.home,
        json: cli.json,
    };
    let result = match cli.command {
        // `run` owns its exit code: the child's, or siftr's own 125/126/127.
        Command::Run(args) => return cmd::run::run(args, &globals),
        Command::Ingest(args) => cmd::ingest::run(args, &globals),
        Command::Changes(args) => cmd::changes::run(args, &globals),
        Command::Summary(args) => cmd::summary::run(args, &globals),
        Command::Evidence(args) => cmd::evidence::run(args, &globals),
        Command::Explain(args) => cmd::explain::run(args, &globals),
        Command::Ack(args) => cmd::feedback::run(FeedbackKind::Acked, "ack", args, &globals),
        Command::Dismiss(args) => {
            cmd::feedback::run(FeedbackKind::Dismissed, "dismiss", args, &globals)
        }
        Command::History(args) => cmd::history::run(args, &globals),
        Command::Status(args) => cmd::status::run(args, &globals),
        Command::Gc(args) => cmd::gc::run(args, &globals),
    };
    result.unwrap_or_else(|error| {
        output::error(&error, globals.json);
        ExitCode::from(2)
    })
}

/// Whether the arguments ask for JSON, read before clap parses them so an argument error can honor it.
/// Stops at `--`: what follows belongs to the command `run` wraps.
fn json_requested(args: impl IntoIterator<Item = OsString>) -> bool {
    args.into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .take_while(|arg| arg != "--")
        .any(|arg| {
            arg == "--json"
                || arg.strip_prefix('-').is_some_and(|flags| {
                    flags.bytes().all(|b| b.is_ascii_alphabetic()) && flags.contains('j')
                })
        })
}

/// clap's message, one line, without its `error:` prefix and usage footer.
fn clap_message(error: &clap::Error) -> String {
    let text = error.to_string();
    let head = text.split("\n\n").next().unwrap_or_default();
    let head = head.strip_prefix("error: ").unwrap_or(head);
    head.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_is_requested_by_a_flag_before_the_wrapped_command() {
        let cases: [(&[&str], bool); 6] = [
            (&["-j", "changes", "bogus"], true),
            (&["changes", "--json"], true),
            (&["run", "-qj", "--", "rspec"], true),
            (&["run", "--", "sh", "-j"], false),
            (&["summary", "-n5"], false),
            (&["ack", "s1", "-m", "jq"], false),
        ];
        for (args, expected) in cases {
            assert_eq!(
                json_requested(args.iter().map(OsString::from)),
                expected,
                "{args:?}"
            );
        }
    }
}
