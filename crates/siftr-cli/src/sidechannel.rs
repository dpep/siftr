//! Side channels: what a wrapped command produces somewhere other than stdout and stderr,
//! such as an RSpec listener's event file or the slice of `log/test.log` a run appended.

use std::process::Command;

use anyhow::Result;

use siftr_core::observation::Stream;

use crate::record::Recording;

/// RSpec listener events, one JSON object per line. Feed before `rails_log`: its `log_offset`s index that slice.
pub fn rspec_events() -> Stream {
    Stream::File("rspec-events".into())
}

/// The bytes a run appended to `log/test.log`.
pub fn rails_log() -> Stream {
    Stream::File("log/test.log".into())
}

pub trait SideChannel {
    /// Before spawning: set or append env vars (e.g. `SPEC_OPTS`), note pre-run state such as a log's length.
    fn prepare(&mut self, command: &mut Command) -> Result<()>;

    /// After the child exits: feed what was captured into the same recording, as `Stream::File` streams.
    fn collect(&mut self, recording: &mut Recording) -> Result<()>;
}

/// The side channels that apply to this command. None yet; the rspec and rails lanes register theirs here.
pub fn for_command(_argv: &[String]) -> Vec<Box<dyn SideChannel>> {
    Vec::new()
}
