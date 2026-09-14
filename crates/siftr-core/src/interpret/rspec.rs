//! RSpec's side channel. The listener's ndjson events become `test.example`, `exception` and
//! `test.summary` behaviors, and each example's `log_offset` range scopes the Rails log slice.
//!
//! The log slice is interpreted here rather than by its own interpreter because its scopes come
//! from these events, which is also why [`EVENTS_STREAM`] must be fed before [`LOG_STREAM`].

use std::time::Duration;

use serde::Deserialize;

use super::{Claim, Event, Interpreter, Outcome, generic, literal, rails};
use crate::aggregate::Aggregator;
use crate::behavior::{BehaviorId, Kind};
use crate::normalize::Normalizer;
use crate::observation::{Observation, Stream};

/// `Stream::File` name of the listener's ndjson events.
pub const EVENTS_STREAM: &str = "rspec-events";
/// `Stream::File` name of the run's Rails log slice; `log_offset`s count bytes from its first byte.
pub const LOG_STREAM: &str = "log/test.log";

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
    log: rails::Log,
    /// Offset of the next log line. Exact for `\n` endings and lines under `MAX_LINE`, as Rails writes.
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
                let scope = self.scopes.at(self.log_offset);
                self.log_offset += obs.line.len() as u64 + 1;
                self.log.line(obs, scope, normalizer, emit);
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
            Record::ExampleStarted { id, log_offset } => {
                self.started = log_offset.map(|offset| (id, offset));
            }
            Record::Example(example) => self.example(obs, &example, emit),
            Record::Summary(summary) => summary.emit(obs, emit),
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
        self.template
            .extend_from_slice(example.spec_file().as_bytes());
        self.template.extend_from_slice(b" # ");
        self.template
            .extend_from_slice(example.full_description.as_bytes());
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
    ExampleStarted {
        id: String,
        log_offset: Option<u64>,
    },
    Example(Example),
    Summary(Summary),
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

    fn emit(&self, obs: Observation<'_>, emit: &mut impl FnMut(&Event<'_>)) {
        let failed = self.failures > 0 || self.errors_outside_of_examples > 0;
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
            measures: &[
                ("examples", self.examples as f64),
                ("failures", self.failures as f64),
                ("pending", self.pending as f64),
                (
                    "errors_outside_of_examples",
                    self.errors_outside_of_examples as f64,
                ),
            ],
        });
    }
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

    fn at(&self, offset: u64) -> Option<BehaviorId> {
        let i = self.0.partition_point(|scope| scope.end <= offset);
        self.0
            .get(i)
            .filter(|scope| scope.start <= offset)
            .map(|scope| scope.example)
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::interpret;
    use super::*;

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

    #[test]
    fn the_summary_carries_counts_and_fails_on_errors_outside_examples() {
        let summary = r#"{"event":"summary","duration":0.5,"load_time":0.8,"examples":3,"failures":0,"pending":1,"errors_outside_of_examples":1}"#;
        let [seen] = interpret(&[(EVENTS_STREAM, summary.as_bytes())])
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
                ("errors_outside_of_examples", 1.0)
            ]
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
