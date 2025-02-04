use crate::{
    file_util,
    manifest::{
        File,
        FileKind,
    },
};
use color_eyre::{
    Result,
    eyre::{
        OptionExt,
        eyre,
    },
};
use jwalk::{
    DirEntry,
    WalkDir,
};
use log::{
    error,
    info,
    warn,
};
use std::{
    ffi::OsString,
    fs::{
        self,
        Metadata,
    },
    io::Read,
    os::unix::fs::{
        self as unixFs,
        MetadataExt,
        PermissionsExt,
    },
    path::{
        Path,
        PathBuf,
    },
};

impl File {
    pub fn activate(&mut self, clobber_by_default: bool, prefix: &str) -> Result<()> {
        if self.missing_source() {
            return Ok(());
        }

        self.set_metadata()?;

        let clobber = self.clobber.unwrap_or(clobber_by_default);

        if self.kind != FileKind::RecursiveSymlink {
            if self.check().unwrap_or(false) {
                info!("File '{}' already correct", self.target.display());
                return Ok(());
            }

            if self.metadata.is_some() && ![FileKind::Modify, FileKind::Delete].contains(&self.kind)
            {
                if !clobber {
                    prefix_move(&self.target, prefix)?;
                } else {
                    delete(&self.target, self.metadata.as_ref().unwrap())?;
                }
            }
        };

        match self.kind {
            FileKind::Directory => self.directory(),
            FileKind::RecursiveSymlink => self.recursive_symlink(prefix, clobber),
            FileKind::File => self.copy(),
            FileKind::Symlink => self.symlink(),
            FileKind::Modify => self.chmod_chown(),
            FileKind::Delete => delete(&self.target, self.metadata.as_ref().unwrap()),
        }
    }
    pub fn deactivate(&mut self) -> Result<()> {
        self.set_metadata()?;

        if self.metadata.is_none() {
            info!("File already deleted '{}'", self.target.display());
            return Ok(());
        }

        if self.kind != FileKind::RecursiveSymlink && !self.check()? {
            return Err(eyre!("File is not the same as expected"));
        };

        match self.kind {
            // no-op on deactivation
            FileKind::Delete | FileKind::Modify => Ok(()),
            // delete only if directory is empty
            FileKind::Directory => rmdir(&self.target),
            // this has it's own error handling
            FileKind::RecursiveSymlink => {
                self.recursive_cleanup();
                Ok(())
            }
            // delete only if types match
            FileKind::Symlink | FileKind::File => {
                delete(&self.target, self.metadata.as_ref().unwrap())
            }
        }
    }

    pub fn check(&self) -> Result<bool> {
        let metadata = match self.metadata.as_ref() {
            Some(x) => {
                if self.kind == FileKind::Delete {
                    return Ok(false);
                };
                x
            }
            None => {
                if self.kind == FileKind::Delete {
                    return Ok(true);
                }
                return Ok(false);
            }
        };

        if self.kind != FileKind::Symlink
            && self
                .permissions
                .is_some_and(|x| (metadata.mode() & 0o777) != x)
        {
            return Ok(false);
        }

        if self.uid.is_some_and(|x| x != metadata.uid())
            || self.gid.is_some_and(|x| x != metadata.gid())
        {
            return Ok(false);
        }

        match self.kind {
            FileKind::Symlink => {
                if !metadata.is_symlink() {
                    return Ok(false);
                };

                if fs::canonicalize(&self.target)?
                    != fs::canonicalize(self.source.as_ref().unwrap())?
                {
                    return Ok(false);
                };
            }
            FileKind::File => {
                if !metadata.is_file() {
                    return Ok(false);
                };

                let target_hash = match hash_file(&self.target) {
                    Ok(x) => x,
                    Err(e) => {
                        return Err(eyre!(
                            "Failed to hash file: '{}'\nReason: '{}'",
                            self.target.display(),
                            e
                        ));
                    }
                };
                let source_hash = match hash_file(self.source.as_ref().unwrap()) {
                    Ok(x) => x,
                    Err(e) => {
                        return Err(eyre!(
                            "Failed to hash file: '{}'\nReason: '{}'",
                            self.source.as_ref().unwrap().display(),
                            e
                        ));
                    }
                };

                if target_hash != source_hash {
                    return Ok(false);
                }
            }
            FileKind::Directory => {
                if !metadata.is_dir() {
                    return Ok(false);
                }
            }
            _ => {}
        };

        Ok(true)
    }

    pub fn set_metadata(&mut self) -> Result<()> {
        let metadata = fs::symlink_metadata(&self.target);
        if let Err(ref e) = metadata {
            if e.kind() == std::io::ErrorKind::NotFound {
                self.metadata = None;
                return Ok(());
            };
        }
        self.metadata = Some(metadata?);
        Ok(())
    }
    pub fn missing_source(&self) -> bool {
        let res = [
            FileKind::Symlink,
            FileKind::File,
            FileKind::RecursiveSymlink,
        ]
        .contains(&self.kind)
            && self.source.is_none();
        if res {
            warn!("File '{}' missing source", self.target.display())
        }
        res
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
                };
                info!(
                    "Setting permissions of: '{}' to: '{:o}'",
                    &self.target.display(),
                    new_perms.mode(),
                );

                //This doesn't work with symlinks
                fs::set_permissions(&self.target, new_perms)?
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
                self.uid.unwrap_or(metadata.uid()),
                self.gid.unwrap_or(metadata.gid()),
            );
            if metadata.is_symlink() {
                unixFs::lchown(&self.target, self.uid, self.gid)?;
            } else {
                unixFs::chown(&self.target, self.uid, self.gid)?;
            };
        }
        Ok(())
    }

    pub fn symlink(&mut self) -> Result<()> {
        let _ = file_util::mkdir(
            self.target
                .parent()
                .ok_or_eyre("Failed to get parent directory")?,
        );
        let source = fs::canonicalize(self.source.as_ref().unwrap())?;
        unixFs::symlink(&source, &self.target)?;
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
        let _ = file_util::mkdir(
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

    pub fn recursive_symlink(&self, prefix: &str, clobber: bool) -> Result<()> {
        pub fn handle_entry(
            file: &File,
            entry: &DirEntry<((), ())>,
            base_path: &Path,
            clobber: bool,
            prefix: &str,
        ) -> Result<()> {
            let target_file = &file.target.join(entry.path().strip_prefix(base_path)?);
            let metadata = fs::symlink_metadata(target_file);

            match metadata {
                Ok(x) => {
                    if entry.file_type().is_dir() && x.is_dir() {
                        return Ok(());
                    };

                    if fs::canonicalize(target_file)? == fs::canonicalize(entry.path())? {
                        return Ok(());
                    };

                    if clobber {
                        delete(target_file, &x)?;
                    } else {
                        prefix_move(target_file, prefix)?;
                    };
                }
                Err(e) => {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        return Err(eyre!("{}", e));
                    };
                }
            };

            if entry.file_type().is_dir() {
                mkdir(target_file)?;
                return Ok(());
            };

            unixFs::symlink(fs::canonicalize(entry.path())?, target_file)?;

            info!(
                "Symlinked '{}' -> '{}'",
                entry.path().display(),
                target_file.display(),
            );

            Ok(())
        }

        let base_path = self.source.as_ref().unwrap();
        let walkdir = WalkDir::new(base_path)
            .follow_links(true)
            .into_iter()
            .filter_map(|f| match f {
                Ok(x) => Some(x),
                Err(e) => {
                    error!(
                        "Recursive file walking error on base path: {}\n{}",
                        base_path.display(),
                        e
                    );
                    None
                }
            });

        for entry in walkdir {
            if let Err(e) = handle_entry(self, &entry, base_path, clobber, prefix) {
                error!(
                    "Failed to create file '{}'\nReason: {}",
                    entry.path().display(),
                    e
                )
            };
        }
        Ok(())
    }

    pub fn recursive_cleanup(&self) {
        pub fn handle_entry(
            file: &File,
            entry: &DirEntry<((), ())>,
            base_path: &Path,
            dirs: &mut Vec<(PathBuf, usize)>,
        ) -> Result<()> {
            let target_file = &file.target.join(entry.path().strip_prefix(base_path)?);

            let metadata = match fs::symlink_metadata(target_file) {
                Ok(x) => x,
                Err(e) => {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        return Err(eyre!("Error on file '{}', {}", target_file.display(), e));
                    };
                    return Ok(());
                }
            };

            if metadata.is_symlink() {
                if fs::canonicalize(target_file)? == fs::canonicalize(entry.path())? {
                    fs::remove_file(target_file)?;
                };
            } else if metadata.is_dir() && entry.file_type().is_dir() {
                dirs.push((target_file.clone(), entry.depth()));
                return Ok(());
            } else {
                info!(
                    "Ignoring file: '{}', in recursiveSymlink directory: '{}'",
                    &target_file.display(),
                    base_path.display()
                )
            }
            info!(
                "Deleting '{}' -> '{}'",
                entry.path().display(),
                target_file.display(),
            );
            Ok(())
        }

        let base_path = self.source.as_ref().unwrap();
        let walkdir = WalkDir::new(base_path)
            .follow_links(true)
            .into_iter()
            .filter_map(|f| match f {
                Ok(x) => Some(x),
                Err(e) => {
                    error!(
                        "Recursive file walking error on base path: '{}'\n{}",
                        base_path.display(),
                        e
                    );
                    None
                }
            });

        let mut dirs: Vec<(PathBuf, usize)> = vec![];

        for entry in walkdir {
            if let Err(e) = handle_entry(self, &entry, base_path, &mut dirs) {
                error!(
                    "Failed to remove file '{}'\nReason: {}",
                    entry.path().display(),
                    e
                )
            };
        }
        dirs.sort_by(|a, b| b.1.cmp(&a.1));
        dirs.into_iter().for_each(|dir| {
            if let Err(e) = rmdir(&dir.0) {
                error!(
                    "Didn't remove directory '{}' of recursiveSymlink '{}'\n Error: {}",
                    dir.0.display(),
                    base_path.display(),
                    e
                )
            };
        });
    }
}

pub fn mkdir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(_) => {
            fs::create_dir_all(path)?;
            info!("Created directory '{}'", path.display())
        }
        Ok(x) => {
            if !x.is_dir() {
                return Err(eyre!("File in way of '{}'", path.display()));
            } else {
                info!("Directory '{}' already exists", path.display());
            };
        }
    };
    Ok(())
}

pub fn prefix_move(path: &Path, prefix: &str) -> Result<()> {
    //TODO this is wrong (at least with relative paths)
    let Ok(_) = fs::symlink_metadata(path) else {
        return Ok(());
    };

    let mut appended_path = OsString::from(prefix);
    appended_path.push(path.file_name().ok_or_eyre(eyre!("test"))?);

    let new_path = PathBuf::from(appended_path);

    if fs::symlink_metadata(&new_path).is_ok() {
        prefix_move(&new_path, prefix)?
    };

    fs::rename(path, &new_path)?;
    info!("Renaming '{}' -> '{}'", path.display(), new_path.display());
    Ok(())
}

pub fn rmdir(path: &Path) -> Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if !metadata.is_dir() {
        return Err(eyre!("Path '{}' is not a directory", path.display()));
    }
    fs::remove_dir(path)?;
    info!("Deleting directory '{}'", path.display());
    Ok(())
}

pub fn hash_file(filepath: &Path) -> Result<u64> {
    let mut file = std::fs::File::open(filepath)?;
    let mut buffer = Vec::new();
    buffer.clear();
    file.read_to_end(&mut buffer)?;
    Ok(xxhash_rust::xxh3::xxh3_64(&buffer))
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
