use anyhow::Result;
use clap::{Parser, Subcommand};
use foxy_testkit_mutate::{MutateOptions, MutationProfile, mutate, restore};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "foxy-testkit-mutate")]
#[command(about = "Deterministically mutate and restore PBO test fixtures")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Mutate {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        journal: PathBuf,
        #[arg(long, value_enum)]
        profile: MutationProfile,
        #[arg(long)]
        seed: u64,
        #[arg(long = "entries", alias = "k", default_value_t = 8)]
        entry_count: usize,
        #[arg(long = "files", alias = "m", default_value_t = 4)]
        file_count: usize,
        #[arg(long, default_value_t = 1)]
        truncate_by: u64,
        #[arg(long)]
        preserve_mtime: bool,
        #[arg(long = "target")]
        targets: Vec<PathBuf>,
    },
    Restore {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        journal: PathBuf,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Mutate {
            root,
            journal,
            profile,
            seed,
            entry_count,
            file_count,
            truncate_by,
            preserve_mtime,
            targets,
        } => {
            let journal = mutate(
                &MutateOptions {
                    root,
                    targets,
                    profile,
                    seed,
                    entry_count,
                    file_count,
                    truncate_by,
                    preserve_mtime,
                },
                &journal,
            )?;
            println!("{}", serde_json::to_string(&journal.summary())?);
        }
        Command::Restore { root, journal } => {
            let journal = restore(&root, &journal)?;
            println!("{}", serde_json::to_string(&journal.summary())?);
        }
    }
    Ok(())
}
