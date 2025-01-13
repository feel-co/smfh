use clap::Subcommand;
use std::{
    error::Error,
    fs::{self, create_dir_all, rename},
    os::unix::fs::PermissionsExt,
    path::Path,
};

use clap::Parser;
use serde::{Deserialize, Serialize};
use std::os::unix::fs as unixFs;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    sub_command: SubCommands,
    #[arg()]
    manifest: String,
    #[clap(long, short, action)]
    prefix: Option<String>,
}

#[derive(Subcommand, Clone, Debug)]
enum SubCommands {
    Activate,
    Deactivate,
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    files: Vec<File>,
    clobber_by_default: bool,
    version: u16,
}

#[derive(Serialize, Deserialize)]
struct File {
    source: Option<String>,
    target: String,
    r#type: Types,
    clobber: Option<bool>,
    permissions: Option<u32>,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[allow(non_camel_case_types)]
enum Types {
    symlink,
    file,
    folder,
    //recursiveSymlink,
    delete,
}

fn read_manifest(manifest: &str) -> Result<Manifest, Box<dyn Error>> {
    let read_manifest = fs::read_to_string(manifest)?;
    let deserialized_manifest: Manifest = serde_json::from_str(&read_manifest)?;
    Ok(deserialized_manifest)
}

fn symlink(file: &File) -> Result<(), Box<dyn Error>> {
    let source = file.source.clone().unwrap();
    unixFs::symlink(Path::new(&source), Path::new(&file.target))?;
    Ok(())
}

fn copy(file: &File) -> Result<(), Box<dyn Error>> {
    let source = file.source.clone().unwrap();
    fs::copy(Path::new(&source), Path::new(&file.target))?;
    chmod(file)?;
    Ok(())
}

fn delete_if_exists(path: &str) -> Result<(), Box<dyn Error>> {
    let Ok(metdata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metdata.is_file() || metdata.is_symlink() {
        fs::remove_file(path)?;
    } else {
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

fn mkdir(file: &File) -> Result<(), Box<dyn Error>> {
    if fs::symlink_metadata(&file.target).is_err() {
        create_dir_all(&file.target)?
    };
    chmod(file)?;
    Ok(())
}

fn chmod(file: &File) -> Result<(), Box<dyn Error>> {
    if let Some(x) = file.permissions {
        let new_perms = fs::Permissions::from_mode(x);
        if fs::symlink_metadata(&file.target)?.permissions() == new_perms {
            return Ok(());
        };
        fs::set_permissions(&file.target, new_perms)?
    }
    Ok(())
}

fn prefix_move(path: &str, prefix: &str) -> Result<(), Box<dyn Error>> {
    let Ok(_) = fs::symlink_metadata(path) else {
        return Ok(());
    };

    let as_path = Path::new(path);
    let new_path = format!(
        "{}-{}",
        prefix,
        as_path
            .file_name()
            .ok_or("Failed to get previous filename")?
            .to_str()
            .ok_or("Failed to turn path into string")?
    );
    delete_if_exists(&new_path)?;
    rename(
        as_path,
        as_path
            .parent()
            .ok_or("Failed to get parent")?
            .join(new_path),
    )?;
    Ok(())
}

fn activate(manifest: Manifest, prefix: String) {
    for file in manifest.files {
        if [
            Types::symlink,
            Types::file,
            // Types::recursiveSymlink,
        ]
        .contains(&file.r#type)
            && file.source.is_none()
        {
            eprintln!(
                "File '{}', of type {:?} missing source attribute",
                file.target, file.r#type
            );
            continue;
        }

        if ![Types::delete, Types::folder].contains(&file.r#type) {
            let cleanup = match file.clobber.unwrap_or(manifest.clobber_by_default) {
                true => delete_if_exists(&file.target),
                false => prefix_move(&file.target, &prefix),
            };
            match cleanup {
                Ok(x) => x,
                Err(e) => eprintln!(
                    "Couldn't move conflicting file '{}'\nReason: {}",
                    file.target, e
                ),
            }
        }

        {
            let handle = match file.r#type {
                Types::symlink => symlink(&file),
                Types::file => copy(&file),
                Types::delete => delete_if_exists(&file.target),
                Types::folder => mkdir(&file),
            };
            match handle {
                Ok(x) => x,
                Err(e) => eprintln!("Failed to handle '{}'\nReason: {}", file.target, e),
            };
        }
    }
}
fn deactivate(manifest: Manifest) {
    for file in manifest.files {
        if [Types::delete, Types::folder].contains(&file.r#type) {
            continue;
        }
        match delete_if_exists(&file.target) {
            Ok(x) => x,
            Err(e) => eprintln!("Didn't cleanup file '{}'\nReason:{}", file.target, e),
        }
    }
}

fn main() {
    let args = Args::parse();

    let manifest = match read_manifest(&args.manifest) {
        Ok(x) => x,
        Err(e) => panic!("Failed to read or parse manifest!\n {}", e),
    };
    println!("Deserialized manifest {}", args.manifest);
    println!("Manifest version {}", manifest.version);
    match args.sub_command {
        SubCommands::Deactivate => deactivate(manifest),
        SubCommands::Activate => activate(manifest, args.prefix.unwrap_or("hjlem".to_string())),
    }
}
