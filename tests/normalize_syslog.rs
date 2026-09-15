//! System logs: what a line's day, host or connection spells must not split its behavior.
//! Hostnames and messages are synthetic.

use std::io::Write as _;
use std::process::{Command, Stdio};

use serde_json::Value;
use siftr::normalize::{Normalizer, SlotKind};

#[test]
fn one_message_across_days_hosts_and_months_is_one_behavior_with_no_changes() {
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
    let days = [
        ("Sep 14", "mbp-a"),
        ("Sep 15", "mbp-b"),
        ("Oct  1", "build-01.local"),
    ];
    for (run, (day, host)) in days.iter().enumerate() {
        let log: String = (0..5)
            .map(|i| {
                format!(
                    "{day} 10:2{i}:07 {host} backupd[{}]: Backup completed\n",
                    400 + run * 10 + i
                )
            })
            .collect();
        let ingested = siftr(&["ingest", "-j", "--context", "system.log"], Some(&log));
        assert_eq!(
            ingested["signals"],
            serde_json::json!([]),
            "day {}: {ingested}",
            run + 1
        );
    }
    let behaviors: Vec<Value> = (1..=days.len())
        .map(|run| siftr(&["summary", &format!("r{run}"), "-j"], None))
        .flat_map(|summary| summary["behaviors"].as_array().unwrap().clone())
        .map(|b| b["behavior"]["id"].clone())
        .collect();
    assert_eq!(
        behaviors.len(),
        days.len(),
        "one behavior per run: {behaviors:?}"
    );
    assert!(
        behaviors.iter().all(|id| *id == behaviors[0]),
        "{behaviors:?}"
    );
}

#[test]
fn table() {
    let cases: &[(&str, &str)] = &[
        // BSD syslog: the timestamp and host are where and when, the process is who.
        (
            "Sep 14 10:21:07 mbp-a backupd[412]: Backup completed",
            "<timestamp> <host> backupd[<int>]: Backup completed",
        ),
        (
            "Oct  3 09:00:01 build-01.local com.apple.xpc.launchd[1]: Service exited with abnormal code: 78",
            "<timestamp> <host> com.apple.xpc.launchd[<int>]: Service exited with abnormal code: <int>",
        ),
        // A process name is identity, digits and spaces included.
        (
            "Sep 14 10:21:07 mbp-b python3[9]: started",
            "<timestamp> <host> python3[<int>]: started",
        ),
        (
            "Sep 14 10:21:07 mbp-b postgres-14[77]: ready",
            "<timestamp> <host> postgres-14[<int>]: ready",
        ),
        (
            "Sep 14 10:21:07 mbp-a Google Chrome Helper[5120]: exiting",
            "<timestamp> <host> Google Chrome Helper[<int>]: exiting",
        ),
        (
            "Sep 14 10:21:07 mbp-a --- last message repeated 3 times ---",
            "<timestamp> <host> --- last message repeated <int> times ---",
        ),
        (
            "\x1b[2mSep 14 10:21:07 mbp-a backupd[1]: ok\x1b[0m",
            "<timestamp> <host> backupd[<int>]: ok",
        ),
        // Not a syslog prefix: left as before.
        (
            "Scheduled for May 14 10:21:07 mbp-a backupd[1]: ok",
            "Scheduled for May <int> <timestamp> mbp-a backupd[<int>]: ok",
        ),
        (
            "May 14 10:21:07 deploy started",
            "May <int> <timestamp> deploy started",
        ),
        (
            "Sep 14 10:21:07 INFO worker: started",
            "Sep <int> <timestamp> INFO worker: started",
        ),
        (
            "Sept 14 10:21:07 mbp-a backupd[1]: ok",
            "Sept <int> <timestamp> mbp-a backupd[<int>]: ok",
        ),
        (
            "Sep 14 10:21 mbp-a backupd[1]: ok",
            "Sep <int> <int>:<int> mbp-a backupd[<int>]: ok",
        ),
        (
            "Sep 44 10:21:07 mbp-a backupd[1]: ok",
            "Sep <int> <timestamp> mbp-a backupd[<int>]: ok",
        ),
        (
            "Sep 14 10:21:07 mbp-a backupd[x1]: ok",
            "Sep <int> <timestamp> mbp-a backupd[x1]: ok",
        ),
        (
            "Mayday 14 10:21:07 mbp-a backupd[1]: ok",
            "Mayday <int> <timestamp> mbp-a backupd[<int>]: ok",
        ),
        (
            "  Sep 14 10:21:07 mbp-a backupd[1]: ok",
            "Sep <int> <timestamp> mbp-a backupd[<int>]: ok",
        ),
        // Month words in prose.
        (
            "Backup scheduled for May; next in Sep",
            "Backup scheduled for May; next in Sep",
        ),
        (
            "backupd[1]: Scheduled for May 14",
            "backupd[<int>]: Scheduled for May <int>",
        ),
        // `0x` hex glued to what precedes it is a pointer or connection id.
        (
            "nw_connection_copy_connected_peer peer[3].0x7fa1c2d3e4f0 failed",
            "nw_connection_copy_connected_peer peer[<int>].<hex> failed",
        ),
        (
            "conn name=tcp.0x1a2b.local closed",
            "conn name=tcp.<hex>.local closed",
        ),
        (
            "peer[3]0x7fa1c2 (0x7fa1c2) [a,0x7fa1c2]",
            "peer[<int>]<hex> (<hex>) [a,<hex>]",
        ),
        // ...but not `0x` inside a word, or a word that only looks hex.
        (
            "foo.0xford box0x12 v1.0x deadbeef",
            "foo.0xford box0x12 v1.0x deadbeef",
        ),
        ("tcp.0x1a2bzz closed", "tcp.0x1a2bzz closed"),
        ("Api::V1::Widgets#create", "Api::V1::Widgets#create"),
    ];
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
fn the_same_message_on_another_day_and_host_is_one_template() {
    let mut n = Normalizer::new();
    let hashes: Vec<u64> = [
        "Sep 14 10:21:07 mbp-a backupd[412]: Backup completed",
        "Oct  1 23:59:59 mbp-b backupd[9]: Backup completed",
        "Dec 31 00:00:00 build-01.local backupd[70000]: Backup completed",
    ]
    .iter()
    .map(|line| n.normalize(line.as_bytes()).template_hash)
    .collect();
    assert!(hashes.iter().all(|&h| h == hashes[0]), "{hashes:?}");
}

#[test]
fn prefix_slots_span_the_raw_timestamp_and_host() {
    let line = "\x1b[2mSep  4 10:21:07 mbp-a backupd[412]: took 5ms";
    let mut n = Normalizer::new();
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
            (SlotKind::Timestamp, "Sep  4 10:21:07"),
            (SlotKind::Host, "mbp-a"),
            (SlotKind::Int, "412"),
            (SlotKind::Duration, "5ms"),
        ]
    );
}

#[test]
fn unified_log_columns_mask_to_stable_slots() {
    let mut n = Normalizer::new();
    let a = "2026-09-14 10:21:07.123456-0700  0x1a2b  Default  0x0  1234  0  backupd: (Subsystem) [Category] Backup completed";
    let b = "2026-10-02 23:01:00.000001-0800  0x9  Default  0x7f00a  88  0  backupd: (Subsystem) [Category] Backup completed";
    let want = "<timestamp> <hex> Default <hex> <int> <int> backupd: (Subsystem) [Category] Backup completed";
    for line in [a, b] {
        assert_eq!(
            String::from_utf8_lossy(n.normalize(line.as_bytes()).template),
            want
        );
    }
    // The type column is a real distinction.
    let error = a.replace("Default", "Error");
    assert_ne!(n.normalize(error.as_bytes()).template, want.as_bytes());
}
