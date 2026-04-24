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
fn verify(manifest: &Path, impure: bool) -> smfh_core::manifest::Manifest {
    let m = read_or_exit(manifest, impure);
    let errors = m.verify();
    if !errors.is_empty() {
        for e in &errors {
            error!("{e}");
        }
        process::exit(3);
    }
    m
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
            let failures = read_or_exit(&manifest, args.impure).deactivate();
            if !failures.is_empty() {
                for (path, err) in &failures {
                    error!("Failed to deactivate {}: {err:?}", path.display());
                }
                process::exit(1);
            }
        }
        Subcommands::Activate { manifest, prefix } => {
            let failures = read_or_exit(&manifest, args.impure).activate(&prefix);
            if !failures.is_empty() {
                for (path, err) in &failures {
                    error!("Failed to activate {}: {err:?}", path.display());
                }
                process::exit(1);
            }
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
                    DiffError::ActivationFailed(failures) => {
                        for (path, err) in &failures {
                            error!("Failed to activate {}: {err}", path.display());
                        }
                        process::exit(1);
                    }
                    DiffError::Other(e) => {
                        error!("{e:?}");
                        process::exit(1);
                    }
                }
            }
        }
        Subcommands::Verify { manifest } => {
            _ = verify(&manifest, args.impure);
            info!("Manifest '{}' is valid", manifest.display());
        }
        Subcommands::Clean { manifest } => {
            let m = verify(&manifest, args.impure);
            match serde_json::to_string_pretty(&m) {
                Ok(s) => println!("{s}"),
                Err(e) => {
                    error!("{e:?}");
                    process::exit(1);
                }
            }
        }
    }
}
