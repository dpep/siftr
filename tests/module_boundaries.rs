//! The dependency rules the old crate split enforced, kept now that everything is one crate.

use std::fs;
use std::path::{Path, PathBuf};

const ROOT: &str = env!("CARGO_MANIFEST_DIR");

/// `[dependencies]` names from Cargo.toml as path roots (`signal-hook` → `signal_hook`).
fn dependencies() -> Vec<String> {
    let manifest = fs::read_to_string(format!("{ROOT}/Cargo.toml")).unwrap();
    let names: Vec<String> = manifest
        .lines()
        .skip_while(|line| *line != "[dependencies]")
        .skip(1)
        .take_while(|line| !line.starts_with('['))
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(name, _)| name.trim().replace('-', "_"))
        .collect();
    // An unreadable manifest shape must fail here, not pass every rule vacuously.
    assert!(
        names.iter().any(|name| name == "rusqlite"),
        "read {names:?} from [dependencies]"
    );
    names
}

fn sources(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        for entry in fs::read_dir(path).unwrap() {
            sources(&entry.unwrap().path(), out);
        }
    } else if path.extension().is_some_and(|ext| ext == "rs") {
        out.push(path.to_path_buf());
    }
}

/// Lines of `files` that use a path starting with `root::`.
fn uses(files: &[PathBuf], root: &str) -> Vec<String> {
    let needle = format!("{root}::");
    let mut found = Vec::new();
    for file in files {
        for (n, line) in fs::read_to_string(file).unwrap().lines().enumerate() {
            // `serde::` inside `other_serde::` or `a::serde::` is not a use of the dependency.
            let starts_a_path = line.match_indices(&needle).any(|(at, _)| {
                !line[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_' || c == ':')
            });
            if starts_a_path {
                found.push(format!("{}:{}: {}", file.display(), n + 1, line.trim()));
            }
        }
    }
    found
}

#[test]
fn normalize_uses_nothing_but_std_and_itself() {
    let mut files = vec![PathBuf::from(format!("{ROOT}/src/normalize.rs"))];
    sources(Path::new(&format!("{ROOT}/src/normalize")), &mut files);

    let mut found: Vec<String> = dependencies()
        .iter()
        .flat_map(|dep| uses(&files, dep))
        .collect();
    found.extend(uses(&files, "crate").into_iter().filter(|line| {
        line.matches("crate::").count() != line.matches("crate::normalize").count()
    }));
    assert!(
        found.is_empty(),
        "normalize must stay liftable into its own crate:\n{}",
        found.join("\n")
    );
}

#[test]
fn the_domain_reaches_neither_storage_nor_heavy_dependencies() {
    let mut all = Vec::new();
    sources(Path::new(&format!("{ROOT}/src")), &mut all);
    let outside = [
        "lib.rs",
        "store.rs",
        "store",
        "normalize.rs",
        "normalize",
        "bin",
    ]
    .map(|name| PathBuf::from(format!("{ROOT}/src/{name}")));
    let domain: Vec<PathBuf> = all
        .into_iter()
        .filter(|file| !outside.iter().any(|path| file.starts_with(path)))
        .collect();
    assert!(
        domain.iter().any(|file| file.ends_with("signal.rs")),
        "the domain modules moved"
    );

    let mut found: Vec<String> = dependencies()
        .iter()
        .filter(|dep| !["serde", "serde_json"].contains(&dep.as_str()))
        .flat_map(|dep| uses(&domain, dep))
        .collect();
    found.extend(
        uses(&domain, "crate")
            .into_iter()
            .filter(|line| line.contains("crate::store")),
    );
    assert!(
        found.is_empty(),
        "the domain must not reach store or a dependency beyond serde:\n{}",
        found.join("\n")
    );
}
