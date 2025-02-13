use crate::{
    VERSION,
    file_util::{
        self,
        FileWithMetadata,
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
};
use serde::{
    Deserialize,
    Deserializer,
    Serialize,
};
use std::{
    cmp::Ordering,
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
    RecursiveSymlink,
    File,
    Symlink,
    Modify,
    Delete,
}

impl Ord for FileKind {
    fn cmp(&self, other: &Self) -> Ordering {
        fn value(kind: FileKind) -> u8 {
            match kind {
                FileKind::Directory => 1,
                FileKind::RecursiveSymlink => 2,
                FileKind::File => 3,
                FileKind::Symlink => 4,
                FileKind::Modify => 5,
                FileKind::Delete => 7,
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
        for file in &self.files {
            let mut with_metadata = FileWithMetadata::from_file(file);
            if let Err(e) = with_metadata.activate(self.clobber_by_default, prefix, false) {
                error!(
                    "Failed to activate file: '{}'\n Reason: '{}'",
                    file.target.display(),
                    e
                );
            }
        }
    }

    pub fn deactivate(&mut self) {
        for file in self.files.iter().rev() {
            let mut with_metadata = FileWithMetadata::from_file(file);
            if let Err(e) = with_metadata.deactivate() {
                error!(
                    "Failed to deactivate file: '{}'\n Reason: '{}'",
                    file.target.display(),
                    e
                );
            }
        }
    }

    pub fn diff(mut self, mut old_manifest: Self, prefix: &str) {
        let mut atomic_goodness: Vec<File> = vec![];
        let mut intersection: Vec<File> = vec![];

        old_manifest.files.retain(|f| {
            let mut keep = true;

            if let Some(index) = self.files.iter().position(|inner| inner == f) {
                intersection.push(self.files.swap_remove(index));

                keep = !keep;
            } else if let Some(index) = self.files.iter().position(|inner| {
                [FileKind::Symlink, FileKind::File, FileKind::Directory].contains(&inner.kind)
                    && inner.kind == f.kind
                    && inner.target == f.target
            }) {
                atomic_goodness.push(self.files.swap_remove(index));

                keep = !keep;
            }

            keep
        });

        // Remove files in old manifest
        // which aren't in new manifest
        old_manifest.deactivate();

        // Atomic actions on files
        // with same target but
        // different source
        let with_metadata: Vec<FileWithMetadata> = atomic_goodness
            .iter()
            .map(FileWithMetadata::from_file)
            .collect();
        for mut file in with_metadata {
            if let Err(e) = file_util::atomic_activate(&mut file, self.clobber_by_default, prefix) {
                error!(
                    "Failed to activate file: '{}'\n Reason: '{}'",
                    file.target.display(),
                    e
                );
            };
        }

        // These files could technically just be
        // Verified
        self.files.append(&mut intersection);
        // Activate new files
        self.activate(prefix);
    }
}
