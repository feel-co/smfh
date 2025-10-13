mod args;
mod file_util;
mod manifest;
use args::{
    Args,
    Subcommands,
};
use clap::Parser as _;
use log::{
    error,
    info,
};
use manifest::Manifest;
use simplelog::{
    ColorChoice,
    Config,
    LevelFilter,
    TermLogger,
    TerminalMode,
};

pub const VERSION: u16 = 1;

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
        Subcommands::Deactivate { manifest } => Manifest::read(&manifest, args.impure).deactivate(),
        Subcommands::Activate { manifest, prefix } => {
            Manifest::read(&manifest, args.impure).activate(&prefix);
        }
        Subcommands::Diff {
            prefix,
            fallback,
            manifest,
            old_manifest,
        } => {
            let mut new = Manifest::read(&manifest, args.impure);
            match old_manifest.try_exists() {
                Ok(true) => new.diff(Manifest::read(&old_manifest, args.impure), &prefix),
                Ok(false) if fallback => new.activate(&prefix),
                Ok(false) => {
                    error!(
                        "Old manifest {} does not exist and `--fallback` is not set",
                        old_manifest.display(),
                    );
                    std::process::exit(3);
                }
                Err(err) => {
                    error!("{err}");
                    std::process::exit(1);
                }
            }
        }
    }
}
