//! Where a run happens: the working directory, and the project root that scopes history and baselines.

use std::path::Path;

use anyhow::{Context as _, Result};

pub struct Location {
    pub cwd: String,
    pub project: String,
}

pub fn current() -> Result<Location> {
    let cwd = std::env::current_dir().context("reading the current directory")?;
    Ok(Location {
        project: root_of(&cwd).to_string_lossy().into_owned(),
        cwd: cwd.to_string_lossy().into_owned(),
    })
}

/// Files a build or test tool treats as the top of its project.
const MANIFESTS: &[&str] = &[
    "Gemfile",
    "Cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
    "setup.py",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "mix.exs",
    "composer.json",
];

/// Inside a git repository, the nearest ancestor holding a manifest, else the repository root: two apps in one
/// repo (or a Cargo workspace and its member) run the same command to different effect, so they must not share a
/// baseline. Outside one, `cwd` itself — a stray `~/package.json` must not merge every directory under it.
fn root_of(cwd: &Path) -> &Path {
    let mut manifest = None;
    for dir in cwd.ancestors() {
        if manifest.is_none() && MANIFESTS.iter().any(|name| dir.join(name).is_file()) {
            manifest = Some(dir);
        }
        if dir.join(".git").exists() {
            return manifest.unwrap_or(dir);
        }
    }
    cwd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_root_is_the_nearest_manifest_inside_the_repository() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let spec = repo.join("apps/web/spec/models");
        std::fs::create_dir_all(&spec).unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        assert_eq!(
            root_of(&spec),
            spec,
            "no repository: the directory itself, whatever lies above"
        );

        std::fs::create_dir(repo.join(".git")).unwrap();
        assert_eq!(root_of(&spec), repo, "no manifest: the repository root");

        std::fs::write(repo.join("Cargo.toml"), "").unwrap();
        std::fs::write(repo.join("apps/web/Gemfile"), "").unwrap();
        assert_eq!(
            root_of(&spec),
            repo.join("apps/web"),
            "a subdirectory of an app is that app"
        );
        assert_eq!(
            root_of(&repo.join("apps")),
            repo,
            "a workspace root is not its member"
        );
    }
}
