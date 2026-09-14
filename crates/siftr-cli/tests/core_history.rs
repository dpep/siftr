//! `siftr history` exits as the other query commands do: 0 results, 1 empty, 2 error.

use std::process::{Command, Output};

use serde_json::Value;

fn history(home: &std::path::Path, project: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_siftr"))
        .arg("history")
        .args(args)
        .current_dir(project)
        .env("SIFTR_HOME", home)
        .env_remove("XDG_DATA_HOME")
        .output()
        .unwrap()
}

#[test]
fn an_unknown_context_is_not_found_as_in_changes() {
    let (home, project) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    for args in [
        &["--context", "nope"][..],
        &["--signals", "--context", "nope"],
    ] {
        let json = history(home.path(), project.path(), &[args, &["-j"]].concat());
        assert_eq!(json.status.code(), Some(2), "{args:?}");
        let error: Value = serde_json::from_slice(&json.stdout).unwrap();
        assert_eq!(error["error"]["code"], "not_found", "{args:?}");

        let human = history(home.path(), project.path(), args);
        assert_eq!(human.status.code(), Some(2), "{args:?}");
        let stderr = String::from_utf8_lossy(&human.stderr);
        assert!(stderr.contains("\"nope\""), "{args:?}: {stderr}");
    }
}
