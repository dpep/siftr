//! When a reminder expires. A regression that is fixed and breaks again keeps being reported for as long as it
//! returns; one that is simply left in place stops once every run siftr compares against shows it.
//!
//! The rolling baseline absorbs a value it has already seen, so no rule can fire on either case: by r16 the N+1's
//! query count reads `[1,9,1,9,1,9,1,9,1,9]` against a current 9, inside its own range by construction. Only the
//! signal's frozen baseline still says 9 is a change. Bounding that judgement by the baseline window — which is
//! what 0.1.6 did — made the alternating case go silent on its sixth return with the regression present.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

const CLEAN: [&str; 3] = ["baseline", "baseline_2", "baseline_documentation"];

struct Sandbox {
    home: TempDir,
    project: TempDir,
}

impl Sandbox {
    /// Three clean runs, so r4 onwards has a baseline that can raise the N+1.
    fn new() -> Self {
        let sandbox = Sandbox {
            home: tempfile::tempdir().unwrap(),
            project: tempfile::tempdir().unwrap(),
        };
        for scenario in CLEAN {
            sandbox.ingest(scenario);
        }
        sandbox
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

    fn ingest(&self, scenario: &str) -> Value {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/rails_demo")
            .join(scenario);
        let args = ["ingest", "-j", "--context", "rd", "--dir"];
        let output = self.siftr(&[&args[..], &[dir.to_str().unwrap()]].concat());
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// `changes -j` for one run, read back from the store.
    fn json_changes(&self, run: &str) -> Value {
        let output = self.siftr(&["changes", run, "-j"]);
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

/// `(id, run)` of each signal reported as still open.
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

/// The N+1 raised in r4, then fixed and broken again on alternating runs. Its own run leaves the 10-run baseline
/// window after r14, but half of every later window is a clean run, so the change never becomes normal here — and
/// the run that has it is the run a CI gate is about to pass.
#[test]
fn a_regression_that_keeps_returning_is_reported_on_every_return() {
    let sandbox = Sandbox::new();
    assert_eq!(sandbox.ingest("n_plus_one")["changes"], 1, "raised in r4");

    // r5…r15: fixed, broken, fixed, … The change is present in r6, r8, r10, r12, r14 and r16.
    for _ in 0..6 {
        let fixed = sandbox.ingest("baseline");
        assert_eq!(open_ids(&fixed), [], "nothing open on a run that is clean");
        let broken = sandbox.ingest("n_plus_one");
        assert_eq!(broken["changes"], 0, "the baseline has absorbed the value");
        assert_eq!(
            open_ids(&broken)
                .first()
                .map(|(id, run)| (id.as_str(), run.as_str())),
            Some(("s1", "r4")),
            "the N+1 is present and must be reported: {broken:#}"
        );
    }

    // r16 is the sixth return, the first past the window. A gate reading `changes` alone passes it.
    let human = String::from_utf8(sandbox.siftr(&["changes", "r16"]).stdout).unwrap();
    assert!(
        human.contains("still open"),
        "r16 must not report silence while the N+1 is there: {human}"
    );
    assert_eq!(
        sandbox.siftr(&["changes", "r16"]).status.code(),
        Some(0),
        "a regression that is present is something to look at"
    );
}

/// The other half of the rule, and the reason it is not "never expire": a change nobody fixed is reported until
/// every run siftr compares against has it, and then it is what this context does. Without this, an accepted
/// change would nag for as long as the project lives and `dismiss` would stop being optional.
#[test]
fn a_change_left_in_place_stops_being_reported_once_every_run_has_it() {
    let sandbox = Sandbox::new();
    // r4…r15 all have it.
    for _ in 0..12 {
        sandbox.ingest("n_plus_one");
    }

    // r14's window is r4…r13, so the signal's own run is still in it.
    assert_eq!(
        open_ids(&sandbox.json_changes("r14"))
            .first()
            .map(|(id, run)| (id.as_str(), run.as_str())),
        Some(("s1", "r4")),
        "r4 is still in r14's baseline window"
    );

    // r15's window is r5…r14: every run it compares against has the N+1, and it has been there on all of them.
    assert_eq!(
        open_ids(&sandbox.json_changes("r15")),
        [],
        "present on every run since r4, so by r15 it is what this context does"
    );
}

/// Dismissal must bound the nagging whatever the expiry rule is: a change the developer called intended is never
/// reported again, however many times it comes and goes.
#[test]
fn a_dismissed_regression_stays_dismissed_however_often_it_returns() {
    let sandbox = Sandbox::new();
    sandbox.ingest("n_plus_one");
    sandbox.siftr(&["dismiss", "s1", "-m", "intended"]);

    for _ in 0..6 {
        sandbox.ingest("baseline");
        let broken = sandbox.ingest("n_plus_one");
        assert_eq!(
            open_ids(&broken),
            [],
            "dismissed in r4, so no return of it is reported: {broken:#}"
        );
    }
    assert!(
        !String::from_utf8(sandbox.siftr(&["changes", "r16"]).stdout)
            .unwrap()
            .contains("still open"),
        "a dismissed change is never reminded"
    );
}
