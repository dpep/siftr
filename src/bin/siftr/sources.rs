//! Where a run's lines come from: the command's own stdout and stderr always, plus the side channels it writes
//! somewhere else — an RSpec listener's events, the slice of `log/test.log` a run appended.
//!
//! A source is named by the stream it feeds, so `siftr sources`, a run's `-j` `sources` and an exemplar's
//! `stream` all say the same word.

mod rails_log;
mod rspec;

use std::path::Path;
use std::process::Command;

use anyhow::Result;
use siftr::interpret::rspec::{EVENTS_STREAM, LOG_STREAM};
use siftr::observation::Stream;

use crate::record::Recording;
use rails_log::RailsLog;
use rspec::Rspec;

/// The command's own output, which every run reads.
const STDOUT: &str = "stdout";
const STDERR: &str = "stderr";

/// RSpec listener events, one JSON object per line. Feed before `rails_log`: its `log_offset`s index that slice.
pub fn rspec_events() -> Stream {
    Stream::File(EVENTS_STREAM.into())
}

/// The bytes a run appended to `log/test.log`.
pub fn rails_log() -> Stream {
    Stream::File(LOG_STREAM.into())
}

/// The source a stream came from, as everything siftr prints names it. An exemplar's `stream` tags a side
/// channel with `file:`, since it must parse back into a [`Stream`]; a source name never does.
pub fn name_of(stream: &Stream) -> &str {
    match stream {
        Stream::Stdout => STDOUT,
        Stream::Stderr => STDERR,
        Stream::File(name) => name,
    }
}

/// What a wrapped command produces somewhere other than stdout and stderr.
pub trait Source {
    /// As `siftr sources` lists it and a run's `-j` `sources` reports it.
    fn name(&self) -> &'static str;

    /// Before spawning: set or append env vars (e.g. `SPEC_OPTS`), note pre-run state such as a log's length.
    fn prepare(&mut self, command: &mut Command) -> Result<()>;

    /// After the child exits: feed what was captured into the same recording, as `Stream::File` streams.
    fn collect(&mut self, recording: &mut Recording) -> Result<()>;
}

/// Which sources siftr may use, whether or not they apply to a given command.
#[derive(Debug, Clone, Copy)]
pub struct Enabled {
    pub rspec: bool,
    pub rails_log: bool,
}

impl Default for Enabled {
    fn default() -> Self {
        Enabled {
            rspec: true,
            rails_log: true,
        }
    }
}

/// The one place a source is switched on or off; configuration replaces this body.
pub fn enabled() -> Enabled {
    Enabled::default()
}

/// The sources that apply to this command in `dir`, ready to prepare.
pub fn for_command(argv: &[String], dir: &Path, on: &Enabled) -> Vec<Box<dyn Source>> {
    let Some(suite) = suite(argv) else {
        return Vec::new();
    };
    let log = on.rails_log.then(|| RailsLog::detect(dir)).flatten();
    match suite {
        // The listener's events carry offsets into the log, so one source owns both and feeds them in order.
        Suite::Rspec if on.rspec => vec![Box::new(Rspec::new(log))],
        _ => log
            .into_iter()
            .map(|log| Box::new(log) as Box<dyn Source>)
            .collect(),
    }
}

/// One source as `siftr sources` lists it.
pub struct Listed {
    pub name: &'static str,
    /// What it reads.
    pub about: &'static str,
    pub on: bool,
    /// Whether it would capture anything for this command here.
    pub applies: bool,
    /// Why it applies, or why it doesn't.
    pub why: &'static str,
}

/// Every source siftr knows: whether it's on, and whether it applies to `argv` in `dir`. An empty `argv` asks
/// about the directory alone, which can't settle the sources that depend on the command.
pub fn survey(argv: &[String], dir: &Path, on: &Enabled) -> Vec<Listed> {
    let suite = suite(argv);
    let given = !argv.is_empty();
    let log = RailsLog::detect(dir);
    let passthrough = |name| Listed {
        name,
        about: "the command's own output",
        on: true,
        applies: true,
        why: "always read",
    };
    let (rspec_applies, rspec_why) = match (given, &suite) {
        (false, _) => (false, "no command given"),
        (_, Some(Suite::Rspec)) => (true, "the command runs rspec"),
        _ => (false, "the command isn't an rspec run"),
    };
    let (log_applies, log_why) = match (given, &suite, &log) {
        (false, ..) => (false, "no command given"),
        (_, None, _) => (false, "the command isn't a Ruby test run"),
        (_, Some(_), None) => (false, "no log/test.log, and no Gemfile naming rails"),
        (_, Some(_), Some(log)) if log.path().is_file() => (true, "log/test.log is there"),
        (_, Some(_), Some(_)) => (true, "log/ is there and the Gemfile names rails"),
    };
    vec![
        passthrough(STDOUT),
        passthrough(STDERR),
        Listed {
            name: EVENTS_STREAM,
            about: "RSpec's per-example results, from a listener added to SPEC_OPTS",
            on: on.rspec,
            applies: rspec_applies,
            why: rspec_why,
        },
        Listed {
            name: LOG_STREAM,
            about: "the SQL and request lines the run appends to the Rails test log",
            on: on.rails_log,
            applies: log_applies,
            why: log_why,
        },
    ]
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

    fn rails_project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("log")).unwrap();
        std::fs::write(dir.path().join("log/test.log"), "").unwrap();
        dir
    }

    fn names(sources: &[Box<dyn Source>]) -> Vec<&str> {
        sources.iter().map(|source| source.name()).collect()
    }

    #[test]
    fn a_source_switched_off_is_never_prepared() {
        let dir = rails_project();
        let argv: Vec<String> = ["bundle", "exec", "rspec"].map(str::to_owned).to_vec();
        let all = Enabled::default();
        assert_eq!(
            names(&for_command(&argv, dir.path(), &all)),
            [EVENTS_STREAM]
        );

        // The rspec source owns the log slice its offsets index, so without it the log is read on its own.
        let no_rspec = Enabled {
            rspec: false,
            ..all
        };
        assert_eq!(
            names(&for_command(&argv, dir.path(), &no_rspec)),
            [LOG_STREAM]
        );
        assert!(
            for_command(
                &argv,
                dir.path(),
                &Enabled {
                    rspec: false,
                    rails_log: false
                }
            )
            .is_empty()
        );

        let listed = survey(&argv, dir.path(), &no_rspec);
        let off: Vec<&str> = listed.iter().filter(|s| !s.on).map(|s| s.name).collect();
        assert_eq!(off, [EVENTS_STREAM]);
        assert!(
            listed.iter().all(|s| s.applies),
            "being off is not the same as not applying here"
        );
    }

    #[test]
    fn what_applies_depends_on_the_command_as_well_as_the_directory() {
        let rails = rails_project();
        let bare = tempfile::tempdir().unwrap();
        let on = Enabled::default();
        let applies = |argv: &[&str], dir: &Path| -> Vec<&'static str> {
            let argv: Vec<String> = argv.iter().map(|a| (*a).to_owned()).collect();
            survey(&argv, dir, &on)
                .into_iter()
                .filter(|s| s.applies)
                .map(|s| s.name)
                .collect()
        };
        assert_eq!(
            applies(&["bundle", "exec", "rspec"], rails.path()),
            [STDOUT, STDERR, EVENTS_STREAM, LOG_STREAM]
        );
        assert_eq!(
            applies(&["bundle", "exec", "rspec"], bare.path()),
            [STDOUT, STDERR, EVENTS_STREAM],
            "no Rails log outside a Rails project"
        );
        assert_eq!(
            applies(&["bin/rails", "test"], rails.path()),
            [STDOUT, STDERR, LOG_STREAM],
            "only the log can see into a non-rspec suite"
        );
        assert_eq!(
            applies(&["ls"], rails.path()),
            [STDOUT, STDERR],
            "another command's lines are never attributed to this run"
        );
        assert_eq!(applies(&[], rails.path()), [STDOUT, STDERR]);
    }
}
