//! Windows adapters with the same signatures as the Unix transport.

use std::path::Path;
use std::time::Duration;

use crate::error::{HerdrError, Result};

pub(crate) type Stream = board_ipc::Stream;

pub(crate) fn connect_with_deadline(path: &Path, timeout: Duration) -> Result<Stream> {
    board_ipc::connect_timeout(path, timeout).map_err(|error| {
        if error.kind() == std::io::ErrorKind::TimedOut {
            HerdrError::Deadline {
                operation: "connect",
            }
        } else {
            HerdrError::Io(error)
        }
    })
}

pub(crate) fn set_nonblocking(stream: &Stream, nonblocking: bool) -> Result<()> {
    stream
        .set_nonblocking(nonblocking)
        .map_err(HerdrError::from)
}

pub(crate) fn poll_read_ready(stream: &Stream, deadline: Duration) -> Result<bool> {
    stream
        .wait_readable(Some(deadline))
        .map_err(HerdrError::from)
}

pub(crate) fn poll_read_ready_infinite(stream: &Stream) -> Result<bool> {
    stream.wait_readable(None).map_err(HerdrError::from)
}
