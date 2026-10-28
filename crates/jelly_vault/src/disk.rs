//! Crash-safe vault replacement. A synced temporary file in the destination directory is
//! renamed over the vault, so a failure before the rename leaves the old vault intact.
//!
//! On Unix, new directories are 0o700, files are 0o600, and the directory is synced after
//! the rename. Windows inherits the directory's permissions and gets no directory sync.
//! Either way, confidentiality rests on the encryption, not on file permissions.

use std::fs;
use std::io::{ErrorKind, Write as _};
use std::path::Path;

use tempfile::NamedTempFile;

use crate::error::VaultError;

pub(crate) fn create(destination: &Path, bytes: &[u8]) -> Result<(), VaultError> {
    let directory = parent_of(destination);
    stage(directory, bytes)?.persist_noclobber(destination).map_err(|error| {
        if error.error.kind() == ErrorKind::AlreadyExists {
            VaultError::AlreadyExists(destination.to_path_buf())
        } else {
            VaultError::io(destination, error.error)
        }
    })?;
    sync_dir(directory);
    Ok(())
}

pub(crate) fn write(destination: &Path, bytes: &[u8]) -> Result<(), VaultError> {
    let directory = parent_of(destination);
    stage(directory, bytes)?
        .persist(destination)
        .map_err(|error| VaultError::io(destination, error.error))?;
    sync_dir(directory);
    Ok(())
}

pub(crate) fn remove_if_present(path: &Path) -> Result<(), VaultError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(VaultError::io(path, error)),
    }
}

/// A synced temporary file in `directory`. Dropping it without persisting it removes it.
fn stage(directory: &Path, bytes: &[u8]) -> Result<NamedTempFile, VaultError> {
    // Only directories created here are made owner-only. An existing one keeps its
    // permissions.
    let mut directories = fs::DirBuilder::new();
    directories.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        directories.mode(0o700);
    }
    directories.create(directory).map_err(|error| VaultError::io(directory, error))?;

    let mut builder = tempfile::Builder::new();
    builder.prefix(".jelly-vault-").suffix(".tmp");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        builder.permissions(fs::Permissions::from_mode(0o600));
    }

    let mut staged =
        builder.tempfile_in(directory).map_err(|error| VaultError::io(directory, error))?;
    staged.write_all(bytes).map_err(|error| VaultError::io(staged.path(), error))?;
    staged.as_file().sync_all().map_err(|error| VaultError::io(staged.path(), error))?;
    Ok(staged)
}

fn parent_of(path: &Path) -> &Path {
    path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."))
}

/// Best effort. The file itself is already durable, and a failure here only risks the
/// rename not surviving a power loss right after it.
#[cfg(unix)]
fn sync_dir(directory: &Path) {
    if let Err(error) = fs::File::open(directory).and_then(|handle| handle.sync_all()) {
        tracing::warn!(%error, "couldn't fsync the vault directory");
    }
}

#[cfg(not(unix))]
const fn sync_dir(_directory: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_creates_missing_directories_and_replaces_the_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sub").join("wallet.vault");
        write(&path, b"first").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"first");
        write(&path, b"second-and-longer").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second-and-longer");
    }

    #[test]
    fn an_abandoned_stage_leaves_the_destination_and_the_directory_clean() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wallet.vault");
        write(&path, b"original").unwrap();

        let staged = stage(directory.path(), b"replacement").unwrap();
        let staged_path = staged.path().to_path_buf();
        assert!(staged_path.exists());
        assert_ne!(staged_path, path);
        assert_eq!(fs::read(&path).unwrap(), b"original");
        drop(staged);

        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert!(!staged_path.exists(), "an abandoned temp file must be cleaned up");
    }

    #[test]
    fn a_crash_before_the_rename_preserves_the_previous_contents() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wallet.vault");
        write(&path, b"original").unwrap();

        let staged = stage(directory.path(), b"replacement").unwrap();
        let (_file, leaked) = staged.keep().unwrap();
        assert!(leaked.exists());
        assert_ne!(leaked, path);
        assert_eq!(fs::read(&path).unwrap(), b"original");
    }

    #[cfg(unix)]
    #[test]
    fn created_files_and_directories_are_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("data");
        let path = directory.join("wallet.vault");
        write(&path, b"x").unwrap();
        assert_eq!(fs::metadata(&directory).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
