//! A recorded run canonicalizes paths under its own project root, and keeps what those paths are.

use std::process::Command;

use serde_json::Value;

fn siftr(home: &std::path::Path, project: &std::path::Path, args: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_siftr"))
        .args(args)
        .current_dir(project)
        .env("SIFTR_HOME", home)
        .env_remove("XDG_DATA_HOME")
        .output()
        .unwrap();
    assert!(
        output.status.code().is_some_and(|code| code < 2),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn ingest_writes_paths_under_the_project_as_root_and_records_their_roles() {
    let (home, project) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    // As the command sees its directory: macOS resolves a temp dir's `/var` to `/private/var`.
    let root = std::fs::canonicalize(project.path()).unwrap();
    let log = home.path().join("run.log");
    std::fs::write(
        &log,
        format!(
            "DEPRECATION WARNING: old (called from {root}/app/views/users/show.html.erb:12)\n\
             DEPRECATION WARNING: old (called from {root}/app/views/users/show.html.erb:30)\n\
             SQLite3::BusyException: database is locked (db/test.sqlite3)\n",
            root = root.display()
        ),
    )
    .unwrap();

    siftr(
        home.path(),
        project.path(),
        &["ingest", "-j", log.to_str().unwrap()],
    );
    let summary = siftr(home.path(), project.path(), &["summary", "-j"]);
    let mut behaviors: Vec<(&str, Vec<&str>)> = summary["behaviors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            let roles = b["behavior"]["roles"].as_array().unwrap();
            (
                b["behavior"]["template"].as_str().unwrap(),
                roles.iter().map(|r| r.as_str().unwrap()).collect(),
            )
        })
        .collect();
    behaviors.sort();
    assert_eq!(
        behaviors,
        [
            (
                "DEPRECATION WARNING: old (called from <root>/app/views/users/show.html.erb:<int>)",
                vec!["source", "view"]
            ),
            (
                "SQLite3::BusyException: database is locked (db/test.sqlite3)",
                vec!["database"]
            ),
        ]
    );
}
