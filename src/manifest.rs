use crate::file_util;
use serde::{Deserialize, Deserializer, Serialize};
use std::{
    cmp::Ordering,
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

pub const VERSION: u16 = 1;

#[derive(Serialize, Deserialize)]
pub struct Manifest {
    pub files: BTreeSet<File>,
    pub clobber_by_default: bool,
    pub version: u16,
}

fn deserialize_octal<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u32>, D::Error> {
    if let Some(str) = Option::<String>::deserialize(deserializer)? {
        match u32::from_str_radix(&str, 8) {
            Ok(x) => Ok(Some(x)),
            Err(e) => Err(serde::de::Error::custom(e)),
        }
    } else {
        Ok(None)
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct File {
    pub source: Option<PathBuf>,
    pub target: PathBuf,
    #[serde(rename = "type")]
    pub kind: FileKind,
    pub clobber: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_octal")]
    pub permissions: Option<u32>,
}

impl Ord for File {
    fn cmp(&self, other: &Self) -> Ordering {
        self.kind.cmp(&other.kind)
    }
}

impl PartialOrd for File {
    fn partial_cmp(&self, other: &File) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum FileKind {
    Symlink,
    File,
    Directory,
    RecursiveSymlink,
    Delete,
    Chmod,
}

impl Ord for FileKind {
    fn cmp(&self, other: &Self) -> Ordering {
        fn value(kind: &FileKind) -> u8 {
            match kind {
                FileKind::Directory => 1,
                FileKind::RecursiveSymlink => 2,
                FileKind::File => 3,
                FileKind::Symlink => 4,
                FileKind::Chmod => 5,
                FileKind::Delete => 6,
            }
        }
        value(self).cmp(&value(other))
    }
}

impl PartialOrd for FileKind {
    fn partial_cmp(&self, other: &FileKind) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Manifest {
    pub fn read(manifest_path: &Path) -> Manifest {
        let read_manifest = fs::read_to_string(manifest_path).expect("Failed to read manifest");
        let deserialized_manifest: Manifest =
            serde_json::from_str(&read_manifest).expect("Failed to read manifest");

        println!("Deserialized manifest: '{}'", manifest_path.display());
        println!("Manifest version: '{}'", deserialized_manifest.version);
        println!("Program version: '{}'", VERSION);

        if deserialized_manifest.version != VERSION {
            panic!("Version mismatch!\n Program and manifest version must be the same");
        };
        deserialized_manifest
    }

    pub fn activate(&self, prefix: &str) {
        for file in self.files.iter() {
            if [
                FileKind::Symlink,
                FileKind::File,
                FileKind::RecursiveSymlink,
            ]
            .contains(&file.kind)
            {
                if file.source.is_none() {
                    eprintln!(
                        "File '{}', of type {:?} missing source attribute",
                        file.target.display(),
                        file.kind,
                    );
                    continue;
                }

                if fs::symlink_metadata(file.source.as_ref().unwrap()).is_err() {
                    eprintln!(
                        "File source '{}', does not exist",
                        file.source.as_ref().unwrap().display(),
                    );
                    continue;
                }
            };

            if [FileKind::File, FileKind::Symlink].contains(&file.kind) {
                match file_util::mkdir(file.target.parent().expect("Failed to get parent")) {
                    Ok(x) => x,
                    Err(e) => eprintln!(
                        "Couldn't create directory '{}'\n Reason: {}",
                        file.target.display(),
                        e
                    ),
                };
            };
            let clobber = file.clobber.unwrap_or(self.clobber_by_default);

            if ![
                FileKind::Delete,
                FileKind::Chmod,
                FileKind::Directory,
                FileKind::RecursiveSymlink,
            ]
            .contains(&file.kind)
            {
                if let Err(e) = file_util::delete_or_move(&file.target, prefix, clobber) {
                    eprintln!(
                        "Couldn't move/delete conflicting file '{}'\nReason: {}",
                        file.target.display(),
                        e
                    );
                };
            }

            let activation = match file.kind {
                FileKind::Directory => match file_util::mkdir(&file.target) {
                    Err(e) => Err(e),
                    Ok(_) => file_util::chmod(file),
                },
                FileKind::RecursiveSymlink => file_util::recursive_symlink(file, prefix, clobber),
                FileKind::File => file_util::copy(file),
                FileKind::Symlink => file_util::symlink(file),
                FileKind::Chmod => file_util::chmod(file),
                FileKind::Delete => file_util::delete_if_exists(&file.target),
            };

            match activation {
                Ok(x) => x,
                Err(e) => eprintln!(
                    "Failed to handle '{}'\nReason: {}",
                    file.target.display(),
                    e
                ),
            };
        }
    }

    pub fn deactivate(&self) {
        for file in self.files.iter().rev() {
            if [
                FileKind::Symlink,
                FileKind::File,
                FileKind::RecursiveSymlink,
            ]
            .contains(&file.kind)
            {
                if file.source.is_none() {
                    eprintln!(
                        "File '{}', of type {:?} missing source attribute",
                        file.target.display(),
                        file.kind
                    );
                    continue;
                }

                if fs::symlink_metadata(file.source.as_ref().unwrap()).is_err() {
                    println!(
                        "File '{}' already deleted",
                        file.source.as_ref().unwrap().display()
                    );
                    continue;
                }
            };

            if let Err(e) = match file.kind {
                // delete and chmod are a no-op on deactivation
                FileKind::Delete => continue,
                FileKind::Chmod => continue,
                // delete only if directory is empty
                FileKind::Directory => file_util::rmdir(&file.target),
                // this has it's own error handling
                FileKind::RecursiveSymlink => {
                    file_util::recursive_cleanup(file);
                    Ok(())
                }
                // delete only if types match
                FileKind::Symlink => file_util::type_checked_delete(file),
                // delete only if types match
                FileKind::File => file_util::type_checked_delete(file),
            } {
                eprintln!(
                    "Didn't cleanup file '{}'\nReason: {}",
                    file.target.display(),
                    e
                )
            };
        }
    }

    pub fn diff(mut self, mut old_manifest: Self, prefix: &str) {
        let mut intersection: BTreeSet<File> = BTreeSet::new();

        old_manifest.files.retain(|f| {
            let contains = self.files.remove(f);
            if contains {
                intersection.insert(f.clone());
            }
            !contains
        });

        old_manifest.deactivate();
        self.activate(prefix);

        //TODO: Verify same files
    }
}
