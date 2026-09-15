//! Where a run happened is canonicalized out of a template; what its paths name stays in it.

use siftr::normalize::{Normalizer, PathRole, Roots, SlotKind};

fn laptop() -> Roots {
    Roots::default()
        .project("/Users/dpepper/code/app")
        .home("/Users/dpepper")
        .tmp("/var/folders/yr/gngx90zx/T/")
}

fn ci() -> Roots {
    Roots::default()
        .project("/home/runner/work/app/app")
        .home("/home/runner")
        .tmp("/tmp")
}

const CALLED_FROM: &str = "DEPRECATION WARNING: User#display_name is deprecated (called from _app_views_users_show_html_erb__484173706772255391_5232 at ";

#[test]
fn table() {
    let warning = |path: &str| format!("{CALLED_FROM}{path})");
    let view = "DEPRECATION WARNING: User#display_name is deprecated (called from _app_views_users_show_html_erb__<int>_<int> at <root>/app/views/users/show.html.erb:<int>)";
    let none = Roots::default;
    let cases: Vec<(Roots, String, &str, &[&str])> = vec![
        // One warning on three machines: each run knows its own project root. GitHub checks out to `work/<repo>/<repo>`.
        (laptop(), warning("~/code/app/app/views/users/show.html.erb:12"), view, &["source", "view"]),
        (ci(), warning("/home/runner/work/app/app/app/views/users/show.html.erb:12"), view, &["source", "view"]),
        (laptop(), warning("/Users/dpepper/code/app/app/views/users/show.html.erb:12"), view, &["source", "view"]),
        // Another user's checkout at the same place under their home is the same project.
        (laptop(), "at /Users/someone/code/app/app/models/user.rb:9".into(), "at <root>/app/models/user.rb:<int>", &["source"]),
        // With no roots, every home is `~`.
        (none(), "at /Users/alice/src/app/show.html.erb:3".into(), "at ~/src/app/show.html.erb:<int>", &["view"]),
        (none(), "at /home/bob/src/app/show.html.erb:3".into(), "at ~/src/app/show.html.erb:<int>", &["view"]),
        (none(), "at ~/src/app/show.html.erb:3".into(), "at ~/src/app/show.html.erb:<int>", &["view"]),
        (Roots::default().home("/var/lib/jenkins"), "tail /var/lib/jenkins/workspace/app/log/test.log".into(), "tail ~/workspace/app/log/test.log", &["log"]),
        // A project in a temp dir is the project, as a test suite's is.
        (Roots::default().project("/var/folders/yr/gngx90zx/T/.tmpAbC123"), "loading /var/folders/yr/gngx90zx/T/.tmpAbC123/spec/a_spec.rb:3".into(), "loading <root>/spec/a_spec.rb:<int>", &["test"]),
        (laptop(), "Writing /Users/dpepper/code/app/log/test.log".into(), "Writing <root>/log/test.log", &["log"]),
        (laptop(), "Changed /Users/dpepper/code/app/Gemfile".into(), "Changed <root>/Gemfile", &["manifest"]),
        // Temp dirs, and the names generated inside them.
        (none(), "Wrote /tmp/1a2b3c4d5e6f-4821-xyz/cache.bin".into(), "Wrote <tmp>/<tmpname>/cache.bin", &["temp"]),
        (none(), "Wrote /var/folders/yr/gngx90zx/T/9f8e7d6c5b4a-4822-abc/cache.bin".into(), "Wrote <tmp>/<tmpname>/cache.bin", &["temp"]),
        (none(), "Wrote /private/tmp/d20260915-123-abc/report.log.".into(), "Wrote <tmp>/<tmpname>/report.log.", &["log", "temp"]),
        (none(), "listening on /tmp/tmp.aBcDeFgHiJ/puma.sock".into(), "listening on <tmp>/<tmpname>/puma.sock", &["temp"]),
        (none(), "saved /tmp/upload-123.png".into(), "saved <tmp>/<tmpname>.png", &["temp"]),
        (none(), "db=/tmp/d2026-1/x.db".into(), "db=<tmp>/<tmpname>/x.db", &["database", "temp"]),
        // Installed packages, wherever they are installed.
        (none(), "/Users/dpepper/.rvm/gems/ruby-3.4.9/gems/activerecord-7.1.3/lib/active_record/base.rb:42:in 'find'".into(), "<gem:activerecord>/lib/active_record/base.rb:<int>:in 'find'", &["dependency"]),
        (none(), "/home/runner/.rbenv/versions/3.3.0/lib/ruby/gems/3.3.0/gems/rspec-core-3.13.6/lib/rspec/core/runner.rb:45:in 'run'".into(), "<gem:rspec-core>/lib/rspec/core/runner.rb:<int>:in 'run'", &["dependency"]),
        (laptop(), "/Users/dpepper/code/app/vendor/bundle/ruby/3.4.0/gems/rspec-core-3.13.6/lib/rspec/core/runner.rb:45:in 'run'".into(), "<gem:rspec-core>/lib/rspec/core/runner.rb:<int>:in 'run'", &["dependency"]),
        (none(), "from /opt/hostedtoolcache/Ruby/3.4.9/x64/lib/ruby/gems/3.4.0/bundler/gems/rails-1a2b3c4d5e6f/activerecord/lib/active_record.rb:3".into(), "from <gem:rails>/activerecord/lib/active_record.rb:<int>", &["dependency"]),
        // rvm's per-Ruby gem home isn't a gem: its binstubs stay under the home.
        (none(), "/Users/dpepper/.rvm/gems/ruby-3.4.9/bin/rspec:25:in 'Kernel#load'".into(), "~/.rvm/gems/ruby-<version>/bin/rspec:<int>:in 'Kernel#load'", &[]),
        (none(), "~/.cargo/registry/src/index.crates.io-6f17d22bba15001f/serde-1.0.200/src/de.rs:12:5".into(), "<crate:serde>/src/de.rs:<int>:<int>", &["dependency"]),
        (none(), "at Object.<anonymous> (node_modules/@babel/core/lib/index.js:10:5)".into(), "at Object.<anonymous> (<npm:@babel/core>/lib/index.js:<int>:<int>)", &["dependency"]),
        // Project-relative paths were already machine-independent: only their roles are new.
        (none(), "SQLite3::BusyException: database is locked (db/test.sqlite3)".into(), "SQLite3::BusyException: database is locked (db/test.sqlite3)", &["database"]),
        (none(), "could not obtain lock on tmp/pids/server.pid".into(), "could not obtain lock on tmp/pids/server.pid", &["lock"]),
        (none(), "rspec ./spec/models/user_spec.rb:42 # User validates email".into(), "rspec ./spec/models/user_spec.rb:<int> # User validates email", &["test"]),
        (none(), "Rendered users/show.html.erb within layouts/application".into(), "Rendered users/show.html.erb within layouts/application", &["view"]),
        (none(), "failed to load command: rspec (./Gemfile.lock)".into(), "failed to load command: rspec (./Gemfile.lock)", &["manifest"]),
        (none(), "reading config/database.yml".into(), "reading config/database.yml", &["config"]),
        (none(), "Loaded db/structure.sql".into(), "Loaded db/structure.sql", &["database"]),
        // Left alone. `/etc/hosts` is spelled alike on every machine, and reads like a route without a file name.
        (none(), "could not read /etc/hosts".into(), "could not read /etc/hosts", &[]),
        (Roots::default().project("/"), "could not read /etc/hosts".into(), "could not read /etc/hosts", &[]),
        (none(), r"C:\Users\alice\app\models\user.rb:12:in 'save'".into(), r"C:\Users\alice\app\models\user.rb:<int>:in 'save'", &[]),
        (none(), "C:/Users/alice/app/models/user.rb:12".into(), "C:/Users/alice/app/models/user.rb:<int>", &[]),
        // URLs and routes were already right.
        (none(), "Started GET \"/users/85320\" for 10.0.24.37".into(), "Started GET \"/users/<int>\" for <ip>", &[]),
        (none(), "Started GET \"/home/posts/5\" for ::1".into(), "Started GET \"/home/posts/<int>\" for <ip>", &[]),
        (none(), "GET http://localhost:3000/api/v1/users/5?page=2&per=50".into(), "GET http://localhost:<int>/api/v1/users/<int>?page=<int>&per=<int>", &[]),
    ];
    let mut failures = Vec::new();
    for (roots, line, template, roles) in cases {
        let mut n = Normalizer::with_roots(roots);
        let got = n.normalize(line.as_bytes());
        let got_template = String::from_utf8_lossy(got.template);
        let got_roles: Vec<&str> = got.roles.iter().map(PathRole::as_str).collect();
        if (got_template.as_ref(), got_roles.as_slice()) != (template, roles) {
            failures.push(format!(
                "  line: {line}\n   got: {got_template} {got_roles:?}\n  want: {template} {roles:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn a_path_slot_spans_the_raw_path_and_the_values_inside_it_still_mask() {
    let cases: [(&str, &[(SlotKind, &str)]); 3] = [
        (
            "at /Users/alice/src/app/show.html.erb:12)",
            &[
                (SlotKind::Path, "/Users/alice/src/app/show.html.erb"),
                (SlotKind::Int, "12"),
            ],
        ),
        (
            "Wrote /tmp/d20260915-123-abc/cache.bin",
            &[
                (SlotKind::Path, "/tmp/d20260915-123-abc/cache.bin"),
                (SlotKind::TempName, "d20260915-123-abc"),
            ],
        ),
        (
            "locked (db/test.sqlite3) after 5 tries",
            &[(SlotKind::Path, "db/test.sqlite3"), (SlotKind::Int, "5")],
        ),
    ];
    let mut n = Normalizer::new();
    for (line, expected) in cases {
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
        assert_eq!(slots, expected, "{line}");
    }
}
