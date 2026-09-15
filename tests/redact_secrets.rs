//! Credential redaction, table-driven. Every secret here is synthetic: random characters in a real format. Rows
//! marked launder come from launder's `tests/cases.rs`, with siftr's numbered `<PASSWORD_N>`.

use siftr::analyze::Analyzer;
use siftr::normalize::Normalizer;
use siftr::normalize::secrets::{Mode, Redactor, Scanner, redact_text};
use siftr::observation::{LineSplitter, Observation, Stream};

/// Each line through one fresh run's redactor, in `mode`: (masked, evidence).
fn redact(lines: &[&str], mode: Mode) -> Vec<(String, String)> {
    let mut redactor = Redactor::new(Some(b"/var/lib/ci"));
    let mut scanner = Scanner::default();
    lines
        .iter()
        .map(|line| {
            let views = redactor.line(&mut scanner, line.as_bytes(), mode);
            let text = |b: &[u8]| String::from_utf8(b.to_vec()).unwrap();
            (text(views.masked), text(views.evidence))
        })
        .collect()
}

fn masked(line: &str) -> String {
    redact(&[line], Mode::Secrets).remove(0).0
}

/// Every mismatch at once.
fn check(rows: &[(&str, &str)], mode: Mode, view: fn(&(String, String)) -> &String) {
    let failures: Vec<String> = rows
        .iter()
        .filter_map(|(line, expected)| {
            let got = redact(&[line], mode).remove(0);
            let got = view(&got);
            (got != expected).then(|| {
                format!("  line:     {line:?}\n  expected: {expected:?}\n  got:      {got:?}")
            })
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{} row(s) failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

const GHP: &str = concat!("ghp_", "Q7m2Xk9Lp4Rz8Wv1Tn6Ys3Hb5Jd0Fc2GaK8x");
const AKIA: &str = "AKIAQ7M2XK9LP4RZ8WV1";
const STRIPE: &str = concat!("sk_live_", "Q7m2Xk9Lp4Rz8Wv1Tn6Ys3Hb");

/// (line, masked).
const CAUGHT: &[(&str, &str)] = &[
    // The exposure reproduced on 0.1.0.
    (
        concat!(
            "Authorization: Bearer ghp_",
            "Q7m2Xk9Lp4Rz8Wv1Tn6Ys3Hb5Jd0Fc2GaK8x"
        ),
        "Authorization: Bearer <TOKEN_1>",
    ),
    (
        "aws access key AKIAQ7M2XK9LP4RZ8WV1 loaded",
        "aws access key <TOKEN_1> loaded",
    ),
    (
        concat!("stripe key=sk_live_", "Q7m2Xk9Lp4Rz8Wv1Tn6Ys3Hb ok"),
        "stripe key=<TOKEN_1> ok",
    ),
    // Token prefixes.
    (
        "push github_pat_11ABCDEFG0Zq8vN2kLp4Rx_Hk3Jd8Ws1Qz5Pm7R done",
        "push <TOKEN_1> done",
    ),
    ("gho_Zq8vN2kLp4RxQm7Tz9Lw", "<TOKEN_1>"),
    (
        concat!("gitlab glpat-", "Zq8vN2kLp4Rx-Qm7Tz9Lw"),
        "gitlab <TOKEN_1>",
    ),
    (
        "OPENAI sk-proj-Zq8vN2kLp4RxQm7Tz9LwHk3Jd8Ws",
        "OPENAI <TOKEN_1>",
    ),
    (
        concat!(
            "rk_test_",
            "Zq8vN2kLp4RxQm7T and pk_live_",
            "Hk3Jd8Ws1Qz5Pm7R"
        ),
        "<TOKEN_1> and <TOKEN_2>",
    ),
    ("ASIAZQ8VN2KLP4RXQM7T", "<TOKEN_1>"),
    (
        concat!("maps AIzaSy", "Zq8vN2kLp4RxQm7Tz9LwHk3Jd8Ws1Qz5P"),
        "maps <TOKEN_1>",
    ),
    (
        concat!(
            "slack notify xoxb-123456789012-",
            "Zq8vN2kLp4RxQm7Tz9LwHk3J"
        ),
        "slack notify <TOKEN_1>",
    ),
    (
        concat!("npm_", "Zq8vN2kLp4RxQm7Tz9LwHk3Jd8Ws1Qz5Pm7R"),
        "<TOKEN_1>",
    ),
    (
        concat!(
            "SG.",
            "Zq8vN2kLp4RxQm7Tz9LwHk.3Jd8Ws1Qz5Pm7RaB9Zq8vN2kLp4RxQm7Tz9LwHk3Jd8"
        ),
        "<TOKEN_1>",
    ),
    (
        concat!(
            r#"  Parameters: {"jwt"=>"eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0"#,
            "NTY3ODkwIn0.Zq8vN2kLp4Rx-Qm7Tz9Lw_Hk3",
            r#"Jd8Ws1Qz5Pm7RaB9"}"#
        ),
        r#"  Parameters: {"jwt"=>"<JWT_1>"}"#,
    ),
    // Private keys and URL passwords.
    (
        concat!(
            "-----BEGIN RSA PRIVATE KEY",
            "----- MIIEZq8vN2kLp4Rx -----END RSA PRIVATE KEY",
            "----- loaded"
        ),
        "<PRIVATE_KEY_1> loaded",
    ),
    (
        "Connecting to postgres://app:Zq8vN2kLp4Rx@db.example.com:5432/app_test",
        "Connecting to postgres://app:<PASSWORD_1>@db.example.com:5432/app_test",
    ),
    // launder
    (
        "redis://:Zq8vN2kLp4Rx@cache.example.com:6379/0",
        "redis://:<PASSWORD_1>@cache.example.com:6379/0",
    ),
    // Keys. launder: quoted, prefixed and env-style keys.
    (
        concat!("ENV api_key=", "Zq8vN2kLp4RxQm7Tz9LwHk3Jd8Ws1Qz5 loaded"),
        "ENV api_key=<SECRET_1> loaded",
    ),
    (concat!("password=", "Zq8vN2kLp4Rx"), "password=<SECRET_1>"),
    (
        concat!(r#"  Parameters: {"password"=>""#, "Zq8vN2kLp4Rx", r#""}"#),
        r#"  Parameters: {"password"=>"<SECRET_1>"}"#,
    ),
    (
        concat!(r#"{"api_key": ""#, "Qm7Tz9Lw2Xc4Vb6N", r#""}"#),
        r#"{"api_key": "<SECRET_1>"}"#,
    ),
    (
        "access_token=Hk3Jd8Ws1Qz5Pm7R&page=2",
        "access_token=<SECRET_1>&page=2",
    ),
    (
        concat!("auth_token: ", "Hk3Jd8Ws1Qz5Pm7R"),
        "auth_token: <SECRET_1>",
    ),
    (
        concat!(r#"{"refreshToken":""#, "Hk3Jd8Ws1Qz5Pm7R", r#""}"#),
        r#"{"refreshToken":"<SECRET_1>"}"#,
    ),
    (
        concat!("SECRET_KEY_BASE=", "Hk3Jd8Ws1Qz5Pm7RaB9"),
        "SECRET_KEY_BASE=<SECRET_1>",
    ),
    (
        concat!("RAILS_MASTER_KEY=", "Hk3Jd8Ws1Qz5Pm7RaB9"),
        "RAILS_MASTER_KEY=<SECRET_1>",
    ),
    (
        "CMD (backup.sh --token=Hk3Jd8Ws1Qz5Pm7R)",
        "CMD (backup.sh --token=<SECRET_1>)",
    ),
    (concat!("password=Zq8)", "vN2kLp4Rx"), "password=<SECRET_1>"),
    (
        concat!("X-Api-Key: ", "Qm7Tz9Lw2Xc4Vb6N"),
        "X-Api-Key: <SECRET_1>",
    ),
    (
        "clientSecret='Qm7Tz9Lw 2Xc4Vb6N'",
        "clientSecret='<SECRET_1>'",
    ),
    (
        concat!("token=ghp_", "Q7m2Xk9Lp4Rz8Wv1Tn6Ys3Hb5Jd0Fc2GaK8x"),
        "token=<TOKEN_1>",
    ),
    // Headers. launder: cookie values, names and attributes kept.
    (
        "Cookie: _app_session=Qm7Tz9Lw2Xc4Vb6NHk3Jd8Ws1Qz5Pm7R",
        "Cookie: _app_session=<TOKEN_1>",
    ),
    (
        "Set-Cookie: _app_session=Qm7Tz9Lw2Xc4Vb6NHk3Jd8Ws1Qz5Pm7R; path=/; HttpOnly",
        "Set-Cookie: _app_session=<TOKEN_1>; path=/; HttpOnly",
    ),
    (
        "Cookie: locale=en; _app_session=Qm7Tz9Lw2Xc4Vb6NHk3Jd8Ws1Qz5Pm7R",
        "Cookie: locale=en; _app_session=<TOKEN_1>",
    ),
    (
        "Set-Cookie: sid=Qm7Tz9Lw2Xc4Vb6N; Domain=app.example.com; Expires=Wed, 21 Oct 2026 07:28:00 GMT",
        "Set-Cookie: sid=<TOKEN_1>; Domain=app.example.com; Expires=Wed, 21 Oct 2026 07:28:00 GMT",
    ),
    (
        "Authorization: Basic dXNlcjpaOHF2TjJrTHA0",
        "Authorization: Basic <TOKEN_1>",
    ),
    (
        r#"headers: {"Authorization"=>"Token abc123"}"#,
        r#"headers: {"Authorization"=>"Token <TOKEN_1>"}"#,
    ),
    // launder: after ANSI escapes, which end in a letter.
    (
        "\x1b[31mghp_Q7m2Xk9Lp4Rz8Wv1Tn\x1b[0m",
        "\x1b[31m<TOKEN_1>\x1b[0m",
    ),
    (
        "\x1b[1m\x1b[36mAuthorization: Bearer Zq8vN2kLp4RxQm7Tz9Lw\x1b[0m",
        "\x1b[1m\x1b[36mAuthorization: Bearer <TOKEN_1>\x1b[0m",
    ),
    (
        "  \x1b[31mAKIAQ7M2XK9LP4RZ8WV1\x1b[0m rejected",
        "  \x1b[31m<TOKEN_1>\x1b[0m rejected",
    ),
    (
        "\x1b[1;36mpassword=Zq8vN2kLp4Rx\x1b[0m",
        "\x1b[1;36mpassword=<SECRET_1>\x1b[0m",
    ),
    // Rails SQL binds, plain and inside an RSpec event's JSON string.
    (
        concat!(
            r#"  ApiToken Load (0.2ms)  SELECT "api_tokens".* FROM "api_tokens" WHERE "api_tokens"."token" = $1 LIMIT $2  [["token", ""#,
            "Hk3Jd8Ws1Qz5Pm7R",
            r#""], ["LIMIT", 1]]"#
        ),
        r#"  ApiToken Load (0.2ms)  SELECT "api_tokens".* FROM "api_tokens" WHERE "api_tokens"."token" = $1 LIMIT $2  [["token", "<SECRET_1>"], ["LIMIT", 1]]"#,
    ),
    (
        r#"{"event":"example","exception":{"message":"expected {\"password\"=>\"Zq8v\\\"N2kLp4Rx\"} [[\"token\", \"Hk3Jd8Ws1Qz5Pm7R\"]]"}}"#,
        r#"{"event":"example","exception":{"message":"expected {\"password\"=>\"<SECRET_1>\"} [[\"token\", \"<SECRET_2>\"]]"}}"#,
    ),
    (
        concat!(
            r#"{"message":"key -----BEGIN PRIVATE KEY"#,
            r#"-----\nMIIEvZq8vN2kLp4Rx\n"}"#
        ),
        r#"{"message":"key <PRIVATE_KEY_1>"}"#,
    ),
];

/// Lines no rule may touch: the shapes of real test logs, and near misses.
const UNTOUCHED: &[&str] = &[
    "Processing by Api::V1::AccountsController#show as HTML",
    r#"Started GET "/users/1?page=2" for 127.0.0.1 at 2026-09-15 10:00:00 -0700"#,
    r#"  User Load (0.2ms)  SELECT "users".* FROM "users" WHERE "users"."id" = $1 LIMIT $2  [["id", 1], ["LIMIT", 1]]"#,
    r#"  User Create (0.4ms)  INSERT INTO "users" ("password_digest", "created_at") VALUES ($1, $2) RETURNING "id""#,
    r#"  Parameters: {"password"=>"[FILTERED]", "token"=>"[FILTERED]"}"#,
    "password=<SECRET_1>",
    concat!("token=", "0b9f8c3e-6a1d-4f8e-9c2a-2d1e5f7a9b0c"),
    "PWD=/Users/someone/code/app OLDPWD=~/code/other",
    "token_type: Bearer max_tokens=4096 password: aaaaaaaaaaaa",
    "author: JaneDoe12345 tokenizer: cl100k_base passage=Zq8vN2kLp4Rx",
    // launder: bare `key` is not a credential word.
    "sort_key=created_at_desc_Z9 cache_key=views/users/1-20260915",
    "Cookie: locale=en; theme=dark",
    "Failure/Error: expect(user.email).to eq(\"pat.jones@example.com\")",
    "Finished in 1 minute 3.5 seconds (files took 0.24948 seconds to load)",
    "if a == b && token != nil || x <= 3 => ok",
    "Authorization: Bearer <TOKEN_1>",
    "Authorization: Bearer",
    "git@github.com:dpep/siftr.git https://example.com:8080/path",
    "sk-learn sk_live_short xoxb-1 AKIA1234",
];

#[test]
fn every_rule_masks_its_credential() {
    check(CAUGHT, Mode::Secrets, |(masked, _)| masked);
}

#[test]
fn lines_without_a_credential_are_untouched() {
    let rows: Vec<(&str, &str)> = UNTOUCHED.iter().map(|line| (*line, *line)).collect();
    check(&rows, Mode::Secrets, |(masked, _)| masked);
}

#[test]
fn redacting_twice_changes_nothing_more() {
    let rows: Vec<(&str, &str)> = CAUGHT
        .iter()
        .map(|(_, masked)| (*masked, *masked))
        .collect();
    check(&rows, Mode::Secrets, |(masked, _)| masked);
}

#[test]
fn a_private_key_block_is_masked_across_lines_and_ends_with_its_footer() {
    let lines = [
        "config loaded",
        concat!("-----BEGIN OPENSSH PRIVATE KEY", "-----"),
        concat!(
            "b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAAB",
            "AAAAMwAAAAtzc2gtZW"
        ),
        concat!("-----END OPENSSH PRIVATE KEY", "----- after"),
        "next line",
    ];
    let masked: Vec<String> = redact(&lines, Mode::Secrets)
        .into_iter()
        .map(|(m, _)| m)
        .collect();
    assert_eq!(
        masked,
        [
            "config loaded",
            "<PRIVATE_KEY_1>",
            "<PRIVATE_KEY_2>",
            "<PRIVATE_KEY_3> after",
            "next line"
        ]
    );
}

#[test]
fn a_value_keeps_its_number_for_the_whole_run() {
    let other = "ghp_Hk3Jd8Ws1Qz5Pm7RaB9Zq8vN2kLp4RxQm7T";
    let lines = [
        format!("first {GHP}"),
        format!("second {other} then {GHP}"),
        format!("aws {AKIA} {STRIPE}"),
    ];
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    let masked: Vec<String> = redact(&lines, Mode::Secrets)
        .into_iter()
        .map(|(m, _)| m)
        .collect();
    assert_eq!(
        masked,
        [
            "first <TOKEN_1>",
            "second <TOKEN_2> then <TOKEN_1>",
            "aws <TOKEN_3> <TOKEN_4>"
        ]
    );
}

#[test]
fn identity_does_not_depend_on_a_placeholders_number() {
    let template = |line: &str| {
        let mut normalizer = Normalizer::new();
        String::from_utf8(normalizer.normalize(line.as_bytes()).template.to_vec()).unwrap()
    };
    assert_eq!(
        template("Authorization: Bearer <TOKEN_1>"),
        "Authorization: Bearer <TOKEN>"
    );
    assert_eq!(
        template("postgres://app:<PASSWORD_3>@db.example.com/x"),
        template("postgres://app:<PASSWORD_1>@db.example.com/x")
    );

    // Two runs whose credentials differ, and appear in another order, keep the same behaviors.
    let behaviors = |text: &str| {
        let mut redactor = Redactor::new(None);
        let mut scanner = Scanner::default();
        let mut analyzer = Analyzer::new();
        let stream = Stream::Stdout;
        LineSplitter::new().feed(&stream, text.as_bytes(), |obs| {
            let views = redactor.line(&mut scanner, obs.line, Mode::Secrets);
            analyzer.observe(Observation {
                line: views.masked,
                ..obs
            });
        });
        let mut ids: Vec<(String, String)> = analyzer
            .finish()
            .aggregates
            .iter()
            .map(|a| (a.behavior.id.to_string(), a.behavior.template.clone()))
            .collect();
        ids.sort();
        ids
    };
    let one = behaviors(&format!("warmup {AKIA}\nAuthorization: Bearer {GHP}\n"));
    let two = behaviors(&format!(
        "Authorization: Bearer ghp_Hk3Jd8Ws1Qz5Pm7RaB9Zq8vN2kLp4RxQm7T\nwarmup {AKIA}\n"
    ));
    assert_eq!(one, two);
    assert!(
        one.iter()
            .any(|(_, t)| t == "Authorization: Bearer <TOKEN>"),
        "{one:?}"
    );
}

#[test]
fn an_examples_identity_does_not_depend_on_a_placeholders_number() {
    let example = |n: u32| {
        let events = format!(
            "{{\"event\":\"example\",\"id\":\"./spec/a_spec.rb[1:1]\",\"full_description\":\"A signs in with <TOKEN_{n}>\",\"file_path\":\"./spec/a_spec.rb\",\"status\":\"passed\",\"run_time\":0.1}}\n"
        );
        let stream = Stream::File("rspec-events".into());
        let mut analyzer = Analyzer::new();
        LineSplitter::new().feed(&stream, events.as_bytes(), |obs| analyzer.observe(obs));
        let analysis = analyzer.finish();
        let behavior = &analysis.aggregates[0].behavior;
        (behavior.id, behavior.template.clone())
    };
    assert_eq!(example(1), example(7));
    assert_eq!(example(1).1, "./spec/a_spec.rb # A signs in with <TOKEN>");
}

#[test]
fn pii_mode_also_masks_emails_public_ips_and_home_prefixes_in_evidence_only() {
    let rows: &[(&str, &str)] = &[
        (
            "Failure/Error: expect(user.email).to eq(\"pat.jones@example.com\")",
            "Failure/Error: expect(user.email).to eq(\"<EMAIL_1>\")",
        ),
        (
            "sshd[311]: Accepted publickey for alice from 198.51.100.23 port 52144",
            "sshd[311]: Accepted publickey for alice from <IP_1> port 52144",
        ),
        // launder
        (
            "peer 2001:db8:85a3::8a2e:370:7334 connected",
            "peer <IP_1> connected",
        ),
        (
            "connect to 2001:db8::1: refused",
            "connect to <IP_1>: refused",
        ),
        (
            "(/home/deploy/bin/backup.sh --token=Hk3Jd8Ws1Qz5Pm7R) in /Users/pat/code",
            "(~/bin/backup.sh --token=<SECRET_1>) in ~/code",
        ),
        ("cache at /var/lib/ci/cache", "cache at ~/cache"),
        // launder: private addresses, times and Ruby constants stay.
        (
            "listening on [::1]:3000 and fe80::1%en0",
            "listening on [::1]:3000 and fe80::1%en0",
        ),
        (
            "class User < ActiveRecord::Base",
            "class User < ActiveRecord::Base",
        ),
        (
            "Processing by Api::V1::AccountsController#show for 127.0.0.1 and 10.0.24.37 at 10:00:00",
            "Processing by Api::V1::AccountsController#show for 127.0.0.1 and 10.0.24.37 at 10:00:00",
        ),
        ("/usr/home/x a@b 7.1.3", "/usr/home/x a@b 7.1.3"),
    ];
    check(rows, Mode::Pii, |(_, evidence)| evidence);
    for (line, _) in rows {
        let (masked, _) = redact(&[line], Mode::Pii).remove(0);
        assert!(
            !masked.contains("<EMAIL") && !masked.contains("<IP"),
            "{masked}"
        );
    }
}

#[test]
fn off_keeps_raw_evidence_but_still_masks_what_templates_are_built_from() {
    let line = format!("Authorization: Bearer {GHP}");
    let (masked, evidence) = redact(&[&line], Mode::Off).remove(0);
    assert_eq!(
        (masked.as_str(), evidence.as_str()),
        ("Authorization: Bearer <TOKEN_1>", line.as_str())
    );
}

#[test]
fn stored_text_is_redacted_the_same_way_every_time() {
    let command = format!("curl -H 'Authorization: Bearer {GHP}' https://app:Zq8vN2kLp4Rx@h/");
    let once = redact_text(&command);
    assert_eq!(
        once,
        "curl -H 'Authorization: Bearer <TOKEN_1>' https://app:<PASSWORD_1>@h/"
    );
    assert_eq!(redact_text(&command), once, "a stable key");
    assert_eq!(redact_text(&once), once);
    assert!(matches!(
        redact_text("bundle exec rspec"),
        std::borrow::Cow::Borrowed(_)
    ));
    assert_eq!(masked("bundle exec rspec"), "bundle exec rspec");
}
