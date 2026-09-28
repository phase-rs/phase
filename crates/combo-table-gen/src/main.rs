use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use combo_table_gen::{build_combo_table, sha256_file_prefix, BuildLimits, PINNED_SNAPSHOT};
use engine::database::{CardDatabase, ComboTable};

#[derive(Debug, Parser)]
#[command(about = "Build or check the pinned facts-only Commander combo table")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Build(Args),
    Check(Args),
}

#[derive(Debug, clap::Args)]
struct Args {
    #[arg(long, default_value = PINNED_SNAPSHOT.file)]
    snapshot: PathBuf,
    #[arg(long)]
    pool: PathBuf,
    #[arg(long)]
    out: PathBuf,
    #[arg(long, default_value_t = combo_table_gen::DEFAULT_MAX_BYTES)]
    max_bytes: usize,
}

fn generate(args: &Args) -> Result<Vec<u8>> {
    let pool = CardDatabase::from_export(&args.pool)
        .map_err(|error| anyhow::anyhow!(error.to_string()))
        .with_context(|| format!("failed to load card pool {}", args.pool.display()))?;
    let limits = BuildLimits {
        max_bytes: args.max_bytes,
        card_pool_version: Some(sha256_file_prefix(&args.pool)?),
    };
    let snapshot = File::open(&args.snapshot)
        .with_context(|| format!("failed to open snapshot {}", args.snapshot.display()))?;
    let doc = build_combo_table(snapshot, &pool, &PINNED_SNAPSHOT, &limits)?;
    let bytes = serde_json::to_vec(&doc)?;
    let raw = std::str::from_utf8(&bytes).context("generated combo table was not valid UTF-8")?;
    ComboTable::from_json_str(raw)
        .map_err(|error| anyhow::anyhow!(error.to_string()))
        .context("generated combo table failed runtime validation")?;
    Ok(bytes)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("output path has no filename")?;
    let temporary = parent.join(format!(".{file_name}.tmp-{}", std::process::id()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Build(args) => {
            let bytes = generate(&args)?;
            atomic_write(&args.out, &bytes)?;
            let doc: engine::database::ComboTableDoc = serde_json::from_slice(&bytes)?;
            eprintln!(
                "wrote {} bytes and {} entries to {}",
                bytes.len(),
                doc.entries.len(),
                args.out.display()
            );
            eprintln!("filter counts: {:?}", doc.provenance.filtered);
        }
        Command::Check(args) => {
            let generated = generate(&args)?;
            let existing = fs::read(&args.out)
                .with_context(|| format!("failed to read {}", args.out.display()))?;
            if generated != existing {
                let first = generated
                    .iter()
                    .zip(&existing)
                    .position(|(left, right)| left != right);
                bail!("combo table drift: generated {} bytes, existing {} bytes, first differing byte {}", generated.len(), existing.len(), first.map_or_else(|| generated.len().min(existing.len()).to_string(), |offset| offset.to_string()));
            }
            eprintln!("combo table is current: {} bytes", generated.len());
        }
    }
    Ok(())
}
