//! No credential siftr recognizes reaches disk, while the wrapped command's output passes through unchanged and
//! log lines stay attributed to the right example. Every secret here is synthetic.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rusqlite::Connection;
use serde_json::Value;
use tempfile::TempDir;

const GHP: &str = concat!("ghp_", "Q7m2Xk9Lp4Rz8Wv1Tn6Ys3Hb5Jd0Fc2GaK8x");
const AKIA: &str = "AKIAQ7M2XK9LP4RZ8WV1";
const STRIPE: &str = concat!("sk_live_", "Q7m2Xk9Lp4Rz8Wv1Tn6Ys3Hb");

/// The lines that reproduced the exposure on 0.1.0.
fn exposure() -> String {
    format!("Authorization: Bearer {GHP}\naws access key {AKIA} loaded\nstripe key={STRIPE} ok\n")
}

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

    fn siftr(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let output = Command::new(env!("CARGO_BIN_EXE_siftr"))
            .args(args)
            .current_dir(self.project.path())
            .env("SIFTR_HOME", self.home.path())
            .env_remove("XDG_DATA_HOME")
            .env_remove("SIFTR_REDACT")
            .env_remove("SIFTR_CAPTURE")
            .envs(env.iter().copied())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn ingest(&self, text: &str, env: &[(&str, &str)]) {
        let file = self.project.path().join("input.log");
        std::fs::write(&file, text).unwrap();
        self.siftr(&["ingest", file.to_str().unwrap()], env);
    }

    fn db(&self) -> Connection {
        Connection::open(self.home.path().join("siftr.db")).unwrap()
    }

    fn column(&self, sql: &str) -> Vec<String> {
        let db = self.db();
        let mut stmt = db.prepare(sql).unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn capture(&self, run: &str, file: &str) -> Option<String> {
        std::fs::read_to_string(self.home.path().join("runs").join(run).join(file)).ok()
    }

    /// Every file under the data dir, database and its WAL included, that contains `needle`.
    fn files_containing(&self, needle: &str) -> Vec<PathBuf> {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, out);
                } else {
                    out.push(path);
                }
            }
        }
        let mut files = Vec::new();
        walk(self.home.path(), &mut files);
        files
            .into_iter()
            .filter(|file| {
                let bytes = std::fs::read(file).unwrap();
                bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
            })
            .collect()
    }
}

#[test]
fn an_ingested_credential_never_reaches_disk() {
    let sandbox = Sandbox::new();
    sandbox.ingest(&exposure(), &[]);

    for secret in [GHP, AKIA, STRIPE] {
        assert_eq!(
            sandbox.files_containing(secret),
            Vec::<PathBuf>::new(),
            "{secret}"
        );
    }
    let mut templates = sandbox.column("SELECT template FROM behaviors");
    templates.sort();
    assert_eq!(
        templates,
        [
            "Authorization: Bearer <TOKEN>",
            "aws access key <TOKEN> loaded",
            "stripe key=<TOKEN> ok"
        ]
    );
    assert_eq!(
        sandbox.capture("r1", "stdout.log").unwrap(),
        "Authorization: Bearer <TOKEN_1>\naws access key <TOKEN_2> loaded\nstripe key=<TOKEN_3> ok\n"
    );
    let mut lines = sandbox.column("SELECT line FROM exemplars");
    lines.sort();
    assert_eq!(lines[0], "Authorization: Bearer <TOKEN_1>");
}

#[test]
fn run_passes_the_command_output_through_unchanged_and_stores_it_masked() {
    let sandbox = Sandbox::new();
    let script = format!(
        concat!(
            "printf '%s' '{}'; printf '",
            "password=",
            "Zq8vN2kLp4Rx",
            "\\r\\nno newline {}'"
        ),
        exposure(),
        GHP
    );
    let output = sandbox.siftr(&["run", "--", "sh", "-c", &script], &[]);

    let expected = format!(
        concat!("{}", "password=", "Zq8vN2kLp4Rx", "\r\nno newline {}"),
        exposure(),
        GHP
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        expected,
        "the terminal sees the raw bytes"
    );
    assert_eq!(
        sandbox.capture("r1", "stdout.log").unwrap(),
        "Authorization: Bearer <TOKEN_1>\naws access key <TOKEN_2> loaded\nstripe key=<TOKEN_3> ok\n\
         password=<SECRET_1>\r\nno newline <TOKEN_1>",
        "same line numbers and endings, masked"
    );
    for secret in [GHP, AKIA, STRIPE, "Zq8vN2kLp4Rx"] {
        assert_eq!(
            sandbox.files_containing(secret),
            Vec::<PathBuf>::new(),
            "{secret}"
        );
    }
    let command = &sandbox.column("SELECT command FROM runs")[0];
    assert!(command.contains("Bearer <TOKEN_1>"), "{command}");
    assert_eq!(sandbox.column("SELECT context FROM runs")[0], *command);
}

#[test]
fn settings_choose_what_evidence_keeps_but_never_what_templates_hold() {
    let text = format!("login pat.jones@example.com from 198.51.100.23 with {GHP}\n");
    let run = |env: &[(&str, &str)]| {
        let sandbox = Sandbox::new();
        sandbox.ingest(&text, env);
        let template = sandbox
            .column("SELECT id || ' ' || template FROM behaviors")
            .remove(0);
        let exemplar = sandbox.column("SELECT line FROM exemplars").remove(0);
        (
            sandbox.capture("r1", "stdout.log"),
            exemplar,
            template,
            sandbox,
        )
    };

    let (capture, exemplar, secrets, _) = run(&[]);
    let masked = "login pat.jones@example.com from 198.51.100.23 with <TOKEN_1>";
    assert_eq!(
        (capture.as_deref(), exemplar.as_str()),
        (Some(&*format!("{masked}\n")), masked)
    );

    let (capture, exemplar, pii, _) = run(&[("SIFTR_REDACT", "pii")]);
    let private = "login <EMAIL_1> from <IP_1> with <TOKEN_1>";
    assert_eq!(
        (capture.as_deref(), exemplar.as_str()),
        (Some(&*format!("{private}\n")), private)
    );

    let (capture, exemplar, off, sandbox) = run(&[("SIFTR_REDACT", "off")]);
    assert_eq!(
        capture.as_deref(),
        Some(text.as_str()),
        "off: the raw capture"
    );
    assert_eq!(
        exemplar, masked,
        "kept lines stay masked: the database never holds a credential"
    );
    assert_eq!(sandbox.files_containing(GHP).len(), 1, "the capture only");

    let (capture, exemplar, no_capture, _) = run(&[("SIFTR_CAPTURE", "off")]);
    assert_eq!((capture, exemplar.as_str()), (None, masked));

    assert_eq!(
        [&pii, &off, &no_capture],
        [&secrets; 3],
        "behavior ids don't depend on settings"
    );
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/rails_demo/baseline")
        .join(name)
}

/// The baseline scenario with `line` inserted into `log/test.log` where the second example starts, and the
/// listener's offsets moved past it as a real run would have measured them.
fn with_log_line(line: &str) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let log = std::fs::read(fixture("test.log")).unwrap();
    let events = std::fs::read_to_string(fixture("rspec.ndjson")).unwrap();
    let offsets: Vec<u64> = events
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|e| e["event"] == "example_started")
        .filter_map(|e| e["log_offset"].as_u64())
        .collect();
    let at = offsets[1];
    let mut moved = log[..at as usize].to_vec();
    moved.extend_from_slice(line.as_bytes());
    moved.extend_from_slice(&log[at as usize..]);
    std::fs::write(dir.path().join("test.log"), moved).unwrap();
    let events: String = events
        .lines()
        .map(|l| {
            let mut event: Value = serde_json::from_str(l).unwrap();
            if let Some(offset) = event["log_offset"].as_u64().filter(|&o| o >= at) {
                event["log_offset"] = (offset + line.len() as u64).into();
            }
            event.to_string() + "\n"
        })
        .collect();
    std::fs::write(dir.path().join("rspec.ndjson"), events).unwrap();
    for name in ["stdout.txt", "stderr.txt", "exit_code.txt"] {
        std::fs::copy(fixture(name), dir.path().join(name)).unwrap();
    }
    dir
}

/// Redaction shortens a log line, but log offsets count the real file's bytes: every query keeps its example.
#[test]
fn a_redacted_log_line_leaves_every_query_in_its_example() {
    let scopes = |line: &str| {
        let scenario = with_log_line(line);
        let sandbox = Sandbox::new();
        sandbox.siftr(&["ingest", "--dir", scenario.path().to_str().unwrap()], &[]);
        let mut scopes = sandbox.column(
            "SELECT b.template || ' @ ' || e.template || ' x' || s.count FROM aggregate_scopes s
             JOIN behaviors b ON b.id = s.behavior_id LEFT JOIN behaviors e ON e.id = s.scope_id
             WHERE b.kind = 'db.query'",
        );
        scopes.sort();
        let captured = sandbox.capture("r1", "file-log_test.log").unwrap();
        (scopes, captured, sandbox)
    };
    let credential = concat!("  Rails config: password=", "Zq8vN2kLp4RxQm7Tz9Lw\n");
    let same_length = "  Rails config: username=Zq8vN2kLp4RxQm7Tz9Lw\n";
    assert_eq!(credential.len(), same_length.len());

    let (redacted, captured, sandbox) = scopes(credential);
    let (plain, _, _) = scopes(same_length);
    assert!(
        redacted.len() > 1,
        "queries in more than one example: {redacted:?}"
    );
    assert_eq!(
        redacted, plain,
        "the credential's line moved no query to another example"
    );

    let source =
        std::fs::read_to_string(with_log_line(credential).path().join("test.log")).unwrap();
    assert_eq!(
        captured.lines().count(),
        source.lines().count(),
        "line numbers stay aligned"
    );
    let at = source
        .lines()
        .position(|l| l.contains("password="))
        .unwrap();
    assert_eq!(
        captured.lines().nth(at),
        Some("  Rails config: password=<SECRET_1>")
    );
    assert!(sandbox.files_containing("Zq8vN2kLp4RxQm7Tz9Lw").is_empty());
}
