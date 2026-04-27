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
    /// One or more files failed to activate or deactivate. Each entry is the
    /// target path and the formatted error. Returned instead of `Ok(())` so
    /// the manifest rename is skipped and the next run can retry.
    ActivationFailed(Vec<(PathBuf, String)>),
    Other(color_eyre::Report),
}

impl Display for DiffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OldManifestMissing => write!(f, "old manifest does not exist"),
            Self::OldManifestRead(e) => write!(f, "{e}"),
            Self::ActivationFailed(failures) => {
                write!(
                    f,
                    "{} file(s) failed to activate/deactivate:",
                    failures.len()
                )?;
                for (path, err) in failures {
                    write!(f, "\n  {}: {err}", path.display())?;
                }
                Ok(())
            }
            Self::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for DiffError {}

/// A single validation violation found by [`Manifest::verify`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Violation {
    MissingSource,
    UnexpectedSource,
    UnexpectedFollowSymlinks,
    UnexpectedIgnoreModification,
}

/// Error returned by [`Manifest::verify`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyError {
    pub target: PathBuf,
    pub kind: FileKind,
    pub violation: Violation,
}

impl Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self.violation {
            Violation::MissingSource => "requires a source",
            Violation::UnexpectedSource => "should not have a source",
            Violation::UnexpectedFollowSymlinks => "should not have follow_symlinks",
            Violation::UnexpectedIgnoreModification => "should not have ignore_modification",
        };
        write!(
            f,
            "file '{}' of type '{}' {msg}",
            self.target.display(),
            self.kind
        )
    }
}

impl std::error::Error for VerifyError {}

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

#[allow(clippy::ref_option, clippy::trivially_copy_pass_by_ref)]
fn is_false(t: &Option<bool>) -> bool {
    t.is_none_or(|x| !x)
}
#[allow(clippy::ref_option, clippy::trivially_copy_pass_by_ref)]
fn is_true(t: &Option<bool>) -> bool {
    t.is_none_or(|x| x)
}

/// Deserialized representation of a smfh manifest file.
#[derive(Serialize, Deserialize, Debug)]
pub struct Manifest {
    pub files: Vec<File>,
    #[serde(skip_serializing_if = "is_false")]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<PathBuf>,
    pub target: PathBuf,
    #[serde(rename = "type")]
    pub kind: FileKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clobber: Option<bool>,
    #[serde(
        default,
        deserialize_with = "deserialize_octal",
        skip_serializing_if = "Option::is_none"
    )]
    pub permissions: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,
    #[serde(skip_serializing_if = "is_true")]
    pub deactivate: Option<bool>,
    #[serde(skip_serializing_if = "is_true")]
    pub follow_symlinks: Option<bool>,
    #[serde(skip_serializing_if = "is_false")]
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

    /// Verifies that every file entry complies with the manifest spec.
    ///
    /// # Errors
    ///
    /// Returns a [`VerifyError`] if:
    ///
    /// - [`VerifyError::MissingSource`]: a `Copy` or `Symlink` file has no
    ///   `source`
    /// - [`VerifyError::UnexpectedSource`]: a `Delete`, `Directory`, or
    ///   `Modify` file has a `source`
    /// - [`VerifyError::UnexpectedFollowSymlinks`]: a non-`Symlink` file has
    ///   `follow_symlinks` set
    /// - [`VerifyError::UnexpectedIgnoreModification`]: a non-`Copy` file has
    ///   `ignore_modification` set
    #[must_use]
    pub fn verify(&self) -> Vec<VerifyError> {
        let mut errors = Vec::new();
        for file in &self.files {
            match file.kind {
                FileKind::Copy | FileKind::Symlink if file.source.is_none() => {
                    errors.push(VerifyError {
                        target: file.target.clone(),
                        kind: file.kind,
                        violation: Violation::MissingSource,
                    });
                }
                FileKind::Delete | FileKind::Directory | FileKind::Modify
                    if file.source.is_some() =>
                {
                    errors.push(VerifyError {
                        target: file.target.clone(),
                        kind: file.kind,
                        violation: Violation::UnexpectedSource,
                    });
                }
                _ => {}
            }

            if file.follow_symlinks.is_some() && file.kind != FileKind::Symlink {
                errors.push(VerifyError {
                    target: file.target.clone(),
                    kind: file.kind,
                    violation: Violation::UnexpectedFollowSymlinks,
                });
            }

            if file.ignore_modification.is_some()
                && !matches!(file.kind, FileKind::Copy | FileKind::Symlink)
            {
                errors.push(VerifyError {
                    target: file.target.clone(),
                    kind: file.kind,
                    violation: Violation::UnexpectedIgnoreModification,
                });
            }
        }
        errors
    }

    /// Activates every file in the manifest, applying them to the filesystem in
    /// dependency order. Returns per-file failures; the caller decides whether
    /// any failure is fatal.
    pub fn activate(&mut self, prefix: &str) -> Vec<(PathBuf, color_eyre::Report)> {
        self.files.sort();
        let mut failures = Vec::new();
        for mut file in self.files.iter().map(FileWithMetadata::from) {
            if let Err(err) = file.activate(self.clobber_by_default, prefix) {
                error!(
                    "Failed to activate file: '{}'\n{:?}",
                    file.target.display(),
                    err
                );
                failures.push((file.target.clone(), err));
            }
        }
        failures
    }

    /// Removes every file in the manifest from the filesystem in reverse
    /// dependency order. Returns per-file failures; the caller decides whether
    /// any failure is fatal.
    pub fn deactivate(&mut self) -> Vec<(PathBuf, color_eyre::Report)> {
        self.files.sort();
        let mut failures = Vec::new();
        for mut file in self.files.iter().map(FileWithMetadata::from).rev() {
            if let Err(err) = file.deactivate() {
                error!(
                    "Failed to deactivate file: '{}'\n{:?}",
                    file.target.display(),
                    err
                );
                failures.push((file.target.clone(), err));
            }
        }
        failures
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
    #[allow(clippy::too_many_lines)]
    pub fn diff(mut self, old_path: &Path, prefix: &str, fallback: bool) -> Result<(), DiffError> {
        let mut old_manifest = match old_path.try_exists() {
            Ok(true) => Self::read(old_path, self.impure).map_err(DiffError::OldManifestRead)?,
            Ok(false) if fallback => {
                let failures = self.activate(prefix);
                return if failures.is_empty() {
                    Ok(())
                } else {
                    Err(DiffError::ActivationFailed(
                        failures
                            .into_iter()
                            .map(|(p, e)| (p, format!("{e:?}")))
                            .collect(),
                    ))
                };
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
        let mut failures: Vec<(PathBuf, String)> = old_manifest
            .deactivate()
            .into_iter()
            .map(|(p, e)| (p, format!("{e:?}")))
            .collect();

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
        failures.extend(
            self.activate(prefix)
                .into_iter()
                .map(|(p, e)| (p, format!("{e:?}"))),
        );
        if failures.is_empty() {
            Ok(())
        } else {
            Err(DiffError::ActivationFailed(failures))
        }
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

    fn manifest_with(files: Vec<File>) -> Manifest {
        Manifest {
            files,
            clobber_by_default: None,
            version: 3,
            impure: false,
        }
    }

    #[test]
    fn verify_rejects_missing_source_for_copy() {
        let errors = manifest_with(vec![file(FileKind::Copy, "/a")]).verify();
        assert_eq!(
            errors,
            vec![VerifyError {
                target: PathBuf::from("/a"),
                kind: FileKind::Copy,
                violation: Violation::MissingSource,
            }]
        );
    }

    #[test]
    fn verify_rejects_missing_source_for_symlink() {
        let errors = manifest_with(vec![file(FileKind::Symlink, "/a")]).verify();
        assert_eq!(
            errors,
            vec![VerifyError {
                target: PathBuf::from("/a"),
                kind: FileKind::Symlink,
                violation: Violation::MissingSource,
            }]
        );
    }

    #[test]
    fn verify_rejects_unexpected_source_for_delete() {
        let mut f = file(FileKind::Delete, "/a");
        f.source = Some(PathBuf::from("/b"));
        let errors = manifest_with(vec![f]).verify();
        assert_eq!(
            errors,
            vec![VerifyError {
                target: PathBuf::from("/a"),
                kind: FileKind::Delete,
                violation: Violation::UnexpectedSource,
            }]
        );
    }

    #[test]
    fn verify_rejects_unexpected_follow_symlinks_for_copy() {
        let mut f = file(FileKind::Copy, "/a");
        f.source = Some(PathBuf::from("/b"));
        f.follow_symlinks = Some(true);
        let errors = manifest_with(vec![f]).verify();
        assert_eq!(
            errors,
            vec![VerifyError {
                target: PathBuf::from("/a"),
                kind: FileKind::Copy,
                violation: Violation::UnexpectedFollowSymlinks,
            }]
        );
    }

    #[test]
    fn verify_rejects_unexpected_ignore_modification_for_directoy() {
        let mut f = file(FileKind::Directory, "/a");
        f.ignore_modification = Some(true);
        let errors = manifest_with(vec![f]).verify();
        assert_eq!(
            errors,
            vec![VerifyError {
                target: PathBuf::from("/a"),
                kind: FileKind::Directory,
                violation: Violation::UnexpectedIgnoreModification,
            }]
        );
    }

    #[test]
    fn verify_accepts_valid_manifest() {
        let mut copy = file(FileKind::Copy, "/a");
        copy.source = Some(PathBuf::from("/b"));
        let mut symlink = file(FileKind::Symlink, "/c");
        symlink.source = Some(PathBuf::from("/d"));
        assert!(
            manifest_with(vec![copy, symlink, file(FileKind::Delete, "/e")])
                .verify()
                .is_empty()
        );
    }

    #[test]
    fn verify_reports_all_errors() {
        let mut copy = file(FileKind::Copy, "/a");
        copy.follow_symlinks = Some(true);
        let symlink = file(FileKind::Symlink, "/b");
        let mut delete = file(FileKind::Delete, "/c");
        delete.source = Some(PathBuf::from("/d"));
        let errors = manifest_with(vec![copy, symlink, delete]).verify();
        assert_eq!(errors.len(), 4);
        assert!(errors.contains(&VerifyError {
            target: PathBuf::from("/a"),
            kind: FileKind::Copy,
            violation: Violation::MissingSource,
        }));
        assert!(errors.contains(&VerifyError {
            target: PathBuf::from("/a"),
            kind: FileKind::Copy,
            violation: Violation::UnexpectedFollowSymlinks,
        }));
        assert!(errors.contains(&VerifyError {
            target: PathBuf::from("/b"),
            kind: FileKind::Symlink,
            violation: Violation::MissingSource,
        }));
        assert!(errors.contains(&VerifyError {
            target: PathBuf::from("/c"),
            kind: FileKind::Delete,
            violation: Violation::UnexpectedSource,
        }));
    }
}
