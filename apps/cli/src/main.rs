use std::ffi::OsString;
use std::path::PathBuf;
use std::process;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

mod apply;
mod backup;
mod clone;
mod config;
mod drop_ins;
mod features;
mod home_fs;
mod lint;
mod runner;
mod state;
mod workspace;

#[derive(Debug, Parser)]
#[command(version, about = "Maintain dotfiles from a Git repository")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Clone or update a dotfiles repository in the dof workspace
    Clone(clone::Args),

    /// Statically validate a dotfiles workspace
    Lint(LintArgs),

    /// List enabled workspace features
    Features(FeaturesArgs),

    /// Apply enabled feature resources to the home directory
    Apply,

    /// Enable or disable workspace features
    Feature(FeatureArgs),

    /// Run an executable from the dof bin directory
    Run(RunArgs),
}

#[derive(Debug, Args)]
struct LintArgs {
    /// Directory to validate
    #[arg(value_name = "DIRECTORY")]
    directory: PathBuf,
}

#[derive(Debug, Args)]
struct FeaturesArgs {
    /// Format feature names as a JSON array
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct FeatureArgs {
    #[command(subcommand)]
    command: FeatureCommand,
}

#[derive(Debug, Subcommand)]
enum FeatureCommand {
    /// Enable a workspace feature
    Enable(FeatureNameArgs),

    /// Disable a workspace feature
    Disable(FeatureNameArgs),
}

#[derive(Debug, Args)]
struct FeatureNameArgs {
    /// Workspace feature name
    #[arg(value_name = "FEATURE")]
    feature: String,
}

#[derive(Debug, Args)]
struct RunArgs {
    /// Executable name in $HOME/.dof/bin
    #[arg(value_name = "SCRIPT")]
    script: OsString,

    /// Arguments passed to the executable
    #[arg(
        value_name = "ARGUMENTS",
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    arguments: Vec<OsString>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        process::exit(1);
    }
}

fn run() -> Result<()> {
    match Cli::parse().command {
        Commands::Clone(args) => clone::run(args),
        Commands::Lint(args) => lint::lint_directory(&args.directory),
        Commands::Features(args) => features::list(args.json),
        Commands::Apply => apply::apply(),
        Commands::Feature(args) => match args.command {
            FeatureCommand::Enable(args) => features::set_enabled(&args.feature, true),
            FeatureCommand::Disable(args) => features::set_enabled(&args.feature, false),
        },
        Commands::Run(args) => runner::run(&args.script, &args.arguments),
    }
}
