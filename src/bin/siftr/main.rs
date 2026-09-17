//! `siftr`: wrap a command, record its behaviors, and surface what changed since recent runs.

mod cmd;
mod config;
mod dispatch;
mod home;
mod output;
mod privacy;
mod project;
mod record;
mod sources;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use siftr::store::FeedbackKind;

const AFTER_HELP: &str = "\
Without a subcommand:
  siftr -- CMD…    same as siftr run -- CMD…
  siftr FILE       same as siftr ingest FILE, compared with earlier ingests of that file (./cron for a file named like a preset)
  siftr -          ingest stdin; a bare siftr does too when stdin is piped
  Any other word is an error, never a file name.

Exit codes:
  run      the command's own code; 125 if siftr fails before starting it, 126 if it can't be executed, 127 if not found
  ingest   0 recorded, 2 error
  cron     0 found jobs or cron output, 1 nothing found, 2 error
  sources  0 listed, 2 error
  queries  0 results, 1 nothing found, 2 error
  status   0 healthy, 1 something needs attention, 2 error
  ack      0 recorded, 2 error
  dismiss  0 recorded, 2 error
  gc       0 done, 2 error

Machine-readable output:
  -j prints exactly one JSON document on stdout, on every command, empty results and errors included.
  Every document's fields, and the shapes that differ between commands: docs/json.md

What siftr stores (the command's own output always passes through unchanged):
  SIFTR_REDACT=secrets  default: credentials (tokens, keys, passwords, cookies) are masked before anything is stored
  SIFTR_REDACT=pii      also emails, public IPs and home directories in raw captures and kept lines
  SIFTR_REDACT=off      raw captures keep the output as it was; the database still never holds a credential
  SIFTR_CAPTURE=off     no raw capture on disk; explain and evidence then show kept lines, not whole messages

Examples:
  siftr run -- bundle exec rspec
  siftr --quiet-unless-changed -- backup.sh   silent unless something changed, for cron, CI and git hooks
  siftr log/production.log
  siftr sources -- bundle exec rspec
  siftr cron
  siftr changes
  siftr explain s3
  siftr ack s3 -m 'fixing the N+1'";

#[derive(Parser)]
#[command(
    name = "siftr",
    version,
    about = "Record a command's behaviors and surface what changed since its recent runs.",
    after_help = AFTER_HELP,
    arg_required_else_help = true
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
    /// Preset: what runs on a schedule here, where cron's output goes, and how to record a job. Read-only
    Cron(cmd::cron::Args),
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
    /// What siftr can read here, whether each source is on, and whether it applies. Read-only
    Sources(cmd::sources::Args),
    /// What the data dir holds and what retention does about it; exits 1 when something needs attention
    Status(cmd::status::Args),
    /// Prune what's past the retention limits now, and reclaim the space
    Gc(cmd::gc::Args),
}

fn main() -> ExitCode {
    let raw: Vec<OsString> = std::env::args_os().skip(1).collect();
    let json = json_requested(raw.iter().cloned());
    let env = dispatch::Env {
        stdin_piped: stdin_piped(),
        path_kind: &path_kind,
        context_for: &ingest_context,
    };
    let args = match dispatch::dispatch(raw, &env) {
        Ok(args) => args,
        Err(error) => {
            output::error(&error, json);
            return ExitCode::from(2);
        }
    };
    let cli = match Cli::try_parse_from(std::iter::once(OsString::from("siftr")).chain(args)) {
        Ok(cli) => cli,
        // Help and --version aren't errors; an argument error under -j is still one JSON document.
        Err(error) if error.use_stderr() && json => {
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
        Command::Cron(args) => cmd::cron::run(args, &globals),
        Command::Changes(args) => cmd::changes::run(args, &globals),
        Command::Summary(args) => cmd::summary::run(args, &globals),
        Command::Evidence(args) => cmd::evidence::run(args, &globals),
        Command::Explain(args) => cmd::explain::run(args, &globals),
        Command::Ack(args) => cmd::feedback::run(FeedbackKind::Acked, "ack", args, &globals),
        Command::Dismiss(args) => {
            cmd::feedback::run(FeedbackKind::Dismissed, "dismiss", args, &globals)
        }
        Command::History(args) => cmd::history::run(args, &globals),
        Command::Sources(args) => cmd::sources::run(args, &globals),
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

/// Piped or redirected from a file. A terminal, or `/dev/null` (a character device), isn't input.
fn stdin_piped() -> bool {
    use std::os::fd::AsFd as _;
    use std::os::unix::fs::FileTypeExt as _;
    let Ok(fd) = std::io::stdin().as_fd().try_clone_to_owned() else {
        return false;
    };
    std::fs::File::from(fd).metadata().is_ok_and(|meta| {
        let kind = meta.file_type();
        kind.is_fifo() || kind.is_file() || kind.is_socket()
    })
}

fn path_kind(path: &Path) -> Option<dispatch::PathKind> {
    let meta = std::fs::metadata(path).ok()?;
    Some(match meta.is_dir() {
        true => dispatch::PathKind::Dir,
        false => dispatch::PathKind::File,
    })
}

/// A file ingested by path compares with earlier ingests of the same file, however it was spelled: its path under
/// the project, else its absolute path. One shared context would compare unrelated files with each other.
fn ingest_context(path: &Path) -> String {
    let absolute = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    project::current()
        .ok()
        .and_then(|here| {
            absolute
                .strip_prefix(&here.project)
                .ok()
                .map(Path::to_path_buf)
        })
        .unwrap_or(absolute)
        .to_string_lossy()
        .into_owned()
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
