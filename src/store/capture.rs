//! A run's raw output on disk, one file per stream, so every exemplar's `seq` is a line number in a real file.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use crate::observation::Stream;
use crate::store::{RunId, Store};

pub struct Capture {
    dir: PathBuf,
    files: Vec<(Stream, BufWriter<File>)>,
}

impl Store {
    /// Starts the raw capture for `run`.
    pub fn capture(&self, run: RunId) -> Result<Capture> {
        let dir = self.run_dir(run);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Capture {
            dir,
            files: Vec::new(),
        })
    }

    /// Where `stream`'s raw capture for `run` lives. A run recorded before file streams stopped doubling
    /// their extension (`file-log_test.log.log`) keeps its old name.
    pub fn capture_file(&self, run: RunId, stream: &Stream) -> PathBuf {
        let dir = self.run_dir(run);
        let path = dir.join(file_name(stream));
        match legacy_file_name(stream) {
            Some(legacy) if !path.exists() && dir.join(&legacy).exists() => dir.join(legacy),
            _ => path,
        }
    }
}

impl Capture {
    pub fn write(&mut self, stream: &Stream, bytes: &[u8]) -> io::Result<()> {
        let index = match self.files.iter().position(|(s, _)| s == stream) {
            Some(index) => index,
            None => {
                let file = File::create(self.dir.join(file_name(stream)))?;
                self.files.push((stream.clone(), BufWriter::new(file)));
                self.files.len() - 1
            }
        };
        self.files[index].1.write_all(bytes)
    }

    pub fn finish(self) -> io::Result<()> {
        self.files
            .into_iter()
            .try_for_each(|(_, mut file)| file.flush())
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// A file stream keeps its own extension, and gets `.log` only without one: `log/test.log` is `file-log_test.log`.
fn file_name(stream: &Stream) -> String {
    match stream {
        Stream::Stdout => "stdout.log".to_owned(),
        Stream::Stderr => "stderr.log".to_owned(),
        Stream::File(path) if Path::new(&**path).extension().is_some() => {
            format!("file-{}", safe(path))
        }
        Stream::File(path) => format!("file-{}.log", safe(path)),
    }
}

/// The name a file stream with an extension had before: `.log` added regardless.
fn legacy_file_name(stream: &Stream) -> Option<String> {
    match stream {
        Stream::File(path) if Path::new(&**path).extension().is_some() => {
            Some(format!("file-{}.log", safe(path)))
        }
        _ => None,
    }
}

fn safe(path: &str) -> String {
    path.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::context::Context;

    use super::*;
    use crate::store::NewRun;

    #[test]
    fn a_file_stream_keeps_its_own_extension() {
        let name = |path: &str| file_name(&Stream::File(path.into()));
        assert_eq!(name("log/test.log"), "file-log_test.log");
        assert_eq!(name("rspec-events"), "file-rspec-events.log");
        assert_eq!(file_name(&Stream::Stdout), "stdout.log");
    }

    #[test]
    fn a_capture_recorded_under_the_old_name_is_still_found() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open(home.path()).unwrap();
        let context = Context::named("/project", "rspec");
        let new_run = NewRun {
            context: &context,
            command: "rspec",
            cwd: "/project",
            started_at: std::time::SystemTime::now(),
        };
        let log = Stream::File("log/test.log".into());
        let (old, new) = (
            store.begin_run(&new_run).unwrap(),
            store.begin_run(&new_run).unwrap(),
        );
        std::fs::create_dir_all(store.run_dir(old)).unwrap();
        std::fs::write(store.run_dir(old).join("file-log_test.log.log"), "old\n").unwrap();
        let mut capture = store.capture(new).unwrap();
        capture.write(&log, b"new\n").unwrap();
        capture.finish().unwrap();

        assert_eq!(
            std::fs::read_to_string(store.capture_file(old, &log)).unwrap(),
            "old\n"
        );
        assert_eq!(
            std::fs::read_to_string(store.capture_file(new, &log)).unwrap(),
            "new\n"
        );
        assert!(store.capture_file(new, &log).ends_with("file-log_test.log"));
    }
}
