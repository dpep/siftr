//! The bytes a run appended to `log/test.log`, exact across one logger rotation. Anything that can't be
//! placed exactly (several rotations, truncation) is refused rather than guessed. See `docs/findings/capture.md`.

use std::ffi::OsString;
use std::fs::{File, Metadata};
use std::io;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, bail};
use siftr::interpret::rspec::LOG_STREAM;

use super::Source;
use crate::record::Recording;

/// Re-read at the end: a truncation that regrew past the start offset changes these bytes, which size can't see.
const GUARD_BYTES: u64 = 4096;
/// Ruby's Logger writes this line first into every file it creates, including after a rotation.
const HEADER: &[u8] = b"# Logfile created on ";
const CHUNK_BYTES: usize = 256 * 1024;

pub struct RailsLog {
    path: PathBuf,
    start: Option<Start>,
}

struct Start {
    /// `test.log` and its size, if it existed.
    log: Option<(Held, u64)>,
    /// `test.log.0`, to tell a rotation during the run from an old one.
    rotated: Option<Held>,
    guard: Vec<u8>,
}

/// A file open from snapshot to measure. An inode number names a file only while the file exists, and ext4 hands
/// a freed number to the next file created; holding the file keeps it existing, so an identity match is this file.
struct Held {
    identity: Identity,
    _open: File,
}

impl Held {
    fn new((file, metadata): (File, Metadata)) -> Self {
        Held {
            identity: Identity::of(&metadata),
            _open: file,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Identity {
    dev: u64,
    ino: u64,
}

impl Identity {
    fn of(metadata: &Metadata) -> Self {
        Identity {
            dev: metadata.dev(),
            ino: metadata.ino(),
        }
    }
}

/// The run's bytes: `[from, to)` of each file, in order. At most the rotated-away file, then the current one.
pub struct Slice {
    segments: Vec<Segment>,
}

struct Segment {
    file: File,
    ino: u64,
    from: u64,
    to: u64,
}

impl RailsLog {
    /// A Rails project's test log under `dir`: `log/test.log` exists, or `log/` does and the Gemfile names rails.
    pub fn detect(dir: &Path) -> Option<Self> {
        let path = dir.join("log/test.log");
        let rails = || {
            std::fs::read_to_string(dir.join("Gemfile"))
                .is_ok_and(|gemfile| gemfile.contains("\"rails\"") || gemfile.contains("'rails'"))
        };
        (path.is_file() || (dir.join("log").is_dir() && rails()))
            .then_some(RailsLog { path, start: None })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Notes where the log ends before the child starts.
    pub fn snapshot(&mut self) -> Result<()> {
        let (log, guard) = match open(&self.path)? {
            Some((file, metadata)) => {
                let size = metadata.len();
                let guard = read_range(&file, size.saturating_sub(GUARD_BYTES), size)?;
                (Some((Held::new((file, metadata)), size)), guard)
            }
            None => (None, Vec::new()),
        };
        let rotated = open(&rotated_path(&self.path))?.map(Held::new);
        self.start = Some(Start {
            log,
            rotated,
            guard,
        });
        Ok(())
    }

    /// Where the run's bytes are now, or why they can't be placed exactly.
    pub fn measure(&mut self) -> Result<Slice> {
        let start = self.start.take().context("the log was never snapshotted")?;
        let current = open(&self.path)?;
        let rotated = open(&rotated_path(&self.path))?;
        let segments = match (start.log, current) {
            (None, None) => Vec::new(),
            (Some(_), None) => bail!("it disappeared during the run"),
            (Some((held, size)), Some((file, metadata)))
                if Identity::of(&metadata) == held.identity =>
            {
                vec![Segment::after(file, &metadata, size, &start.guard)?]
            }
            (Some((held, size)), Some((file, metadata))) => {
                let Some((old, old_metadata)) =
                    rotated.filter(|(_, metadata)| Identity::of(metadata) == held.identity)
                else {
                    bail!(
                        "it rotated more than once during the run, so some of the run's lines are gone"
                    );
                };
                vec![
                    Segment::after(old, &old_metadata, size, &start.guard)?,
                    Segment::created(file, &metadata)?,
                ]
            }
            (None, Some((file, metadata))) => {
                if rotated.map(|(_, metadata)| Identity::of(&metadata))
                    != start.rotated.map(|held| held.identity)
                {
                    bail!(
                        "it was created and rotated during the run, so some of the run's lines are gone"
                    );
                }
                vec![Segment::created(file, &metadata)?]
            }
        };
        Ok(Slice { segments })
    }
}

impl Segment {
    /// The bytes appended to a file that was `size` long at the start.
    fn after(file: File, metadata: &Metadata, size: u64, guard: &[u8]) -> Result<Self> {
        if metadata.len() < size {
            bail!("it was truncated during the run");
        }
        let from = size - guard.len() as u64;
        if read_range(&file, from, size)? != guard {
            bail!("it was truncated or rewritten during the run");
        }
        Ok(Segment {
            ino: metadata.ino(),
            from: size,
            to: metadata.len(),
            file,
        })
    }

    /// A file the logger created during the run, without the header line a run without rotation wouldn't have.
    fn created(file: File, metadata: &Metadata) -> Result<Self> {
        let to = metadata.len();
        let head = read_range(&file, 0, to.min(512))?;
        let from = match head.iter().position(|&b| b == b'\n') {
            Some(newline) if head.starts_with(HEADER) => newline as u64 + 1,
            _ => 0,
        };
        Ok(Segment {
            ino: metadata.ino(),
            from,
            to,
            file,
        })
    }
}

impl Slice {
    /// Where `offset` bytes into the file with inode `ino` lands in the slice, if it lands in it at all.
    pub fn place(&self, ino: u64, offset: u64) -> Option<u64> {
        let mut base = 0;
        for segment in &self.segments {
            if segment.ino == ino && (segment.from..=segment.to).contains(&offset) {
                return Some(base + offset - segment.from);
            }
            base += segment.to - segment.from;
        }
        None
    }

    pub fn copy(&self, mut sink: impl FnMut(&[u8])) -> io::Result<()> {
        let mut buf = vec![0; CHUNK_BYTES];
        for segment in &self.segments {
            let mut at = segment.from;
            while at < segment.to {
                let want = ((segment.to - at) as usize).min(buf.len());
                let read = segment.file.read_at(&mut buf[..want], at)?;
                if read == 0 {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
                sink(&buf[..read]);
                at += read as u64;
            }
        }
        Ok(())
    }

    pub fn feed(&self, recording: &mut Recording) -> Result<()> {
        let stream = super::rails_log();
        self.copy(|bytes| recording.chunk(&stream, bytes))
            .context("the log slice is incomplete")
    }
}

impl Source for RailsLog {
    fn name(&self) -> &'static str {
        LOG_STREAM
    }

    fn prepare(&mut self, _command: &mut Command) -> Result<()> {
        self.snapshot()
    }

    fn collect(&mut self, recording: &mut Recording) -> Result<()> {
        self.measure()
            .context("log/test.log skipped")?
            .feed(recording)
    }
}

fn rotated_path(path: &Path) -> PathBuf {
    let mut rotated = OsString::from(path);
    rotated.push(".0");
    rotated.into()
}

fn open(path: &Path) -> Result<Option<(File, Metadata)>> {
    match File::open(path) {
        Ok(file) => {
            let metadata = file.metadata()?;
            Ok(Some((file, metadata)))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("opening {}", path.display())),
    }
}

fn read_range(file: &File, from: u64, to: u64) -> io::Result<Vec<u8>> {
    let mut bytes = vec![0; (to - from) as usize];
    file.read_exact_at(&mut bytes, from)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::Write;

    use super::*;

    struct Project {
        dir: tempfile::TempDir,
    }

    impl Project {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join("log")).unwrap();
            std::fs::write(dir.path().join("Gemfile"), "gem 'rails'\n").unwrap();
            Project { dir }
        }

        fn log(&self) -> PathBuf {
            self.dir.path().join("log/test.log")
        }

        fn append(&self, bytes: &str) {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.log())
                .unwrap();
            file.write_all(bytes.as_bytes()).unwrap();
        }

        fn ino(&self) -> u64 {
            std::fs::metadata(self.log()).unwrap().ino()
        }

        /// What Ruby's Logger writes into a file it creates.
        fn create(&self) {
            self.append("# Logfile created on 2026-09-13 17:00:00 -0700 by logger.rb/v1.7.0\n");
        }

        /// What Ruby's Logger does at its size limit with one file kept.
        fn rotate(&self) {
            std::fs::rename(self.log(), rotated_path(&self.log())).unwrap();
            self.create();
        }

        fn snapshot(&self) -> RailsLog {
            let mut log = RailsLog::detect(self.dir.path()).expect("a log dir");
            log.snapshot().unwrap();
            log
        }
    }

    fn bytes(slice: &Slice) -> String {
        let mut out = Vec::new();
        slice.copy(|chunk| out.extend_from_slice(chunk)).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn the_slice_is_exactly_what_the_run_appended() {
        let project = Project::new();
        project.append("before\n");
        let mut log = project.snapshot();
        project.append("one\ntwo\n");
        let slice = log.measure().unwrap();
        assert_eq!(bytes(&slice), "one\ntwo\n");
        assert_eq!(
            slice.place(project.ino(), 11),
            Some(4),
            "the start of `two`"
        );
        assert_eq!(slice.place(project.ino(), 3), None, "before the run");
        assert_eq!(slice.place(project.ino() + 1, 11), None, "another file");
    }

    #[test]
    fn one_rotation_joins_the_old_files_tail_to_the_new_file_without_its_header() {
        let project = Project::new();
        project.append("before\n");
        let old = project.ino();
        let mut log = project.snapshot();
        project.append("one\n");
        project.rotate();
        let header = std::fs::metadata(project.log()).unwrap().len();
        project.append("two\n");
        let slice = log.measure().unwrap();
        assert_eq!(bytes(&slice), "one\ntwo\n");
        assert_eq!(slice.place(old, 11), Some(4), "the old file's end");
        assert_eq!(
            slice.place(project.ino(), header),
            Some(4),
            "the new file's first line"
        );
        assert_eq!(slice.place(project.ino(), header + 4), Some(8));
    }

    #[test]
    fn a_log_created_during_the_run_starts_after_its_header() {
        let project = Project::new();
        let mut log = project.snapshot();
        project.create();
        project.append("one\n");
        assert_eq!(bytes(&log.measure().unwrap()), "one\n");
    }

    /// Whether this process still holds inode `ino` open though its last link is gone.
    fn held_after_unlink(ino: u64) -> bool {
        // No dev check: macOS's /dev/fd reports its own device, not the file's.
        std::fs::read_dir("/dev/fd")
            .unwrap()
            .filter_map(|entry| std::fs::metadata(entry.ok()?.path()).ok())
            .any(|m| m.ino() == ino && m.nlink() == 0)
    }

    #[test]
    fn the_start_files_stay_held_so_no_new_file_can_take_their_inode_numbers() {
        let project = Project::new();
        project.append("before\n");
        project.rotate();
        let [rotated, log] = [rotated_path(&project.log()), project.log()]
            .map(|path| std::fs::metadata(path).unwrap());
        let mut start = project.snapshot();
        // Unlinks both: the first rotation replaces the old `.0`, the second the starting `test.log`.
        project.rotate();
        project.rotate();
        for (name, file) in [("test.log", log), ("test.log.0", rotated)] {
            assert!(held_after_unlink(file.ino()), "{name}");
        }
        let error = start.measure().err().expect("two rotations");
        assert!(error.to_string().contains("more than once"), "{error}");
    }

    #[test]
    fn what_cannot_be_placed_exactly_is_refused() {
        // Empty, as `rails log:clear` leaves it: no guard bytes, so only identity can tell a new file from this one.
        let rotated_twice = Project::new();
        rotated_twice.append("");
        let mut log = rotated_twice.snapshot();
        rotated_twice.rotate();
        rotated_twice.rotate();
        let error = log.measure().err().expect("two rotations");
        assert!(error.to_string().contains("more than once"), "{error}");

        let shrunk = Project::new();
        shrunk.append("a long line from an earlier run\n");
        let mut log = shrunk.snapshot();
        std::fs::write(shrunk.log(), "").unwrap();
        shrunk.append("short\n");
        let error = log.measure().err().expect("truncated");
        assert!(error.to_string().contains("truncated"), "{error}");

        let regrown = Project::new();
        regrown.append("before\n");
        let mut log = regrown.snapshot();
        std::fs::write(regrown.log(), "").unwrap();
        regrown.append("the run wrote more than was there before\n");
        let error = log
            .measure()
            .err()
            .expect("truncated, then regrew past the start");
        assert!(error.to_string().contains("rewritten"), "{error}");
    }

    #[test]
    fn only_rails_projects_have_a_test_log() {
        let dir = tempfile::tempdir().unwrap();
        assert!(RailsLog::detect(dir.path()).is_none(), "nothing there");
        std::fs::create_dir(dir.path().join("log")).unwrap();
        assert!(RailsLog::detect(dir.path()).is_none(), "a log dir alone");
        std::fs::write(dir.path().join("Gemfile"), "gem \"rails\", \"~> 8.1\"\n").unwrap();
        assert!(RailsLog::detect(dir.path()).is_some());
    }
}
