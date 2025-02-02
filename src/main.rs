extern crate log;
extern crate simplelog;

mod args;
mod file_util;
mod manifest;

use args::{
    Args,
    Subcommands,
};
use clap::Parser;
use color_eyre::eyre::Result;
use manifest::Manifest;
use simplelog::{
    ColorChoice,
    CombinedLogger,
    Config,
    LevelFilter,
    TermLogger,
    TerminalMode,
};

fn main() -> Result<()> {
    let args = Args::parse();
    color_eyre::install().expect("Failed to setup color_eyre");
    //TODO: implement clap args for logging options
    CombinedLogger::init(vec![
        //        TermLogger::new(
        //            LevelFilter::Warn,
        //            Config::default(),
        //            TerminalMode::Mixed,
        //            ColorChoice::Auto,
        //        ),
        //        TermLogger::new(
        //            LevelFilter::Info,
        //            Config::default(),
        //            TerminalMode::Mixed,
        //            ColorChoice::Auto,
        //        ),
        TermLogger::new(
            LevelFilter::Debug,
            Config::default(),
            TerminalMode::Mixed,
            ColorChoice::Auto,
        ),
    ])?;

    match args.sub_command {
        Subcommands::Deactivate { manifest } => Manifest::read(&manifest).deactivate(),
        Subcommands::Activate { manifest, prefix } => Manifest::read(&manifest).activate(&prefix),
        Subcommands::Diff {
            prefix,
            manifest,
            old_manifest,
        } => Manifest::read(&manifest).diff(Manifest::read(&old_manifest), &prefix),
    };
    Ok(())
}
