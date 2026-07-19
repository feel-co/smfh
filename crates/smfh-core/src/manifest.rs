use crate::{
    VERSION,
    file_util::{
        get_metadata,
        is_dangling_symlink,
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
    error::Error,
    fmt::{
        self,
        Display,
    },
};

/// Error returned by [`Manifest::read`].
#[derive(Debug)]
pub enum ReadError {
    ExpandFailed(color_eyre::Report),
    Io(color_eyre::Report),
    VersionTooNew { manifest: u64 },
}

impl Display for ReadError {
    #[inline]
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

impl Error for ReadError {}

/// Error returned by [`Manifest::diff`].
#[derive(Debug)]
pub enum DiffError {
    OldManifestMissing,
    OldManifestRead(ReadError),
    /// Existing targets that differ from the old manifest while clobbering is
    /// disabled.
    ProtectedTargets(Vec<PathBuf>),
    /// One or more files failed to activate or deactivate. Each entry is the
    /// target path and the formatted error. Returned instead of `Ok(())` so
    /// the manifest rename is skipped and the next run can retry.
    ActivationFailed(Vec<(PathBuf, String)>),
    Other(color_eyre::Report),
}

impl Display for DiffError {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OldManifestMissing => write!(f, "old manifest does not exist"),
            Self::OldManifestRead(e) => write!(f, "{e}"),
            Self::ProtectedTargets(targets) => {
                write!(
                    f,
                    "{} target(s) differ from the old manifest while clobber is disabled:",
                    targets.len()
                )?;
                for target in targets {
                    write!(f, "\n  {}", target.display())?;
                }
                Ok(())
            }
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

impl Error for DiffError {}

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
#[non_exhaustive]
pub struct VerifyError {
    pub target: PathBuf,
    pub kind: FileKind,
    pub violation: Violation,
}

impl Display for VerifyError {
    #[inline]
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

impl Error for VerifyError {}

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
    collections::HashMap,
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

#[expect(clippy::ref_option, clippy::trivially_copy_pass_by_ref)]
fn is_false(t: &Option<bool>) -> bool {
    t.is_none_or(|x| !x)
}
#[expect(clippy::ref_option, clippy::trivially_copy_pass_by_ref)]
fn is_true(t: &Option<bool>) -> bool {
    t.is_none_or(|x| x)
}

trait Merge {
    fn merge(&mut self, other: &Self);
}

impl<T: Clone> Merge for Option<T> {
    fn merge(&mut self, other: &Self) {
        if other.is_some() {
            self.clone_from(other);
        }
    }
}
#[inline]
/// Merges a list of [`manifest's`][Manifest] [`.file`][File] entries.
/// Right hand side overrides left
/// Not a deep merge only entire entries are overridden .
///
/// # Errors
///
///  Returns an error if:
///
///  - The Vec passed is empty
pub fn merge_files_from_manifests(manifests: Vec<Manifest>) -> Result<Manifest> {
    let right_most: &mut Manifest =
        &mut manifests.last().ok_or_eyre("No manifests passed!")?.clone();
    let mut map: HashMap<String, File> = HashMap::new();

    for m in manifests {
        for f in m.files {
            let key = format!("{}-{}", f.kind, f.target.to_string_lossy());
            map.entry(key)
                .and_modify(|inner_file| {
                    inner_file.source.merge(&f.source);
                    inner_file.clobber.merge(&f.clobber);
                    inner_file.permissions.merge(&f.permissions);
                    inner_file.uid.merge(&f.uid);
                    inner_file.gid.merge(&f.gid);
                    inner_file.deactivate.merge(&f.deactivate);
                    inner_file.follow_symlinks.merge(&f.follow_symlinks);
                    inner_file.ignore_modification.merge(&f.ignore_modification);
                })
                .or_insert(f.clone());
        }
    }
    right_most.files = map.into_values().collect();
    Ok(right_most.to_owned())
}

/// Deserialized representation of a smfh manifest file.
#[derive(Serialize, Deserialize, Debug, Clone)]
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
///
/// Files are ordered by [`kind`](Self::kind) and then by path depth
/// (shallowest first). See the [`Ord`] implementation for details.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
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
    #[inline]
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
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The operation smfh performs for a given [`File`].
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum FileKind {
    Copy,
    Delete,
    Directory,
    Modify,
    Symlink,
}
impl fmt::Display for FileKind {
    #[inline]
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
    #[inline]
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
            manifest.files.retain(|f| {
                let absolute = f.target.is_absolute()
                    && !f.target.components().any(|x| x == Component::ParentDir)
                    && f.source.as_ref().is_none_or(|x| x.is_absolute());
                if !absolute {
                    warn!(
                        "{} with target '{}' is not absolute, ignoring.",
                        f.kind,
                        f.target.display()
                    );
                }
                absolute
            });
        } else if impure {
            fn expand(path_buf: &PathBuf) -> Result<PathBuf> {
                return Ok(shellexpand(path_buf)
                    .map_err(|err| eyre!("{err:?}"))?
                    .to_path_buf());
            }
            for f in &mut manifest.files {
                if let Some(ref src) = f.source.clone() {
                    f.source = Some(expand(src).map_err(ReadError::ExpandFailed)?);
                }
                f.target = expand(&f.target.clone()).map_err(ReadError::ExpandFailed)?;
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
    /// - [`Violation::MissingSource`]: a `Copy` or `Symlink` file has no
    ///   `source`
    /// - [`Violation::UnexpectedSource`]: a `Delete`, `Directory`, or `Modify`
    ///   file has a `source`
    /// - [`Violation::UnexpectedFollowSymlinks`]: a non-`Symlink` file has
    ///   `follow_symlinks` set
    /// - [`Violation::UnexpectedIgnoreModification`]: a non-`Copy` file has
    ///   `ignore_modification` set
    #[must_use]
    #[inline]
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
    ///
    /// `prefix` is used when backing up existing files that would be
    /// overwritten. See [`prefix_move`].
    #[inline]
    pub fn activate(&mut self, prefix: &str) -> Vec<(PathBuf, color_eyre::Report)> {
        self.files.sort();
        let mut failures = Vec::new();
        for file in &mut self.files {
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
    /// dependency order (deletes first, then modifies, symlinks, copies, and
    /// finally directories). Returns per-file failures; the caller decides
    /// whether any failure is fatal.
    #[inline]
    pub fn deactivate(&mut self) -> Vec<(PathBuf, color_eyre::Report)> {
        self.files.sort();
        let mut failures = Vec::new();
        for file in self.files.iter_mut().rev() {
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
    /// - [`DiffError::ProtectedTargets`]: an existing target differs from the
    ///   old manifest while clobbering is disabled
    /// - [`DiffError::Other`]: probing the old manifest path fails
    #[inline]
    #[expect(clippy::too_many_lines)]
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

        // Files which have attributes other than `target` changed
        let mut updated_files: Vec<(File, File)> = vec![];
        // Files which nothing needs to be done to
        let mut same_files: Vec<File> = vec![];
        // self.files is files which are new or failed to be atomically updated

        old_manifest.files.retain(|file| {
            if let Some(index) = self.files.iter().position(|inner| {
                inner == file
                    || matches!(inner.clone(), File {
                    kind: FileKind::Symlink | FileKind::Copy,
                    target,
                    ..
                } if target == file.target)
            }) {
                #[expect(clippy::indexing_slicing)]
                if &self.files[index] == file {
                    same_files.push(self.files.swap_remove(index));
                } else {
                    updated_files.push((file.clone(), self.files.swap_remove(index)));
                }
                false
            } else {
                true
            }
        });

        let protected_targets: Vec<_> = updated_files
            .iter()
            .filter_map(|(old, new)| {
                let clobber = new
                    .clobber
                    .unwrap_or_else(|| self.clobber_by_default.unwrap_or(false));

                if clobber {
                    return None;
                }

                let Ok(Some(metadata)) = get_metadata(&new.target) else {
                    return None;
                };

                // A dangling symlink holds no user content worth
                // protecting; it is repaired instead of refused.
                if is_dangling_symlink(&new.target, &metadata) {
                    return None;
                }

                (!old.check().unwrap_or(false)).then(|| new.target.clone())
            })
            .collect();
        if !protected_targets.is_empty() {
            return Err(DiffError::ProtectedTargets(protected_targets));
        }

        // Remove files in old manifest which aren't in new manifest.
        let mut failures: Vec<(PathBuf, String)> = old_manifest
            .deactivate()
            .into_iter()
            .map(|(p, e)| (p, format!("{e:?}")))
            .collect();

        for (_, new) in updated_files {
            let clobber = new
                .clobber
                .unwrap_or_else(|| self.clobber_by_default.unwrap_or(false));

            let mut atomic = new.clone();

            match get_metadata(&atomic.target) {
                Err(err) => {
                    warn!(
                        "Failed to get metadata for file '{}'\n{:?}",
                        atomic.target.display(),
                        err
                    );
                    continue;
                }
                Ok(None) => {
                    self.files.push(new);
                    continue;
                }
                Ok(_) => {}
            }
            if atomic.check_source() {
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
                let mut mut_new = new;
                if !clobber {
                    mut_new.clobber = Some(true);
                }
                self.files.push(mut_new);
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

#[allow(clippy::restriction, clippy::pedantic)]
#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
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

    fn copy_file(source: PathBuf, target: PathBuf, clobber: Option<bool>) -> File {
        File {
            source: Some(source),
            target,
            kind: FileKind::Copy,
            clobber,
            permissions: None,
            uid: None,
            gid: None,
            deactivate: None,
            follow_symlinks: None,
            ignore_modification: None,
        }
    }

    fn symlink_file(source: PathBuf, target: PathBuf, clobber: Option<bool>) -> File {
        File {
            source: Some(source),
            target,
            kind: FileKind::Symlink,
            clobber,
            permissions: None,
            uid: None,
            gid: None,
            deactivate: None,
            follow_symlinks: None,
            ignore_modification: None,
        }
    }

    #[test]
    fn activate_preserves_existing_copy_when_clobber_false() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let target = dir.path().join("target");
        fs::write(&source, b"managed").unwrap();
        fs::write(&target, b"local").unwrap();

        let mut manifest = manifest_with(vec![copy_file(source, target.clone(), Some(false))]);

        assert!(manifest.activate(".backup-").is_empty());
        assert_eq!(fs::read(&target).unwrap(), b"local");
        assert!(!dir.path().join(".backup-target").exists());
    }

    #[test]
    fn diff_updates_managed_copy_when_clobber_false() {
        let dir = tempfile::tempdir().unwrap();
        let old_source = dir.path().join("old-source");
        let new_source = dir.path().join("new-source");
        let target = dir.path().join("target");
        fs::write(&old_source, b"old").unwrap();
        fs::write(&new_source, b"new").unwrap();
        fs::write(&target, b"old").unwrap();

        let old_manifest = manifest_with(vec![copy_file(old_source, target.clone(), Some(false))]);
        let new_manifest = manifest_with(vec![copy_file(new_source, target.clone(), Some(false))]);
        let old_manifest_path = dir.path().join("old.json");
        fs::write(
            &old_manifest_path,
            serde_json::to_vec(&old_manifest).unwrap(),
        )
        .unwrap();

        new_manifest
            .diff(&old_manifest_path, ".backup-", false)
            .unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert!(!dir.path().join(".backup-target").exists());
    }

    #[test]
    fn diff_updates_managed_symlink_when_clobber_false() {
        let dir = tempfile::tempdir().unwrap();
        let old_source = dir.path().join("old-source");
        let new_source = dir.path().join("new-source");
        let target = dir.path().join("target");
        fs::write(&old_source, b"old").unwrap();
        fs::write(&new_source, b"new").unwrap();
        std::os::unix::fs::symlink(&old_source, &target).unwrap();

        let old_manifest =
            manifest_with(vec![symlink_file(old_source, target.clone(), Some(false))]);
        let new_manifest = manifest_with(vec![symlink_file(
            new_source.clone(),
            target.clone(),
            Some(false),
        )]);
        let old_manifest_path = dir.path().join("old.json");
        fs::write(
            &old_manifest_path,
            serde_json::to_vec(&old_manifest).unwrap(),
        )
        .unwrap();

        new_manifest
            .diff(&old_manifest_path, ".backup-", false)
            .unwrap();
        assert_eq!(
            fs::canonicalize(&target).unwrap(),
            fs::canonicalize(new_source).unwrap()
        );
    }

    #[test]
    fn diff_repairs_dangling_symlink_when_clobber_false() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let missing = dir.path().join("missing");
        let target = dir.path().join("target");
        fs::write(&source, b"managed").unwrap();
        std::os::unix::fs::symlink(&missing, &target).unwrap();

        let old_manifest =
            manifest_with(vec![symlink_file(source.clone(), target.clone(), Some(false))]);
        let new_manifest =
            manifest_with(vec![symlink_file(source.clone(), target.clone(), Some(false))]);
        let old_manifest_path = dir.path().join("old.json");
        fs::write(
            &old_manifest_path,
            serde_json::to_vec(&old_manifest).unwrap(),
        )
        .unwrap();

        new_manifest
            .diff(&old_manifest_path, ".backup-", false)
            .unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"managed");
        assert_eq!(fs::read_link(&target).unwrap(), source);
        assert!(!dir.path().join(".backup-target").exists());
    }

    #[test]
    fn diff_repairs_dangling_symlink_when_source_changes() {
        let dir = tempfile::tempdir().unwrap();
        let old_source = dir.path().join("old-source");
        let new_source = dir.path().join("new-source");
        let target = dir.path().join("target");
        fs::write(&old_source, b"old").unwrap();
        fs::write(&new_source, b"new").unwrap();
        std::os::unix::fs::symlink(&old_source, &target).unwrap();
        // Simulate the old source vanishing (e.g. garbage collection).
        fs::remove_file(&old_source).unwrap();

        let old_manifest =
            manifest_with(vec![symlink_file(old_source, target.clone(), Some(false))]);
        let new_manifest = manifest_with(vec![symlink_file(
            new_source.clone(),
            target.clone(),
            Some(false),
        )]);
        let old_manifest_path = dir.path().join("old.json");
        fs::write(
            &old_manifest_path,
            serde_json::to_vec(&old_manifest).unwrap(),
        )
        .unwrap();

        new_manifest
            .diff(&old_manifest_path, ".backup-", false)
            .unwrap();
        assert_eq!(fs::read_link(&target).unwrap(), new_source);
    }

    #[test]
    fn diff_rejects_modified_copy_when_clobber_false() {
        let dir = tempfile::tempdir().unwrap();
        let old_source = dir.path().join("old-source");
        let new_source = dir.path().join("new-source");
        let target = dir.path().join("target");
        fs::write(&old_source, b"old").unwrap();
        fs::write(&new_source, b"new").unwrap();
        fs::write(&target, b"local").unwrap();

        let old_manifest = manifest_with(vec![copy_file(old_source, target.clone(), Some(false))]);
        let new_manifest = manifest_with(vec![copy_file(new_source, target.clone(), Some(false))]);
        let old_manifest_path = dir.path().join("old.json");
        fs::write(
            &old_manifest_path,
            serde_json::to_vec(&old_manifest).unwrap(),
        )
        .unwrap();

        assert!(matches!(
            new_manifest.diff(&old_manifest_path, ".backup-", false),
            Err(DiffError::ProtectedTargets(targets)) if targets == vec![target.clone()]
        ));
        assert_eq!(fs::read(&target).unwrap(), b"local");
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
