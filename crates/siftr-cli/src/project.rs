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

/// The nearest ancestor holding `.git`, else `cwd` itself.
fn root_of(cwd: &Path) -> &Path {
    cwd.ancestors()
        .find(|dir| dir.join(".git").exists())
        .unwrap_or(cwd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_root_is_the_nearest_git_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("repo/spec/models");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            root_of(&nested),
            nested,
            "no repository: the directory itself"
        );
        std::fs::create_dir(dir.path().join("repo/.git")).unwrap();
        assert_eq!(root_of(&nested), dir.path().join("repo"));
    }
}
