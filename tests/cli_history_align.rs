//! `siftr history` keeps its columns aligned once run ids reach two digits.

use std::path::Path;
use std::process::Command;

#[test]
fn history_columns_align_across_one_and_two_digit_run_ids() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/rails_demo/baseline");
    let siftr = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_siftr"))
            .args(args)
            .current_dir(project.path())
            .env("SIFTR_HOME", home.path())
            .env_remove("XDG_DATA_HOME")
            .output()
            .unwrap()
    };
    for _ in 0..11 {
        let ingest = siftr(&[
            "ingest",
            "--context",
            "demo",
            "--dir",
            fixture.to_str().unwrap(),
        ]);
        assert!(
            ingest.status.success(),
            "{}",
            String::from_utf8_lossy(&ingest.stderr)
        );
    }

    let output = siftr(&["history", "--context", "demo"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let age_column = |id: &str| {
        let row = stdout
            .lines()
            .find(|line| line.trim_start().starts_with(&format!("{id} ")))
            .unwrap_or_else(|| panic!("no row for {id}:\n{stdout}"));
        row.find(" ago").unwrap()
    };
    assert_eq!(age_column("r9"), age_column("r11"), "\n{stdout}");
}
