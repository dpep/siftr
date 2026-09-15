//! `siftr run` on an RSpec command in a Rails project: the listener is wired through SPEC_OPTS, and the events
//! and log slice are captured with offsets relative to the slice, across a log rotation.
//! A shell script stands in for rspec; the real listener is exercised by dogfooding `dogfood/rails_demo`.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use tempfile::TempDir;

/// Checks what siftr set up, then writes the log and events the way the listener would, rotating the log once
/// (twice with ROTATE_TWICE) mid-run.
const FAKE_RSPEC: &str = r#"#!/bin/sh
set -e
log=log/test.log
case "$SPEC_OPTS" in
  "--format documentation --require "*/siftr_rspec_listener.rb) ;;
  *) echo "SPEC_OPTS was not appended to: $SPEC_OPTS" >&2; exit 90 ;;
esac
[ "$SIFTR_RSPEC_LOG" = "$(pwd -P)/log/test.log" ] || { echo "SIFTR_RSPEC_LOG: $SIFTR_RSPEC_LOG" >&2; exit 91; }
grep -q register_listener "${SPEC_OPTS##*--require }"
event() {
  printf '{"event":"%s","log_offset":%s,"log_ino":%s}\n' "$1" \
    "$(wc -c < $log | tr -d ' ')" "$(ls -i $log | awk '{print $1}')" >> "$SIFTR_RSPEC_EVENTS"
}
rotate() { mv $log $log.0; echo '# Logfile created on 2026-09-13 17:00:00 -0700 by logger.rb/v1.7.0' > $log; }
event start
echo 'first line' >> $log
event example
rotate
[ -z "$ROTATE_TWICE" ] || rotate
event example_started
echo 'second line' >> $log
event summary
echo "1 example, 0 failures"
"#;

struct Project {
    home: TempDir,
    dir: TempDir,
}

impl Project {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("log")).unwrap();
        std::fs::write(dir.path().join("log/test.log"), "an earlier run\n").unwrap();
        std::fs::create_dir(dir.path().join("bin")).unwrap();
        let rspec = dir.path().join("bin/rspec");
        std::fs::write(&rspec, FAKE_RSPEC).unwrap();
        std::fs::set_permissions(&rspec, std::fs::Permissions::from_mode(0o755)).unwrap();
        Project {
            home: tempfile::tempdir().unwrap(),
            dir,
        }
    }

    fn run(&self, env: &[(&str, &str)]) -> (Output, Value) {
        let output = Command::new(env!("CARGO_BIN_EXE_siftr"))
            .args(["run", "-j", "--", "bin/rspec"])
            .current_dir(self.dir.path())
            .env("SIFTR_HOME", self.home.path())
            .env("SPEC_OPTS", "--format documentation")
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        let run = serde_json::from_slice(&output.stdout).unwrap();
        (output, run)
    }

    fn captured(&self, run: &Value, file: &str) -> Option<String> {
        let dir = Path::new(self.home.path())
            .join("runs")
            .join(run["run"]["id"].as_str().unwrap());
        std::fs::read_to_string(dir.join(file)).ok()
    }
}

#[test]
fn events_index_the_captured_log_slice_across_a_rotation() {
    let project = Project::new();
    let (_, run) = project.run(&[]);

    let log = project
        .captured(&run, "file-log_test.log")
        .expect("log slice");
    assert_eq!(
        log, "first line\nsecond line\n",
        "the run's bytes, without the new file's header"
    );
    let events = project
        .captured(&run, "file-rspec-events.log")
        .expect("events");
    assert_eq!(
        events,
        concat!(
            "{\"event\":\"start\",\"log_offset\":0}\n",
            "{\"event\":\"example\",\"log_offset\":11}\n",
            "{\"event\":\"example_started\",\"log_offset\":11}\n",
            "{\"event\":\"summary\",\"log_offset\":23}\n",
        )
    );
    for line in events.lines() {
        let offset = serde_json::from_str::<Value>(line).unwrap()["log_offset"]
            .as_u64()
            .unwrap() as usize;
        assert!(
            offset == log.len() || offset == 0 || log.as_bytes()[offset - 1] == b'\n',
            "{line}"
        );
    }
}

#[test]
fn a_log_that_cannot_be_placed_is_skipped_loudly_and_no_offsets_are_kept() {
    let project = Project::new();
    let (output, run) = project.run(&[("ROTATE_TWICE", "1")]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("log/test.log skipped: it rotated more than once"),
        "{stderr}"
    );
    assert_eq!(project.captured(&run, "file-log_test.log"), None);
    let events = project
        .captured(&run, "file-rspec-events.log")
        .expect("events");
    assert!(!events.contains("log_offset"), "{events}");
    assert_eq!(events.lines().count(), 4);
}
