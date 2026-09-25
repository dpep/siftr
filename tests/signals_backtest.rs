//! Monte Carlo backtest of the FREQUENCY rule: how often it fires on counts that never change, and
//! what each candidate guard costs on counts that do.
//!
//! `docs/findings/signals.md` §3 backtested the rule on a corpus whose counts were *exactly
//! deterministic* (§1: 24 of 24 count behaviors identical in all 25 clean runs), so it could price
//! neither branch of the rule against a count that varies. This does, by generating counts that vary
//! by construction, and prices the exact branch separately since that is the one with no tolerance.
//! Results and what they settle: `docs/findings/frequency-small-n.md`.
//!
//! The sweeps are `#[ignore]`d (a minute of sampling each) and print the doc's tables; the tests that
//! run in the gate pin the numbers the doc quotes, from the same seeds.

use siftr::signal::rules::{self, Frequency};

// ----- sampling ---------------------------------------------------------------------------------

/// xorshift64*: a fixed, portable stream, so every number here is reproducible from its seed.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn normal(&mut self) -> f64 {
        let (u, v) = (self.unit().max(f64::MIN_POSITIVE), self.unit());
        (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
    }

    /// Knuth's product-of-uniforms sampler; the means here are small enough that `exp(-mean)` is normal.
    fn poisson(&mut self, mean: f64) -> f64 {
        let limit = (-mean).exp();
        let (mut k, mut p) = (0.0, 1.0);
        loop {
            p *= self.unit();
            if p <= limit {
                return k;
            }
            k += 1.0;
        }
    }
}

/// Trims a float to the shortest form that reads as the number it is.
fn trim(x: f64) -> String {
    let s = format!("{x}");
    s.trim_end_matches(".0").to_owned()
}

/// How a behavior's count per run is generated. A generator is *stable*: its parameters never change
/// from run to run, so every signal raised against one is a false positive.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Gen {
    /// A test suite's counts as §1 measured them: identical in every run.
    Fixed(f64),
    /// Independent arrivals at a fixed rate — the least noisy way a count can actually vary.
    Poisson(f64),
    /// Arrivals at a rate that itself wanders (lognormal, `cv` = coefficient of variation): what a log
    /// window looks like when the traffic behind it is not constant.
    Over(f64, f64),
}

impl Gen {
    fn draw(self, rng: &mut Rng) -> f64 {
        match self {
            Gen::Fixed(v) => v,
            Gen::Poisson(mean) => rng.poisson(mean),
            Gen::Over(mean, cv) => {
                let sigma = (1.0 + cv * cv).ln().sqrt();
                let rate = (sigma * rng.normal() - sigma * sigma / 2.0).exp();
                rng.poisson(mean * rate)
            }
        }
    }

    /// The same shape with its mean multiplied: a sustained, injected change.
    fn scaled(self, by: f64) -> Self {
        match self {
            Gen::Fixed(v) => Gen::Fixed((v * by).round()),
            Gen::Poisson(mean) => Gen::Poisson(mean * by),
            Gen::Over(mean, cv) => Gen::Over(mean * by, cv),
        }
    }

    fn label(self) -> String {
        match self {
            Gen::Fixed(v) => format!("fixed {v:.0}"),
            Gen::Poisson(mean) => format!("poisson {}", trim(mean)),
            Gen::Over(mean, cv) => format!("over {} cv{}", trim(mean), trim(cv)),
        }
    }
}

// ----- the rule, and the candidate guards -------------------------------------------------------

/// Each candidate is the shipped rule plus a *floor* under its tolerance: a move of no more than the
/// floor counts as agreement. A candidate can therefore only ever fire where the rule already does.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Rule {
    /// What ships: the tolerance is `VARYING_WIDTHS × (max − min)`, which an exact baseline makes zero.
    Shipped,
    /// The exact branch alone gets a floor of `d − 1` counts, below `upto` baseline runs.
    Steps(f64, usize),
    /// The exact branch alone gets a floor of `k` Poisson sigmas of its own value, below `upto` runs.
    Sigma(f64, usize),
    /// Both branches get a floor of `k` sigmas, where sigma is estimated from *every* behavior of the
    /// context rather than from this one's `n` values. Zero on a context whose counts never vary.
    Pooled(f64),
    /// The same, estimated from the median behavior instead of the sum, so a behavior that genuinely
    /// moved inside the baseline window cannot raise the floor for the rest of the context.
    PooledMedian(f64),
}

impl Rule {
    /// `dispersion` is the context's pooled variance-to-mean ratio; only [`Rule::Pooled`] reads it.
    fn floor(self, n: usize, median: f64, exact: bool, dispersion: f64) -> f64 {
        let sigma = |k: f64, phi: f64| k * (phi * median.max(1.0)).sqrt();
        match self {
            Rule::Shipped => 0.0,
            Rule::Steps(d, upto) if exact && n < upto => d - 1.0,
            Rule::Sigma(k, upto) if exact && n < upto => sigma(k, 1.0),
            Rule::Pooled(k) | Rule::PooledMedian(k) => sigma(k, dispersion),
            _ => 0.0,
        }
    }

    /// How this rule reads a context's dispersion off its behaviors' baselines.
    fn dispersion(self, baselines: &[Vec<f64>]) -> f64 {
        match self {
            Rule::PooledMedian(_) => median_dispersion(baselines),
            _ => dispersion(baselines),
        }
    }

    fn label(self) -> String {
        match self {
            Rule::Shipped => "shipped".to_owned(),
            Rule::Steps(d, upto) => format!("exact ±{} below n={upto}", trim(d)),
            Rule::Sigma(k, upto) => format!("exact {}√med below n={upto}", trim(k)),
            Rule::Pooled(k) => format!("pooled {}σ", trim(k)),
            Rule::PooledMedian(k) => format!("median-pooled {}σ", trim(k)),
        }
    }
}

fn fires(rule: Rule, baseline: &[f64], current: f64, dispersion: f64) -> Option<Frequency> {
    let f = rules::frequency(baseline, current)?;
    let floor = rule.floor(baseline.len(), f.median, f.exact, dispersion);
    ((current - f.median).abs() > floor).then_some(f)
}

/// The context's variance-to-mean ratio, pooled over every behavior whose baseline siftr can compare:
/// 0 where every count held still, about 1 for independent arrivals, more where the rate wanders.
/// Sums rather than averages, so the counts that carry the most information weigh the most.
fn dispersion(baselines: &[Vec<f64>]) -> f64 {
    let (mut var, mut mean) = (0.0, 0.0);
    for values in baselines {
        let n = values.len();
        if n < 2 {
            continue;
        }
        let m = values.iter().sum::<f64>() / n as f64;
        var += values.iter().map(|v| (v - m).powi(2)).sum::<f64>() / (n - 1) as f64;
        mean += m;
    }
    if mean == 0.0 { 0.0 } else { var / mean }
}

/// The median of a chi-square with `d` degrees of freedom, over `d`: what a sample variance reads on
/// average *at the median* rather than in expectation, and so what a median of them must be divided
/// by to estimate a variance. `d = 1` (a baseline of two runs) is the one that matters most: a single
/// pair reads under half the variance it came from, half the time.
fn median_bias(d: usize) -> f64 {
    const TABLE: [f64; 9] = [
        0.4549,
        std::f64::consts::LN_2,
        0.7887,
        0.8392,
        0.8703,
        0.8914,
        0.9067,
        0.9184,
        0.9275,
    ];
    TABLE[(d - 1).min(TABLE.len() - 1)]
}

/// The same ratio as [`dispersion`], read off the median behavior instead of the sum. Zero whenever
/// at least half the context's counts held still in every baseline run, whatever the rest did.
fn median_dispersion(baselines: &[Vec<f64>]) -> f64 {
    let mut ratios: Vec<f64> = Vec::new();
    for values in baselines {
        let n = values.len();
        let m = values.iter().sum::<f64>() / n as f64;
        if n < 2 || m == 0.0 {
            continue;
        }
        let var = values.iter().map(|v| (v - m).powi(2)).sum::<f64>() / (n - 1) as f64;
        ratios.push(var / m / median_bias(n - 1));
    }
    ratios.sort_by(f64::total_cmp);
    siftr::num::median(&ratios).unwrap_or(0.0)
}

// ----- one comparison at a time -----------------------------------------------------------------

#[derive(Default, Clone, Copy)]
struct Tally {
    trials: u64,
    exact: u64,
    varying: u64,
    /// Baselines that happened to be identical in every run, whether or not anything fired.
    identical: u64,
}

impl Tally {
    fn rate(self, hits: u64) -> f64 {
        hits as f64 / self.trials as f64
    }
}

/// `trials` independent comparisons of one behavior: `n` baseline runs and one current run, all from
/// `dist`. The pooled dispersion is the generator's own, as a context wide enough to estimate it well
/// would measure it.
fn sweep(dist: Gen, n: usize, rule: Rule, trials: u64, seed: u64) -> Tally {
    let mut rng = Rng::new(seed);
    let phi = true_dispersion(dist);
    let mut tally = Tally {
        trials,
        ..Tally::default()
    };
    for _ in 0..trials {
        let baseline: Vec<f64> = (0..n).map(|_| dist.draw(&mut rng)).collect();
        let current = dist.draw(&mut rng);
        if baseline.iter().all(|v| *v == baseline[0]) {
            tally.identical += 1;
        }
        match fires(rule, &baseline, current, phi) {
            Some(f) if f.exact => tally.exact += 1,
            Some(_) => tally.varying += 1,
            None => {}
        }
    }
    tally
}

/// The variance-to-mean ratio a generator has by construction.
fn true_dispersion(dist: Gen) -> f64 {
    match dist {
        Gen::Fixed(_) => 0.0,
        Gen::Poisson(_) => 1.0,
        Gen::Over(mean, cv) => 1.0 + cv * cv * mean,
    }
}

fn pct(x: f64) -> String {
    if x == 0.0 {
        "0".to_owned()
    } else if x >= 0.095 {
        format!("{:.0}%", x * 100.0)
    } else if x >= 0.0095 {
        format!("{:.1}%", x * 100.0)
    } else {
        format!("{:.2}%", x * 100.0)
    }
}

// ----- a context's life -------------------------------------------------------------------------

struct Life {
    /// Runs that reported at least one FREQUENCY signal.
    loud: u64,
    /// Every signal raised, summed over runs.
    signals: u64,
    /// Runs that could compare at all (n ≥ 2).
    compared: u64,
    /// Further runs each signal stays open for — a later run reproduces it against the signal's own
    /// baseline — summed over signals.
    reminders: u64,
    /// The pooled dispersion estimated from the baseline, summed over comparisons.
    phi: f64,
}

/// A whole context: `runs` runs of `behaviors` independent counts, compared the way siftr compares —
/// the baseline is every earlier run, capped at [`siftr::baseline::MAX_RUNS`], and the dispersion is
/// estimated from that baseline rather than given.
fn life(dist: Gen, behaviors: usize, runs: usize, rule: Rule, trials: u64, seed: u64) -> Life {
    let mut rng = Rng::new(seed);
    let mut out = Life {
        loud: 0,
        signals: 0,
        compared: 0,
        reminders: 0,
        phi: 0.0,
    };
    for _ in 0..trials {
        let history: Vec<Vec<f64>> = (0..runs)
            .map(|_| (0..behaviors).map(|_| dist.draw(&mut rng)).collect())
            .collect();
        for r in 2..runs {
            let start = r.saturating_sub(siftr::baseline::MAX_RUNS);
            let baselines: Vec<Vec<f64>> = (0..behaviors)
                .map(|b| (start..r).rev().map(|i| history[i][b]).collect())
                .collect();
            let phi = rule.dispersion(&baselines);
            out.compared += 1;
            out.phi += phi;
            let mut loud = false;
            for (b, baseline) in baselines.iter().enumerate() {
                if fires(rule, baseline, history[r][b], phi).is_none() {
                    continue;
                }
                loud = true;
                out.signals += 1;
                for later in history.iter().skip(r + 1) {
                    if fires(rule, baseline, later[b], phi).is_none() {
                        break;
                    }
                    out.reminders += 1;
                }
            }
            out.loud += u64::from(loud);
        }
    }
    out
}

// ----- injected change --------------------------------------------------------------------------

/// Runs that follow the one a change landed on, and how wide the context around it is.
const FOLLOW_UPS: usize = 4;
const BEHAVIORS: usize = 20;

/// A sustained change of `by` landing at run `at + 1` of a context whose earlier runs were quiet, and
/// which holds [`BEHAVIORS`] counts in all. Returns (caught on the run it landed, caught on that run
/// or any of the [`FOLLOW_UPS`] that follow it).
fn caught(dist: Gen, by: f64, at: usize, rule: Rule, trials: u64, seed: u64) -> (f64, f64) {
    let (after, behaviors) = (FOLLOW_UPS, BEHAVIORS);
    let mut rng = Rng::new(seed);
    let (mut on_landing, mut ever) = (0u64, 0u64);
    let changed = dist.scaled(by);
    for _ in 0..trials {
        let mut history: Vec<f64> = (0..at).map(|_| dist.draw(&mut rng)).collect();
        let (mut now, mut hit) = (false, false);
        for r in 0..=after {
            let current = changed.draw(&mut rng);
            let start = history.len().saturating_sub(siftr::baseline::MAX_RUNS);
            let baseline: Vec<f64> = history[start..].iter().rev().copied().collect();
            // The rest of the context is quiet, and wide enough to estimate its dispersion well.
            let others: Vec<Vec<f64>> = (1..behaviors)
                .map(|_| (0..baseline.len()).map(|_| dist.draw(&mut rng)).collect())
                .collect();
            let fired = fires(rule, &baseline, current, rule.dispersion(&others)).is_some();
            now |= fired && r == 0;
            hit |= fired;
            history.push(current);
        }
        on_landing += u64::from(now);
        ever += u64::from(hit);
    }
    (
        on_landing as f64 / trials as f64,
        ever as f64 / trials as f64,
    )
}

// ----- ranking ----------------------------------------------------------------------------------

/// Whether an exact baseline's undiscounted confidence outranks a varying one's discounted
/// confidence: over runs where both a real change and at least one noise signal fired, the share
/// where the real change carries the strictly highest confidence — as shipped, and with an exact
/// baseline's confidence discounted to `E(n − 1)`, since `n` identical runs are `n − 1` confirmations
/// that the count holds still.
fn ranking(
    dist: Gen,
    by: f64,
    n: usize,
    behaviors: usize,
    trials: u64,
    seed: u64,
) -> (f64, f64, u64) {
    let mut rng = Rng::new(seed);
    let changed = dist.scaled(by);
    let discounted = |f: &Frequency| {
        let n = n as f64;
        if f.exact {
            f.confidence * (n / (n + 1.0)) / ((n + 1.0) / (n + 2.0))
        } else {
            f.confidence
        }
    };
    let (mut wins, mut wins_discounted, mut contested) = (0u64, 0u64, 0u64);
    for _ in 0..trials {
        let real = {
            let baseline: Vec<f64> = (0..n).map(|_| dist.draw(&mut rng)).collect();
            rules::frequency(&baseline, changed.draw(&mut rng))
        };
        let noise: Vec<Frequency> = (1..behaviors)
            .filter_map(|_| {
                let baseline: Vec<f64> = (0..n).map(|_| dist.draw(&mut rng)).collect();
                rules::frequency(&baseline, dist.draw(&mut rng))
            })
            .collect();
        let (Some(real), false) = (real, noise.is_empty()) else {
            continue;
        };
        contested += 1;
        wins += u64::from(noise.iter().all(|f| real.confidence > f.confidence));
        wins_discounted += u64::from(noise.iter().all(|f| discounted(&real) > discounted(f)));
    }
    let share = |hits: u64| hits as f64 / contested.max(1) as f64;
    (share(wins), share(wins_discounted), contested)
}

// ----- the sweeps -------------------------------------------------------------------------------

const GENS: [Gen; 8] = [
    Gen::Fixed(3.0),
    Gen::Poisson(1.0),
    Gen::Poisson(3.0),
    Gen::Poisson(10.0),
    Gen::Poisson(30.0),
    Gen::Poisson(100.0),
    Gen::Over(10.0, 0.3),
    Gen::Over(100.0, 0.3),
];

const TRIALS: u64 = 200_000;

fn seed(n: usize) -> u64 {
    0x5157_0000 + n as u64
}

#[test]
#[ignore = "sweep: prints a table for docs/findings/frequency-small-n.md"]
fn false_positives_by_baseline_size() {
    println!("\n| generator | n | identical baseline | fires exact | fires varying | either |");
    println!("|---|---|---|---|---|---|");
    for dist in GENS {
        for n in [2usize, 3, 4, 5, 7, 10] {
            let t = sweep(dist, n, Rule::Shipped, TRIALS, seed(n));
            println!(
                "| {} | {n} | {} | {} | {} | {} |",
                dist.label(),
                pct(t.rate(t.identical)),
                pct(t.rate(t.exact)),
                pct(t.rate(t.varying)),
                pct(t.rate(t.exact + t.varying)),
            );
        }
    }
}

#[test]
#[ignore = "sweep: prints a table for docs/findings/frequency-small-n.md"]
fn false_positives_under_each_candidate() {
    let rules = [
        Rule::Shipped,
        Rule::Steps(2.0, 3),
        Rule::Steps(2.0, 99),
        Rule::Sigma(1.0, 99),
        Rule::Sigma(2.0, 99),
        Rule::Pooled(2.0),
        Rule::Pooled(3.0),
        Rule::PooledMedian(3.0),
    ];
    println!();
    print!("| generator | n |");
    for rule in rules {
        print!(" {} |", rule.label());
    }
    println!("\n|---|---|{}", "---|".repeat(rules.len()));
    for dist in GENS {
        for n in [2usize, 3, 5] {
            print!("| {} | {n} |", dist.label());
            for rule in rules {
                let t = sweep(dist, n, rule, TRIALS, seed(n));
                print!(" {} |", pct(t.rate(t.exact + t.varying)));
            }
            println!();
        }
    }
}

#[test]
#[ignore = "sweep: prints a table for docs/findings/frequency-small-n.md"]
fn a_new_contexts_first_runs() {
    println!(
        "\n| generator | guard | est. dispersion | loud runs | signals per loud run | reminder-runs per signal |"
    );
    println!("|---|---|---|---|---|---|");
    for dist in [
        Gen::Fixed(3.0),
        Gen::Poisson(3.0),
        Gen::Poisson(30.0),
        Gen::Over(10.0, 0.3),
    ] {
        for rule in [
            Rule::Shipped,
            Rule::Steps(2.0, 3),
            Rule::Sigma(1.0, 99),
            Rule::Pooled(3.0),
            Rule::PooledMedian(3.0),
        ] {
            let l = life(dist, BEHAVIORS, 12, rule, 5_000, 0xC0FFEE);
            println!(
                "| {} | {} | {:.2} | {} | {:.2} | {:.2} |",
                dist.label(),
                rule.label(),
                l.phi / l.compared as f64,
                pct(l.loud as f64 / l.compared as f64),
                l.signals as f64 / l.loud.max(1) as f64,
                l.reminders as f64 / l.signals.max(1) as f64,
            );
        }
    }
}

#[test]
#[ignore = "sweep: prints a table for docs/findings/frequency-small-n.md"]
fn true_positives_under_each_candidate() {
    println!(
        "\n| generator | change | lands at | guard | caught then | caught within 4 more runs |"
    );
    println!("|---|---|---|---|---|---|");
    for dist in [
        Gen::Fixed(3.0),
        Gen::Fixed(28.0),
        Gen::Poisson(3.0),
        Gen::Poisson(30.0),
    ] {
        for by in [1.25, 2.0, 3.33] {
            for at in [2usize, 3] {
                for rule in [
                    Rule::Shipped,
                    Rule::Steps(2.0, 3),
                    Rule::Sigma(1.0, 99),
                    Rule::Pooled(3.0),
                ] {
                    let (now, ever) = caught(dist, by, at, rule, 20_000, 0xBEEF + at as u64);
                    println!(
                        "| {} | ×{} | run {} | {} | {} | {} |",
                        dist.label(),
                        trim(by),
                        at + 1,
                        rule.label(),
                        pct(now),
                        pct(ever),
                    );
                }
            }
        }
    }
}

#[test]
#[ignore = "sweep: prints a table for docs/findings/frequency-small-n.md"]
fn a_real_change_outranked_by_noise() {
    println!(
        "\n| generator | change | n | contested runs | ranks first, shipped | ranks first, exact discounted |"
    );
    println!("|---|---|---|---|---|---|");
    for dist in [Gen::Poisson(3.0), Gen::Poisson(30.0), Gen::Over(10.0, 0.3)] {
        for by in [2.0, 3.33] {
            for n in [2usize, 3, 5] {
                let (wins, discounted, contested) = ranking(dist, by, n, BEHAVIORS, 50_000, 0x5A11);
                println!(
                    "| {} | ×{} | {n} | {contested} | {} | {} |",
                    dist.label(),
                    trim(by),
                    pct(wins),
                    pct(discounted),
                );
            }
        }
    }
}

// ----- what runs in the gate --------------------------------------------------------------------

/// The real regressions the rule has to keep: count vectors from `signals.md` §3's toggles and from
/// the `hunt` fixtures. A guard that loses one of these is not worth its false positives.
#[test]
fn candidates_keep_the_measured_regressions() {
    let cases: [(&str, &[f64], f64); 6] = [
        ("n+1 request queries, n=2", &[3.0, 3.0], 10.0),
        ("n+1 request queries, n=3", &[3.0, 3.0, 3.0], 10.0),
        ("n+1 sql Comment Load", &[1.0, 1.0, 1.0], 9.0),
        ("n+1 example queries", &[28.0, 28.0, 28.0], 35.0),
        ("n+1 example queries, n=2", &[28.0, 28.0], 35.0),
        ("hunt warning count", &[1.0, 1.0], 3.0),
    ];
    for rule in [
        Rule::Steps(2.0, 3),
        Rule::Sigma(1.0, 99),
        // A suite's counts hold still, so the pooled floor is zero there whatever `k` is.
        Rule::Pooled(3.0),
    ] {
        for (name, baseline, current) in cases {
            assert!(
                fires(rule, baseline, current, 0.0).is_some(),
                "{}: {name}",
                rule.label()
            );
        }
    }
    // Two sigmas is where a Poisson floor starts costing: it loses the N+1's example-query total.
    assert!(fires(Rule::Sigma(2.0, 99), &[28.0, 28.0, 28.0], 35.0, 0.0).is_none());
}

/// A context whose counts never vary has no dispersion to estimate, so the pooled floor is zero and
/// every verdict is the shipped rule's. This is what makes the candidate inert on §3's corpus.
#[test]
fn a_deterministic_context_has_no_pooled_floor() {
    let baselines = vec![vec![3.0; 5], vec![28.0; 5], vec![0.0; 5], vec![1.0; 5]];
    for estimator in [Rule::Pooled(3.0), Rule::PooledMedian(3.0)] {
        assert_eq!(estimator.dispersion(&baselines), 0.0);
        assert_eq!(estimator.floor(5, 28.0, true, 0.0), 0.0);
    }
}

/// What a *real* change sitting in the baseline window does to the floor the rest of the context is
/// judged by. Summing over behaviors lets one mover deafen the other nineteen; taking the median
/// leaves the floor at zero until half of them move, which is the property a test suite needs.
#[test]
fn one_mover_in_the_baseline_must_not_deafen_the_context() {
    // Twenty counts of 3 that never move, `moved` of which stepped to 10 two runs ago.
    let context = |moved: usize| -> Vec<Vec<f64>> {
        (0..20)
            .map(|b| {
                if b < moved {
                    vec![10.0, 10.0, 3.0, 3.0, 3.0]
                } else {
                    vec![3.0; 5]
                }
            })
            .collect()
    };
    // The smallest whole count a still-quiet behavior must move by to be heard.
    let audible = |estimator: Rule, moved: usize| {
        let phi = estimator.dispersion(&context(moved));
        (1..40)
            .find(|d| fires(estimator, &[3.0; 5], 3.0 + f64::from(*d), phi).is_some())
            .unwrap()
    };
    for (moved, summed, median) in [
        (0, 1, 1),
        (1, 3, 1),
        (5, 6, 1),
        (9, 7, 1),
        (10, 7, 7),
        (11, 7, 10),
    ] {
        assert_eq!(
            (
                audible(Rule::Pooled(3.0), moved),
                audible(Rule::PooledMedian(3.0), moved)
            ),
            (summed, median),
            "{moved} of 20 behaviors moved inside the baseline window"
        );
    }
}

/// The headline rates quoted in `docs/findings/frequency-small-n.md`, from the sweeps' own seeds.
#[test]
fn pinned_false_positive_rates() {
    let round = |x: f64| (x * 1000.0).round() / 1000.0;
    let cell = |dist: Gen, n: usize, rule: Rule| {
        let t = sweep(dist, n, rule, TRIALS, seed(n));
        (
            round(t.rate(t.identical)),
            round(t.rate(t.exact)),
            round(t.rate(t.varying)),
        )
    };
    // A count that holds still never fires, and its baseline is always identical.
    assert_eq!(cell(Gen::Fixed(3.0), 2, Rule::Shipped), (1.0, 0.0, 0.0));
    // A count that varies fires on about a quarter of comparisons at n = 2 whatever its size: what
    // moves with the size is only which branch does it.
    for (dist, identical, exact, varying) in [
        (Gen::Poisson(3.0), 0.168, 0.136, 0.109),
        (Gen::Poisson(30.0), 0.053, 0.050, 0.202),
        (Gen::Over(100.0, 0.3), 0.010, 0.010, 0.252),
    ] {
        assert_eq!(
            cell(dist, 2, Rule::Shipped),
            (identical, exact, varying),
            "{} n=2",
            dist.label()
        );
    }
    // By n = 5 the exact branch has all but stopped, and the varying branch carries what is left.
    assert_eq!(
        cell(Gen::Poisson(3.0), 5, Rule::Shipped),
        (0.001, 0.001, 0.01)
    );
}
