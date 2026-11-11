//! A tab's history file. Nothing else can open it, and it never outlives
//! the process: on Unix it is unlinked as soon as it is created, on Windows
//! it is deleted when its handle closes, a crash included.


use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Chunks kept in the read cache.
pub const CACHED: usize = 4;

pub struct DiskFile {
    file: File,
    len: AtomicU64,
    cache: Mutex<VecDeque<(u64, super::Chunk)>>,
}

impl DiskFile {
    /// A new history file in `dir`, which is created if needed (owner only).
    pub fn create(dir: &Path) -> io::Result<Arc<DiskFile>> {
        static N: AtomicU64 = AtomicU64::new(0);
        create_dir(dir)?;
        let name = format!("{}-{}.hist", std::process::id(), N.fetch_add(1, Ordering::Relaxed));
        let path = dir.join(name);
        let file = open_new(&path)?;
        #[cfg(unix)]
        std::fs::remove_file(&path)?;
        Ok(Arc::new(DiskFile { file, len: AtomicU64::new(0), cache: Mutex::new(VecDeque::new()) }))
    }

    /// Appends `bytes` and returns where they start.
    pub fn append(&self, bytes: &[u8]) -> io::Result<u64> {
        let offset = self.len.fetch_add(bytes.len() as u64, Ordering::Relaxed);
        write_at(&self.file, bytes, offset)?;
        Ok(offset)
    }

    /// The `len` bytes at `offset`, from the cache when it has them.
    pub fn read(&self, offset: u64, len: u32) -> io::Result<super::Chunk> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, bytes)) = cache.iter().find(|(o, b)| *o == offset && b.len() == len as usize) {
            return Ok(bytes.clone());
        }
        let mut buf = vec![0u8; len as usize];
        read_at(&self.file, &mut buf, offset)?;
        let bytes = Arc::new(buf);
        if cache.len() == CACHED {
            cache.pop_front();
        }
        cache.push_back((offset, bytes.clone()));
        Ok(bytes)
    }

    /// Bytes held by the read cache.
    #[cfg(test)]
    pub fn cached_bytes(&self) -> usize {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(_, b)| b.len()).sum()
    }
}

#[cfg(unix)]
fn create_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

#[cfg(not(unix))]
fn create_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)
}

#[cfg(unix)]
fn open_new(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new().read(true).write(true).create_new(true).mode(0o600).open(path)
}

#[cfg(windows)]
fn open_new(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
    const FILE_ATTRIBUTE_TEMPORARY: u32 = 0x100;
    const FILE_SHARE_ALL: u32 = 7;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
        .attributes(FILE_ATTRIBUTE_TEMPORARY)
        .share_mode(FILE_SHARE_ALL)
        .open(path)
}

#[cfg(unix)]
fn write_at(file: &File, bytes: &[u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, bytes, offset)
}

#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

#[cfg(windows)]
fn write_at(file: &File, mut bytes: &[u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !bytes.is_empty() {
        let n = file.seek_write(bytes, offset)?;
        if n == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        bytes = &bytes[n..];
        offset += n as u64;
    }
    Ok(())
}

#[cfg(windows)]
fn read_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = file.seek_read(buf, offset)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buf = &mut buf[n..];
        offset += n as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testterm::temp_dir;

    fn entries(dir: &Path) -> usize {
        std::fs::read_dir(dir).map(|d| d.count()).unwrap_or(0)
    }

    #[test]
    fn files_never_outlive_history() {
        let dir = temp_dir("diskfile");
        let file = DiskFile::create(&dir).unwrap();
        let at = file.append(b"hello").unwrap();
        let at2 = file.append(b"world!").unwrap();
        assert_eq!((at, at2), (0, 5));
        assert_eq!(&*file.read(at2, 6).unwrap(), b"world!");
        assert_eq!(&*file.read(at, 5).unwrap(), b"hello");
        if cfg!(unix) {
            assert_eq!(entries(&dir), 0, "unlinked right away");
        }
        drop(file);
        assert_eq!(entries(&dir), 0);
    }
}
