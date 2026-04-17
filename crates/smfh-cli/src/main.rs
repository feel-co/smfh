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
    manifest::Manifest,
};
use std::process;

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

    let result = match args.sub_command {
        Subcommands::Deactivate { manifest } => {
            Manifest::read(&manifest, args.impure).map(|mut m| m.deactivate())
        }
        Subcommands::Activate { manifest, prefix } => {
            Manifest::read(&manifest, args.impure).map(|mut m| m.activate(&prefix))
        }
        Subcommands::Diff {
            prefix,
            fallback,
            manifest,
            old_manifest,
        } => Manifest::read(&manifest, args.impure)
            .and_then(|m| m.diff(&old_manifest, &prefix, fallback)),
    };

    if let Err(err) = result {
        error!("{err:?}");
        process::exit(1);
    }
}
