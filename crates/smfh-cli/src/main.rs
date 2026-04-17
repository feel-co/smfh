mod args;

use args::{
    Args,
    Subcommands,
};
use clap::Parser as _;
use log::{
    error,
    info,
};
use simplelog::{
    ColorChoice,
    Config,
    LevelFilter,
    TermLogger,
    TerminalMode,
};
use smfh_core::{
    VERSION,
    manifest::{
        DiffError,
        Manifest,
        ReadError,
    },
};
use std::{
    path::Path,
    process,
};

fn handle_read_error(err: ReadError) -> ! {
    match err {
        ReadError::VersionTooNew { manifest } => {
            error!(
                "Program version: '{VERSION}' Manifest version: '{manifest}'\n Manifest version is newer, exiting!"
            );
            process::exit(2);
        }
        ReadError::Io(e) => {
            error!("{e:?}");
            process::exit(3);
        }
        ReadError::ExpandFailed(e) => {
            error!("{e:?}");
            process::exit(4);
        }
    }
}

fn read_or_exit(path: &Path, impure: bool) -> Manifest {
    match Manifest::read(path, impure) {
        Ok(m) => m,
        Err(e) => handle_read_error(e),
    }
}

fn main() {
    color_eyre::install().expect("Failed to setup color_eyre");

    let args = Args::parse();

    let level = if args.verbose {
        LevelFilter::Info
    } else {
        LevelFilter::Warn
    };

    TermLogger::init(
        level,
        Config::default(),
        TerminalMode::Mixed,
        ColorChoice::Auto,
    )
    .expect("Failed to initialize logger");

    info!("Program version: '{VERSION}'");

    match args.sub_command {
        Subcommands::Deactivate { manifest } => {
            read_or_exit(&manifest, args.impure).deactivate();
        }
        Subcommands::Activate { manifest, prefix } => {
            read_or_exit(&manifest, args.impure).activate(&prefix);
        }
        Subcommands::Diff {
            prefix,
            fallback,
            manifest,
            old_manifest,
        } => {
            if let Err(e) =
                read_or_exit(&manifest, args.impure).diff(&old_manifest, &prefix, fallback)
            {
                match e {
                    DiffError::OldManifestMissing => {
                        error!(
                            "Old manifest {} does not exist and `--fallback` is not set",
                            old_manifest.display()
                        );
                        process::exit(3);
                    }
                    DiffError::OldManifestRead(e) => handle_read_error(e),
                    DiffError::Other(e) => {
                        error!("{e:?}");
                        process::exit(1);
                    }
                }
            }
        }
    }
}
