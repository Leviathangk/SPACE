//! Two fixed inodes bound tmpfs use even when an observer holds an old fd.
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;

use super::super::{OUTPUT_LIMIT, OUTPUT_PATH};

const CAPACITY: u64 = OUTPUT_LIMIT / 2;

pub(super) struct Output {
    current: File,
    position: u64,
}

impl Output {
    pub fn open() -> io::Result<Self> {
        let mut current = open_regular(OUTPUT_PATH)?;
        let mut previous = open_regular("/run/space-runtime/output.previous.log")?;
        let length = current.metadata()?.len();
        if length > 0 {
            // Preserve the previous invocation even when the next one fails
            // before producing output. No rename/unlink can strand tmpfs pages.
            current.seek(SeekFrom::Start(length.saturating_sub(CAPACITY)))?;
            previous.set_len(0)?;
            io::copy(
                &mut Read::by_ref(&mut current).take(CAPACITY),
                &mut previous,
            )?;
        } else {
            previous.set_len(previous.metadata()?.len().min(CAPACITY))?;
        }
        current.set_len(0)?;
        current.seek(SeekFrom::Start(0))?;
        Ok(Self {
            current,
            position: 0,
        })
    }

    pub fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.position + bytes.len() as u64 > CAPACITY {
            // Keep half a file of recent output at every wrap, so a crash just
            // after wrapping does not erase the context leading up to it.
            self.current
                .seek(SeekFrom::Start(self.position.saturating_sub(CAPACITY / 2)))?;
            let mut tail = Vec::with_capacity((CAPACITY / 2) as usize);
            Read::by_ref(&mut self.current)
                .take(CAPACITY / 2)
                .read_to_end(&mut tail)?;
            self.current.seek(SeekFrom::Start(0))?;
            self.current.write_all(&tail)?;
            self.current.set_len(tail.len() as u64)?;
            self.position = tail.len() as u64;
        }
        self.current.write_all(bytes)?;
        self.position += bytes.len() as u64;
        Ok(())
    }
}

fn open_regular(path: &str) -> io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("runtime output is not a regular file"));
    }
    Ok(file)
}
