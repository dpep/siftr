//! `siftr status` says what the data dir holds and what retention will do; `siftr gc` does exactly that.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

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

    /// `siftr` with retention from `env` alone.
    fn siftr(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_siftr"));
        command
            .args(args)
            .current_dir(self.project.path())
            .env("SIFTR_HOME", self.home.path())
            .env_remove("XDG_DATA_HOME");
        for name in ["SIFTR_KEEP_RUNS", "SIFTR_KEEP_EVIDENCE", "SIFTR_KEEP_DAYS"] {
            command.env_remove(name);
        }
        command.envs(env.iter().copied()).output().unwrap()
    }

    fn json(&self, args: &[&str], env: &[(&str, &str)], code: i32) -> Value {
        let output = self.siftr(args, env);
        assert_eq!(
            output.status.code(),
            Some(code),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn ingest(&self) {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/rails_demo/baseline");
        let args = [
            "ingest",
            "--context",
            "demo",
            "--dir",
            dir.to_str().unwrap(),
        ];
        assert!(self.siftr(&args, &[]).status.success());
    }

    fn captured(&self, run: &str) -> bool {
        self.home.path().join("runs").join(run).exists()
    }
}

#[test]
fn status_reads_without_creating_and_gc_prunes_what_it_reports() {
    let sandbox = Sandbox::new();
    let empty = sandbox.json(&["status", "-j"], &[], 0);
    assert_eq!(empty["database"], Value::Null);
    assert!(
        !sandbox.home.path().join("siftr.db").exists(),
        "status is read-only"
    );

    // r14's baseline is r4–r13, and r4 was judged against r1–r3: past SIFTR_KEEP_EVIDENCE=2, only r1 and r2
    // point nowhere a reminder or explain still reads.
    for _ in 0..14 {
        sandbox.ingest();
    }
    let status = sandbox.json(&["status", "-j"], &[], 0);
    assert_eq!(
        status["database"]["schema"],
        status["database"]["supported_schema"]
    );
    assert_eq!(status["captures"]["runs"], 14);
    assert_eq!(status["retention"]["evidence"]["source"], "default");
    assert_eq!(status["commands"][0]["runs"], 14);
    assert_eq!(status["problems"], serde_json::json!([]));

    let two = [("SIFTR_KEEP_EVIDENCE", "2")];
    let status = sandbox.json(&["status", "-j"], &two, 0);
    assert_eq!(status["pending"]["evidence"], 2);
    assert_eq!(status["retention"]["evidence"]["source"], "env");

    let dry = sandbox.json(&["gc", "--dry-run", "-j"], &two, 0);
    let steps: Vec<(&str, &str, &str)> = dry["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["run"].as_str().unwrap(),
                s["tier"].as_str().unwrap(),
                s["by"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        steps,
        ["r1", "r2"].map(|run| (run, "evidence", "SIFTR_KEEP_EVIDENCE=2"))
    );
    assert!(sandbox.captured("r1"), "a dry run removes nothing");

    let gc = sandbox.json(&["gc", "-j"], &two, 0);
    assert_eq!(gc["steps"], dry["steps"]);
    assert!(gc["capture_bytes"].as_u64().unwrap() > 0);
    assert!(!sandbox.captured("r2") && sandbox.captured("r3"));
    let status = sandbox.json(&["status", "-j"], &two, 0);
    assert_eq!(status["pending"]["evidence"], 0);
    assert_eq!(status["commands"][0]["with_evidence"], 12);

    let bad = sandbox.json(&["status", "-j"], &[("SIFTR_KEEP_RUNS", "lots")], 1);
    assert_eq!(
        bad["problems"][0],
        "SIFTR_KEEP_RUNS=lots is not a whole number; siftr uses 100"
    );
}
