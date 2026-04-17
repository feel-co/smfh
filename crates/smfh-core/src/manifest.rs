use crate::{
    VERSION,
    file_util::{
        FileWithMetadata,
        prefix_move,
    },
};
use color_eyre::{
    Result,
    eyre::{
        Context as _,
        OptionExt as _,
        eyre,
    },
};
use core::{
    cmp::Ordering,
    fmt::{
        self,
        Display,
    },
};

/// Error returned by [`Manifest::read`].
#[derive(Debug)]
pub enum ReadError {
    VersionTooNew { manifest: u64 },
    ExpandFailed(color_eyre::Report),
    Io(color_eyre::Report),
}

impl Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VersionTooNew { manifest } => write!(
                f,
                "manifest version too new: program {VERSION}, manifest {manifest}"
            ),
            Self::ExpandFailed(e) | Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ReadError {}

/// Error returned by [`Manifest::diff`].
#[derive(Debug)]
pub enum DiffError {
    OldManifestMissing,
    OldManifestRead(ReadError),
    Other(color_eyre::Report),
}

impl Display for DiffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OldManifestMissing => write!(f, "old manifest does not exist"),
            Self::OldManifestRead(e) => write!(f, "{e}"),
            Self::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for DiffError {}
use log::{
    error,
    info,
    warn,
};
use serde::{
    Deserialize,
    Deserializer,
    Serialize,
    de::Error as serdeErr,
};
use serde_json::Value;
use shellexpand::path::full as shellexpand;
use std::{
    fs::{
        self,
    },
    io::BufReader,
    path::{
        Component,
        Path,
        PathBuf,
    },
};

/// Deserialized representation of a smfh manifest file.
#[derive(Serialize, Deserialize, Debug)]
pub struct Manifest {
    pub files: Vec<File>,
    pub clobber_by_default: Option<bool>,
    pub version: u64,
    #[serde(skip)]
    impure: bool,
}

fn deserialize_octal<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u32>, D::Error> {
    let deserialized_value = Option::<String>::deserialize(deserializer)?;
    let Some(value) = deserialized_value else {
        // Don't error here because it's null!
        return Ok(None);
    };
    let x = u32::from_str_radix(&value, 8).map_err(serdeErr::custom)?;
    Ok(Some(x))
}

/// A single file entry in a [`Manifest`].
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct File {
    pub source: Option<PathBuf>,
    pub target: PathBuf,
    #[serde(rename = "type")]
    pub kind: FileKind,
    pub clobber: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_octal")]
    pub permissions: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub deactivate: Option<bool>,
    pub follow_symlinks: Option<bool>,
    pub ignore_modification: Option<bool>,
}

impl Ord for File {
    fn cmp(&self, other: &Self) -> Ordering {
        const fn value(file: &File) -> u8 {
            match file.kind {
                FileKind::Directory => 1,
                FileKind::Copy => 2,
                FileKind::Symlink => 3,
                FileKind::Modify => 4,
                FileKind::Delete => 5,
            }
        }

        if other.kind == self.kind {
            fn parents(path: &Path) -> usize {
                path.ancestors().count()
            }
            parents(&self.target).cmp(&parents(&other.target))
        } else {
            value(self).cmp(&value(other))
        }
    }
}

impl PartialOrd for File {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The operation smfh performs for a given [`File`].
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum FileKind {
    Directory,
    Copy,
    Symlink,
    Modify,
    Delete,
}
impl fmt::Display for FileKind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let name = match *self {
            Self::Copy => "copy",
            Self::Delete => "delete",
            Self::Directory => "directory",
            Self::Modify => "modify",
            Self::Symlink => "symlink",
        };
        write!(f, "{name}")
    }
}

impl Manifest {
    /// Reads and deserializes a manifest from `manifest_path`. In impure mode,
    /// shell-expands all paths; otherwise discards any entry whose path is
    /// not absolute.
    ///
    /// # Errors
    ///
    /// Returns a [`ReadError`] if:
    /// - [`ReadError::VersionTooNew`]: the manifest version exceeds [`VERSION`]
    /// - [`ReadError::Io`]: the file cannot be opened or deserialized
    /// - [`ReadError::ExpandFailed`]: shell expansion of a path fails (impure
    ///   mode only)
    pub fn read(manifest_path: &Path, impure: bool) -> Result<Self, ReadError> {
        let file = fs::File::open(manifest_path)
            .wrap_err("Failed to open manifest")
            .map_err(ReadError::Io)?;
        let root: Value = serde_json::from_reader(BufReader::new(&file))
            .wrap_err("Failed to deserialize manifest")
            .map_err(ReadError::Io)?;
        let version = root
            .get("version")
            .ok_or_eyre("Failed to get version from manifest")
            .map_err(ReadError::Io)?;

        let manifest_version = version
            .as_u64()
            .ok_or_else(|| ReadError::Io(eyre!("manifest version is not a valid integer")))?;

        if manifest_version > VERSION {
            return Err(ReadError::VersionTooNew {
                manifest: manifest_version,
            });
        }

        let mut manifest: Self = serde_json::from_value(root)
            .wrap_err("Failed to deserialize manifest")
            .map_err(ReadError::Io)?;

        info!("Deserialized manifest: '{}'", manifest_path.display());

        if !cfg!(debug_assertions) && !impure {
            manifest.files.retain(|file| {
                let absolute = file.target.is_absolute()
                    && !file.target.components().any(|x| x == Component::ParentDir)
                    && file.source.as_ref().is_none_or(|x| x.is_absolute());
                if !absolute {
                    warn!(
                        "{} with target '{}' is not absolute, ignoring.",
                        file.kind,
                        file.target.display()
                    );
                }
                absolute
            });
        } else if impure {
            fn expand(path_buf: &PathBuf) -> Result<PathBuf> {
                Ok(shellexpand(path_buf)
                    .map_err(|err| eyre!("{err:?}"))?
                    .to_path_buf())
            }
            for file in &mut manifest.files {
                if let Some(ref src) = file.source.clone() {
                    file.source = Some(expand(src).map_err(ReadError::ExpandFailed)?);
                }
                file.target = expand(&file.target.clone()).map_err(ReadError::ExpandFailed)?;
            }
        }

        manifest.impure = impure;
        Ok(manifest)
    }

    /// Activates every file in the manifest, applying them to the filesystem in
    /// dependency order.
    pub fn activate(&mut self, prefix: &str) {
        self.files.sort();
        for mut file in self.files.iter().map(FileWithMetadata::from) {
            _ = file
                .activate(self.clobber_by_default, prefix)
                .inspect_err(|err| {
                    error!(
                        "Failed to activate file: '{}'\n{:?}",
                        file.target.display(),
                        err
                    );
                });
        }
    }

    /// Removes every file in the manifest from the filesystem in reverse
    /// dependency order.
    pub fn deactivate(&mut self) {
        self.files.sort();
        for mut file in self.files.iter().map(FileWithMetadata::from).rev() {
            _ = file.deactivate().inspect_err(|err| {
                error!(
                    "Failed to deactivate file: '{}'\n{:?}",
                    file.target.display(),
                    err
                );
            });
        }
    }

    /// Brings the filesystem from the state described by the manifest at
    /// `old_path` to the state described by `self`. Files removed from the
    /// new manifest are deactivated; files added or updated are
    /// (re-)activated. If `fallback` is `true` and no old manifest exists,
    /// falls back to a full activation.
    ///
    /// # Errors
    ///
    /// Returns a [`DiffError`] if:
    /// - [`DiffError::OldManifestMissing`]: the old manifest does not exist and
    ///   `fallback` is `false`
    /// - [`DiffError::OldManifestRead`]: the old manifest exists but cannot be
    ///   read
    /// - [`DiffError::Other`]: probing the old manifest path fails
    pub fn diff(mut self, old_path: &Path, prefix: &str, fallback: bool) -> Result<(), DiffError> {
        let mut old_manifest = match old_path.try_exists() {
            Ok(true) => Self::read(old_path, self.impure).map_err(DiffError::OldManifestRead)?,
            Ok(false) if fallback => {
                self.activate(prefix);
                return Ok(());
            }
            Ok(false) => return Err(DiffError::OldManifestMissing),
            Err(err) => return Err(DiffError::Other(color_eyre::Report::from(err))),
        };

        let mut updated_files: Vec<(File, File)> = vec![];
        let mut same_files: Vec<File> = vec![];

        old_manifest.files.retain(|file| {
            if let Some(index) = self.files.iter().position(|inner| inner == file) {
                same_files.push(self.files.swap_remove(index));
                false
            } else if let Some(index) = self.files.iter().position(|inner| {
                matches!(inner.clone(), File {
                    kind: FileKind::Symlink | FileKind::Copy,
                   target,
                    ..
                } if (target == file.target))
            }) {
                updated_files.push((file.clone(), self.files.swap_remove(index)));
                false
            } else {
                true
            }
        });

        // Remove files in old manifest
        // which aren't in new manifest
        old_manifest.deactivate();

        for (old, new) in updated_files {
            if !old
                .clobber
                .unwrap_or_else(|| old_manifest.clobber_by_default.unwrap_or(false))
            {
                let mut file = FileWithMetadata::from(&old);

                // Don't care if this errors
                // metadata will just be none
                if let Err(err) = file.set_metadata() {
                    warn!(
                        "Failed to get metadata for file '{}'\n{:?}",
                        file.target.display(),
                        err
                    );
                }

                if file.metadata.is_some()
                    && !file
                        .check()
                        .inspect_err(|err| {
                            warn!(
                                "Failed to check file: '{}', assuming file is incorrect\n{:?}",
                                file.target.display(),
                                err
                            );
                        })
                        .unwrap_or(false)
                {
                    if let Err(err) = prefix_move(&file.target, prefix) {
                        warn!(
                            "Failed to backup file '{}'\n{:?}",
                            file.target.display(),
                            err
                        );
                    }
                    // if file existed but was wrong,
                    // atomic action cannot be taken
                    // so there's no point of forcing clobber

                    // except this double checks
                    self.files.push(new.clone());
                    continue;
                }
            }

            let mut atomic = FileWithMetadata::from(&new.clone());

            if let Err(err) = atomic.set_metadata() {
                warn!(
                    "Failed to get metadata for file '{}'\n{:?}",
                    atomic.target.display(),
                    err
                );
                continue;
            }

            if atomic.metadata.is_none() {
                self.files.push(new);
                continue;
            }

            let res = atomic.atomic_activate().inspect_err(|err| {
                error!(
                    "Failed to (atomic) activate file: '{}'\n{:?}",
                    new.target.display(),
                    err
                );
            });
            if !res.unwrap_or(false) {
                self.files.push(new);
            }
        }

        // These files could technically just be
        // Verified
        self.files.append(&mut same_files);
        // Activate new files
        self.activate(prefix);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write as _,
        path::PathBuf,
    };

    fn file(kind: FileKind, target: &str) -> File {
        File {
            source: None,
            target: PathBuf::from(target),
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

    fn write_manifest(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "{content}").unwrap();
        f
    }

    #[test]
    fn read_rejects_future_version() {
        let f = write_manifest(r#"{"files":[],"version":9999}"#);
        assert!(matches!(
            Manifest::read(f.path(), false),
            Err(ReadError::VersionTooNew { manifest: 9999 })
        ));
    }

    #[test]
    fn read_valid_empty_manifest() {
        let f = write_manifest(r#"{"files":[],"version":3}"#);
        let m = Manifest::read(f.path(), false).unwrap();
        assert!(m.files.is_empty());
        assert_eq!(m.version, 3);
    }

    #[test]
    fn read_parses_octal_permissions() {
        let f = write_manifest(
            r#"{"files":[{"type":"directory","target":"/tmp/x","permissions":"755"}],"version":3}"#,
        );
        let m = Manifest::read(f.path(), false).unwrap();
        assert_eq!(m.files[0].permissions, Some(0o755));
    }

    #[test]
    fn read_null_permissions_is_none() {
        let f = write_manifest(
            r#"{"files":[{"type":"directory","target":"/tmp/x","permissions":null}],"version":3}"#,
        );
        let m = Manifest::read(f.path(), false).unwrap();
        assert_eq!(m.files[0].permissions, None);
    }

    #[test]
    fn file_ordering_by_kind() {
        let dir = file(FileKind::Directory, "/a");
        let copy = file(FileKind::Copy, "/a");
        let sym = file(FileKind::Symlink, "/a");
        let modify = file(FileKind::Modify, "/a");
        let del = file(FileKind::Delete, "/a");

        assert!(dir < copy);
        assert!(copy < sym);
        assert!(sym < modify);
        assert!(modify < del);
    }

    #[test]
    fn file_ordering_same_kind_by_depth() {
        let shallow = file(FileKind::Copy, "/a/b");
        let deep = file(FileKind::Copy, "/a/b/c");
        assert!(shallow < deep);
    }
}
