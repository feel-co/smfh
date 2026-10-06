use pound::Parse;
use std::path::PathBuf;

/// Sleek Manifest File Handler
#[derive(Parse, Debug)]
#[pound(name = "smfh")]
pub struct Args {
    #[pound(short, long)]
    pub verbose: bool,

    #[pound(
        long,
        default = "false",
        help = "Allows use of relative paths and environment variable substitutions in paths"
    )]
    pub impure: bool,

    #[pound(subcommand)]
    pub sub_command: Subcommands,
}

#[derive(Parse, Clone, Debug)]
pub enum Subcommands {
    Activate {
        manifest: PathBuf,
        #[pound(short, long, default = ".backup-")]
        prefix: String,
    },
    Deactivate {
        manifest: PathBuf,
    },
    Diff {
        #[pound(short, long, default = ".backup-")]
        prefix: String,
        #[pound(
            long,
            default = "false",
            help = "Continue with activation if old_manifest doesn't exist"
        )]
        fallback: bool,
        manifest: PathBuf,
        old_manifest: PathBuf,
    },
    Verify {
        manifest: PathBuf,
    },
    Clean {
        manifest: PathBuf,
    },
    Merge {
        manifests: Vec<String>,
    },
}
