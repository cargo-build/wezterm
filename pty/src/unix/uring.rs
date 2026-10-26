//! Blocking PTY streams using Linux submission and completion queues.

use super::{pty_read_result, PtyFd, RawFd};
use io_uring::{opcode, squeue, types, IoUring, Probe};
use std::convert::TryFrom;
use std::io::{self, Read, Write};
use std::os::unix::io::AsRawFd;

const IO_REQUEST: u64 = 1;
const CANCEL_REQUEST: u64 = 2;
/// Bounds read staging independently of the mux's larger output buffer.
const READ_BUFFER_SIZE: usize = 8 * 1024;

pub(super) struct PtyStream {
    inner: Option<Inner>,
}

struct Inner {
    ring: IoUring,
    fd: PtyFd,
    buffer: Vec<u8>,
    pending: bool,
    failed: bool,
}

impl PtyStream {
    pub(super) fn new(fd: PtyFd) -> io::Result<Self> {
        let ring = IoUring::builder().dontfork().build(8)?;
        let mut probe = Probe::new();
        ring.submitter().register_probe(&mut probe)?;
        if ![opcode::Read::CODE, opcode::Write::CODE, opcode::AsyncCancel::CODE]
            .iter()
            .all(|opcode| probe.is_supported(*opcode))
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "PTY io_uring requires read, write and cancellation support (Linux 5.6 or newer)",
            ));
        }
        Ok(Self {
            inner: Some(Inner {
                ring,
                fd,
                buffer: Vec::new(),
                pending: false,
                failed: false,
            }),
        })
    }

    fn inner(&mut self) -> &mut Inner {
        self.inner.as_mut().expect("PTY stream owns its state until drop")
    }
}

impl Inner {
    fn prepare(&mut self, length: usize) -> io::Result<u32> {
        if self.failed || self.pending {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "PTY io_uring handle is unusable after a submission failure",
            ));
        }
        let length32 = u32::try_from(length).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "PTY I/O buffer exceeds u32::MAX")
        })?;
        self.buffer.resize(length, 0);
        Ok(length32)
    }

    fn execute(&mut self, entry: squeue::Entry) -> io::Result<usize> {
        loop {
            match self.issue(&entry) {
                Err(error) if !self.failed && error.kind() == io::ErrorKind::Interrupted => continue,
                result => return result,
            }
        }
    }

    fn issue(&mut self, entry: &squeue::Entry) -> io::Result<usize> {
        self.pending = true;
        // SAFETY: only the owned buffer and descriptor are referenced. They stay
        // alive until the target completion, or are retained by PtyStream::drop.
        if unsafe { self.ring.submission().push(entry) }.is_err() {
            self.pending = false;
            return Err(io::Error::new(io::ErrorKind::Other, "PTY submission queue is full"));
        }
        loop {
            if let Some(completion) = self.ring.completion().next() {
                if completion.user_data() == IO_REQUEST {
                    self.pending = false;
                    let result = completion.result();
                    return usize::try_from(result)
                        .map_err(|_| io::Error::from_raw_os_error(-result));
                }
            }
            match self.ring.submit_and_wait(1) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => {
                    self.failed = true;
                    if let Err(cancel_error) = self.cancel_pending() {
                        return Err(io::Error::new(
                            error.kind(),
                            format!("PTY io_uring wait failed: {error}; cancellation failed: {cancel_error}"),
                        ));
                    }
                    return Err(error);
                }
            }
        }
    }

    fn cancel_pending(&mut self) -> io::Result<()> {
        let cancel = opcode::AsyncCancel::new(IO_REQUEST)
            .build()
            .user_data(CANCEL_REQUEST);
        // SAFETY: cancellation contains only a request identifier, no borrowed memory.
        unsafe { self.ring.submission().push(&cancel) }.map_err(|_| {
            io::Error::new(io::ErrorKind::Other, "PTY cancellation queue is full")
        })?;
        let mut cancel_pending = true;
        while self.pending || cancel_pending {
            for completion in self.ring.completion() {
                match completion.user_data() {
                    IO_REQUEST => self.pending = false,
                    CANCEL_REQUEST => cancel_pending = false,
                    _ => {}
                }
            }
            if !self.pending && !cancel_pending {
                return Ok(());
            }
            match self.ring.submit_and_wait(1) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

impl AsRawFd for PtyStream {
    fn as_raw_fd(&self) -> RawFd {
        self.inner.as_ref().expect("PTY stream owns its descriptor until drop").fd.as_raw_fd()
    }
}

impl Read for PtyStream {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let inner = self.inner();
        let length = inner.prepare(output.len().min(READ_BUFFER_SIZE))?;
        let entry = opcode::Read::new(
            types::Fd(inner.fd.as_raw_fd()),
            inner.buffer.as_mut_ptr(),
            length,
        )
        .build()
        .user_data(IO_REQUEST);
        let count = pty_read_result(inner.execute(entry))?;
        output[..count].copy_from_slice(&inner.buffer[..count]);
        Ok(count)
    }
}

impl Write for PtyStream {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if input.is_empty() {
            return Ok(0);
        }
        let inner = self.inner();
        let length = inner.prepare(input.len())?;
        inner.buffer.copy_from_slice(input);
        let entry = opcode::Write::new(
            types::Fd(inner.fd.as_raw_fd()),
            inner.buffer.as_ptr(),
            length,
        )
        .build()
        .user_data(IO_REQUEST);
        inner.execute(entry)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for PtyStream {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            if inner.pending {
                // Ring closure cancels asynchronously and cannot protect this allocation.
                std::mem::forget(inner);
                log::error!("retaining PTY io_uring resources: an I/O completion is still pending");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PtySize;

    #[test]
    fn cancellation_drains_an_idle_pty_read() -> anyhow::Result<()> {
        let (master, _slave) = super::super::openpty(PtySize::default())?;
        let mut stream = PtyStream::new(PtyFd(master.fd.try_clone()?))?;
        let inner = stream.inner();
        let length = inner.prepare(1)?;
        let entry = opcode::Read::new(
            types::Fd(inner.fd.as_raw_fd()),
            inner.buffer.as_mut_ptr(),
            length,
        )
        .build()
        .user_data(IO_REQUEST);
        inner.pending = true;
        // SAFETY: the stream retains its owned buffer/fd until cancellation drains the read.
        unsafe { inner.ring.submission().push(&entry) }?;
        inner.ring.submit()?;
        inner.cancel_pending()?;
        assert!(!inner.pending, "the read must finish before its buffer can be reused");
        assert!(inner.ring.completion().next().is_none(), "both completions must be drained");
        inner.prepare(READ_BUFFER_SIZE)?;
        Ok(())
    }
}
