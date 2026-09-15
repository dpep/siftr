//! What the store writes whole, a run's command and working directory and a feedback note, is redacted before
//! insert. Synthetic secrets only.

use std::time::{Duration, SystemTime};

use rusqlite::Connection;
use siftr::analyze::{Analysis, Analyzer};
use siftr::context::Context;
use siftr::observation::{LineSplitter, Stream};
use siftr::store::{Feedback, FeedbackKind, Finished, Interface, NewRun, RunEnd, Store};

const GHP: &str = concat!("ghp_", "Q7m2Xk9Lp4Rz8Wv1Tn6Ys3Hb5Jd0Fc2GaK8x");

fn analyze(text: &str) -> Analysis {
    let mut analyzer = Analyzer::new();
    let stream = Stream::Stdout;
    LineSplitter::new().feed(&stream, text.as_bytes(), |obs| analyzer.observe(obs));
    analyzer.finish()
}

#[test]
fn a_runs_command_cwd_and_a_note_are_stored_masked() {
    let home = tempfile::tempdir().unwrap();
    let mut store = Store::open(home.path()).unwrap();
    let command = format!("deploy --token={GHP}");
    let context = Context::named("/app", "deploy");
    let run = store
        .begin_run(&NewRun {
            context: &context,
            command: &command,
            cwd: concat!("/app/password=", "Zq8vN2kLp4RxQm7T"),
            started_at: SystemTime::now(),
        })
        .unwrap();
    let analysis = analyze("hello\n");
    let finished = Finished {
        end: RunEnd {
            wall: Duration::from_millis(1),
            exit_code: Some(0),
            lines: 1,
        },
        analysis: &analysis,
        baseline_runs: &[],
        signals: &[],
    };
    store.finish_run(run, &finished).unwrap();
    let behavior = analysis.aggregates[0].behavior.id;
    let feedback = Feedback {
        note: Some(format!("rotated {GHP} after the leak")),
        ..Feedback::on_behavior(FeedbackKind::Acked, "ack", Interface::Human, run, behavior)
    };
    store.record_feedback(&[feedback]).unwrap();
    drop(store);

    let db = Connection::open(home.path().join("siftr.db")).unwrap();
    let row: (String, String, String) = db
        .query_row(
            "SELECT r.command, r.cwd, f.note FROM runs r JOIN feedback f ON f.run_id = r.id",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        row,
        (
            "deploy --token=<TOKEN_1>".to_owned(),
            "/app/password=<SECRET_1>".to_owned(),
            "rotated <TOKEN_1> after the leak".to_owned()
        )
    );
}
