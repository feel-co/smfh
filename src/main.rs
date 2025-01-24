use clap::Subcommand;
use jwalk::{DirEntry, WalkDir};

use std::{
    error::Error,
    ffi::OsString,
    fs::{self, create_dir_all, rename},
    io::Read,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
};

use clap::Parser;
use serde::{
    Deserialize, Deserializer, Serialize,
};
use std::os::unix::fs as unixFs;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    sub_command: SubCommands,
    #[arg()]
    manifest: PathBuf,
}

#[derive(Subcommand, Clone, Debug)]
enum SubCommands {
    Activate {
        #[clap(long, short, action, default_value = ".backup")]
        prefix: String,
    },
    Deactivate,
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    files: Vec<File>,
    clobber_by_default: bool,
    version: u16,
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

#[derive(Serialize, Deserialize, Debug)]
struct File {
    source: Option<PathBuf>,
    target: PathBuf,
    r#type: Types,
    clobber: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_octal")]
    permissions: Option<u32>,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
enum Types {
    Symlink,
    File,
    Directory,
    RecursiveSymlink,
    Delete,
    Chmod,
}
impl Types {
    fn order(self) -> u8 {
        match self {
            Types::Directory => 1,
            Types::RecursiveSymlink => 2,
            Types::File => 3,
            Types::Symlink => 4,
            Types::Chmod=> 5,
            Types::Delete => 6,
        }
    }
}

fn read_manifest(manifest: &PathBuf) -> Result<Manifest, Box<dyn Error>> {
    let read_manifest = fs::read_to_string(manifest)?;
    let deserialized_manifest: Manifest = serde_json::from_str(&read_manifest)?;
    println!("Deserialized manifest '{}'", manifest.display());
    Ok(deserialized_manifest)
}

fn symlink(file: &File) -> Result<(), Box<dyn Error>> {
    let source = fs::canonicalize(file.source.as_ref().unwrap())?;
    unixFs::symlink(&source, &file.target)?;
    println!(
        "Symlinked '{}' -> '{}'",
        source.display(),
        &file.target.display()
    );
    Ok(())
}

fn copy(file: &File) -> Result<(), Box<dyn Error>> {
    let source = fs::canonicalize(file.source.as_ref().unwrap())?;
    fs::copy(&source, &file.target)?;
    println!(
        "Copied '{}' -> '{}'",
        source.display(),
        &file.target.display()
    );
    chmod(file)?;
    Ok(())
}

fn delete_if_exists(path: &PathBuf) -> Result<(), Box<dyn Error>> {
    let Ok(metdata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metdata.is_file() || metdata.is_symlink() {
        fs::remove_file(path)?;
        println!("Deleted '{}'", path.display());
    } else {
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

fn mkdir(path: &PathBuf) -> Result<(), Box<dyn Error>> {
    match fs::symlink_metadata(path) {
        Err(_) => {
            create_dir_all(path)?;
            println!("Created directory '{}'", path.display())
        }
        Ok(x) => {
            if !x.is_dir() {
                return Err(format!("File in way of '{}'", path.display()).into());
            } else {
                println!("Directory '{}' already exists", path.display());
            };
        }
    };
    Ok(())
}

fn chmod(file: &File) -> Result<(), Box<dyn Error>> {
    if let Some(x) = file.permissions {
        let new_perms = fs::Permissions::from_mode(x);
        if fs::symlink_metadata(&file.target)?.permissions() == new_perms {
            return Ok(());
        };
        println!(
            "Setting permissions of: '{:o}' to: '{}'",
            new_perms.mode(),
            &file.target.display()
        );

        fs::set_permissions(&file.target, new_perms)?
    }
    Ok(())
}

fn prefix_move(path: &PathBuf, prefix: &str) -> Result<(), Box<dyn Error>> {
    let Ok(_) = fs::symlink_metadata(path) else {
        return Ok(());
    };

    let mut appended_path = OsString::from(prefix);
    appended_path.push(path.file_name().ok_or("Failed to get file name")?);

    let new_path = PathBuf::from(appended_path);

    if fs::symlink_metadata(&new_path).is_ok() {
        prefix_move(&new_path, prefix)?
    };

    rename(path, &new_path)?;
    println!("Renaming '{}' -> '{}'", path.display(), &new_path.display());
    Ok(())
}

fn recursive_symlink(file: &File, prefix: &str, clobber: bool) -> Result<(), Box<dyn Error>> {
    let base_path = file.source.as_ref().unwrap();
    let walkdir = WalkDir::new(base_path)
        .follow_links(true)
        .into_iter()
        .filter_map(|f| match f {
            Ok(x) => Some(x),
            Err(e) => {
                eprintln!(
                    "Recursive file walking error on base path: {}\n{}",
                    base_path.display(),
                    e
                );
                None
            }
        });

    for entry in walkdir {
        let target_file = &file.target.join(entry.path().strip_prefix(base_path)?);

        if entry.file_type().is_dir() {
            mkdir(target_file)?;
            continue;
        }

        delete_or_move(target_file, prefix, clobber)?;
        unixFs::symlink(fs::canonicalize(entry.path())?, target_file)?;

        println!(
            "Symlinked '{}' -> '{}'",
            entry.path().display(),
            target_file.display()
        );
    }
    Ok(())
}

fn recursive_cleanup(file: &File) {
    fn handle_entry(
        file: &File,
        entry: &DirEntry<((), ())>,
        base_path: &PathBuf,
        dirs: &mut Vec<(PathBuf, usize)>,
    ) -> Result<(), Box<dyn Error>> {
        let path = entry.path();
        let target_file = file.target.join(path.strip_prefix(base_path)?);

        let metadata = fs::symlink_metadata(&target_file)?;

        if metadata.is_symlink() && entry.file_type().is_file() {
            if fs::canonicalize(&target_file)? == fs::canonicalize(entry.path())? {
                fs::remove_file(&target_file)?;
            };
        } else if metadata.is_file() {
            println!(
                "Ignoring file in recursiveSymlink directory: '{}'",
                &target_file.display()
            )
        } else if metadata.is_dir() && entry.file_type().is_dir() {
            dirs.push((target_file.clone(), entry.depth()));
            // Don't println on directories
            // they'll be listed on the rmdir call
            return Ok(());
        } else {
            return Err("File is not symlink, directory, or file".into());
        };

        println!(
            "Deleting '{}' -> '{}'",
            entry.path().display(),
            target_file.display()
        );
        Ok(())
    }

    let base_path = file.source.as_ref().unwrap();
    let walkdir = WalkDir::new(base_path)
        .follow_links(true)
        .into_iter()
        .filter_map(|f| match f {
            Ok(x) => Some(x),
            Err(e) => {
                eprintln!(
                    "Recursive file walking error on base path: '{}'\n{}",
                    base_path.display(),
                    e
                );
                None
            }
        });

    let mut dirs: Vec<(PathBuf, usize)> = vec![];

    for entry in walkdir {
        if let Err(e) = handle_entry(file, &entry, base_path, &mut dirs) {
            eprintln!(
                "Failed to remove file '{}'\nReason: {}",
                entry.path().display(),
                e
            )
        };
    }
    dirs.sort_by(|a, b| b.1.cmp(&a.1));
    dirs.into_iter().for_each(|dir| {
        if let Err(e) = rmdir(&dir.0) {
            eprintln!(
                "Didn't remove directory '{}' of recursiveSymlink '{}'\n Error: {}",
                dir.0.display(),
                base_path.display(),
                e
            )
        };
    });
}

fn delete_or_move(path: &PathBuf, prefix: &str, clobber: bool) -> Result<(), Box<dyn Error>> {
    match clobber {
        true => delete_if_exists(path)?,
        false => prefix_move(path, prefix)?,
    }
    Ok(())
}

fn rmdir(path: &PathBuf) -> Result<(), Box<dyn Error>> {
    let Ok(metdata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if !metdata.is_dir() {
        return Err(format!("Path '{}' is not a directory", path.display()).into());
    }
    fs::remove_dir(path)?;
    println!("Deleting directory '{}'", path.display());
    Ok(())
}

fn hash_file(filepath: &PathBuf) -> Result<u64, Box<dyn Error>> {
    let mut file = std::fs::File::open(filepath)?;
    let mut buffer = Vec::new();
    buffer.clear();
    file.read_to_end(&mut buffer)?;
    Ok(xxhash_rust::xxh3::xxh3_64(&buffer))
}

fn type_checked_delete(file: &File) -> Result<(), Box<dyn Error>> {
    let metadata = fs::symlink_metadata(&file.target)?;

    if metadata.is_symlink() && file.r#type == Types::Symlink {
        if fs::canonicalize(&file.target)? == fs::canonicalize(file.source.as_ref().ok_or("")?)? {
            fs::remove_file(&file.target)?;
        };
    } else if metadata.is_file() && file.r#type == Types::File {
        let Ok(target_hash) = hash_file(&file.target) else {
            return Err("Failed to hash target".into());
        };
        let Ok(source_hash) = hash_file(file.source.as_ref().unwrap()) else {
            return Err("Failed to hash source".into());
        };
        if target_hash == source_hash {
            fs::remove_file(&file.target)?;
        }
    } else {
        return Err("File is not symlink, directory, or file".into());
    };
    Ok(())
}

fn activate(mut manifest: Manifest, prefix: String) {
    manifest
        .files
        .sort_by(|a, b| a.r#type.order().cmp(&b.r#type.order()));

    for file in manifest.files {
        if [Types::Symlink, Types::File, Types::RecursiveSymlink].contains(&file.r#type) {
            if file.source.is_none() {
                eprintln!(
                    "File '{}', of type {:?} missing source attribute",
                    file.target.display(),
                    file.r#type
                );
                continue;
            }

            if fs::symlink_metadata(file.source.as_ref().unwrap()).is_err() {
                eprintln!(
                    "File source '{}', does not exist",
                    file.source.unwrap().display(),
                );
                continue;
            }
        };

        if [Types::File, Types::Symlink].contains(&file.r#type) {
            match mkdir(
                &file
                    .target
                    .parent()
                    .expect("Failed to get parent")
                    .to_path_buf(),
            ) {
                Ok(x) => x,
                Err(e) => eprintln!(
                    "Couldn't create directory '{}'\n Reason: {}",
                    file.target.display(),
                    e
                ),
            };
        };
        let clobber = file.clobber.unwrap_or(manifest.clobber_by_default);

        if ![Types::Delete, Types::Chmod, Types::Directory, Types::RecursiveSymlink].contains(&file.r#type) {
            if let Err(e) = delete_or_move(&file.target, &prefix, clobber) {
                eprintln!(
                    "Couldn't move/delete conflicting file '{}'\nReason: {}",
                    file.target.display(),
                    e
                );
            };
        }

        let activation = match file.r#type {
            Types::Directory => match mkdir(&file.target) {
                Err(e) => Err(e),
                Ok(_) => chmod(&file),
            },
            Types::RecursiveSymlink => recursive_symlink(&file, &prefix, clobber),
            Types::File => copy(&file),
            Types::Symlink => symlink(&file),
            Types::Chmod=> chmod(&file),
            Types::Delete => delete_if_exists(&file.target),
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

fn deactivate(mut manifest: Manifest) {
    manifest
        .files
        .sort_by(|a, b| b.r#type.order().cmp(&a.r#type.order()));

    for file in manifest.files {
        if [Types::Symlink, Types::File, Types::RecursiveSymlink].contains(&file.r#type) {
            if file.source.is_none() {
                eprintln!(
                    "File '{}', of type {:?} missing source attribute",
                    file.target.display(),
                    file.r#type
                );
                continue;
            }

            if fs::symlink_metadata(file.source.as_ref().unwrap()).is_err() {
                println!("File '{}', already deleted", file.source.unwrap().display(),);
                continue;
            }
        };

        if let Err(e) = match file.r#type {
            // delete and chmod are a no-op on deactivation
            Types::Delete => continue,
            Types::Chmod => continue,
            // delete only if directory is empty
            Types::Directory => rmdir(&file.target),
            // this has it's own error handling
            Types::RecursiveSymlink => {
                recursive_cleanup(&file);
                Ok(())
            }
            // delete only if types match
            Types::Symlink => type_checked_delete(&file),
            // delete only if types match
            Types::File => type_checked_delete(&file),
        } {
            eprintln!(
                "Didn't cleanup file '{}'\nReason: {}",
                file.target.display(),
                e
            )
        };
    }
}

fn main() {
    const VERSION: u16 = 1;
    let args = Args::parse();

    let manifest = match read_manifest(&args.manifest) {
        Ok(x) => x,
        Err(e) => panic!("Failed to read or parse manifest!\n{}", e),
    };
    println!("Deserialized manifest: '{}'", args.manifest.display());
    println!("Manifest version: '{}'", manifest.version);
    println!("Program version: '{}'", VERSION);
    if manifest.version != VERSION {
        panic!("Version mismatch!\n Program and manifest version must be the same");
    };

    match args.sub_command {
        SubCommands::Deactivate => deactivate(manifest),
        SubCommands::Activate { prefix } => activate(manifest, prefix),
    }
}
