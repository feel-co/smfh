use crate::{
    file_util,
    manifest,
};
use blake3::Hash;
use color_eyre::{
    Result,
    eyre::{
        Context as _,
        OptionExt as _,
        eyre,
    },
};
use core::result::Result::Ok;
use log::{
    info,
    warn,
};
use manifest::{
    File,
    FileKind,
};
use rand::distr::{
    Alphanumeric,
    SampleString as _,
};
use std::{
    ffi::OsString,
    fs::{
        self,
        Metadata,
        read_link,
    },
    io::ErrorKind,
    os::unix::fs::{
        MetadataExt as _,
        PermissionsExt as _,
        chown,
        lchown,
        symlink,
    },
    path::{
        self,
        Path,
        PathBuf,
        absolute,
    },
};

impl File {
    /// Activates the file at [`target`][Self::target] by performing the
    /// operation described by [`kind`][Self::kind]. Handles clobber and
    /// backup (via `prefix`) before writing.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    ///
    /// - metadata access fails
    /// - atomic activation fails
    /// - existing-file removal or backup fails
    /// - the underlying file operation (symlink, copy, directory creation,
    ///   chmod/chown) fails
    #[inline]
    pub fn activate(&mut self, clobber_by_default: Option<bool>, prefix: &str) -> Result<()> {
        if self.check().unwrap_or(false) {
            info!("File '{}' already correct", self.target.display());
            return Ok(());
        }

        if let Some(metadata) = get_metadata(&self.target)? {
            if self.kind == FileKind::Delete {
                delete(&self.target, &metadata)?;
                return Ok(());
            }

            let clobber = self
                .clobber
                .unwrap_or_else(|| clobber_by_default.unwrap_or(false));

            // A dangling symlink holds no user content worth protecting
            // (e.g. its link target was a garbage-collected store path),
            // so it is repaired even when clobbering is disabled.
            let repair_broken_link = matches!(self.kind, FileKind::Copy | FileKind::Symlink)
                && is_dangling_symlink(&self.target, &metadata);

            if !clobber && !repair_broken_link {
                match self.kind {
                    FileKind::Copy | FileKind::Symlink => return Ok(()),
                    FileKind::Directory if !metadata.is_dir() => return Ok(()),
                    _ => {}
                }
            }

            if clobber
                && self
                    .atomic_activate()
                    .wrap_err("While attempting atomic activation")?
            {
                return Ok(());
            }

            if repair_broken_link {
                // Nothing worth backing up; drop the broken link.
                delete(&self.target, &metadata)?;
            } else if match *self {
                Self {
                    kind: FileKind::Modify,
                    ..
                } => false,
                // Don't clobber directories
                // If they're supposed to be
                // directories
                Self {
                    kind: FileKind::Directory,
                    ..
                } => !metadata.is_dir(),
                _ => true,
            } {
                if clobber {
                    delete(&self.target, &metadata)?;
                } else {
                    prefix_move(&self.target, prefix)?;
                }
            }
        }

        match self.kind {
            FileKind::Directory => self.directory(),
            FileKind::Copy => self.copy(),
            FileKind::Symlink => self.symlink(),
            FileKind::Modify => self.chmod_chown(),
            FileKind::Delete => Ok(()),
        }
    }

    /// Attempts an atomic replacement of an existing
    /// [`Symlink`][FileKind::Symlink] or [`Copy`][FileKind::Copy] target by
    /// writing to a random temporary name in the same directory, then
    /// renaming into place. Returns `true` if the swap succeeded, `false` if
    /// the kind does not support atomic replacement or the target and
    /// source types are incompatible.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    ///
    /// - source metadata cannot be accessed
    /// - directory listing fails
    /// - symlink or copy creation fails
    /// - the final rename fails
    #[inline]
    pub fn atomic_activate(&mut self) -> Result<bool> {
        fn randomize_filename(file: &mut File) {
            let string = Alphanumeric.sample_string(&mut rand::rng(), 16);
            file.target.set_file_name(string);
            if file.target.exists() {
                randomize_filename(file);
            }
        }
        #[expect(clippy::type_complexity)]
        // This is gross but allows me to only match FileKind once
        // and doesn't require leaving unreachable code blocks
        let exit_on_definition: Box<dyn Fn(&mut Self) -> Result<()>> = match self.kind {
            FileKind::Directory | FileKind::Modify | FileKind::Delete => {
                return Ok(false);
            }
            FileKind::Copy => Box::new(move |f| f.copy()),
            FileKind::Symlink => Box::new(move |f| f.symlink()),
        };

        let Some(metadata) = get_metadata(&self.target)? else {
            return Ok(false);
        };

        let Some(ref source) = self.source else {
            return Err(eyre!("Missing source"));
        };

        let target_is_dir = metadata.is_dir();
        let source_is_dir = fs::symlink_metadata(source)?.is_dir();

        if target_is_dir != source_is_dir
            || target_is_dir && source_is_dir && source.read_dir()?.next().is_some()
        {
            return Ok(false);
        }

        let target = self.target.clone();

        randomize_filename(self);
        let temp_path = self.target.clone();

        {
            if let Err(e) = exit_on_definition(self) {
                Err(e)
            } else {
                info!(
                    "Renaming '{}' -> '{}'",
                    temp_path.display(),
                    target.display()
                );
                fs::rename(&temp_path, &target).map_err(Into::into)
            }
        }
        .inspect_err(|_| {
            _ = fs::remove_file(&temp_path);
        })?;

        Ok(true)
    }

    /// Returns `true` if the file at [`target`][Self::target] matches the
    /// expected kind, permissions, ownership, and content.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    ///
    /// - a `Symlink` or `Copy` file has no `source`
    /// - canonicalization, symlink resolution, or stat calls fail
    #[inline]
    pub fn check(&self) -> Result<bool> {
        // check if the source exists prior to everything else
        if self.check_source() {
            return Ok(false);
        }
        // then check if the target exists at all
        let Some(metadata) = get_metadata(&self.target)? else {
            return Ok(self.kind == FileKind::Delete);
        };

        match *self {
            Self {
                kind: FileKind::Delete,
                ..
            } => Ok(false),
            // This should never happen
            // as it's checked by check_source
            Self {
                source: None,
                kind: FileKind::Symlink | FileKind::Copy,
                ref target,
                ..
            } => Err(eyre!("File '{}' missing_source", target.display())),
            Self {
                kind: FileKind::Copy,
                ..
            } if !metadata.is_file() => Ok(false),
            Self {
                kind: FileKind::Symlink,
                ..
            } if !metadata.is_symlink() => Ok(false),
            Self {
                kind: FileKind::Copy | FileKind::Symlink,
                ignore_modification: Some(x),
                ..
            } if x => Ok(true),
            Self {
                kind: FileKind::Copy | FileKind::Directory | FileKind::Modify,
                permissions: Some(perms),
                ..
            } if perms != (metadata.mode() & 0o7_777) => Ok(false),
            Self { uid: Some(uid), .. } if uid != metadata.uid() => Ok(false),
            Self { gid: Some(gid), .. } if gid != metadata.gid() => Ok(false),
            Self {
                kind: FileKind::Symlink,
                ref target,
                source: Some(ref source),
                follow_symlinks: canonicalize,
                ..
            } => {
                // This will fail if target
                // is a dead symlink
                // which should only happen
                // if source does not exist
                // which should never happen
                if canonicalize.unwrap_or(true) {
                    Ok(fs::canonicalize(target)? == fs::canonicalize(source)?)
                } else {
                    Ok(read_link(target)? == absolute(source)?)
                }
            }
            Self {
                kind: FileKind::Directory,
                ..
            } => Ok(metadata.is_dir()),

            Self {
                kind: FileKind::Copy,
                ref target,
                source: Some(ref source),
                ..
            } => {
                if metadata.len() != fs::symlink_metadata(source)?.len() {
                    return Ok(false);
                }

                match (hash_file(target), hash_file(source)) {
                    (Some(left), Some(right)) => Ok(left == right),
                    _ => Ok(false),
                }
            }
            Self {
                kind: FileKind::Modify,
                ..
            } => Ok(true),
        }
    }

    /// Returns `true` if the source is absent or invalid for a
    /// [`Copy`][FileKind::Copy] or [`Symlink`][FileKind::Symlink] file,
    /// logging a warning. When `true`, the caller should skip activation.
    #[inline]
    #[must_use]
    pub fn check_source(&self) -> bool {
        match *self {
            Self {
                source: Some(ref metadata),
                kind: FileKind::Copy | FileKind::Symlink,
                ..
            } if fs::symlink_metadata(metadata)
                .is_err_and(|err| err.kind() == ErrorKind::NotFound) =>
            {
                warn!(
                    "{} with target '{}' source '{}' does not exist",
                    self.kind,
                    self.target.display(),
                    metadata.display()
                );
                true
            }
            Self {
                source: None,
                kind: FileKind::Copy | FileKind::Symlink,
                ..
            } => {
                warn!(
                    "{} with target '{}' missing source, skipping...",
                    self.kind,
                    self.target.display()
                );
                true
            }
            Self {
                source: Some(ref source),
                kind: FileKind::Copy,
                ..
            } if fs::symlink_metadata(source).is_ok_and(|x| !x.is_file()) => {
                warn!(
                    "{} with target '{}' source '{}' is a directory, only files are permitted. Skipping...",
                    self.kind,
                    self.target.display(),
                    source.display()
                );
                true
            }

            _ => false,
        }
    }

    /// Applies the configured [`permissions`][Self::permissions],
    /// [`uid`][Self::uid], and [`gid`][Self::gid] to the target file.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    ///
    /// - the target does not exist
    /// - setting permissions fails
    /// - `chown` or `lchown` fails
    #[inline]
    pub fn chmod_chown(&self) -> Result<()> {
        let Some(metadata) = get_metadata(&self.target)? else {
            return Err(eyre!(
                "Can't modify file '{}', file does not exist",
                self.target.display()
            ));
        };

        if self.kind != FileKind::Symlink
            && let Some(x) = self.permissions
        {
            let new_perms = fs::Permissions::from_mode(x);

            if metadata.mode() & 0o7_777 == new_perms.mode() {
                return Ok(());
            }
            info!(
                "Setting permissions of: '{}' to: '{:o}'",
                &self.target.display(),
                new_perms.mode(),
            );

            //This doesn't work with symlinks
            fs::set_permissions(&self.target, new_perms)?;
        }

        if self.uid.is_some() || self.gid.is_some() {
            if (self.uid.is_some_and(|x| x == metadata.uid()))
                && (self.gid.is_some_and(|x| x == metadata.gid()))
            {
                return Ok(());
            }
            info!(
                "Chowning '{}': 'uid:{} gid:{}' -> 'uid:{} gid::{}'",
                self.target.display(),
                metadata.uid(),
                metadata.gid(),
                self.uid.unwrap_or_else(|| metadata.uid()),
                self.gid.unwrap_or_else(|| metadata.gid()),
            );
            if metadata.is_symlink() {
                lchown(&self.target, self.uid, self.gid)?;
            } else {
                chown(&self.target, self.uid, self.gid)?;
            }
        }
        Ok(())
    }

    /// Copies [`source`][Self::source] to [`target`][Self::target], then
    /// applies permissions and ownership.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - parent directory cannot be created
    /// - source path cannot be canonicalized
    /// - file copy fails
    #[inline]
    pub fn copy(&self) -> Result<()> {
        _ = file_util::mkdir(
            self.target
                .parent()
                .ok_or_eyre("Failed to get parent directory")?,
        );
        let Some(ref source) = self.source else {
            return Err(eyre!("Missing source"));
        };
        let source_path = fs::canonicalize(source)?;

        fs::copy(&source_path, &self.target)?;
        info!(
            "Copied '{}' -> '{}'",
            source.display(),
            &self.target.display(),
        );

        self.chmod_chown()?;
        Ok(())
    }

    /// Removes the file at [`target`][Self::target] if it still matches the
    /// expected state. No-op for [`Delete`][FileKind::Delete] and
    /// [`Modify`][FileKind::Modify] kinds.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - the file has been modified since activation
    /// - the target is not the expected type
    /// - filesystem removal fails
    #[inline]
    pub fn deactivate(&self) -> Result<()> {
        if !self.deactivate.unwrap_or(true) {
            return Ok(());
        }

        let Some(metadata) = get_metadata(&self.target)? else {
            info!("File already deleted '{}'", self.target.display());
            return Ok(());
        };

        if !self.check()? {
            return Err(eyre!("File is not the same as expected"));
        }

        match self.kind {
            // no-op on deactivation
            FileKind::Delete | FileKind::Modify => Ok(()),
            // deletes only if directory is empty
            FileKind::Directory if metadata.is_dir() => {
                fs::remove_dir(&self.target)?;
                info!("Deleting directory '{}'", self.target.display());
                Ok(())
            }
            FileKind::Directory => Err(eyre!("File is not directory")),
            // delete only if types match
            FileKind::Symlink | FileKind::Copy => delete(&self.target, &metadata),
        }
    }

    /// Creates [`target`][Self::target] as a directory, then applies
    /// permissions and ownership.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - directory creation fails
    /// - permission or ownership changes fail
    #[inline]
    pub fn directory(&self) -> Result<()> {
        mkdir(&self.target)?;
        self.chmod_chown()?;
        Ok(())
    }

    /// Creates a symlink at [`target`][Self::target] pointing to
    /// [`source`][Self::source], then applies permissions and ownership.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    ///
    /// - parent directory cannot be created
    /// - source path cannot be canonicalized
    /// - symlink creation fails
    #[inline]
    pub fn symlink(&self) -> Result<()> {
        _ = file_util::mkdir(
            self.target
                .parent()
                .ok_or_eyre("Failed to get parent directory")?,
        );
        let Some(ref source) = self.source else {
            return Err(eyre!("Missing source"));
        };

        let source_path = if self.follow_symlinks.unwrap_or(true) {
            fs::canonicalize(source)?
        } else {
            path::absolute(source)?
        };

        symlink(&source_path, &self.target)?;
        info!(
            "Symlinked '{}' -> '{}'",
            source.display(),
            &self.target.display(),
        );

        self.chmod_chown()?;
        Ok(())
    }
}

/// Creates `path` as a directory, including any missing parent directories.
/// No-op if the directory already exists.
///
/// # Errors
///
/// Returns an error if:
/// - the path exists but is not a directory
/// - directory creation fails
#[inline]
pub fn mkdir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(_) => {
            fs::create_dir_all(path)?;
            info!("Created directory '{}'", path.display());
        }
        Ok(x) => {
            if !x.is_dir() {
                return Err(eyre!("File in way of '{}'", path.display()));
            }
            info!("Directory '{}' already exists", path.display());
        }
    }
    Ok(())
}

/// Renames the file at `path` to a prefixed name in the same parent directory,
/// backing it up. No-op if the path does not exist.
///
/// # Errors
///
/// Returns an error if:
/// - the path has no filename or parent component
/// - an existing file at the destination cannot be deleted
/// - the rename fails
#[inline]
pub fn prefix_move(path: &Path, prefix: &str) -> Result<()> {
    let Ok(_) = fs::symlink_metadata(path) else {
        return Ok(());
    };

    let mut appended_path = OsString::from(prefix);
    appended_path.push(path.file_name().ok_or_eyre(format!(
        "Failed to get file name of file '{}'",
        path.display()
    ))?);

    let new_path = path
        .parent()
        .ok_or_eyre(format!("Failed to get parent of file '{}'", path.display()))?
        .join(PathBuf::from(appended_path));

    if let Ok(metadata) = fs::symlink_metadata(&new_path) {
        delete(&new_path, &metadata)?;
    }

    fs::rename(path, &new_path)?;
    info!("Renaming '{}' -> '{}'", path.display(), new_path.display());
    Ok(())
}

/// Returns the BLAKE3 hash of the file at `filepath` using memory-mapped I/O,
/// or `None` if hashing fails.
#[must_use]
#[inline]
pub fn hash_file(filepath: &Path) -> Option<Hash> {
    let mut hasher = blake3::Hasher::new();

    if let Err(err) = hasher.update_mmap(filepath) {
        warn!("Failed to hash file: '{}'\n{:?}", filepath.display(), err);
        return None;
    }
    Some(hasher.finalize())
}

/// Removes the file or directory tree at `filepath`.
///
/// # Errors
///
/// Returns an error if filesystem removal fails.
#[inline]
pub fn delete(filepath: &Path, metadata: &Metadata) -> Result<()> {
    if metadata.is_dir() {
        fs::remove_dir_all(filepath)?;
    } else {
        fs::remove_file(filepath)?;
    }
    info!("Deleted '{}'", filepath.display());
    Ok(())
}

/// Fetches symlink metadata.
///
/// # Errors
///
/// Returns an error if the filesystem returns anything other than
/// `NotFound`. `NotFound` is silently treated as `None`.
#[inline]
pub fn get_metadata(path: &Path) -> Result<Option<Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).wrap_err("While getting metadata"),
    }
}

/// Returns `true` if `target` is a symlink whose link target no longer
/// exists (e.g. a garbage-collected Nix store path).
#[inline]
#[must_use]
pub fn is_dangling_symlink(target: &Path, metadata: &Metadata) -> bool {
    metadata.is_symlink()
        && fs::metadata(target).is_err_and(|err| err.kind() == ErrorKind::NotFound)
}

#[allow(clippy::restriction, clippy::pedantic)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::FileKind;

    fn make_file(kind: FileKind, target: PathBuf, source: Option<PathBuf>) -> File {
        File {
            source,
            target,
            kind,
            clobber: None,
            permissions: None,
            uid: None,
            gid: None,
            deactivate: None,
            follow_symlinks: None,
            ignore_modification: None,
        }
    }

    #[test]
    fn check_no_metadata_delete_returns_true() {
        assert!(
            make_file(FileKind::Delete, PathBuf::from("/x"), None)
                .check()
                .unwrap()
        );
    }

    #[test]
    fn check_no_metadata_non_delete_returns_false() {
        assert!(
            !make_file(FileKind::Copy, PathBuf::from("/x"), None)
                .check()
                .unwrap()
        );
    }

    #[test]
    fn check_metadata_present_delete_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        fs::write(&path, b"").unwrap();
        let f = make_file(FileKind::Delete, path, None);
        assert!(!f.check().unwrap());
    }

    #[test]
    fn mkdir_existing_directory_ok() {
        let dir = tempfile::tempdir().unwrap();
        mkdir(dir.path()).unwrap();
    }

    #[test]
    fn mkdir_file_in_way_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        fs::write(&path, b"").unwrap();
        assert!(mkdir(&path).is_err());
    }

    #[test]
    fn prefix_move_renames_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        fs::write(&path, b"").unwrap();
        prefix_move(&path, ".bak-").unwrap();
        assert!(!path.exists());
        assert!(dir.path().join(".bak-file").exists());
    }

    #[test]
    fn prefix_move_nonexistent_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        prefix_move(&dir.path().join("nonexistent"), ".bak-").unwrap();
    }
}
