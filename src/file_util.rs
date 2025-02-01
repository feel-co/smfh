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
};
use std::{
    ffi::OsString,
    fs::{
        self,
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
    pub fn check(&self) -> Result<bool> {
        let Some(metadata) = self.metadata.as_ref() else {
            if self.kind == FileKind::Delete {
                return Ok(true);
            } else {
                return Ok(false);
            }
        };

        if self.kind != FileKind::Chmod {
            if self.kind == FileKind::Symlink {
                if !metadata.is_symlink() {
                    return Ok(false);
                };

                if fs::canonicalize(&self.target)?
                    != fs::canonicalize(self.source.as_ref().unwrap())?
                {
                    return Ok(false);
                };
            } else if self.kind == FileKind::File {
                if !metadata.is_file() {
                    return Ok(false);
                };

                let Ok(target_hash) = hash_file(&self.target) else {
                    return Err(eyre!("Failed to hash target"));
                };
                let Ok(source_hash) = hash_file(self.source.as_ref().unwrap()) else {
                    return Err(eyre!("Failed to hash source"));
                };
                if target_hash != source_hash {
                    return Ok(false);
                }
            } else if [FileKind::Directory, FileKind::RecursiveSymlink].contains(&self.kind)
                && !metadata.is_dir()
            {
                return Ok(false);
            };
        };
        if self
            .permissions
            .is_some_and(|x| (metadata.mode() & 0o777) != x)
        {
            return Ok(false);
        };

        if self.uid.is_some_and(|x| x != metadata.uid())
            || self.gid.is_some_and(|x| x != metadata.gid())
        {
            return Ok(false);
        }

        //TODO: actually check here
        if self.kind == FileKind::RecursiveSymlink {
            return Ok(false);
        }

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
        [
            FileKind::Symlink,
            FileKind::File,
            FileKind::RecursiveSymlink,
        ]
        .contains(&self.kind)
            && self.source.is_none()
    }

    pub fn chmod_chown(&mut self) -> Result<()> {
        self.set_metadata()?;
        let Some(metadata) = self.metadata.clone() else {
            return Err(eyre!(
                "Can't modify file '{}', file does not exist",
                self.target.display()
            ));
        };

        if let Some(x) = self.permissions {
            let new_perms = fs::Permissions::from_mode(x);

            if dbg!(metadata.mode() & 0o777) == dbg!(new_perms.mode()) {
                return Ok(());
            };
            info!(
                "Setting permissions of: '{:o}' to: '{}'",
                new_perms.mode(),
                &self.target.display(),
            );

            fs::set_permissions(&self.target, new_perms)?
        }
        self.set_metadata()?;

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

    pub fn type_check_delete(&self) -> Result<()> {
        let metadata = self.metadata.as_ref().unwrap();

        if metadata.is_symlink() && self.kind == FileKind::Symlink {
            if fs::canonicalize(&self.target)? == fs::canonicalize(self.source.as_ref().unwrap())? {
                fs::remove_file(&self.target)?;
            };
        } else if metadata.is_file() && self.kind == FileKind::File {
            let Ok(target_hash) = hash_file(&self.target) else {
                return Err(eyre!("Failed to hash target"));
            };
            let Ok(source_hash) = hash_file(self.source.as_ref().unwrap()) else {
                return Err(eyre!("Failed to hash source"));
            };
            if target_hash == source_hash {
                fs::remove_file(&self.target)?;
            }
        } else {
            return Err(eyre!("File is not symlink, directory, or file"));
        };
        Ok(())
    }

    pub fn recursive_symlink(&self, prefix: &str, clobber: bool) -> Result<()> {
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
            let target_file = &self.target.join(entry.path().strip_prefix(base_path)?);

            if entry.file_type().is_dir() {
                mkdir(target_file)?;
                continue;
            }

            delete_or_move(target_file, prefix, clobber)?;
            unixFs::symlink(fs::canonicalize(entry.path())?, target_file)?;

            info!(
                "Symlinked '{}' -> '{}'",
                entry.path().display(),
                target_file.display(),
            );
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
            let path = entry.path();
            let target_file = file.target.join(path.strip_prefix(base_path)?);

            let metadata = fs::symlink_metadata(&target_file)?;

            if metadata.is_symlink() && entry.file_type().is_file() {
                if fs::canonicalize(&target_file)? == fs::canonicalize(entry.path())? {
                    fs::remove_file(&target_file)?;
                };
            } else if metadata.is_file() {
                info!(
                    "Ignoring file in recursiveSymlink directory: '{}'",
                    &target_file.display()
                )
            } else if metadata.is_dir() && entry.file_type().is_dir() {
                dirs.push((target_file.clone(), entry.depth()));
                // Don't log on directories
                // they'll be listed on the rmdir call
                return Ok(());
            } else {
                return Err(eyre!("File is not symlink, directory, or file"));
            };

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

pub fn delete_if_exists(path: &Path) -> Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.is_file() || metadata.is_symlink() {
        fs::remove_file(path)?;
        info!("Deleted '{}'", path.display());
    } else {
        fs::remove_dir_all(path)?;
    }
    Ok(())
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

pub fn delete_or_move(path: &Path, prefix: &str, clobber: bool) -> Result<()> {
    match clobber {
        true => delete_if_exists(path)?,
        false => prefix_move(path, prefix)?,
    }
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

// TODO: specify where error that come from here come from
pub fn hash_file(filepath: &Path) -> Result<u64> {
    let mut file = std::fs::File::open(filepath)?;
    let mut buffer = Vec::new();
    buffer.clear();
    file.read_to_end(&mut buffer)?;
    Ok(xxhash_rust::xxh3::xxh3_64(&buffer))
}
