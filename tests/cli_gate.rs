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

#[test]
fn a_missing_or_broken_siftr_cannot_change_a_gate_step_s_exit_code() {
    let output = Command::new(script("gate"))
        .arg("--self-test")
        // The row named `real` uses this rather than a release build that may not exist here.
        .env("SIFTR_BIN", env!("CARGO_BIN_EXE_siftr"))
        .env_remove("SIFTR_GATE")
        .output()
        .unwrap();
    let report = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{report}");
    for row in ["absent", "broken", "refuses", "real (wrapping)", "off"] {
        assert!(
            report.contains(row),
            "no {row} row in the self-test:\n{report}"
        );
    }
}
