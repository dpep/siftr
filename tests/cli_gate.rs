//! `script/gate` records the test step with siftr, so the suite that step runs has to hold siftr to
//! principle 6 from the outside: a siftr that is missing, unrunnable or refusing must not change what
//! the gate says about the command.

use std::path::PathBuf;
use std::process::Command;

fn script(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("script")
        .join(name)
}

/// The self-test's own report, from a run that must have succeeded.
fn self_test(gate: &str) -> String {
    let output = Command::new(script("gate"))
        .arg("--self-test")
        // The row named `real` uses this rather than a release build that may not exist here.
        .env("SIFTR_BIN", env!("CARGO_BIN_EXE_siftr"))
        .env("SIFTR_GATE", gate)
        .output()
        .unwrap();
    let report = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{report}");
    report
}

#[test]
fn a_missing_or_broken_siftr_cannot_change_a_gate_step_s_code_or_output() {
    let report = self_test("on");
    for row in ["absent", "broken", "refuses (wrapping)", "real (wrapping)"] {
        assert!(
            report.contains(row),
            "no {row} row in the self-test:\n{report}"
        );
    }
}

#[test]
fn siftr_gate_off_beats_a_siftr_that_works() {
    let report = self_test("off");
    assert!(
        report.contains("real passes") && !report.contains("(wrapping)"),
        "the kill switch left something wrapped:\n{report}"
    );
}
