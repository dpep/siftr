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

/// The nearest ancestor holding a manifest, looking no higher than the git repository's root: two apps in one
/// repo (or a Cargo workspace and its member) run the same command to different effect, so they must not share a
/// baseline. Otherwise `cwd` itself. Not the repository root: with no manifest to say where a suite begins,
/// `a/` and `b/` would share one, and a false NEW costs more than a fresh baseline in a subdirectory. Not above
/// it either: a stray `~/package.json` must not merge every directory under it.
fn root_of(cwd: &Path) -> &Path {
    let Some(repo) = cwd.ancestors().position(|dir| dir.join(".git").exists()) else {
        return cwd;
    };
    cwd.ancestors()
        .take(repo + 1)
        .find(|dir| MANIFESTS.iter().any(|name| dir.join(name).is_file()))
        .unwrap_or(cwd)
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
        assert_eq!(
            root_of(&spec),
            spec,
            "no manifest in the repository: still the directory itself, not the repository root"
        );

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
