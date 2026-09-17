//! A regression left in place stays in view after the rolling baseline absorbs it: the rails_demo N+1, twice.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::{Value, json};
use siftr::aggregate::MAX_BEHAVIORS;
use tempfile::TempDir;

const CLEAN: [&str; 3] = ["baseline", "baseline_2", "baseline_documentation"];

struct Sandbox {
    home: TempDir,
    project: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Sandbox {
            home: tempfile::tempdir().unwrap(),
            project: tempfile::tempdir().unwrap(),
        }
    }

    fn siftr(&self, args: &[&str]) -> Output {
        let output = Command::new(env!("CARGO_BIN_EXE_siftr"))
            .args(args)
            .current_dir(self.project.path())
            .env("SIFTR_HOME", self.home.path())
            .env_remove("XDG_DATA_HOME")
            .output()
            .unwrap();
        assert!(
            output.status.code().is_some_and(|code| code < 2),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_slice(&self.siftr(args).stdout).unwrap()
    }

    fn ingest(&self, scenario: &str) -> Value {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/rails_demo")
            .join(scenario);
        let args = ["ingest", "-j", "--context", "rails_demo", "--dir"];
        self.json(&[&args[..], &[dir.to_str().unwrap()]].concat())
    }

    fn text(&self, args: &[&str]) -> String {
        String::from_utf8(self.siftr(args).stdout).unwrap()
    }
}

fn open_ids(changes: &Value) -> Vec<(String, String)> {
    changes["open_signals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["id"].as_str().unwrap().to_owned(),
                s["run"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[test]
fn an_unfixed_regression_is_reminded_until_it_is_fixed_or_dismissed() {
    let sandbox = Sandbox::new();
    for scenario in CLEAN {
        sandbox.ingest(scenario);
    }
    let first = sandbox.ingest("n_plus_one");
    assert_eq!(first["changes"], 1);
    assert_eq!(
        first["open_signals"],
        json!([]),
        "nothing earlier to be open"
    );

    let again = sandbox.ingest("n_plus_one");
    assert_eq!(again["changes"], 0, "r4 is in r5's baseline");
    let open = open_ids(&again);
    assert_eq!(
        open.first().map(|(id, run)| (id.as_str(), run.as_str())),
        Some(("s1", "r4")),
        "{open:?}"
    );
    assert!(open.iter().all(|(_, run)| run == "r4"), "{open:?}");
    // Everything but `streams`: only the run that did the capturing knows what arrived, and the store doesn't
    // hold it, so a run read back reports null rather than guessing.
    let mut stored = sandbox.json(&["changes", "-j"]);
    assert_eq!(stored["streams"], Value::Null);
    assert!(again["streams"].is_array());
    stored["streams"] = again["streams"].clone();
    assert_eq!(stored, again, "changes says what ingest did");

    let changes = sandbox.siftr(&["changes"]);
    assert_eq!(
        changes.status.code(),
        Some(0),
        "an open change is something to look at"
    );
    let human = String::from_utf8(changes.stdout).unwrap();
    assert!(
        human.starts_with("r5 vs 4 baseline runs (r1…r4): no new changes · 1 still open\n"),
        "{human}"
    );
    let reminder = human
        .lines()
        .find(|line| line.starts_with("  still open: "))
        .unwrap_or_else(|| panic!("{human}"));
    assert!(
        reminder.starts_with(
            "  still open: s1 (r4) FREQUENCY GET UsersController#show 2xx  queries 3 → 10"
        ) && reminder.ends_with(" · siftr explain s1"),
        "{human}"
    );
    assert!(human.ends_with("next: siftr explain s1\n"), "{human}");

    let fixed = sandbox.ingest("baseline");
    assert_eq!(fixed["open_signals"], json!([]), "fixed in r6");
    assert!(!sandbox.text(&["changes"]).contains("still open"));
    assert_eq!(
        open_ids(&sandbox.json(&["changes", "r5", "-j"])),
        open,
        "r5 is judged by the runs up to r5"
    );

    sandbox.siftr(&["dismiss", "s1"]);
    assert_eq!(
        sandbox.json(&["changes", "r5", "-j"])["open_signals"],
        json!([]),
        "a dismissed change isn't reminded"
    );
}

/// A run whose comparison produced more changes than siftr will report has no verdict, so it cannot say
/// whether an earlier change is still there: judging it re-runs the very comparison that was refused, in which
/// nearly everything fires. Reminding from it would present as fact what siftr just declined to say.
#[test]
fn a_run_whose_comparison_was_refused_reminds_of_nothing() {
    let sandbox = Sandbox::new();
    // Letters, not digits: the normalizer masks digit runs, which would fold these into one template.
    let letters = |n: usize| {
        let at = |k: usize| (b'a' + (k % 26) as u8) as char;
        format!("{}{}{}", at(n / 676), at(n / 26), at(n))
    };
    let ingest = |name: &str, body: &str| -> Value {
        let path = sandbox.project.path().join(name);
        std::fs::write(&path, body).unwrap();
        sandbox.json(&["ingest", "-j", "--context", "c", path.to_str().unwrap()])
    };
    let base: String = (0..20)
        .map(|i| format!("alpha item {} ready\n", letters(i)))
        .collect();
    let plus = format!("{base}widget zzz ready\n");

    for _ in 0..3 {
        ingest("base.log", &base);
    }
    // One change in r4, and it is still there in r5: the reminder works in this corpus.
    assert_eq!(ingest("plus.log", &plus)["changes"], 1);
    let again = ingest("plus.log", &plus);
    assert_eq!(
        open_ids(&again)
            .first()
            .map(|(id, run)| (id.as_str(), run.as_str())),
        Some(("s1", "r4")),
        "{again:#}"
    );

    // The same input again, plus a flood of templates that each occur once: the comparison is refused. Only
    // that changes between r5 and r6, so it is the refusal that silences the reminder.
    let flood: String = (0..1500)
        .map(|i| format!("flood q{} ready\n", letters(i)))
        .collect();
    let refused = ingest("flood.log", &format!("{plus}{flood}"));
    assert!(
        refused["run"]["uncompared"].as_u64().is_some_and(|n| n > 0),
        "the comparison was refused: {refused:#}"
    );
    assert_eq!(refused["signals"], json!([]), "so it recorded no changes");
    assert_eq!(
        refused["open_signals"],
        json!([]),
        "and it leaves nothing open: it has no verdict to give"
    );

    let human = sandbox.text(&["changes", "r6"]);
    assert!(
        !human.contains("still open"),
        "a refused comparison reminds of nothing: {human}"
    );
    assert_eq!(
        sandbox.siftr(&["changes", "r6"]).status.code(),
        Some(1),
        "nothing to look at, so nothing found"
    );
}

/// Whether an earlier change is still there is decided by re-running its rule on each later run. A truncated
/// run cannot answer: the cap admits behaviors by the arrival order of their first occurrence, so a behavior
/// it lacks may simply not have fitted, and its absences therefore raise nothing. That silence makes its
/// `fires` set omit an earlier DISAPPEARED's key, and an absent key reads as resolved — while the behavior is
/// still gone. Only siftr's willingness to say so changed, so such a run must not adjudicate.
#[test]
fn a_truncated_later_run_does_not_resolve_an_earlier_disappearance() {
    let sandbox = Sandbox::new();
    // One line per distinct template, four letters so the normalizer cannot fold them into one behavior.
    let token = |mut n: usize| -> String {
        (0..4)
            .map(|_| {
                let letter = (b'a' + (n % 26) as u8) as char;
                n /= 26;
                letter
            })
            .collect()
    };
    let ingest = |name: &str, body: &str| -> Value {
        let path = sandbox.project.path().join(format!("{name}.log"));
        std::fs::write(&path, body).unwrap();
        sandbox.json(&["ingest", "-j", "--context", "cap", path.to_str().unwrap()])
    };
    // One under the cap, so adding a single behavior still fits and adding two does not.
    let common: String = (0..MAX_BEHAVIORS - 1)
        .map(|i| format!("widget {} ready\n", token(i)))
        .collect();
    let gone = "gadget zzz done\n";

    // Exactly at the cap: the baseline runs keep every behavior they saw, `gone` included.
    for run in 1..=3 {
        let full = ingest(&format!("run{run}"), &format!("{gone}{common}"));
        assert_eq!(
            full["run"]["overflow_events"].as_u64(),
            Some(0),
            "a baseline run must not be truncated itself: {full}"
        );
    }

    // r4 drops that one behavior and nothing else: one honest DISAPPEARED.
    let disappeared = ingest("run4", &common);
    assert_eq!(
        disappeared["signals"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["kind"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["disappeared"],
        "{disappeared}"
    );

    // r5 lacks it too, and is one behavior over the cap. Only its truncation differs from r4, and it is
    // truncated rather than refused, so nothing but truncation can explain a change of verdict.
    let truncated = ingest(
        "run5",
        &format!("{common}gizmo aaaa ready\ngizmo aaab ready\n"),
    );
    assert!(
        truncated["run"]["overflow_events"]
            .as_u64()
            .is_some_and(|events| events > 0),
        "r5 must actually be truncated: {truncated}"
    );
    assert_eq!(
        truncated["run"]["uncompared"],
        Value::Null,
        "truncated, not refused for producing too many changes"
    );

    let outcomes = sandbox.json(&["history", "--signals", "-j"]);
    let verdict = outcomes
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["signal"]["run"] == "r4" && row["signal"]["kind"] == "disappeared")
        .unwrap_or_else(|| panic!("no DISAPPEARED from r4 in {outcomes}"));
    assert_eq!(
        (&verdict["outcome"], &verdict["resolved_in"]),
        (&json!("open"), &Value::Null),
        "the behavior is still gone; a run that declined to judge absences did not fix it"
    );
}

/// `siftr run -q` is what a coding agent reads, so its summary carries the reminder too.
#[test]
fn the_run_summary_reminds_of_an_unfixed_regression() {
    let sandbox = Sandbox::new();
    let queries = |n: usize| {
        let log: String = (0..n)
            .map(|i| format!("User Load SELECT * FROM users WHERE id = {i}\n"))
            .collect();
        std::fs::write(sandbox.project.path().join("app.log"), log).unwrap();
    };
    let suite = ["run", "-q", "--", "cat", "app.log"];
    let suite_json = ["run", "-j", "--", "cat", "app.log"];

    queries(5);
    for _ in 0..3 {
        sandbox.siftr(&suite);
    }
    queries(20);
    assert_eq!(sandbox.json(&suite_json)["changes"], 1);

    let again = sandbox.siftr(&suite);
    let report = String::from_utf8(again.stderr).unwrap();
    assert!(
        report.starts_with("r5 vs 4 baseline runs (r1…r4): no new changes · 1 still open\n"),
        "{report}"
    );
    assert!(
        report.contains("\n  still open: s1 (r4) FREQUENCY User Load"),
        "{report}"
    );
    assert!(report.ends_with("next: siftr explain s1\n"), "{report}");

    let third = sandbox.json(&suite_json);
    assert_eq!(
        open_ids(&third)
            .first()
            .map(|(id, run)| (id.as_str(), run.as_str())),
        Some(("s1", "r4")),
        "{third:#}"
    );
}
