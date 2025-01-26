use clap::Parser;

mod args;
mod file_util;
mod manifest;

use args::{Args, Subcommands};
use manifest::Manifest;

fn main() {
    let args = Args::parse();

    match args.sub_command {
        Subcommands::Deactivate { manifest } => {
            let manifest = Manifest::read(&manifest);
            manifest.deactivate();
        }
        Subcommands::Activate { manifest, prefix } => {
            let manifest = Manifest::read(&manifest);
            manifest.activate(&prefix);
        }
        Subcommands::Diff {
            prefix,
            manifest,
            old_manifest,
        } => {
            let manifest = Manifest::read(&manifest);
            let old_manifest = Manifest::read(&old_manifest);
            manifest.diff(old_manifest, &prefix);
        }
    }
}
