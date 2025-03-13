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
        Context,
        eyre,
    },
};
use log::{
    error,
    info,
    warn,
};
use serde::{
    Deserialize,
    Deserializer,
    Serialize,
};
use std::{
    cmp::Ordering,
    fmt,
    fs::{
        self,
    },
    io::BufReader,
    path::{
        Path,
        PathBuf,
    },
};

#[derive(Serialize, Deserialize)]
pub struct Manifest {
    pub files: Vec<File>,
    pub clobber_by_default: bool,
    pub version: u16,
}

fn deserialize_octal<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u32>, D::Error> {
    let deserialized_value = Option::<String>::deserialize(deserializer)?;
    let Some(value) = deserialized_value else {
        // Don't error here because it's null!
        return Ok(None);
    };
    let x = u32::from_str_radix(&value, 8).map_err(serde::de::Error::custom)?;
    Ok(Some(x))
}

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
}

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
        let name = match self {
            FileKind::Directory => "directory",
            FileKind::Copy => "copy",
            FileKind::Symlink => "symlink",
            FileKind::Modify => "modify",
            FileKind::Delete => "delete",
        };
        write!(f, "{name}")
    }
}

impl Ord for FileKind {
    fn cmp(&self, other: &Self) -> Ordering {
        fn value(kind: FileKind) -> u8 {
            match kind {
                FileKind::Directory => 1,
                FileKind::Copy => 2,
                FileKind::Symlink => 3,
                FileKind::Modify => 4,
                FileKind::Delete => 5,
            }
        }
        value(*self).cmp(&value(*other))
    }
}

impl PartialOrd for FileKind {
    fn partial_cmp(&self, other: &FileKind) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Manifest {
    pub fn read(manifest_path: &Path) -> Result<Manifest> {
        let file = fs::File::open(manifest_path).wrap_err("Failed to read manifest")?;
        let deserialized_manifest: Manifest = serde_json::from_reader(BufReader::new(file))
            .wrap_err("Failed to deserialize manifest")?;

        info!("Deserialized manifest: '{}'", manifest_path.display());

        if deserialized_manifest.version != VERSION {
            error!(
                "Program version: '{}' Manifest version: '{}'",
                VERSION, deserialized_manifest.version
            );
            return Err(eyre!(
                "Version mismatch!\n Program and manifest version must be the same"
            ));
        };
        Ok(deserialized_manifest)
    }

    pub fn activate(&mut self, prefix: &str) {
        self.files.sort_by_key(|x| x.kind);
        for mut file in self.files.iter().map(FileWithMetadata::from) {
            let _ = file
                .activate(self.clobber_by_default, prefix)
                .inspect_err(|e| {
                    error!(
                        "Failed to activate file: '{}'\n Reason: '{}'",
                        file.target.display(),
                        e
                    );
                });
        }
    }

    pub fn deactivate(&mut self) {
        self.files.sort_by_key(|x| x.kind);
        for mut file in self.files.iter().map(FileWithMetadata::from).rev() {
            let _ = file.deactivate().inspect_err(|e| {
                error!(
                    "Failed to deactivate file: '{}'\n Reason: '{}'",
                    file.target.display(),
                    e
                );
            });
        }
    }

    pub fn diff(mut self, mut old_manifest: Self, prefix: &str) {
        let mut updated_files: Vec<(File, File)> = vec![];
        let mut same_files: Vec<File> = vec![];

        old_manifest.files.retain(|f| {
            let mut keep = true;

            if let Some(index) = self.files.iter().position(|inner| inner == f) {
                same_files.push(self.files.swap_remove(index));

                keep = !keep;
            } else if let Some(index) = self.files.iter().position(|inner| {
                matches!(inner, File {
                    kind: FileKind::Symlink | FileKind::Copy,
                    target,
                    ..
                } if target == &f.target)
            }) {
                updated_files.push((f.clone(), self.files.swap_remove(index)));

                keep = !keep;
            }

            keep
        });

        // Remove files in old manifest
        // which aren't in new manifest
        old_manifest.deactivate();

        for (old, new) in updated_files {
            if !old.clobber.unwrap_or(old_manifest.clobber_by_default) {
                let mut f = FileWithMetadata::from(&old);

                // Don't care if this errors
                // metadata will just be none
                let _ = f.set_metadata();

                if f.metadata.is_some()
                    && !f
                        .check()
                        .inspect_err(|e| warn!("Failed to check file: '{}', assuming file is incorrect\nReason: {}", f.target.display(), e))
                        .unwrap_or(false)
                {
                 if let Err(e) = prefix_move(&f.target, prefix) {
                     warn!("Failed to backup file '{}'\nReason: {}", f.target.display(), e);
                 };
                // if file existed but was wrong,
                // atomic action cannot be taken
                // so there's no point of forcing clobber

                // except this double checks
                 self.files.push(new.clone());
                 continue;
                }
            }

            let atomic = FileWithMetadata::from(&new.clone())
                .atomic_activate()
                .inspect_err(|e| {
                    error!(
                        "Failed to (atomic) activate file: '{}'\n Reason: '{}'",
                        new.target.display(),
                        e
                    );
                });
            if atomic.unwrap_or(false) {
                self.files.push(new);
            };
        }

        // These files could technically just be
        // Verified
        self.files.append(&mut same_files);
        // Activate new files
        self.activate(prefix);
    }
}
