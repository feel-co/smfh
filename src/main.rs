use std::path::Path;
use clap::Parser;

mod args;
mod manifest;
mod file_util;

use args::{Args, Subcommands};
use manifest::{Manifest, VERSION};

fn read_manifest_arg(path: &Path) -> Manifest {
    let manifest = match Manifest::read(path) {
        Ok(x) => x,
        Err(e) => panic!("Failed to read or parse manifest!\n{}", e),
    };
    println!("Deserialized manifest: '{}'", path.display());
    println!("Manifest version: '{}'", manifest.version);
    println!("Program version: '{}'", VERSION);

    if manifest.version != VERSION {
        panic!("Version mismatch!\n Program and manifest version must be the same");
    };

    manifest
}

fn main() {
    let args = Args::parse();

    match args.sub_command {
        Subcommands::Deactivate { manifest } => {
            let manifest = read_manifest_arg(&manifest);
            manifest.deactivate();
        },
        Subcommands::Activate { manifest, prefix } => {
            let manifest = read_manifest_arg(&manifest);
            manifest.activate(&prefix);
        },
        Subcommands::Diff {
            prefix,
            manifest,
            old_manifest,
        } => {
            let manifest = read_manifest_arg(&manifest);
            let old_manifest = read_manifest_arg(&old_manifest);
            manifest.diff(old_manifest, &prefix);
        },
    }
}
