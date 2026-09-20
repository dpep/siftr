//! libtest's own result lines, as `cargo test` prints them on stdout.
//!
//! Only the two lines libtest writes about a test are read: `test <name> ... <status>` and the
//! `test result:` summary. Everything else a test run prints — the failure blocks, the panic, the
//! captured output — stays a `log` behavior, which [`crate::signal`] already declines to judge on the
//! stdout of a run that had a reporter, exactly as it does for RSpec's progress dots.
//!
//! A test's identity is the name libtest prints and nothing else. The test binary it belongs to is
//! named on *stderr* (`Running tests/cli_gate.rs (…)`), and pairing the two streams would need an
//! ordering between them that a capture does not promise — a wrong pairing would rename every
//! behavior in the run, so the name goes unqualified and two same-named tests are one behavior whose
//! count is their number, as equally-described RSpec examples already are.

use std::time::Duration;

use super::{Claim, Event, Interpreter, Outcome, literal};
use crate::aggregate::Aggregator;
use crate::behavior::Kind;
use crate::normalize::Normalizer;
use crate::normalize::secrets::unnumber;
use crate::observation::{Observation, Stream};

/// Measures of the `test.summary` behavior. `examples`, `expected` and `defined` are
/// [`crate::interpret::rspec::summary`]'s, which is what [`crate::baseline`] reads to tell a filtered
/// or truncated run from a complete one; the rest are libtest's own words.
pub mod summary {
    pub const PASSED: &str = "passed";
    pub const FAILED: &str = "failed";
    pub const IGNORED: &str = "ignored";
    pub const FILTERED_OUT: &str = "filtered_out";
}

/// What libtest prints between a test's name and its status.
const VERDICT: &[u8] = b" ... ";

/// The one behavior every `test result:` line is counted under: one summary per test binary.
const SUMMARY: &[u8] = b"cargo test";

#[derive(Default)]
pub struct Cargo {
    template: Vec<u8>,
    /// Inside cargo's list of the targets that failed, on stderr.
    listing_targets: bool,
}

impl Interpreter for Cargo {
    fn observe(
        &mut self,
        obs: Observation<'_>,
        _normalizer: &mut Normalizer,
        sink: &mut Aggregator,
    ) -> Claim {
        self.interpret(obs, &mut |event| sink.record(event))
    }
}

impl Cargo {
    fn interpret(&mut self, obs: Observation<'_>, emit: &mut impl FnMut(&Event<'_>)) -> Claim {
        // libtest writes its results to stdout; cargo's own progress and diagnostics go to stderr.
        if !matches!(obs.stream, Stream::Stdout) {
            return match obs.stream {
                Stream::Stderr => self.notice(obs.line),
                _ => Claim::Declined,
            };
        }
        let Some(rest) = obs.line.strip_prefix(b"test ") else {
            return Claim::Declined;
        };
        if let Some(counts) = rest.strip_prefix(b"result: ") {
            let Some(summary) = Summary::parse(counts) else {
                return Claim::Declined;
            };
            summary.emit(obs, emit);
            return Claim::Claimed;
        }
        let Some((name, outcome)) = result(rest) else {
            return Claim::Declined;
        };
        self.template.clear();
        // Lines arrive redacted, their placeholders numbered per run.
        unnumber(name, &mut self.template);
        emit(&Event {
            kind: Kind::TestExample,
            template: literal(&self.template),
            input: &self.template,
            source: obs,
            // libtest times the binary, not the test: `--report-time` is unstable and off by default.
            duration: None,
            outcome: Some(outcome),
            scope: None,
            measures: &[],
        });
        Claim::Claimed
    }

    /// Cargo's own notice that a test target failed, claimed and dropped.
    ///
    /// It names the *target*, so a different target failing is a behavior that never existed before —
    /// a NEW signal on every first failure of every target, accumulating forever — while saying less
    /// than the `test.example` that failed alongside it already says. Dropping a line that would be a
    /// behavior only because it is rare is the mirror of [`super::generic`] dropping `log show`'s
    /// header, which would be one because it is always there. The raw capture still holds both.
    ///
    /// Narrow on purpose: `error: could not compile …`, and the `Caused by: process didn't exit
    /// successfully … (signal: 6, SIGABRT)` of a harness that aborted without reporting a failure,
    /// are not this shape and stay changes of their own.
    fn notice(&mut self, line: &[u8]) -> Claim {
        if line.starts_with(b"error: test failed, to rerun pass `") {
            return Claim::Claimed;
        }
        // `error: 1 target failed:` then one indented `` `--test cli_gate` `` per target.
        if let Some(count) = line
            .strip_prefix(b"error: ")
            .and_then(|rest| split(rest, b" target"))
            .filter(|(_, rest)| *rest == b"s failed:" || *rest == b" failed:")
            .and_then(|(count, _)| number(count))
        {
            self.listing_targets = count > 0.0;
            return Claim::Claimed;
        }
        let item = trim(line);
        if self.listing_targets && item.starts_with(b"`--") && item.ends_with(b"`") {
            return Claim::Claimed;
        }
        self.listing_targets = false;
        Claim::Declined
    }
}

/// `<name> ... <status>`, the tail of a result line. The status decides: a line libtest didn't write
/// is declined rather than guessed at, since a test's own output reaches this stdout too.
fn result(rest: &[u8]) -> Option<(&[u8], Outcome)> {
    // A doctest's name holds spaces (`src/lib.rs - Foo::bar (line 12)`), so the last separator wins.
    let at = rest
        .windows(VERDICT.len())
        .rposition(|window| window == VERDICT)?;
    let (name, status) = (&rest[..at], &rest[at + VERDICT.len()..]);
    if name.is_empty() {
        return None;
    }
    let outcome = match status {
        b"ok" => Outcome::Success,
        b"FAILED" => Outcome::Failure,
        // `ignored` alone, or `ignored, <the reason the #[ignore] gave>`.
        b"ignored" => Outcome::Skipped,
        _ if status.starts_with(b"ignored, ") => Outcome::Skipped,
        _ => return None,
    };
    Some((name, outcome))
}

/// One test binary's `test result:` line.
struct Summary {
    failed: bool,
    counts: [(&'static str, f64); 4],
    duration: Option<Duration>,
}

impl Summary {
    /// `ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s`
    fn parse(line: &[u8]) -> Option<Self> {
        let (verdict, rest) = split(line, b". ")?;
        let failed = match verdict {
            b"ok" => false,
            b"FAILED" => true,
            _ => return None,
        };
        let mut counts = [
            (summary::PASSED, 0.0),
            (summary::FAILED, 0.0),
            (summary::IGNORED, 0.0),
            (summary::FILTERED_OUT, 0.0),
        ];
        let mut duration = None;
        for field in rest.split(|&b| b == b';') {
            let field = trim(field);
            if let Some(secs) = field.strip_prefix(b"finished in ") {
                duration = seconds(secs);
                continue;
            }
            let Some((count, name)) = split(field, b" ") else {
                continue;
            };
            let count = number(count)?;
            if let Some(slot) = counts.iter_mut().find(|(label, _)| match name {
                b"filtered out" => *label == summary::FILTERED_OUT,
                _ => label.as_bytes() == name,
            }) {
                slot.1 = count;
            }
        }
        Some(Summary {
            failed,
            counts,
            duration,
        })
    }

    fn count(&self, name: &str) -> f64 {
        self.counts
            .iter()
            .find(|(label, _)| *label == name)
            .map_or(0.0, |(_, count)| *count)
    }

    fn emit(&self, obs: Observation<'_>, emit: &mut impl FnMut(&Event<'_>)) {
        use crate::interpret::rspec::summary as rspec;
        // What ran, what was loaded to run, and what the binaries hold: a filtered `cargo test <name>`
        // is a run that skipped existing tests, and this is the only place that says so.
        let ran = self.count(summary::PASSED)
            + self.count(summary::FAILED)
            + self.count(summary::IGNORED);
        let measures = [
            (rspec::EXAMPLES, ran),
            (rspec::EXPECTED, ran),
            (rspec::DEFINED, ran + self.count(summary::FILTERED_OUT)),
            (rspec::FAILURES, self.count(summary::FAILED)),
            (rspec::PENDING, self.count(summary::IGNORED)),
            (summary::PASSED, self.count(summary::PASSED)),
            (summary::FILTERED_OUT, self.count(summary::FILTERED_OUT)),
        ];
        emit(&Event {
            kind: Kind::TestSummary,
            template: literal(SUMMARY),
            input: SUMMARY,
            source: obs,
            duration: self.duration,
            outcome: Some(if self.failed {
                Outcome::Failure
            } else {
                Outcome::Success
            }),
            scope: None,
            measures: &measures,
        });
    }
}

fn split<'a>(text: &'a [u8], separator: &[u8]) -> Option<(&'a [u8], &'a [u8])> {
    let at = text
        .windows(separator.len())
        .position(|window| window == separator)?;
    Some((&text[..at], &text[at + separator.len()..]))
}

fn trim(text: &[u8]) -> &[u8] {
    let start = text.iter().position(|b| !b.is_ascii_whitespace());
    let end = text.iter().rposition(|b| !b.is_ascii_whitespace());
    match (start, end) {
        (Some(start), Some(end)) => &text[start..=end],
        _ => &[],
    }
}

fn number(text: &[u8]) -> Option<f64> {
    if text.is_empty() || !text.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(text).ok()?.parse().ok()
}

/// `0.07s`, as libtest writes an elapsed time.
fn seconds(text: &[u8]) -> Option<Duration> {
    let secs: f64 = std::str::from_utf8(text.strip_suffix(b"s")?)
        .ok()?
        .parse()
        .ok()?;
    Duration::try_from_secs_f64(secs).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpret::rspec::summary as rspec;

    type Emitted = (Kind, String, Option<Outcome>, Vec<(&'static str, f64)>);

    fn seen(lines: &[&str], stream: Stream) -> Vec<Emitted> {
        let mut cargo = Cargo::default();
        let mut out = Vec::new();
        for (seq, line) in (1..).zip(lines) {
            let obs = Observation {
                stream: &stream,
                seq,
                line: line.as_bytes(),
                raw_len: line.len() as u64 + 1,
            };
            let claimed = cargo.interpret(obs, &mut |event: &Event<'_>| {
                out.push((
                    event.kind,
                    String::from_utf8_lossy(event.template.template).into_owned(),
                    event.outcome,
                    event.measures.to_vec(),
                ));
            });
            if claimed == Claim::Declined {
                out.push((Kind::Log, (*line).to_owned(), None, Vec::new()));
            }
        }
        out
    }

    fn examples(lines: &[&str]) -> Vec<(Kind, String, Option<Outcome>)> {
        seen(lines, Stream::Stdout)
            .into_iter()
            .map(|(kind, template, outcome, _)| (kind, template, outcome))
            .collect()
    }

    #[test]
    fn a_tests_identity_is_its_name_and_its_verdict_is_an_outcome() {
        let got = examples(&[
            "test normalize::tests::masks_an_integer ... ok",
            "test normalize::tests::masks_an_integer ... FAILED",
            "test slow::tests::needs_a_network ... ignored",
            "test slow::tests::needs_a_network ... ignored, no network here",
            "test src/lib.rs - Behavior::new (line 12) ... ok",
        ]);
        assert_eq!(
            got,
            [
                (
                    Kind::TestExample,
                    "normalize::tests::masks_an_integer".to_owned(),
                    Some(Outcome::Success)
                ),
                (
                    Kind::TestExample,
                    "normalize::tests::masks_an_integer".to_owned(),
                    Some(Outcome::Failure)
                ),
                (
                    Kind::TestExample,
                    "slow::tests::needs_a_network".to_owned(),
                    Some(Outcome::Skipped)
                ),
                (
                    Kind::TestExample,
                    "slow::tests::needs_a_network".to_owned(),
                    Some(Outcome::Skipped)
                ),
                (
                    Kind::TestExample,
                    "src/lib.rs - Behavior::new (line 12)".to_owned(),
                    Some(Outcome::Success)
                ),
            ]
        );
    }

    /// A test's own output reaches this same stdout, and a line that merely looks like a verdict is
    /// not one: declining leaves it a `log` behavior rather than inventing a test.
    #[test]
    fn only_libtests_own_lines_are_claimed() {
        let declined = [
            "test the thing ... whatever",
            "test  ... ok",
            "testing the waters ... ok",
            "  test indented::case ... ok",
            "test result: something else",
            "running 2 tests",
            "---- a_test stdout ----",
        ];
        let kinds: Vec<Kind> = examples(&declined).into_iter().map(|(k, _, _)| k).collect();
        assert_eq!(kinds, [Kind::Log; 7], "{declined:?}");
    }

    /// cargo's own diagnostics share the words but not the stream.
    #[test]
    fn stderr_is_cargos_not_libtests() {
        let kinds: Vec<Kind> = seen(&["test siftr::tests::a ... ok"], Stream::Stderr)
            .into_iter()
            .map(|(kind, _, _, _)| kind)
            .collect();
        assert_eq!(kinds, [Kind::Log]);
    }

    /// The `Kind::Log` rows are what fell through to the generic interpreter; a claimed notice leaves none.
    fn stderr(lines: &[&str]) -> Vec<String> {
        seen(lines, Stream::Stderr)
            .into_iter()
            .map(|(_, template, _, _)| template)
            .collect()
    }

    #[test]
    fn cargos_notice_that_a_target_failed_is_dropped() {
        let dropped = stderr(&[
            "error: test failed, to rerun pass `--test cli_gate`",
            "error: 1 target failed:",
            "    `--test cli_gate`",
            "error: 2 targets failed:",
            "    `--lib`",
            "    `--test cli_gate`",
        ]);
        assert_eq!(dropped, Vec::<String>::new());
    }

    /// A build that never ran, and a harness that aborted without reporting a failure: neither has a
    /// `test.example` to speak for it, so neither may be dropped.
    #[test]
    fn a_failure_with_no_test_behind_it_is_left_alone() {
        let kept = stderr(&[
            "error: could not compile `siftr` (lib test) due to 1 previous error",
            "Caused by:",
            "  process didn't exit successfully: `/tmp/demo/deps/cli_gate` (signal: 6, SIGABRT)",
            "    `--test cli_gate`",
        ]);
        assert_eq!(
            kept.len(),
            4,
            "the last is a list item with no list open: {kept:?}"
        );
    }

    #[test]
    fn every_test_binary_counts_into_one_summary() {
        let got = seen(
            &[
                "test result: ok. 3 passed; 0 failed; 1 ignored; 0 measured; 2 filtered out; finished in 0.07s",
                "test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.41s",
            ],
            Stream::Stdout,
        );
        let shape: Vec<(Kind, String, Option<Outcome>)> = got
            .iter()
            .map(|(kind, template, outcome, _)| (*kind, template.clone(), *outcome))
            .collect();
        assert_eq!(
            shape,
            [
                (
                    Kind::TestSummary,
                    "cargo test".to_owned(),
                    Some(Outcome::Success)
                ),
                (
                    Kind::TestSummary,
                    "cargo test".to_owned(),
                    Some(Outcome::Failure)
                ),
            ],
            "one behavior, counted once per binary"
        );
        // 3 passed + 1 ignored ran, of 6 the binary defines: a filter left 2 unrun, which is what
        // `baseline` reads to know the run skipped tests that exist.
        assert_eq!(
            got[0].3,
            [
                (rspec::EXAMPLES, 4.0),
                (rspec::EXPECTED, 4.0),
                (rspec::DEFINED, 6.0),
                (rspec::FAILURES, 0.0),
                (rspec::PENDING, 1.0),
                (summary::PASSED, 3.0),
                (summary::FILTERED_OUT, 2.0),
            ]
        );
    }

    #[test]
    fn a_summary_that_isnt_libtests_shape_is_left_alone() {
        let kinds: Vec<Kind> = examples(&[
            "test result: ok. many passed; 0 failed",
            "test result: unclear. 1 passed",
        ])
        .into_iter()
        .map(|(kind, _, _)| kind)
        .collect();
        assert_eq!(kinds, [Kind::Log; 2]);
    }
}
