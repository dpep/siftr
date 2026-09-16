//! The streaming pipeline for one run: observations in, aggregates out.

use crate::aggregate::{Aggregate, Aggregator, RunStats};
use crate::interpret::resources::Resources;
use crate::interpret::{Claim, Interpreter, default_interpreters};
use crate::normalize::{Normalizer, Roots};
use crate::observation::Observation;

pub struct Analyzer {
    normalizer: Normalizer,
    interpreters: Vec<Box<dyn Interpreter>>,
    aggregator: Aggregator,
    observations: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Analysis {
    pub observations: u64,
    /// Most frequent first.
    pub aggregates: Vec<Aggregate>,
}

impl Analysis {
    pub fn stats(&self) -> RunStats {
        RunStats::from_aggregates(&self.aggregates)
    }
}

impl Default for Analyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer {
    pub fn new() -> Self {
        Self::with_interpreters(default_interpreters())
    }

    /// Canonicalizes paths under `roots`, so a behavior doesn't depend on the machine or directory it ran in.
    pub fn with_roots(roots: Roots) -> Self {
        Analyzer {
            normalizer: Normalizer::with_roots(roots),
            ..Self::new()
        }
    }

    /// Interpreters are offered each observation in order until one claims it.
    pub fn with_interpreters(interpreters: Vec<Box<dyn Interpreter>>) -> Self {
        Analyzer {
            normalizer: Normalizer::new(),
            interpreters,
            aggregator: Aggregator::new(),
            observations: 0,
        }
    }

    /// An event with no line behind it: what the kernel charged the run, which arrives once from the
    /// wait rather than from any stream. It adds no observation, so the run's line count is unchanged.
    pub fn record_resources(&mut self, resources: &Resources) {
        resources.record(&mut self.aggregator);
    }

    pub fn observe(&mut self, obs: Observation<'_>) {
        self.observations += 1;
        for interpreter in &mut self.interpreters {
            if interpreter.observe(obs, &mut self.normalizer, &mut self.aggregator)
                == Claim::Claimed
            {
                break;
            }
        }
    }

    pub fn finish(mut self) -> Analysis {
        for interpreter in &mut self.interpreters {
            interpreter.finish(&mut self.normalizer, &mut self.aggregator);
        }
        Analysis {
            observations: self.observations,
            aggregates: self.aggregator.finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::behavior::Kind;
    use crate::interpret::{Event, generic::Generic};
    use crate::observation::{LineSplitter, Stream};

    fn analyze(analyzer: &mut Analyzer, stream: &Stream, text: &str) {
        let mut splitter = LineSplitter::new();
        let mut observe = |obs: Observation<'_>| analyzer.observe(obs);
        splitter.feed(stream, text.as_bytes(), &mut observe);
        splitter.finish(stream, &mut observe);
    }

    #[test]
    fn lines_that_differ_only_in_values_are_one_behavior() {
        let mut analyzer = Analyzer::new();
        let log = "GET /users/1 in 3ms\nGET /users/22 in 5ms\nERROR -- boom id=7\nGET /users/333 in 4ms\n";
        analyze(&mut analyzer, &Stream::Stdout, log);
        let analysis = analyzer.finish();
        assert_eq!(analysis.observations, 4);
        let summary: Vec<_> = analysis
            .aggregates
            .iter()
            .map(|a| (a.behavior.template.as_str(), a.stats.count, a.stats.errors))
            .collect();
        assert_eq!(
            summary,
            [
                ("GET /users/<int> in <duration>", 3, 0),
                ("ERROR -- boom id=<int>", 1, 1)
            ]
        );
        let durations = analysis.aggregates[0]
            .stats
            .duration
            .expect("durations parsed");
        assert_eq!(durations.max, Duration::from_millis(5));
    }

    /// The shape a side-channel interpreter takes: claims its stream, buffers, records at finish.
    struct SideChannel {
        lines: Vec<(u64, Vec<u8>)>,
    }

    impl Interpreter for SideChannel {
        fn observe(
            &mut self,
            obs: Observation<'_>,
            _: &mut Normalizer,
            _: &mut Aggregator,
        ) -> Claim {
            if !matches!(obs.stream, Stream::File(_)) {
                return Claim::Declined;
            }
            self.lines.push((obs.seq, obs.line.to_vec()));
            Claim::Claimed
        }

        fn finish(&mut self, normalizer: &mut Normalizer, sink: &mut Aggregator) {
            let stream = Stream::File("report.json".into());
            for (seq, line) in &self.lines {
                let source = Observation {
                    stream: &stream,
                    seq: *seq,
                    line,
                    raw_len: line.len() as u64 + 1,
                };
                sink.record(&Event {
                    kind: Kind::TestExample,
                    template: normalizer.normalize(line),
                    input: line,
                    source,
                    duration: None,
                    outcome: None,
                    scope: None,
                    measures: &[],
                });
            }
        }
    }

    #[test]
    fn interpreters_claim_in_order_and_flush_at_finish() {
        let chain: Vec<Box<dyn Interpreter>> = vec![
            Box::new(SideChannel { lines: Vec::new() }),
            Box::new(Generic),
        ];
        let mut analyzer = Analyzer::with_interpreters(chain);
        analyze(&mut analyzer, &Stream::Stdout, "plain line\n");
        analyze(
            &mut analyzer,
            &Stream::File("report.json".into()),
            "example 1 passed\n",
        );
        let kinds: Vec<_> = analyzer
            .finish()
            .aggregates
            .iter()
            .map(|a| (a.behavior.kind, a.exemplars[0].stream.to_string()))
            .collect();
        assert!(kinds.contains(&(Kind::Log, "stdout".into())));
        assert!(kinds.contains(&(Kind::TestExample, "file:report.json".into())));
        assert_eq!(
            kinds.len(),
            2,
            "the side channel's line never reached generic: {kinds:?}"
        );
    }
}
