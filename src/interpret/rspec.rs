//! RSpec's side channel. The listener's ndjson events become `test.example`, `exception` and
//! `test.summary` behaviors, and each example's `log_offset` range scopes the Rails log slice.
//!
//! The log slice is interpreted here rather than by its own interpreter because its scopes come
//! from these events, which is also why [`EVENTS_STREAM`] must be fed before [`LOG_STREAM`].

use std::time::Duration;

use serde::Deserialize;

use super::{Claim, Event, Interpreter, Outcome, generic, literal, rails};
use crate::aggregate::{Aggregator, Phase};
use crate::behavior::{BehaviorId, Kind};
use crate::normalize::Normalizer;
use crate::normalize::secrets::unnumber;
use crate::observation::{Observation, Stream};

/// `Stream::File` name of the listener's ndjson events.
pub const EVENTS_STREAM: &str = "rspec-events";
/// `Stream::File` name of the run's Rails log slice; `log_offset`s count bytes from its first byte.
pub const LOG_STREAM: &str = "log/test.log";

/// Measures of the `test.summary` behavior.
pub mod summary {
    /// Examples that ran, pending ones included.
    pub const EXAMPLES: &str = "examples";
    /// Examples loaded to run, from the reporter's start: more than [`EXAMPLES`] when the run stopped early.
    pub const EXPECTED: &str = "expected";
    /// Every example in the files that loaded, before filters: more than [`EXPECTED`] under a focus filter.
    pub const DEFINED: &str = "defined";
    pub const FAILURES: &str = "failures";
    pub const PENDING: &str = "pending";
    /// Load errors and hook errors: RSpec still runs the other files when one fails to load.
    pub const ERRORS_OUTSIDE_OF_EXAMPLES: &str = "errors_outside_of_examples";
}

/// Identity of a `test.example`: `<spec file> # <full description>`.
///
/// Not the RSpec id (`[1:2]` renumbers when an example is inserted above) nor the line number
/// (shifts on any edit above). Examples sharing a file and description are one behavior whose
/// count is their number: ordinals would swap between them under random order.
#[derive(Default)]
pub struct Rspec {
    scopes: Scopes,
    /// The running example's RSpec id and the log offset it started at.
    started: Option<(String, u64)>,
    /// The reporter's start, until its summary.
    start: Start,
    log: rails::Log,
    /// Offset of the next log line.
    log_offset: u64,
    template: Vec<u8>,
}

impl Interpreter for Rspec {
    fn observe(
        &mut self,
        obs: Observation<'_>,
        normalizer: &mut Normalizer,
        sink: &mut Aggregator,
    ) -> Claim {
        self.interpret(obs, normalizer, &mut |event| sink.record(event))
    }
}

impl Rspec {
    pub(super) fn interpret(
        &mut self,
        obs: Observation<'_>,
        normalizer: &mut Normalizer,
        emit: &mut impl FnMut(&Event<'_>),
    ) -> Claim {
        let Stream::File(name) = obs.stream else {
            return Claim::Declined;
        };
        match &**name {
            EVENTS_STREAM => self.event(obs, normalizer, emit),
            LOG_STREAM => {
                let phase = self.scopes.at(self.log_offset);
                self.log_offset += obs.raw_len;
                self.log.line(obs, phase.scope_id(), normalizer, emit);
            }
            _ => return Claim::Declined,
        }
        Claim::Claimed
    }

    fn event(
        &mut self,
        obs: Observation<'_>,
        normalizer: &mut Normalizer,
        emit: &mut impl FnMut(&Event<'_>),
    ) {
        let Ok(record) = serde_json::from_slice::<Record>(obs.line) else {
            // Not the listener's shape: keep it as evidence rather than drop it.
            return generic::log(obs, None, normalizer, emit);
        };
        match record {
            Record::Start(start) => self.start = start,
            Record::ExampleStarted { id, log_offset } => {
                self.started = log_offset.map(|offset| (id, offset));
            }
            Record::Example(example) => self.example(obs, &example, emit),
            Record::Summary(summary) => summary.emit(obs, std::mem::take(&mut self.start), emit),
            Record::ErrorOutsideExamples {
                context,
                class,
                message,
            } => {
                self.template.clear();
                match loaded_file(&context) {
                    Some(file) => {
                        unnumber(file.as_bytes(), &mut self.template);
                        self.template.extend_from_slice(b" failed to load");
                    }
                    None => unnumber(context.trim_end_matches('.').as_bytes(), &mut self.template),
                }
                let cause = message.as_deref().and_then(cause);
                for part in [class.as_deref(), cause.as_deref()].into_iter().flatten() {
                    self.template.extend_from_slice(b": ");
                    unnumber(part.as_bytes(), &mut self.template);
                }
                emit(&Event {
                    kind: Kind::Exception,
                    template: literal(&self.template),
                    input: &self.template,
                    source: obs,
                    duration: None,
                    outcome: Some(Outcome::Failure),
                    scope: None,
                    measures: &[],
                });
            }
            Record::Other => {}
        }
    }

    fn example(
        &mut self,
        obs: Observation<'_>,
        example: &Example,
        emit: &mut impl FnMut(&Event<'_>),
    ) {
        self.template.clear();
        // Event lines arrive redacted, their placeholders numbered per run.
        unnumber(example.spec_file().as_bytes(), &mut self.template);
        self.template.extend_from_slice(b" # ");
        unnumber(example.full_description.as_bytes(), &mut self.template);
        let id = BehaviorId::of(Kind::TestExample, &self.template);
        if let (Some((started, start)), Some(end)) = (self.started.take(), example.log_offset)
            && started == example.id
        {
            self.scopes.push(start, end, id);
        }
        emit(&Event {
            kind: Kind::TestExample,
            template: literal(&self.template),
            input: &self.template,
            source: obs,
            duration: seconds(example.run_time),
            outcome: Some(example.status.into()),
            scope: None,
            measures: &[],
        });
        if let Some(exception) = &example.exception {
            let class = exception.class.as_bytes();
            emit(&Event {
                kind: Kind::Exception,
                template: literal(class),
                input: class,
                source: obs,
                duration: None,
                outcome: Some(Outcome::Failure),
                scope: Some(id),
                measures: &[],
            });
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Record {
    Start(Start),
    ExampleStarted {
        id: String,
        log_offset: Option<u64>,
    },
    Example(Example),
    Summary(Summary),
    /// A spec file that failed to load, or a suite or context hook that raised.
    ErrorOutsideExamples {
        /// RSpec's own first line, e.g. `An error occurred while loading ./spec/a_spec.rb.`
        context: String,
        class: Option<String>,
        message: Option<String>,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct Example {
    id: String,
    full_description: String,
    file_path: String,
    status: Status,
    run_time: f64,
    exception: Option<ExceptionInfo>,
    log_offset: Option<u64>,
}

impl Example {
    /// The spec file that ran the example, from its id; `file_path` names a shared group's own file instead.
    fn spec_file(&self) -> &str {
        self.id
            .split_once('[')
            .map_or(self.file_path.as_str(), |(file, _)| file)
    }
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum Status {
    Passed,
    Failed,
    Pending,
}

impl From<Status> for Outcome {
    fn from(status: Status) -> Self {
        match status {
            Status::Passed => Outcome::Success,
            Status::Failed => Outcome::Failure,
            Status::Pending => Outcome::Skipped,
        }
    }
}

#[derive(Deserialize)]
struct ExceptionInfo {
    class: String,
}

/// What the reporter's start said; `None` from a listener that predates the field.
#[derive(Deserialize, Default)]
struct Start {
    expected: Option<u64>,
    defined: Option<u64>,
}

#[derive(Deserialize)]
struct Summary {
    duration: f64,
    examples: u64,
    failures: u64,
    pending: u64,
    errors_outside_of_examples: u64,
}

impl Summary {
    const TEMPLATE: &[u8] = b"rspec";

    fn emit(&self, obs: Observation<'_>, start: Start, emit: &mut impl FnMut(&Event<'_>)) {
        let failed = self.failures > 0 || self.errors_outside_of_examples > 0;
        let measures = [
            (summary::EXAMPLES, self.examples as f64),
            (summary::FAILURES, self.failures as f64),
            (summary::PENDING, self.pending as f64),
            (
                summary::ERRORS_OUTSIDE_OF_EXAMPLES,
                self.errors_outside_of_examples as f64,
            ),
        ];
        let from_start = [
            (summary::EXPECTED, start.expected),
            (summary::DEFINED, start.defined),
        ]
        .into_iter()
        .filter_map(|(name, count)| Some((name, count? as f64)));
        let measures: Vec<(&'static str, f64)> = measures.into_iter().chain(from_start).collect();
        emit(&Event {
            kind: Kind::TestSummary,
            template: literal(Self::TEMPLATE),
            input: Self::TEMPLATE,
            source: obs,
            duration: seconds(self.duration),
            outcome: Some(if failed {
                Outcome::Failure
            } else {
                Outcome::Success
            }),
            scope: None,
            measures: &measures,
        });
    }
}

/// The spec file RSpec names in a load error's context: `While loading ./spec/a_spec.rb a …`,
/// `An error occurred while loading ./spec/a_spec.rb.`
fn loaded_file(context: &str) -> Option<&str> {
    let (_, rest) = context.split_once("loading ")?;
    let file = rest.split_whitespace().next()?.trim_end_matches('.');
    (!file.is_empty()).then_some(file)
}

/// Longest cause kept in a template: it names a behavior, so it stays one line of what went wrong.
const MAX_CAUSE_CHARS: usize = 160;

/// What went wrong, in one line of an exception message. Ruby's parser points at the source with `^~~` lines,
/// the last naming the root (`expected a block beginning with \`do\` to end with \`end\``); other messages
/// lead with it, after any `path:line:` prefix.
fn cause(message: &str) -> Option<String> {
    let caret = message.lines().rev().find_map(|line| {
        let pointed = line.trim_start().strip_prefix('|')?.trim_start();
        pointed
            .starts_with('^')
            .then(|| pointed.trim_start_matches(['^', '~']).trim())
    });
    let first = || {
        let line = message
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())?;
        let unprefixed = line.split_once(": ").and_then(|(head, rest)| {
            let (_, number) = head.rsplit_once(':')?;
            number.bytes().all(|b| b.is_ascii_digit()).then_some(rest)
        });
        Some(unprefixed.unwrap_or(line))
    };
    let cause = caret.filter(|c| !c.is_empty()).or_else(first)?;
    Some(cause.chars().take(MAX_CAUSE_CHARS).collect())
}

fn seconds(secs: f64) -> Option<Duration> {
    Duration::try_from_secs_f64(secs).ok()
}

/// Each example's `[start, finish)` log range, in execution order.
#[derive(Default)]
struct Scopes(Vec<Scope>);

struct Scope {
    start: u64,
    end: u64,
    example: BehaviorId,
}

impl Scopes {
    fn push(&mut self, start: u64, end: u64, example: BehaviorId) {
        // An overlap means concurrent writers (e.g. parallel_tests): leave it unscoped rather than misattribute.
        if start <= end && self.0.last().is_none_or(|last| last.end <= start) {
            self.0.push(Scope {
                start,
                end,
                example,
            });
        }
    }

    fn at(&self, offset: u64) -> Phase {
        let i = self.0.partition_point(|scope| scope.end <= offset);
        match self.0.get(i) {
            Some(scope) if scope.start <= offset => Phase::Example(scope.example),
            _ if i == 0 => Phase::Setup,
            None => Phase::Teardown,
            Some(_) => Phase::Between,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::interpret;
    use super::*;
    use crate::observation::MAX_LINE;

    fn example(id: &str, description: &str, line: u32, status: &str) -> String {
        format!(
            r#"{{"event":"example","id":"./spec/models/user_spec.rb[{id}]","description":"-","full_description":"User {description}","file_path":"./spec/models/user_spec.rb","line_number":{line},"status":"{status}","run_time":0.001}}"#
        )
    }

    fn ids(ndjson: &[String]) -> Vec<BehaviorId> {
        interpret(&[(EVENTS_STREAM, ndjson.join("\n").as_bytes())])
            .iter()
            .map(|seen| seen.id())
            .collect()
    }

    #[test]
    fn inserting_an_example_above_another_keeps_both_ids() {
        let before = ids(&[
            example("1:1", "requires a name", 4, "passed"),
            example("1:2", "requires an email", 8, "passed"),
        ]);
        // Every id and line number below the insertion shifts; the behaviors must not.
        let after = ids(&[
            example("1:1", "has a nickname", 4, "passed"),
            example("1:2", "requires a name", 8, "passed"),
            example("1:3", "requires an email", 12, "passed"),
        ]);
        assert_eq!(before, after[1..]);
        assert!(!before.contains(&after[0]));
    }

    #[test]
    fn examples_sharing_a_file_and_description_are_one_behavior() {
        let seen = interpret(&[(
            EVENTS_STREAM,
            [
                example("1:1", "is valid", 4, "passed"),
                example("1:2", "is valid", 8, "failed"),
                example("1:3", "is pending", 12, "pending"),
            ]
            .join("\n")
            .as_bytes(),
        )]);
        let shape: Vec<_> = seen
            .iter()
            .map(|s| (s.template.as_str(), s.outcome))
            .collect();
        assert_eq!(
            shape,
            [
                (
                    "./spec/models/user_spec.rb # User is valid",
                    Some(Outcome::Success)
                ),
                (
                    "./spec/models/user_spec.rb # User is valid",
                    Some(Outcome::Failure)
                ),
                (
                    "./spec/models/user_spec.rb # User is pending",
                    Some(Outcome::Skipped)
                ),
            ]
        );
    }

    #[test]
    fn log_lines_take_the_scope_of_the_example_whose_range_holds_their_first_byte() {
        let events = [
            r#"{"event":"start","log_offset":0}"#,
            r#"{"event":"example_started","id":"./a_spec.rb[1:1]","log_offset":6}"#,
            r#"{"event":"example","id":"./a_spec.rb[1:1]","full_description":"A first","file_path":"./a_spec.rb","status":"passed","run_time":0.1,"log_offset":26}"#,
            r#"{"event":"example_started","id":"./a_spec.rb[1:2]","log_offset":26}"#,
            r#"{"event":"example","id":"./a_spec.rb[1:2]","full_description":"A second","file_path":"./a_spec.rb","status":"passed","run_time":0.1,"log_offset":36}"#,
        ]
        .join("\n");
        let log = "setup\nin first\nalso first\nin second\n";
        let seen = interpret(&[
            (EVENTS_STREAM, events.as_bytes()),
            (LOG_STREAM, log.as_bytes()),
        ]);
        let first = BehaviorId::of(Kind::TestExample, b"./a_spec.rb # A first");
        let second = BehaviorId::of(Kind::TestExample, b"./a_spec.rb # A second");
        let scopes: Vec<_> = seen
            .iter()
            .filter(|s| s.kind == Kind::Log)
            .map(|s| (s.template.as_str(), s.scope))
            .collect();
        assert_eq!(
            scopes,
            [
                ("setup", None),
                ("in first", Some(first)),
                ("also first", Some(first)),
                ("in second", Some(second)),
            ]
        );
    }

    /// The phase of each log line in `parts`: `(Some(name), bytes)` is inside example `A <name>`, `(None, bytes)` outside.
    fn phases(parts: &[(Option<&str>, &[u8])]) -> Vec<Phase> {
        let mut events = Vec::new();
        let mut offset = 0;
        for (i, (example, bytes)) in (1..).zip(parts) {
            let end = offset + bytes.len();
            if let Some(name) = example {
                let id = format!("./a_spec.rb[1:{i}]");
                events.push(format!(
                    r#"{{"event":"example_started","id":"{id}","log_offset":{offset}}}"#
                ));
                events.push(format!(
                    r#"{{"event":"example","id":"{id}","full_description":"A {name}","file_path":"./a_spec.rb","status":"passed","run_time":0.1,"log_offset":{end}}}"#
                ));
            }
            offset = end;
        }
        let log: Vec<u8> = parts
            .iter()
            .flat_map(|(_, bytes)| bytes.iter().copied())
            .collect();
        interpret(&[
            (EVENTS_STREAM, events.join("\n").as_bytes()),
            (LOG_STREAM, &log),
        ])
        .into_iter()
        .filter(|s| s.kind == Kind::Log)
        .map(|s| Phase::from_scope_id(s.scope))
        .collect()
    }

    fn example_named(name: &str) -> Phase {
        Phase::Example(BehaviorId::of(
            Kind::TestExample,
            format!("./a_spec.rb # A {name}").as_bytes(),
        ))
    }

    #[test]
    fn lines_outside_examples_are_setup_between_or_teardown() {
        let got = phases(&[
            (None, b"boot\n"),
            (Some("first"), b"in first\n"),
            (None, b"before context\n"),
            (Some("second"), b"in second\n"),
            (None, b"after suite\n"),
        ]);
        let expected = [
            Phase::Setup,
            example_named("first"),
            Phase::Between,
            example_named("second"),
            Phase::Teardown,
        ];
        assert_eq!(got, expected);
    }

    #[test]
    fn crlf_and_overlong_lines_keep_later_lines_in_their_own_example() {
        let long = [vec![b'x'; MAX_LINE + 10], b"\n".to_vec()].concat();
        let bodies: [(&str, &[u8]); 2] = [("crlf", b"one\r\ntwo\r\nthree\r\n"), ("long", &long)];
        for (name, body) in bodies {
            let got = phases(&[
                (None, b"boot\n"),
                (Some("first"), body),
                (Some("second"), b"in second\n"),
            ]);
            assert_eq!(got.last(), Some(&example_named("second")), "{name}");
        }
    }

    #[test]
    fn the_summary_carries_counts_and_fails_on_errors_outside_examples() {
        let events = [
            r#"{"event":"start","expected":4,"defined":6,"load_time":0.8}"#,
            r#"{"event":"summary","duration":0.5,"load_time":0.8,"examples":3,"failures":0,"pending":1,"errors_outside_of_examples":1}"#,
        ]
        .join("\n");
        let [seen] = interpret(&[(EVENTS_STREAM, events.as_bytes())])
            .try_into()
            .expect("one event");
        assert_eq!(seen.kind, Kind::TestSummary);
        assert_eq!(seen.outcome, Some(Outcome::Failure));
        assert_eq!(seen.duration, Some(Duration::from_millis(500)));
        assert_eq!(
            seen.measures,
            [
                ("examples", 3.0),
                ("failures", 0.0),
                ("pending", 1.0),
                ("errors_outside_of_examples", 1.0),
                ("expected", 4.0),
                ("defined", 6.0)
            ]
        );
    }

    #[test]
    fn a_load_error_is_named_by_its_file_class_and_cause() {
        // A spec missing its `end`, as Ruby 3.4's parser reports it through the listener.
        let missing_end = r#"{"event":"error_outside_examples","context":"While loading ./spec/models/post_spec.rb a `raise SyntaxError` occurred, RSpec will now quit.","class":"SyntaxError","message":"/project/spec/models/post_spec.rb:5: syntax errors found\n  3 |     expect(1).to eq 1\n  4 | \n> 5 | end\n    |    ^ unexpected end-of-input, assuming it is closing the parent top level context\n> 6 | \n    | ^ expected a block beginning with `do` to end with `end`\n"}"#;
        let hook = r#"{"event":"error_outside_examples","context":"An error occurred in an `after(:suite)` hook.","class":"RuntimeError","message":"after suite boom"}"#;
        let missing_file = r#"{"event":"error_outside_examples","context":"An error occurred while loading ./spec/gone_spec.rb.","class":"LoadError","message":"/project/spec/gone_spec.rb:1: cannot load such file -- gone"}"#;
        let templates: Vec<String> = interpret(&[(
            EVENTS_STREAM,
            [missing_end, hook, missing_file].join("\n").as_bytes(),
        )])
        .into_iter()
        .map(|seen| seen.template)
        .collect();
        assert_eq!(
            templates,
            [
                "./spec/models/post_spec.rb failed to load: SyntaxError: expected a block beginning with `do` to end with `end`",
                "An error occurred in an `after(:suite)` hook: RuntimeError: after suite boom",
                "./spec/gone_spec.rb failed to load: LoadError: cannot load such file -- gone",
            ]
        );
    }

    #[test]
    fn an_error_outside_examples_is_an_unscoped_failed_exception() {
        let event = r#"{"event":"error_outside_examples","context":"While loading ./spec/a_spec.rb a `raise SyntaxError` occurred, RSpec will now quit.","class":"SyntaxError","message":"unexpected 'end'"}"#;
        let [seen] = interpret(&[(EVENTS_STREAM, event.as_bytes())])
            .try_into()
            .expect("one event");
        assert_eq!(
            (seen.kind, seen.template.as_str(), seen.scope, seen.outcome),
            (
                Kind::Exception,
                "./spec/a_spec.rb failed to load: SyntaxError: unexpected 'end'",
                None,
                Some(Outcome::Failure)
            )
        );
    }

    #[test]
    fn lines_not_in_the_listener_shape_are_kept_as_logs() {
        let seen = interpret(&[(
            EVENTS_STREAM,
            br#"{"event":"example","status":"exploded"}"#.as_slice(),
        )]);
        assert_eq!(seen.iter().map(|s| s.kind).collect::<Vec<_>>(), [Kind::Log]);
    }
}
