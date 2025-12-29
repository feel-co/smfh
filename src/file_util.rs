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
use log::{
    debug,
    info,
    warn,
};
use manifest::{
    File,
    FileKind,
};
use rand::distr::{
    Alphanumeric,
    SampleString,
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
    },
};
pub struct FileWithMetadata {
    pub source: Option<PathBuf>,
    pub target: PathBuf,
    pub kind: FileKind,
    pub clobber: Option<bool>,

    pub permissions: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub deactivate: Option<bool>,
    pub follow_symlinks: Option<bool>,
    pub ignore_modification: Option<bool>,

    pub metadata: Option<Metadata>,
}

impl From<&File> for FileWithMetadata {
    fn from(file: &File) -> Self {
        Self {
            source: file.source.clone(),
            target: file.target.clone(),
            kind: file.kind,
            clobber: file.clobber,
            permissions: file.permissions,
            uid: file.uid,
            gid: file.gid,
            deactivate: file.deactivate,
            follow_symlinks: file.follow_symlinks,
            ignore_modification: file.ignore_modification,
            metadata: None,
        }
    }
}
impl FileWithMetadata {
    pub fn activate(&mut self, clobber_by_default: Option<bool>, prefix: &str) -> Result<()> {
        if self.check_source() {
            return Ok(());
        }

        self.set_metadata()?;

        let clobber = self
            .clobber
            .unwrap_or_else(|| clobber_by_default.unwrap_or(false));

        if clobber
            && self.metadata.is_some()
            && self
                .atomic_activate()
                .wrap_err("While attempting atomic activation")?
        {
            return Ok(());
        }

        if self.check().unwrap_or(false) {
            info!("File '{}' already correct", self.target.display());
            return Ok(());
        }

        if match *self {
            Self { metadata: None, .. }
            | Self {
                kind: FileKind::Modify | FileKind::Delete,
                ..
            } => false,
            // Don't clobber directories
            // If they're supposed to be
            // directories
            Self {
                kind: FileKind::Directory,
                metadata: Some(ref metadata),
                ..
            } => !metadata.is_dir(),
            _ => true,
        } {
            if clobber {
                delete(&self.target, self.metadata.as_ref().unwrap())?;
            } else {
                prefix_move(&self.target, prefix)?;
            }
        }

        match self.kind {
            FileKind::Directory => self.directory(),
            FileKind::Copy => self.copy(),
            FileKind::Symlink => self.symlink(),
            FileKind::Modify => self.chmod_chown(),
            FileKind::Delete => delete(&self.target, self.metadata.as_ref().unwrap()),
        }
    }

    pub fn atomic_activate(&mut self) -> Result<bool> {
        match self.kind {
            FileKind::Symlink | FileKind::Copy => {
                fn randomize_filename(file: &mut FileWithMetadata) -> Result<()> {
                    let string = Alphanumeric.sample_string(&mut rand::rng(), 16);
                    file.target.set_file_name(string);
                    if file.target.exists() {
                        randomize_filename(file)?;
                    }
                    Ok(())
                }

                let target_is_dir = self.metadata.as_ref().unwrap().is_dir();
                let source_is_dir = fs::symlink_metadata(self.source.as_ref().unwrap())?.is_dir();

                if target_is_dir != source_is_dir
                    || target_is_dir
                        && source_is_dir
                        && self.source.as_ref().unwrap().read_dir()?.next().is_some()
                {
                    return Ok(false);
                }

                let target = self.target.clone();

                if target.metadata().unwrap().permissions().readonly() {
                    return Err(eyre!("target file is unwriteable"));
                }

                randomize_filename(self)?;

                match self.kind {
                    FileKind::Symlink => self.symlink(),
                    FileKind::Copy => self.copy(),
                    _ => panic!("This should never happen"),
                }?;
                info!(
                    "Renaming '{}' -> '{}'",
                    &self.target.display(),
                    target.display()
                );
                fs::rename(&self.target, target)?;

                Ok(true)
            }
            _ => Ok(false),
        }
    }

    pub fn deactivate(&mut self) -> Result<()> {
        if !self.deactivate.unwrap_or(true) {
            return Ok(());
        }

        self.set_metadata()?;

        if self.metadata.is_none() {
            info!("File already deleted '{}'", self.target.display());
            return Ok(());
        }

        if !self.check()? {
            return Err(eyre!("File is not the same as expected"));
        }

        match self.kind {
            // no-op on deactivation
            FileKind::Delete | FileKind::Modify => Ok(()),
            // delete only if directory is empty
            FileKind::Directory => match self.metadata.as_ref() {
                Some(x) if x.is_dir() => {
                    fs::remove_dir(&self.target)?;
                    info!("Deleting directory '{}'", self.target.display());
                    Ok(())
                }
                Some(_) => Err(eyre!("File is not directory")),
                None => Err(eyre!("Cannot access file")),
            },
            // delete only if types match
            FileKind::Symlink | FileKind::Copy => {
                delete(&self.target, self.metadata.as_ref().unwrap())
            }
        }
    }

    pub fn check(&self) -> Result<bool> {
        match *self {
            Self {
                metadata: None,
                kind,
                ..
            } => Ok(kind == FileKind::Delete),
            Self {
                metadata: Some(_),
                kind: FileKind::Delete,
                ..
            } => Ok(false),
            // This should never happen
            // as it's checked before this
            // function is ever called
            Self {
                source: None,
                kind: FileKind::Symlink | FileKind::Copy,
                ref target,
                ..
            } => Err(eyre!("File '{}' missing_source", target.display())),
            Self {
                kind: FileKind::Copy | FileKind::Directory | FileKind::Modify,
                permissions: Some(perms),
                metadata: Some(ref metadata),
                ..
            } if perms != (metadata.mode() & 0o777) => Ok(false),
            Self {
                uid: Some(uid),
                metadata: Some(ref metadata),
                ..
            } if uid != metadata.uid() => Ok(false),
            Self {
                gid: Some(gid),
                metadata: Some(ref metadata),
                ..
            } if gid != metadata.gid() => Ok(false),

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
                    Ok(read_link(target)? == std::path::absolute(source)?)
                }
            }

            Self {
                kind: FileKind::Directory,
                metadata: Some(ref metadata),
                ..
            } => Ok(metadata.is_dir()),
            Self {
                kind: FileKind::Copy,
                metadata: Some(ref metadata),
                ..
            } if !metadata.is_file() => Ok(false),
            Self {
                kind: FileKind::Copy,
                ref target,
                source: Some(ref source),
                ref ignore_modification,
                ..
            } => {
                if ignore_modification.is_some_and(|x| x) {
                    return Ok(true);
                }

                if fs::symlink_metadata(target)?.len() != fs::symlink_metadata(source)?.len() {
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

    pub fn set_metadata(&mut self) -> Result<()> {
        match fs::symlink_metadata(&self.target) {
            Ok(metadata) => {
                self.metadata = Some(metadata);
                Ok(())
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {
                self.metadata = None;
                Ok(())
            }
            Err(err) => Err(err).wrap_err("While setting metadata"),
        }
    }
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

    pub fn chmod_chown(&mut self) -> Result<()> {
        self.set_metadata()?;
        let Some(metadata) = self.metadata.clone() else {
            return Err(eyre!(
                "Can't modify file '{}', file does not exist",
                self.target.display()
            ));
        };

        if self.kind != FileKind::Symlink {
            if let Some(x) = self.permissions {
                let new_perms = fs::Permissions::from_mode(x);

                if metadata.mode() & 0o777 == new_perms.mode() {
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
            self.set_metadata()?;
        }

        if self.uid.is_some() || self.uid.is_some() {
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

    pub fn symlink(&mut self) -> Result<()> {
        _ = file_util::mkdir(
            self.target
                .parent()
                .ok_or_eyre("Failed to get parent directory")?,
        );

        let source = if self.follow_symlinks.unwrap_or(true) {
            fs::canonicalize(self.source.as_ref().unwrap())?
        } else {
            path::absolute(self.source.as_ref().unwrap())?
        };

        symlink(&source, &self.target)?;
        info!(
            "Symlinked '{}' -> '{}'",
            source.display(),
            &self.target.display(),
        );

        self.set_metadata()?;
        self.chmod_chown()?;
        Ok(())
    }

    pub fn copy(&mut self) -> Result<()> {
        _ = file_util::mkdir(
            self.target
                .parent()
                .ok_or_eyre("Failed to get parent directory")?,
        );

        let source = fs::canonicalize(self.source.as_ref().unwrap())?;

        fs::copy(&source, &self.target)?;
        info!(
            "Copied '{}' -> '{}'",
            source.display(),
            &self.target.display(),
        );

        self.set_metadata()?;
        self.chmod_chown()?;
        Ok(())
    }

    pub fn directory(&mut self) -> Result<()> {
        mkdir(&self.target)?;
        self.set_metadata()?;
        self.chmod_chown()?;
        Ok(())
    }
}
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
            debug!("Directory '{}' already exists", path.display());
        }
    }
    Ok(())
}

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

pub fn hash_file(filepath: &Path) -> Option<Hash> {
    let mut hasher = blake3::Hasher::new();

    if let Err(err) = hasher.update_mmap(filepath) {
        warn!("Failed to hash file: '{}'\n{:?}", filepath.display(), err);
        return None;
    }
    Some(hasher.finalize())
}

pub fn delete(filepath: &Path, metadata: &Metadata) -> Result<()> {
    if metadata.is_dir() {
        fs::remove_dir_all(filepath)?;
    } else {
        fs::remove_file(filepath)?;
    }
    info!("Deleted '{}'", filepath.display());
    Ok(())
}
