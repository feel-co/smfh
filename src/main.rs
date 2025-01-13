use clap::Subcommand;
use jwalk::WalkDir;
use std::{
    error::Error,
    fs::{self, create_dir_all, rename},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
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
    recursiveSymlink,
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

fn mkdir(target: &str) -> Result<(), Box<dyn Error>> {
    match fs::symlink_metadata(target) {
        Err(_) => create_dir_all(target)?,
        Ok(x) => {
            if !x.is_dir() {
                return Err(format!("File in way of '{}'", target).into());
            } else {
                println!("Directory '{}' already exists!", target);
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

// How do I clean up residual symlinks
fn recursive_symlink(file: &File) -> Result<(), Box<dyn Error>> {
    fn resolve_link(link: PathBuf) -> Result<PathBuf, Box<dyn Error>> {
        Ok(if link.is_symlink() {
            resolve_link(fs::read_link(link)?)?
        } else {
            link
        })
    }

    let base_path = file.source.clone().unwrap();
    let target_path = Path::new(&file.target);
    mkdir(&file.target)?;
    for entry in WalkDir::new(&base_path).follow_links(true) {
        match entry {
            Err(e) => {
                eprintln!(
                    "Recursive file walking error on base path: {}\n{}",
                    base_path, e
                );
                continue;
            }
            Ok(ref x) => {
                let target_file = target_path.join(x.path().strip_prefix(&base_path)?);
                if x.path().is_dir() {
                    mkdir(
                        target_file
                            .to_str()
                            .ok_or("Failed to turn path into string")?,
                    )?;
                    continue;
                } else {
                    delete_if_exists(
                        target_file
                            .to_str()
                            .ok_or("Failed to turn path into string")?,
                    )?;
                    if x.path_is_symlink() {
                        unixFs::symlink(resolve_link(x.path())?, &target_file)?;
                    }
                };
            }
        };
        let entry = entry.unwrap();
        println!("{}", entry.path().display());
    }
    Ok(())
}

fn handle_activation(file: &File) -> Result<(), Box<dyn Error>> {
    match file.r#type {
        Types::symlink => symlink(file),
        Types::file => copy(file),
        Types::delete => delete_if_exists(&file.target),
        Types::folder => {
            {
                mkdir(&file.target)?;
                chmod(file)?;
            };
            Ok(())
        }
        Types::recursiveSymlink => recursive_symlink(file),
    }
}

fn activate(manifest: Manifest, prefix: String) {
    for file in manifest.files {
        if [Types::symlink, Types::file, Types::recursiveSymlink].contains(&file.r#type)
            && file.source.is_none()
        {
            eprintln!(
                "File '{}', of type {:?} missing source attribute",
                file.target, file.r#type
            );
            continue;
        };

        if [Types::file, Types::symlink].contains(&file.r#type) {
            match mkdir(
                Path::new(&file.target)
                    .parent()
                    .expect("Failed to get parent")
                    .to_str()
                    .expect("Failed to turn path into string"),
            ) {
                Ok(x) => x,
                Err(e) => eprintln!(
                    "Couldn't mkdir conflicting file '{}'\nReason: {}",
                    file.target, e
                ),
            };
        };

        if ![Types::delete, Types::folder, Types::recursiveSymlink].contains(&file.r#type) {
            let cleanup = match file.clobber.unwrap_or(manifest.clobber_by_default) {
                true => delete_if_exists(&file.target),
                false => prefix_move(&file.target, &prefix),
            };
            match cleanup {
                Ok(x) => x,
                Err(e) => eprintln!(
                    "Couldn't move/delete conflicting file '{}'\nReason: {}",
                    file.target, e
                ),
            }
        }

        match handle_activation(&file) {
            Ok(x) => x,
            Err(e) => eprintln!("Failed to handle '{}'\nReason: {}", file.target, e),
        };
    }
}
fn deactivate(manifest: Manifest) {
    for file in manifest.files {
        if [Types::delete, Types::folder].contains(&file.r#type) {
            continue;
        }
        match delete_if_exists(&file.target) {
            Ok(x) => x,
            Err(e) => eprintln!("Didn't cleanup file '{}'\nReason: {}", file.target, e),
        }
    }
}

fn main() {
    let args = Args::parse();

    let manifest = match read_manifest(&args.manifest) {
        Ok(x) => x,
        Err(e) => panic!("Failed to read or parse manifest!\n{}", e),
    };
    println!("Deserialized manifest {}", args.manifest);
    println!("Manifest version {}", manifest.version);
    match args.sub_command {
        SubCommands::Deactivate => deactivate(manifest),
        SubCommands::Activate => activate(manifest, args.prefix.unwrap_or("hjlem".to_string())),
    }
}
