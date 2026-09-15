//! The data directory: `--home` > `SIFTR_HOME` (both via clap) > `$XDG_DATA_HOME/siftr` > `~/.local/share/siftr`.

use std::env;
use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Result, anyhow};

pub fn resolve(flag_or_env: Option<PathBuf>) -> Result<PathBuf> {
    flag_or_env
        .or_else(|| default_home(env::var_os("XDG_DATA_HOME"), env::var_os("HOME")))
        .ok_or_else(|| anyhow!("no data directory: set --home or SIFTR_HOME"))
}

fn default_home(xdg_data_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    // The XDG spec says to ignore a relative value.
    let xdg = xdg_data_home
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute());
    xdg.map(|dir| dir.join("siftr"))
        .or_else(|| home.map(|home| PathBuf::from(home).join(".local/share/siftr")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_absolute_xdg_then_home() {
        let os = |s: &str| Some(OsString::from(s));
        let cases = [
            (os("/xdg"), os("/home/me"), Some("/xdg/siftr")),
            (
                os("relative"),
                os("/home/me"),
                Some("/home/me/.local/share/siftr"),
            ),
            (None, os("/home/me"), Some("/home/me/.local/share/siftr")),
            (None, None, None),
        ];
        for (xdg, home, expected) in cases {
            assert_eq!(default_home(xdg, home), expected.map(PathBuf::from));
        }
    }
}
