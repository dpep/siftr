//! A line's leading timestamp is when it was written, never what it says: a
//! ctime weekday and an ISO-8601 line's host must not split one statement into
//! one behavior per day or per machine. Hostnames and messages are synthetic.

use std::io::Write as _;
use std::process::{Command, Stdio};

use serde_json::Value;
use siftr::normalize::{Normalizer, SlotKind};

/// Ingests `log` as one run of its own context, then counts the run's behaviors.
fn behaviors(log: &str, context: &str) -> Vec<String> {
    let home = tempfile::tempdir().unwrap();
    let siftr = |args: &[&str], input: Option<&str>| -> Value {
        let mut child = Command::new(env!("CARGO_BIN_EXE_siftr"))
            .args(args)
            .current_dir(home.path())
            .env("SIFTR_HOME", home.path())
            .env_remove("XDG_DATA_HOME")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin
            .write_all(input.unwrap_or_default().as_bytes())
            .unwrap();
        drop(stdin);
        let output = child.wait_with_output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    };
    siftr(&["ingest", "-j", "--context", context], Some(log));
    siftr(&["summary", "r1", "-j"], None)["behaviors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["behavior"]["template"].as_str().unwrap().to_string())
        .collect()
}

fn templates(cases: &[(&str, &str)]) {
    let mut n = Normalizer::new();
    let mut failures = Vec::new();
    for (line, want) in cases {
        let got = String::from_utf8(n.normalize(line.as_bytes()).template.to_vec()).unwrap();
        if got != *want {
            failures.push(format!("  line: {line:?}\n   got: {got}\n  want: {want}"));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn a_ctime_weekday_is_part_of_the_timestamp() {
    templates(&[
        // The ctime shape `Www Mmm DD HH:MM:SS[.mmm]`, with and without a syslog tail.
        (
            "Thu Sep 10 00:33:52.607 [airport]/637 @[1.6] (foo.m:6110) widget ready",
            "<timestamp> [airport]/<int> @[<float>] (foo.m:<int>) widget ready",
        ),
        (
            "Sun Jan  1 00:00:00.000 starting up",
            "<timestamp> starting up",
        ),
        (
            "Mon Oct  5 09:00:00 mbp-a backupd[412]: Backup completed",
            "<timestamp> <host> backupd[<int>]: Backup completed",
        ),
    ]);
}

#[test]
fn a_weekday_is_masked_only_as_part_of_a_timestamp_it_precedes() {
    templates(&[
        // A leading word that happens to spell a weekday.
        ("Sat down and waited", "Sat down and waited"),
        ("Sun Jan brochure printed", "Sun Jan brochure printed"),
        ("Thu Sep 10 arrived", "Thu Sep <int> arrived"),
        ("Sat Sep 10 00:33 late", "Sat Sep <int> <int>:<int> late"),
        // Not the ctime shape: a day out of range, a longer name, the wrong case.
        ("Wed Sep 32 00:33:52 x", "Wed Sep <int> <timestamp> x"),
        ("Thur Sep 10 00:33:52 x", "Thur Sep <int> <timestamp> x"),
        (
            "Tuesday Sep 10 00:33:52 x",
            "Tuesday Sep <int> <timestamp> x",
        ),
        ("thu Sep 10 00:33:52 x", "thu Sep <int> <timestamp> x"),
        ("Thu 10 Sep 00:33:52 x", "Thu <int> Sep <timestamp> x"),
        // Only at the start of a line, as for a month word.
        (
            "Backup on Thu Sep 10 00:33:52.607 done",
            "Backup on Thu Sep <int> <timestamp> done",
        ),
    ]);
}

#[test]
fn the_host_after_an_iso_timestamp_is_a_slot() {
    templates(&[
        (
            "2026-09-10 00:33:52.607000-0700 alpha-host foo[637]: widget ready",
            "<timestamp> <host> foo[<int>]: widget ready",
        ),
        (
            "2026-09-10 00:33:52 build-01.local com.apple.xpc.launchd[1]: Service exited",
            "<timestamp> <host> com.apple.xpc.launchd[<int>]: Service exited",
        ),
    ]);
}

#[test]
fn a_word_after_an_iso_timestamp_is_a_host_only_with_the_whole_syslog_shape() {
    templates(&[
        // No `proc[pid]:`, so the next word is the message, not a machine.
        (
            "2026-09-10 00:33:52 deploy started",
            "<timestamp> deploy started",
        ),
        (
            "2026-09-10 00:33:52 alpha-host foo: no pid",
            "<timestamp> alpha-host foo: no pid",
        ),
        (
            "2026-09-10 00:33:52.607000-0700 worker ready to serve",
            "<timestamp> worker ready to serve",
        ),
        // `log show`'s column format separates with two spaces, not one.
        (
            "2026-09-10 00:33:52.607000-0700  0x1a2b  Default  0x0  1234  0  backupd: (Subsystem) [Category] Backup completed",
            "<timestamp> <hex> Default <hex> <int> <int> backupd: (Subsystem) [Category] Backup completed",
        ),
    ]);
}

#[test]
fn prefix_slots_span_the_raw_weekday_and_host() {
    let mut n = Normalizer::new();
    let line = "Mon Oct  5 09:00:00.250 mbp-a backupd[412]: took 5ms";
    let slots: Vec<(SlotKind, &str)> = n
        .normalize(line.as_bytes())
        .slots
        .iter()
        .map(|s| {
            (
                s.kind,
                std::str::from_utf8(s.text(line.as_bytes())).unwrap(),
            )
        })
        .collect();
    assert_eq!(
        slots,
        [
            (SlotKind::Timestamp, "Mon Oct  5 09:00:00.250"),
            (SlotKind::Host, "mbp-a"),
            (SlotKind::Int, "412"),
            (SlotKind::Duration, "5ms"),
        ]
    );

    let iso = "2026-09-10 00:33:52.607000-0700 mbp-a backupd[412]: took 5ms";
    let spans: Vec<(SlotKind, &str)> = n
        .normalize(iso.as_bytes())
        .slots
        .iter()
        .map(|s| (s.kind, std::str::from_utf8(s.text(iso.as_bytes())).unwrap()))
        .collect();
    assert_eq!(
        spans,
        [
            (SlotKind::Timestamp, "2026-09-10 00:33:52.607000-0700"),
            (SlotKind::Host, "mbp-a"),
            (SlotKind::Int, "412"),
            (SlotKind::Duration, "5ms"),
        ]
    );
}

#[test]
fn one_statement_is_one_template_whatever_day_host_or_timestamp_format_it_carries() {
    let mut n = Normalizer::new();
    let hashes: Vec<u64> = [
        "Sep 10 00:33:52 alpha-host foo[637]: widget ready",
        "Sep 10 00:33:52 beta-host foo[638]: widget ready",
        "Sep 10 00:33:52.607 alpha-host foo[637]: widget ready",
        "Sep 10 00:33:52.607 beta-host foo[638]: widget ready",
        "2026-09-10 00:33:52.607000-0700 alpha-host foo[637]: widget ready",
        "2026-09-10 00:33:52.607000-0700 beta-host foo[638]: widget ready",
        "Thu Sep 10 00:33:52.607 alpha-host foo[637]: widget ready",
        "Sun Dec 31 23:59:59 beta-host foo[1]: widget ready",
    ]
    .iter()
    .map(|line| n.normalize(line.as_bytes()).template_hash)
    .collect();
    assert!(hashes.iter().all(|&h| h == hashes[0]), "{hashes:?}");
}

/// `docs/findings/dogfood-system-logs.md` §2.1: three weekdays, three behaviors.
#[test]
fn three_weekdays_of_one_statement_are_one_behavior() {
    let log = "Thu Sep 10 00:33:52.607 [airport]/637 @[1.6] (foo.m:6110) widget ready\n\
               Fri Sep 11 00:33:52.607 [airport]/637 @[1.6] (foo.m:6110) widget ready\n\
               Sat Sep 12 00:33:52.607 [airport]/637 @[1.6] (foo.m:6110) widget ready\n";
    assert_eq!(
        behaviors(log, "ts"),
        ["<timestamp> [airport]/<int> @[<float>] (foo.m:<int>) widget ready"]
    );
}

/// §2.2: two hosts in three timestamp formats, one statement.
#[test]
fn two_hosts_in_three_timestamp_formats_are_one_behavior() {
    let log = "Sep 10 00:33:52 alpha-host foo[637]: widget ready\n\
               Sep 10 00:33:52 beta-host foo[638]: widget ready\n\
               Sep 10 00:33:52.607 alpha-host foo[637]: widget ready\n\
               Sep 10 00:33:52.607 beta-host foo[638]: widget ready\n\
               2026-09-10 00:33:52.607000-0700 alpha-host foo[637]: widget ready\n\
               2026-09-10 00:33:52.607000-0700 beta-host foo[638]: widget ready\n";
    assert_eq!(
        behaviors(log, "hosts"),
        ["<timestamp> <host> foo[<int>]: widget ready"]
    );
}
