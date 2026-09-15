//! Side channels: what a wrapped command produces somewhere other than stdout and stderr,
//! such as an RSpec listener's event file or the slice of `log/test.log` a run appended.

mod rails_log;
mod rspec;

use std::process::Command;

use anyhow::Result;
use siftr::interpret::rspec::{EVENTS_STREAM, LOG_STREAM};
use siftr::observation::Stream;

use crate::record::Recording;
use rails_log::RailsLog;
use rspec::Rspec;

/// RSpec listener events, one JSON object per line. Feed before `rails_log`: its `log_offset`s index that slice.
pub fn rspec_events() -> Stream {
    Stream::File(EVENTS_STREAM.into())
}

/// The bytes a run appended to `log/test.log`.
pub fn rails_log() -> Stream {
    Stream::File(LOG_STREAM.into())
}

pub trait SideChannel {
    /// Before spawning: set or append env vars (e.g. `SPEC_OPTS`), note pre-run state such as a log's length.
    fn prepare(&mut self, command: &mut Command) -> Result<()>;

    /// After the child exits: feed what was captured into the same recording, as `Stream::File` streams.
    fn collect(&mut self, recording: &mut Recording) -> Result<()>;
}

/// The side channels that apply to this command, run from the current directory.
pub fn for_command(argv: &[String]) -> Vec<Box<dyn SideChannel>> {
    let Some(suite) = suite(argv) else {
        return Vec::new();
    };
    let log = std::env::current_dir()
        .ok()
        .and_then(|dir| RailsLog::detect(&dir));
    match suite {
        Suite::Rspec => vec![Box::new(Rspec::new(log))],
        Suite::Other => log
            .into_iter()
            .map(|log| Box::new(log) as Box<dyn SideChannel>)
            .collect(),
    }
}

#[derive(Debug, PartialEq)]
enum Suite {
    Rspec,
    /// A Ruby test run that only the Rails log can see into, such as `rails test`.
    Other,
}

/// Only commands known to run a test suite: any other command could be attributed a concurrent run's log lines.
fn suite(argv: &[String]) -> Option<Suite> {
    let mut args = argv.iter().map(String::as_str);
    let mut program = basename(args.next()?);
    if program == "bundle" {
        if args.next()? != "exec" {
            return None;
        }
        program = basename(args.next()?);
    }
    let task = |name: &str, arg: &str| {
        arg == name
            || arg
                .strip_prefix(name)
                .is_some_and(|rest| rest.starts_with(':'))
    };
    match program {
        "rspec" => Some(Suite::Rspec),
        // `rake spec` runs rspec in a child process, which inherits SPEC_OPTS.
        "rake" | "rails" => match args.next()? {
            arg if task("spec", arg) => Some(Suite::Rspec),
            arg if task("test", arg) => Some(Suite::Other),
            _ => None,
        },
        _ => None,
    }
}

fn basename(program: &str) -> &str {
    program.rsplit('/').next().unwrap_or(program)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_ruby_test_commands_only() {
        let cases: [(&str, Option<Suite>); 12] = [
            ("rspec", Some(Suite::Rspec)),
            ("bin/rspec spec/models", Some(Suite::Rspec)),
            ("bundle exec rspec", Some(Suite::Rspec)),
            ("bundle exec rake spec", Some(Suite::Rspec)),
            ("bin/rake spec:models", Some(Suite::Rspec)),
            ("bin/rails test", Some(Suite::Other)),
            ("bundle exec rake test:system", Some(Suite::Other)),
            ("bundle install", None),
            ("rake", None),
            ("rake specs", None),
            ("cargo test", None),
            ("bin/rails server", None),
        ];
        for (command, expected) in cases {
            let argv: Vec<String> = command.split(' ').map(str::to_owned).collect();
            assert_eq!(suite(&argv), expected, "{command}");
        }
    }
}
