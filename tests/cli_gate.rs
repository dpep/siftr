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

/// The gate's baseline is only worth having if it is a baseline of the siftr being developed. It used to
/// prefer `target/release` unconditionally, so a release build from last week went on recording runs whose
/// analyzer it predated -- and nothing said which binary was doing the recording.
#[test]
fn the_gate_records_with_the_newest_local_build_and_says_which() {
    use std::fs;
    use std::time::{Duration, SystemTime};

    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("script")).unwrap();
    fs::copy(script("gate"), root.path().join("script/gate")).unwrap();

    let build = |profile: &str, at: SystemTime| {
        let dir = root.path().join("target").join(profile);
        fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("siftr");
        fs::write(
            &bin,
            "#!/bin/sh\ncase $1 in --version) echo 'siftr 0';; *) exit 125;; esac\n",
        )
        .unwrap();
        let mut perms = fs::metadata(&bin).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        fs::set_permissions(&bin, perms).unwrap();
        // Closed before the gate runs it: Linux refuses to exec a file anyone still holds open for
        // writing (ETXTBSY), where macOS doesn't care.
        let file = fs::File::options().write(true).open(&bin).unwrap();
        file.set_times(fs::FileTimes::new().set_modified(at))
            .unwrap();
        drop(file);
        bin
    };
    // `bash SCRIPT` rather than exec'ing the script: a copy this fresh can still be ETXTBSY on Linux, and
    // `$0` still resolves ROOT either way. Executing it directly passed here and failed on CI.
    let which = || {
        let output = Command::new("bash")
            .arg(root.path().join("script/gate"))
            .arg("--which")
            .env_remove("SIFTR_BIN")
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    };

    let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let new = old + Duration::from_secs(86_400);

    let release = build("release", new);
    let debug = build("debug", old);
    assert_eq!(which(), release.to_string_lossy(), "release is newer here");

    build("debug", new + Duration::from_secs(1));
    assert_eq!(
        which(),
        debug.to_string_lossy(),
        "and a debug build made since wins, because that is the siftr being written"
    );
}
