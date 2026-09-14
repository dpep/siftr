//! A run's raw output on disk, one file per stream, so every exemplar's `seq` is a line number in a real file.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use siftr_core::observation::Stream;

use crate::{RunId, Store};

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

    /// Where `stream`'s raw capture for `run` lives.
    pub fn capture_file(&self, run: RunId, stream: &Stream) -> PathBuf {
        self.run_dir(run).join(file_name(stream))
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

fn file_name(stream: &Stream) -> String {
    match stream {
        Stream::Stdout => "stdout.log".to_owned(),
        Stream::Stderr => "stderr.log".to_owned(),
        Stream::File(path) => {
            let safe: String = path
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || "._-".contains(c) {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            format!("file-{safe}.log")
        }
    }
}
