use std::{
    error::Error,
    ffi::OsString,
    fs,
    io::Read,
    os::unix::fs::{
        self as unixFs,
        PermissionsExt,
    },
    path::{
        Path,
        PathBuf,
    },
};

use jwalk::{
    DirEntry,
    WalkDir,
};

use crate::manifest::{
    File,
    FileKind,
};

pub fn symlink(file: &File) -> Result<(), Box<dyn Error>> {
    let source = fs::canonicalize(file.source.as_ref().unwrap())?;
    unixFs::symlink(&source, &file.target)?;
    println!(
        "Symlinked '{}' -> '{}'",
        source.display(),
        &file.target.display(),
    );
    Ok(())
}

pub fn copy(file: &File) -> Result<(), Box<dyn Error>> {
    let source = fs::canonicalize(file.source.as_ref().unwrap())?;
    fs::copy(&source, &file.target)?;
    println!(
        "Copied '{}' -> '{}'",
        source.display(),
        &file.target.display(),
    );
    chmod(file)?;
    Ok(())
}

pub fn delete_if_exists(path: &Path) -> Result<(), Box<dyn Error>> {
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

pub fn mkdir(path: &Path) -> Result<(), Box<dyn Error>> {
    match fs::symlink_metadata(path) {
        Err(_) => {
            fs::create_dir_all(path)?;
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

pub fn chmod(file: &File) -> Result<(), Box<dyn Error>> {
    if let Some(x) = file.permissions {
        let new_perms = fs::Permissions::from_mode(x);
        if fs::symlink_metadata(&file.target)?.permissions() == new_perms {
            return Ok(());
        };
        println!(
            "Setting permissions of: '{:o}' to: '{}'",
            new_perms.mode(),
            &file.target.display(),
        );

        fs::set_permissions(&file.target, new_perms)?
    }
    Ok(())
}

pub fn prefix_move(path: &Path, prefix: &str) -> Result<(), Box<dyn Error>> {
    let Ok(_) = fs::symlink_metadata(path) else {
        return Ok(());
    };

    let mut appended_path = OsString::from(prefix);
    appended_path.push(path.file_name().ok_or("Failed to get file name")?);

    let new_path = PathBuf::from(appended_path);

    if fs::symlink_metadata(&new_path).is_ok() {
        prefix_move(&new_path, prefix)?
    };

    fs::rename(path, &new_path)?;
    println!("Renaming '{}' -> '{}'", path.display(), new_path.display());
    Ok(())
}

pub fn recursive_symlink(file: &File, prefix: &str, clobber: bool) -> Result<(), Box<dyn Error>> {
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
            target_file.display(),
        );
    }
    Ok(())
}

pub fn recursive_cleanup(file: &File) {
    pub fn handle_entry(
        file: &File,
        entry: &DirEntry<((), ())>,
        base_path: &Path,
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
            target_file.display(),
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

pub fn delete_or_move(path: &Path, prefix: &str, clobber: bool) -> Result<(), Box<dyn Error>> {
    match clobber {
        true => delete_if_exists(path)?,
        false => prefix_move(path, prefix)?,
    }
    Ok(())
}

pub fn rmdir(path: &Path) -> Result<(), Box<dyn Error>> {
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

pub fn hash_file(filepath: &Path) -> Result<u64, Box<dyn Error>> {
    let mut file = std::fs::File::open(filepath)?;
    let mut buffer = Vec::new();
    buffer.clear();
    file.read_to_end(&mut buffer)?;
    Ok(xxhash_rust::xxh3::xxh3_64(&buffer))
}

pub fn type_checked_delete(file: &File) -> Result<(), Box<dyn Error>> {
    let metadata = fs::symlink_metadata(&file.target)?;

    if metadata.is_symlink() && file.kind == FileKind::Symlink {
        if fs::canonicalize(&file.target)? == fs::canonicalize(file.source.as_ref().ok_or("")?)? {
            fs::remove_file(&file.target)?;
        };
    } else if metadata.is_file() && file.kind == FileKind::File {
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
