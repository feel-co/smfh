use clap::{
    Parser,
    Subcommand,
};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(version, about)]
pub struct Args {
    #[arg(short, long)]
    pub verbose: bool,

    #[arg(
        long,
        default_value = "false",
        help = "Allows use of relative paths and environment variable substitutions in paths"
    )]
    pub impure: bool,

    #[command(subcommand)]
    pub sub_command: Subcommands,
}

#[derive(Subcommand, Clone, Debug)]
pub enum Subcommands {
    Activate {
        #[arg()]
        manifest: PathBuf,

        #[clap(long, short, action, default_value = ".backup-")]
        prefix: String,
    },
    Deactivate {
        #[arg()]
        manifest: PathBuf,
    },
    Diff {
        #[clap(long, short, action, default_value = ".backup-")]
        prefix: String,

        #[arg()]
        manifest: PathBuf,

        #[arg()]
        old_manifest: PathBuf,
    },
}
