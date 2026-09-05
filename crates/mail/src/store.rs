use crate::{Error, Result};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

/// Immutable blobs named by lowercase SHA-256 of their exact bytes.
///
/// Blocking filesystem API: async callers should use a blocking task. The root
/// and its ancestors must be controlled by the service/operator, not hostile
/// same-UID processes. Unix roots must be private (0700); blobs are 0600.
/// Publication is atomic and no-clobber on a filesystem supporting hard links.
/// Files and the directory are synced before success; crashes may leave temp
/// files, never a partially published blob. No deletion/GC API is provided.
#[derive(Debug, Clone)]
pub struct FsMessageStore {
    root: PathBuf,
}

impl FsMessageStore {
    /// Open or create a private store. The parent directory must already exist.
    /// Existing insecure roots are rejected, not silently chmodded.
    /// # Errors
    /// Returns unsafe-path, permissions or filesystem failures.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(root) {
            Ok(()) => {
                fs::File::open(
                    root.parent()
                        .filter(|p| !p.as_os_str().is_empty())
                        .unwrap_or_else(|| Path::new(".")),
                )?
                .sync_all()?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let metadata = fs::symlink_metadata(root)?;
        if !metadata.is_dir() {
            return Err(Error::UnsafeStorePath);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o777 != 0o700 {
                return Err(Error::UnsafeStorePath);
            }
        }
        Ok(Self {
            root: fs::canonicalize(root)?,
        })
    }

    /// Persist bytes without replacing an existing blob and return its key.
    /// # Errors
    /// Returns I/O failures or corruption (including an existing mismatched blob).
    pub fn put(&self, raw: &[u8]) -> Result<String> {
        let key = content_key(raw);
        let mut temporary = tempfile::NamedTempFile::new_in(&self.root)?;
        temporary.write_all(raw)?;
        temporary.as_file().sync_all()?;
        match temporary.persist_noclobber(self.root.join(&key)) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                if self.get(&key)? != raw {
                    return Err(Error::CorruptMessage);
                }
            }
            Err(error) => return Err(error.error.into()),
        }
        fs::File::open(&self.root)?.sync_all()?;
        Ok(key)
    }

    /// Read exact bytes and verify their SHA-256 content address.
    /// # Errors
    /// Rejects invalid keys, non-regular files, corruption and filesystem errors.
    /// Missing blobs return `Error::Io` with `ErrorKind::NotFound`.
    pub fn get(&self, key: &str) -> Result<Vec<u8>> {
        if key.len() != 64
            || !key
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::InvalidStoreKey);
        }
        let path = self.root.join(key);
        if !fs::symlink_metadata(&path)?.is_file() {
            return Err(Error::UnsafeStorePath);
        }
        let raw = fs::read(path)?;
        if content_key(&raw) != key {
            return Err(Error::CorruptMessage);
        }
        Ok(raw)
    }
}

fn content_key(raw: &[u8]) -> String {
    format!("{:x}", Sha256::digest(raw))
}
