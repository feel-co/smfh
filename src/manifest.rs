use crate::VERSION;
use derivative::Derivative;
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
    fs::{
        self,
        Metadata,
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
    let errmsg =
        "Failed to deserialize permissions attribute of file\n File permissions will not be set!";
    let Ok(deserialized) = Option::<String>::deserialize(deserializer) else {
        warn!("{}", errmsg);
        return Ok(None);
    };
    let Some(opt) = deserialized else {
        // No message here because it's null!
        return Ok(None);
    };

    match u32::from_str_radix(&opt, 8) {
        Ok(x) => Ok(Some(x)),
        Err(_) => {
            warn!("{}", errmsg);
            Ok(None)
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Derivative)]
#[derivative(PartialEq, Eq)]
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

    #[serde(skip)]
    #[derivative(PartialEq = "ignore")]
    pub metadata: Option<Metadata>,
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
        fn value(kind: &FileKind) -> u8 {
            match kind {
                FileKind::Directory => 1,
                FileKind::RecursiveSymlink => 2,
                FileKind::File => 3,
                FileKind::Symlink => 4,
                FileKind::Modify => 5,
                FileKind::Delete => 7,
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
        let file = fs::File::open(manifest_path).expect("Failed to read manifest");
        let mut reader = BufReader::new(file);
        let deserialized_manifest: Manifest =
            serde_json::from_reader(&mut reader).expect("Failed to deserialize manifest");

        info!("Deserialized manifest: '{}'", manifest_path.display());

        if deserialized_manifest.version != VERSION {
            error!(
                "Program version: '{}' Manifest version: '{}'",
                VERSION, deserialized_manifest.version
            );
            panic!("Version mismatch!\n Program and manifest version must be the same");
        };
        deserialized_manifest
    }

    pub fn activate(&mut self, prefix: &str) {
        self.files.sort_by_key(|f| f.kind);

        self.files.iter_mut().for_each(|file| {
            if let Err(e) = file.activate(self.clobber_by_default, prefix) {
                error!(
                    "Failed to activate file: '{}'\n Reason: '{}'",
                    file.target.display(),
                    e
                );
            }
        })
    }

    pub fn deactivate(&mut self) {
        self.files.sort_by_key(|f| f.kind);

        self.files.iter_mut().rev().for_each(|file| {
            if let Err(e) = file.deactivate() {
                error!(
                    "Failed to deactivate file: '{}'\n Reason: '{}'",
                    file.target.display(),
                    e
                );
            }
        });
    }

    pub fn diff(mut self, mut old_manifest: Self, prefix: &str) {
        let mut intersection: Vec<File> = vec![];

        old_manifest.files.retain(|f| {
            let contains = self.files.contains(f);
            if contains {
                intersection.push(f.clone());
            }
            !contains
        });

        self.files.retain(|f| !old_manifest.files.contains(f));

        old_manifest.deactivate();
        self.activate(prefix);
    }
}
