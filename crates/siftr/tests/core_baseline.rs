//! Baseline eligibility end to end: a captured run that didn't run the whole suite, replayed with
//! `siftr ingest --dir`, must not silence the rules for the runs after it.

use std::path::Path;
use std::process::Command;

use serde_json::Value;

fn ingest(home: &Path, project: &Path, scenario: &str) -> Value {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/rails_demo")
        .join(scenario);
    let output = Command::new(env!("CARGO_BIN_EXE_siftr"))
        .args(["ingest", "--context", "rails_demo", "-j", "--dir"])
        .arg(dir)
        .current_dir(project)
        .env("SIFTR_HOME", home)
        .env_remove("XDG_DATA_HOME")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{scenario}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// A syntax error in `users_spec.rb`: RSpec runs the other files, so only its error count marks the missing examples.
#[test]
fn a_load_error_run_in_the_baseline_does_not_hide_an_n_plus_one() {
    let (home, project) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    for scenario in [
        "baseline",
        "baseline_2",
        "load_error",
        "baseline_documentation",
    ] {
        ingest(home.path(), project.path(), scenario);
    }
    let changes = ingest(home.path(), project.path(), "n_plus_one");
    assert_eq!(
        changes["baseline_runs"],
        serde_json::json!(["r4", "r2", "r1"]),
        "r3, the load error, is skipped"
    );
    let headline = changes["signals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["group"] == 1 && s["headline"] == true)
        .expect("a headline signal");
    assert_eq!(
        (
            headline["kind"].as_str(),
            headline["measure"].as_str(),
            headline["current"].as_f64(),
            headline["baseline"]["runs"].as_u64(),
            headline["behavior"]["template"].as_str(),
        ),
        (
            Some("frequency"),
            Some("queries"),
            Some(10.0),
            Some(3),
            Some("GET UsersController#show 2xx")
        )
    );
}
