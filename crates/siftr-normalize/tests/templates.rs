//! Real-shaped lines (public/synthetic shapes only) → expected templates.

use siftr_normalize::{Normalizer, SlotKind, slot_value_f64};

fn template(line: &str) -> String {
    let mut n = Normalizer::new();
    String::from_utf8(n.normalize(line.as_bytes()).template.to_vec()).unwrap()
}

#[track_caller]
fn same_template(a: &str, b: &str) {
    let mut n = Normalizer::new();
    let (ta, ha) = {
        let r = n.normalize(a.as_bytes());
        (r.template.to_vec(), r.template_hash)
    };
    let r = n.normalize(b.as_bytes());
    assert_eq!(
        String::from_utf8_lossy(&ta),
        String::from_utf8_lossy(r.template)
    );
    assert_eq!(ha, r.template_hash);
}

#[test]
fn table() {
    let cases: &[(&str, &str)] = &[
        // Rails SQL log, MySQL flavor, colored.
        (
            "\x1b[1m\x1b[36m (0.3ms)\x1b[0m  \x1b[1mINSERT INTO `schema_migrations` (version) VALUES ('20170905153814')\x1b[0m",
            "(<duration>) INSERT INTO `schema_migrations` (version) VALUES (<quoted>)",
        ),
        (
            "DEBUG    User Exists? (0.3ms)  SELECT 1 AS one FROM \"users\" WHERE \"users\".\"id\" = 7 LIMIT 1",
            "DEBUG User Exists? (<duration>) SELECT <int> AS one FROM \"users\" WHERE \"users\".\"id\" = <int> LIMIT <int>",
        ),
        (
            "  \x1b[1m\x1b[35mCACHE (0.0ms)\x1b[0m  SELECT  \"users\".* FROM \"users\" WHERE \"users\".\"id\" = $1 LIMIT $2  [[\"id\", 973], [\"LIMIT\", 1]]",
            "CACHE (<duration>) SELECT \"users\".* FROM \"users\" WHERE \"users\".\"id\" = $<int> LIMIT $<int> [[\"id\", <int>], [\"LIMIT\", <int>]]",
        ),
        (
            "User Load (0.4ms)  SELECT \"users\".* FROM \"users\" WHERE \"users\".\"email\" = $1 LIMIT $2  [[\"email\", \"bob@example.com\"], [\"LIMIT\", 1]]",
            "User Load (<duration>) SELECT \"users\".* FROM \"users\" WHERE \"users\".\"email\" = $<int> LIMIT $<int> [[\"email\", <quoted>], [\"LIMIT\", <int>]]",
        ),
        (
            "UPDATE `jobs` SET `state` = 'closed', `updated_at` = '2018-12-19 19:38:36' WHERE `jobs`.`id` = 10",
            "UPDATE `jobs` SET `state` = <quoted>, `updated_at` = <quoted> WHERE `jobs`.`id` = <int>",
        ),
        (
            "SQL (0.4ms)  INSERT INTO `geo_weights` (`rabbit_id`, `geohash`, `weight`) VALUES (321, '9q8vm7', -20.0)",
            "SQL (<duration>) INSERT INTO `geo_weights` (`rabbit_id`, `geohash`, `weight`) VALUES (<int>, <quoted>, <float>)",
        ),
        (
            "UPDATE `users` SET `name` = 'O\\'Brien', `bio` = 'it''s' WHERE `users`.`id` = 5",
            "UPDATE `users` SET `name` = <quoted>, `bio` = <quoted> WHERE `users`.`id` = <int>",
        ),
        ("SAVEPOINT active_record_1", "SAVEPOINT active_record_<int>"),
        // RSpec.
        (
            "Finished in 10.9 seconds (files took 0.24948 seconds to load)",
            "Finished in <duration> (files took <duration> to load)",
        ),
        (
            "3 examples, 0 failures, 1 pending",
            "<int> examples, <int> failures, <int> pending",
        ),
        (
            "rspec ./spec/models/user_spec.rb:42 # User validates email",
            "rspec ./spec/models/user_spec.rb:<int> # User validates email",
        ),
        ("Randomized with seed 12345", "Randomized with seed <int>"),
        // Ruby backtraces: the method name distinguishes behaviors; keep it.
        (
            "app/models/user.rb:42:in 'User#save'",
            "app/models/user.rb:<int>:in 'User#save'",
        ),
        (
            "#<User id: 5, email: \"a@b.co\"> at #<User:0x00007f8b1c8a2b10>",
            "#<User id: <int>, email: \"<email>\"> at #<User:<hex>>",
        ),
        // Rails request logs.
        (
            "Started GET \"/users/85320\" for 10.0.24.37 at 2024-01-15 10:00:00 +0000",
            "Started GET \"/users/<int>\" for <ip> at <timestamp>",
        ),
        (
            "Started GET \"/status/pending\" for ::1 at 2024-01-15 10:00:00 -0500",
            "Started GET \"/status/pending\" for <ip> at <timestamp>",
        ),
        (
            "2026-09-13T12:00:00.000Z INFO [6513270e-269e-0d37-f2a7-4de452e6b438] Completed 200 OK in 524.9ms (Views: 45.5ms | ActiveRecord: 10.7ms)",
            "<timestamp> INFO [<uuid>] Completed <int> OK in <duration> (Views: <duration> | ActiveRecord: <duration>)",
        ),
        (
            "Rendered users/show.html.erb within layouts/application (Duration: 1.2ms | Allocations: 345)",
            "Rendered users/show.html.erb within layouts/application (Duration: <duration> | Allocations: <int>)",
        ),
        (
            "GET http://localhost:3000/api/v1/users/5?page=2&per=50",
            "GET http://localhost:<int>/api/v1/users/<int>?page=<int>&per=<int>",
        ),
        // Synthetic survey shapes.
        (
            "2026-09-13T12:00:00.003Z WARN /Users/alice/code/app/models/user.rb:115:in 'save' job=a09f76b5a170b338",
            "<timestamp> WARN /Users/alice/code/app/models/user.rb:<int>:in 'save' job=<hex>",
        ),
        (
            "deploy sha=6595e60af5 by alice@example.com version v1.3.1 status=complete",
            "deploy sha=<hex> by <email> version <version> status=complete",
        ),
        (
            "order 123515 transitioned /status/failed size=1481KB",
            "order <int> transitioned /status/failed size=<size>",
        ),
        // Misc.
        (
            "Booting Rails 7.1.3 with Ruby v3.3.0",
            "Booting Rails <version> with Ruby <version>",
        ),
        ("[12:34:56.789] tick", "[<timestamp>] tick"),
        (
            "** [10:39:04 2018-12-19] 51234: QueueBus Event published: user_created",
            "** [<timestamp>] <int>: QueueBus Event published: user_created",
        ),
        ("retrying in 5 seconds.", "retrying in <duration>."),
        ("wrote 512 bytes to cache", "wrote <size> to cache"),
        ("processed 1,234,567 rows", "processed <int> rows"),
        ("hash deadbeef facade kept", "hash deadbeef facade kept"),
        ("Connecting to [::1]:5432", "Connecting to [<ip>]:<int>"),
        // Ruby hash inspect and JSON: keys stay, string values are masked.
        (
            "Parameters: {\"user\"=>{\"email\"=>\"bob@example.com\", \"name\"=>\"Bob\"}, \"commit\"=>\"Save\"}",
            "Parameters: {\"user\"=>{\"email\"=><quoted>, \"name\"=><quoted>}, \"commit\"=><quoted>}",
        ),
        (
            "response {\"error\":\"not found\",\"status\":404, \"detail\": \"missing id\"}",
            "response {\"error\":<quoted>,\"status\":<int>, \"detail\": <quoted>}",
        ),
        // A UUID inside a dash-joined token is one slot, not its groups.
        (
            "event bus_id=1545248316-6a4f9f5e-c759-44e9-ba8b-0d3c2b1a9f8e published",
            "event bus_id=<int>-<uuid> published",
        ),
        (
            "job_6513270e-269e-0d37-f2a7-4de452e6b438_retry",
            "job_<uuid>_retry",
        ),
        // Rails compiled-view method names: `__<String#hash>_<id>`, a negative hash's `-` rewritten to `_`.
        (
            "DEPRECATION WARNING: User#display_name is deprecated; use #name (called from _app_views_users_show_html_erb__484173706772255391_5232 at /app/views/users/show.html.erb:1)",
            "DEPRECATION WARNING: User#display_name is deprecated; use #name (called from _app_views_users_show_html_erb__<int>_<int> at /app/views/users/show.html.erb:<int>)",
        ),
        (
            "called from _app_views_users_show_html_erb___129572542000730357_5232 at x",
            "called from _app_views_users_show_html_erb__<int>_<int> at x",
        ),
        // Only a run of two or more collapses: a single `_` before digits is a different name.
        (
            "active_record_1 v_2 v__2",
            "active_record_<int> v_<int> v__<int>",
        ),
        ("snake___case stays", "snake___case stays"),
        ("tail___ and ___12abc", "tail___ and ___12abc"),
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
fn ansi_color_does_not_split_behaviors() {
    same_template(
        "\x1b[1m\x1b[36mUser Load (0.3ms)\x1b[0m  \x1b[1mSELECT 1\x1b[0m",
        "\x1b[1m\x1b[35mUser Load (1.7ms)\x1b[0m  SELECT 2",
    );
    same_template(
        "User Load (0.3ms)  SELECT 1",
        "\x1b[1mUser Load (0.3ms)\x1b[0m SELECT 1",
    );
}

#[test]
fn strip_ansi_keeps_only_the_text() {
    let mut out = vec![b'x'];
    let line = "  \x1b[1m\x1b[36mUser Load (0.0ms)\x1b[0m  \x1b[1m\x1b[34mSELECT 1\x1b[0m";
    siftr_normalize::strip_ansi(line.as_bytes(), &mut out);
    assert_eq!(out, b"  User Load (0.0ms)  SELECT 1");
    siftr_normalize::strip_ansi(b"cut \x1b[3", &mut out);
    assert_eq!(out, b"cut ");
}

#[test]
fn variable_length_in_lists_share_a_template() {
    same_template(
        "SELECT \"posts\".* FROM \"posts\" WHERE \"posts\".\"id\" IN (1, 2, 3)",
        "SELECT \"posts\".* FROM \"posts\" WHERE \"posts\".\"id\" IN (42)",
    );
    same_template(
        "WHERE `jobs`.`state` IN ('open', 'assigned', 'done')",
        "WHERE `jobs`.`state` IN ('open')",
    );
}

#[test]
fn rspec_minutes_and_seconds_share_a_template() {
    same_template(
        "Finished in 1 minute 3.5 seconds (files took 2.1 seconds to load)",
        "Finished in 10.9 seconds (files took 0.24948 seconds to load)",
    );
}

#[test]
fn words_are_never_masked() {
    assert_eq!(template("User Load (0.3ms)"), "User Load (<duration>)");
    assert_eq!(template("Post Load (0.3ms)"), "Post Load (<duration>)");
    assert_eq!(template("GET /status/pending"), "GET /status/pending");
}

#[test]
fn quotes_outside_sql_value_context_stay_literal() {
    assert_eq!(template("in 'save'"), "in 'save'");
    assert_eq!(template("it's 'fine'"), "it's 'fine'");
    assert_eq!(
        template("WHERE name LIKE 'bo%'"),
        "WHERE name LIKE <quoted>"
    );
    // Unterminated: fall back to tokenizing the rest.
    assert_eq!(template("WHERE id = '12"), "WHERE id = '<int>");
}

#[test]
fn emails_without_digits_are_masked() {
    assert_eq!(template("sent to bob@example.com"), "sent to <email>");
}

#[test]
fn sizes_are_not_durations() {
    let line = b"size=1481KB took 12MB";
    let mut n = Normalizer::new();
    let kinds: Vec<_> = n.normalize(line).slots.iter().map(|s| s.kind).collect();
    assert_eq!(kinds, [SlotKind::Size, SlotKind::Size]);
}

#[test]
fn slot_spans_index_the_original_line() {
    let line =
        "\x1b[1m\x1b[36mUser Load (0.3ms)\x1b[0m WHERE id = 7 AND at = 2024-01-15 10:00:00 +0000";
    let mut n = Normalizer::new();
    let r = n.normalize(line.as_bytes());
    let got: Vec<(SlotKind, &str)> = r
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
        got,
        [
            (SlotKind::Duration, "0.3ms"),
            (SlotKind::Int, "7"),
            (SlotKind::Timestamp, "2024-01-15 10:00:00 +0000"),
        ]
    );
}

#[test]
fn slot_values_normalize_units() {
    let line = b"Finished in 1 minute 3.5 seconds, wrote 2KB, id -7";
    let mut n = Normalizer::new();
    let slots = n.normalize(line).slots.to_vec();
    let values: Vec<_> = slots.iter().map(|s| slot_value_f64(line, s)).collect();
    assert_eq!(values, [Some(63_500.0), Some(2048.0), Some(-7.0)]);

    let list = b"IN (1, 2, 3)";
    let slot = n.normalize(list).slots[0];
    assert_eq!(slot_value_f64(list, &slot), None);
}

#[test]
fn whitespace_and_line_endings_do_not_split_behaviors() {
    same_template("a \t  b\r\n", "a b");
    same_template("  leading", "leading");
}

#[test]
fn malformed_escapes_do_not_panic() {
    let mut n = Normalizer::new();
    for line in [
        &b"\x1b"[..],
        b"abc\x1b[",
        b"\x1b]0;title\x07done",
        b"\x1b]0;title\x1b\\done",
        b"x = '",
        b", \"",
        b"",
        b"\xff\xfe 12",
    ] {
        n.normalize(line);
    }
    assert_eq!(template("\x1b]0;title\x07done"), "done");
}

#[test]
fn template_hash_is_pinned() {
    // Persisted behavior ids depend on these; changing them needs a migration.
    let mut n = Normalizer::new();
    assert_eq!(n.normalize(b"").template_hash, 0xcbf2_9ce4_8422_2325);
    assert_eq!(
        n.normalize(b"User Load (0.3ms)").template_hash,
        siftr_normalize::fnv1a64(b"User Load (<duration>)")
    );
    assert_eq!(
        n.normalize(b"User Load (0.3ms)").template_hash,
        5_588_240_263_472_588_943
    );
}
