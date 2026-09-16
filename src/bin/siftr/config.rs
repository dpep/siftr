//! The config file: what a *project* tells siftr that it can't work out for itself.
//!
//! One schema, in two places — `.siftr.toml`, nearest from the working directory up to the project root, then
//! `~/.config/siftr/config.toml` (`$XDG_CONFIG_HOME` when it's absolute). The project file wins.
//!
//! ```toml
//! [sources.rails_log]
//! enabled = false
//! ```
//!
//! That is the whole language, deliberately. Retention (`SIFTR_KEEP_*`) and privacy (`SIFTR_REDACT`,
//! `SIFTR_CAPTURE`) stay environment-only: they're about this machine and its data dir, not about this project,
//! and one data dir shared by two projects can't take two answers.
//!
//! Nothing here fails a command (CLAUDE.md principle 6): an unreadable file, a syntax error, an unknown key or
//! a value of the wrong type warns once on stderr and leaves the default standing.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use siftr::store::Source;

use crate::output;
use crate::project;

use crate::sources::SOURCES;

const PROJECT_FILE: &str = ".siftr.toml";
const USER_FILE: &str = "siftr/config.toml";

/// A setting's value and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub value: bool,
    pub source: Source,
}

/// A file siftr looks for. `read` is false when it isn't there, or couldn't be read or parsed — so `status` can
/// say why a file the user wrote isn't taking effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigFile {
    pub path: PathBuf,
    pub read: bool,
}

/// Every setting a config file can carry, resolved, each knowing its origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    sources: BTreeMap<&'static str, Resolved>,
    files: Vec<ConfigFile>,
}

impl Config {
    /// Reads the project file, then the user file. Warns once per problem; never fails.
    pub fn load() -> Self {
        let location = project::current().ok();
        let project = location
            .as_ref()
            .map(|at| project_file(Path::new(&at.cwd), Path::new(&at.project)));
        let user = user_file(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        );
        Self::read(project.into_iter().chain(user).collect(), output::warn)
    }

    /// Nearest file first: the first one to set a source wins.
    fn read(paths: Vec<PathBuf>, mut warn: impl FnMut(String)) -> Self {
        let mut set: BTreeMap<&'static str, Resolved> = BTreeMap::new();
        let mut files = Vec::new();
        for path in paths {
            let settings =
                contents(&path, &mut warn).and_then(|text| parse(&path, &text, &mut warn));
            let read = settings.is_some();
            for (name, value) in settings.unwrap_or_default() {
                set.entry(name).or_insert_with(|| Resolved {
                    value,
                    source: Source::File { path: path.clone() },
                });
            }
            files.push(ConfigFile { path, read });
        }
        let sources = SOURCES
            .iter()
            .map(|&name| {
                let resolved = set.remove(name).unwrap_or(Resolved {
                    value: true,
                    source: Source::Default,
                });
                (name, resolved)
            })
            .collect();
        Config { sources, files }
    }

    /// Whether `name`'s source runs. A name no file mentions — or that no siftr knows — runs: a typo must never
    /// silently turn a source off.
    pub fn source_enabled(&self, name: &str) -> bool {
        self.sources.get(name).is_none_or(|source| source.value)
    }

    /// Every source siftr knows, for `siftr status`.
    pub fn sources(&self) -> impl Iterator<Item = (&'static str, &Resolved)> {
        self.sources
            .iter()
            .map(|(name, resolved)| (*name, resolved))
    }

    /// Where siftr looked, nearest first.
    pub fn files(&self) -> &[ConfigFile] {
        &self.files
    }
}

/// `None` when there's no file there, which is the normal case and says nothing. Anything else — a directory, a
/// permission — is worth a word, since the user meant that file to be read.
fn contents(path: &Path, warn: &mut impl FnMut(String)) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            warn(format!("{}: {error}; ignoring it", tilde(path)));
            None
        }
    }
}

/// One file's `[sources.<name>] enabled` settings. `None` when the file isn't TOML at all, so the whole file is
/// ignored; anything else siftr doesn't understand is warned about and skipped on its own.
fn parse(
    path: &Path,
    text: &str,
    warn: &mut impl FnMut(String),
) -> Option<Vec<(&'static str, bool)>> {
    let file = tilde(path);
    let table: toml::Table = match text.parse() {
        Ok(table) => table,
        Err(error) => {
            let detail = error.to_string();
            warn(format!(
                "{file}: not valid TOML: {}; ignoring it",
                detail.lines().next().unwrap_or("").trim()
            ));
            return None;
        }
    };
    let mut settings = Vec::new();
    for (key, value) in &table {
        if key != "sources" {
            warn(format!(
                "{file}: {key} is not a setting; siftr reads [sources.<name>]"
            ));
            continue;
        }
        let Some(sources) = value.as_table() else {
            warn(format!(
                "{file}: sources is not a table; siftr reads [sources.<name>]"
            ));
            continue;
        };
        for (name, value) in sources {
            let Some(known) = SOURCES.iter().find(|source| *source == name) else {
                warn(format!(
                    "{file}: {name} is not a source ({}); ignoring it",
                    SOURCES.join(", ")
                ));
                continue;
            };
            let Some(keys) = value.as_table() else {
                warn(format!(
                    "{file}: sources.{name} is not a table; siftr reads [sources.{name}] enabled = true"
                ));
                continue;
            };
            for (key, value) in keys {
                if key != "enabled" {
                    warn(format!(
                        "{file}: sources.{name}.{key} is not a setting (enabled); ignoring it"
                    ));
                    continue;
                }
                match value.as_bool() {
                    Some(enabled) => settings.push((*known, enabled)),
                    // `toml::Value` prints only with the `display` feature, so name the type instead.
                    None => warn(format!(
                        "{file}: sources.{name}.enabled is not true or false ({}); using the default",
                        value.type_str()
                    )),
                }
            }
        }
    }
    Some(settings)
}

/// The nearest `.siftr.toml` from `cwd` up to `project_root`; failing that, where one would go, so `status` can
/// say where siftr looked.
fn project_file(cwd: &Path, project_root: &Path) -> PathBuf {
    for dir in cwd.ancestors() {
        let candidate = dir.join(PROJECT_FILE);
        if candidate.is_file() {
            return candidate;
        }
        if dir == project_root {
            break;
        }
    }
    project_root.join(PROJECT_FILE)
}

/// `$XDG_CONFIG_HOME/siftr/config.toml`, else `~/.config/siftr/config.toml`, as [`crate::home`] does for data.
fn user_file(xdg_config_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    // The XDG spec says to ignore a relative value.
    let xdg = xdg_config_home
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute());
    xdg.or_else(|| home.map(|home| PathBuf::from(home).join(".config")))
        .map(|dir| dir.join(USER_FILE))
}

/// A path as the user would type it: `~` stays unexpanded.
pub fn tilde(path: &Path) -> String {
    let rest = std::env::var_os("HOME").and_then(|home| path.strip_prefix(home).ok());
    match rest {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `read` over files written into a temp dir, with the warnings it produced.
    fn read(files: &[&str]) -> (Config, Vec<String>) {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<PathBuf> = files
            .iter()
            .enumerate()
            .map(|(n, text)| {
                let path = dir.path().join(format!("{n}.toml"));
                std::fs::write(&path, text).unwrap();
                path
            })
            .collect();
        let mut warnings = Vec::new();
        let config = Config::read(paths, |message| warnings.push(message));
        (config, warnings)
    }

    fn enabled(config: &Config) -> Vec<(&str, bool)> {
        config
            .sources()
            .map(|(name, resolved)| (name, resolved.value))
            .collect()
    }

    #[test]
    fn every_source_runs_until_a_file_turns_one_off() {
        let (config, warnings) = read(&[]);
        assert_eq!(
            enabled(&config),
            [("rails_log", true), ("rspec", true), ("rusage", true)]
        );
        assert_eq!(warnings, Vec::<String>::new());
        assert!(config.source_enabled("rspec"));
        assert!(
            config.source_enabled("a_source_this_siftr_never_heard_of"),
            "an unknown source is never silently off"
        );

        let (config, warnings) = read(&["[sources.rspec]\nenabled = false\n"]);
        assert_eq!(
            enabled(&config),
            [("rails_log", true), ("rspec", false), ("rusage", true)]
        );
        assert_eq!(warnings, Vec::<String>::new());
    }

    #[test]
    fn the_nearest_file_wins_and_each_value_keeps_its_origin() {
        let (config, warnings) = read(&[
            "[sources.rspec]\nenabled = true\n",
            "[sources.rspec]\nenabled = false\n[sources.rails_log]\nenabled = false\n",
        ]);
        assert_eq!(
            enabled(&config),
            [("rails_log", false), ("rspec", true), ("rusage", true)]
        );
        assert_eq!(warnings, Vec::<String>::new());

        let origin = |name| match &config.sources().find(|(n, _)| *n == name).unwrap().1.source {
            Source::File { path } => path.file_name().unwrap().to_string_lossy().into_owned(),
            other => panic!("{other:?}"),
        };
        assert_eq!(origin("rspec"), "0.toml", "the nearer file");
        assert_eq!(
            origin("rails_log"),
            "1.toml",
            "only the farther one sets it"
        );
    }

    #[test]
    fn what_siftr_cant_understand_warns_and_leaves_the_default() {
        let (config, warnings) =
            read(
                &["[retention]\nruns = 5\n[sources.rspce]\nenabled = false\n\
             [sources.rspec]\nenable = false\n[sources.rails_log]\nenabled = 1\n"],
            );
        assert_eq!(
            enabled(&config),
            [("rails_log", true), ("rspec", true), ("rusage", true)],
            "nothing understood, nothing changed"
        );
        let warnings: Vec<&str> = warnings
            .iter()
            .map(|w| w.split_once(": ").unwrap().1)
            .collect();
        // In the file's key order, which TOML sorts: one file warns the same way every time.
        assert_eq!(
            warnings,
            [
                "retention is not a setting; siftr reads [sources.<name>]",
                "sources.rails_log.enabled is not true or false (integer); using the default",
                "rspce is not a source (rspec, rails_log, rusage); ignoring it",
                "sources.rspec.enable is not a setting (enabled); ignoring it",
            ]
        );
    }

    #[test]
    fn a_file_that_isnt_toml_is_ignored_whole() {
        let (config, warnings) = read(&["[sources.rspec\nenabled = ", ""]);
        assert_eq!(
            enabled(&config),
            [("rails_log", true), ("rspec", true), ("rusage", true)]
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("not valid TOML"), "{warnings:?}");
        assert!(!config.files()[0].read, "status can say it wasn't used");
    }

    #[test]
    fn the_project_file_is_the_nearest_one_no_higher_than_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let (root, spec) = (dir.path().join("project"), dir.path().join("project/spec"));
        std::fs::create_dir_all(&spec).unwrap();
        std::fs::write(dir.path().join(PROJECT_FILE), "").unwrap();
        assert_eq!(
            project_file(&spec, &root),
            root.join(PROJECT_FILE),
            "never above the project root, and says where one would go"
        );

        std::fs::write(root.join(PROJECT_FILE), "").unwrap();
        assert_eq!(project_file(&spec, &root), root.join(PROJECT_FILE));
        std::fs::write(spec.join(PROJECT_FILE), "").unwrap();
        assert_eq!(project_file(&spec, &root), spec.join(PROJECT_FILE));
    }

    #[test]
    fn the_user_file_prefers_an_absolute_xdg_config_home() {
        let os = |s: &str| Some(OsString::from(s));
        let cases = [
            (os("/xdg"), os("/home/me"), Some("/xdg/siftr/config.toml")),
            (
                os("relative"),
                os("/home/me"),
                Some("/home/me/.config/siftr/config.toml"),
            ),
            (
                None,
                os("/home/me"),
                Some("/home/me/.config/siftr/config.toml"),
            ),
            (None, None, None),
        ];
        for (xdg, home, expected) in cases {
            assert_eq!(user_file(xdg, home), expected.map(PathBuf::from));
        }
    }
}
