//! Where a run's lines come from: the command's own stdout and stderr always, plus the side channels it writes
//! somewhere else — an RSpec listener's events, the slice of `log/test.log` a run appended.
//!
//! A source is named by its configuration key, so the word read in `siftr sources` is the word typed in
//! `.siftr.toml`. What it feeds is a stream, named separately: interpreters match on stream labels, and a run's
//! `-j` `streams` reports the ones that arrived.

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

/// A source's name, which is also its configuration key.
pub const RSPEC: &str = "rspec";
pub const RAILS_LOG: &str = "rails_log";

/// Every source configuration can switch, in listing order. The command's own output isn't one of them: siftr
/// always reads it. Configuration validates its keys against this list.
pub const SOURCES: &[&str] = &[RSPEC, RAILS_LOG];

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

/// What a wrapped command produces somewhere other than stdout and stderr.
pub trait Source {
    /// Its name and configuration key, as `siftr sources` lists it.
    fn name(&self) -> &'static str;

    /// Before spawning: set or append env vars (e.g. `SPEC_OPTS`), note pre-run state such as a log's length.
    fn prepare(&mut self, command: &mut Command) -> Result<()>;

    /// After the child exits: feed what was captured into the same recording, as `Stream::File` streams.
    fn collect(&mut self, recording: &mut Recording) -> Result<()>;
}

/// Which sources siftr may use, whether or not they apply to a given command. One field per [`SOURCES`] key.
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

/// The one place a source is switched on or off: what `.siftr.toml` says, defaulting to on.
pub fn enabled() -> Enabled {
    let config = crate::config::Config::load();
    Enabled {
        rspec: config.source_enabled(RSPEC),
        rails_log: config.source_enabled(RAILS_LOG),
    }
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
    /// Its name and configuration key.
    pub name: &'static str,
    /// The stream it feeds, as an exemplar's `stream` and a run's `streams` spell it.
    pub stream: Stream,
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
    let passthrough = |name, stream| Listed {
        name,
        stream,
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
        passthrough(STDOUT, Stream::Stdout),
        passthrough(STDERR, Stream::Stderr),
        Listed {
            name: RSPEC,
            stream: rspec_events(),
            about: "RSpec's per-example results, from a listener added to SPEC_OPTS",
            on: on.rspec,
            applies: rspec_applies,
            why: rspec_why,
        },
        Listed {
            name: RAILS_LOG,
            stream: rails_log(),
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

    fn rspec_argv() -> Vec<String> {
        ["bundle", "exec", "rspec"].map(str::to_owned).to_vec()
    }

    #[test]
    fn a_source_switched_off_is_never_prepared() {
        let dir = rails_project();
        let argv = rspec_argv();
        let all = Enabled::default();
        assert_eq!(names(&for_command(&argv, dir.path(), &all)), [RSPEC]);

        // The rspec source owns the log slice its offsets index, so without it the log is read on its own.
        let no_rspec = Enabled {
            rspec: false,
            ..all
        };
        assert_eq!(
            names(&for_command(&argv, dir.path(), &no_rspec)),
            [RAILS_LOG]
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
        assert_eq!(off, [RSPEC]);
        assert!(
            listed.iter().all(|s| s.applies),
            "being off is not the same as not applying here"
        );
    }

    /// The config lane reads [`SOURCES`], so a source added to [`Enabled`] and left out of it would be
    /// unswitchable. With everything off, exactly the listed keys read off.
    #[test]
    fn sources_are_exactly_the_keys_configuration_can_switch() {
        let dir = rails_project();
        let off = Enabled {
            rspec: false,
            rails_log: false,
        };
        let switched: Vec<&str> = survey(&rspec_argv(), dir.path(), &off)
            .into_iter()
            .filter(|s| !s.on)
            .map(|s| s.name)
            .collect();
        assert_eq!(switched, SOURCES);
    }

    #[test]
    fn a_source_is_named_by_its_key_and_says_which_stream_it_feeds() {
        let dir = rails_project();
        let listed = survey(&rspec_argv(), dir.path(), &Enabled::default());
        let named: Vec<(&str, String)> = listed
            .iter()
            .map(|s| (s.name, s.stream.to_string()))
            .collect();
        assert_eq!(
            named,
            [
                ("stdout", "stdout".to_owned()),
                ("stderr", "stderr".to_owned()),
                ("rspec", "file:rspec-events".to_owned()),
                ("rails_log", "file:log/test.log".to_owned()),
            ],
            "the key is what configuration takes; the stream is what evidence points at"
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
            [STDOUT, STDERR, RSPEC, RAILS_LOG]
        );
        assert_eq!(
            applies(&["bundle", "exec", "rspec"], bare.path()),
            [STDOUT, STDERR, RSPEC],
            "no Rails log outside a Rails project"
        );
        assert_eq!(
            applies(&["bin/rails", "test"], rails.path()),
            [STDOUT, STDERR, RAILS_LOG],
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
