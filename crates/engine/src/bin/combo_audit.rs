//! Measure the static pair detector against the combo corpus, an optional
//! generated combo table, and zero or more deck lists.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use engine::analysis::{pair_audit_report, COMBO_AUDIT_BLIND_SPOT};
use engine::database::combo_table::ComboTable;
use engine::database::CardDatabase;
use engine::types::card::CardFace;

fn main() -> ExitCode {
    match run(std::env::args_os().skip(1)) {
        Ok(json) => {
            eprintln!("{COMBO_AUDIT_BLIND_SPOT}");
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("combo-audit: {error}");
            ExitCode::from(2)
        }
    }
}

fn run(args: impl Iterator<Item = OsString>) -> Result<String, String> {
    let arguments = parse_args(args)?;
    let db = CardDatabase::from_export(&arguments.pool)
        .map_err(|error| format!("could not load {}: {error}", arguments.pool.display()))?;
    let table = arguments.table.as_deref().map(load_table).transpose()?;
    let decks = arguments
        .decks
        .iter()
        .map(|path| load_deck(path, &db))
        .collect::<Result<Vec<_>, _>>()?;
    let report = pair_audit_report(&db, table.as_ref(), &decks);
    serde_json::to_string_pretty(&report)
        .map_err(|error| format!("could not serialize audit report: {error}"))
}

struct Arguments {
    pool: PathBuf,
    table: Option<PathBuf>,
    decks: Vec<PathBuf>,
}

fn parse_args(args: impl Iterator<Item = OsString>) -> Result<Arguments, String> {
    let mut pool = None;
    let mut table = None;
    let mut decks = Vec::new();
    let mut args = args;
    while let Some(flag) = args.next() {
        let value = args.next().ok_or_else(usage)?;
        match flag.to_str() {
            Some("--pool") if pool.is_none() => pool = Some(PathBuf::from(value)),
            Some("--table") if table.is_none() => table = Some(PathBuf::from(value)),
            Some("--deck") => decks.push(PathBuf::from(value)),
            _ => return Err(usage()),
        }
    }
    let pool = pool.ok_or_else(usage)?;
    Ok(Arguments { pool, table, decks })
}

fn usage() -> String {
    "Usage: combo-audit --pool <card-data.json> [--table <combo-table.json>] [--deck <decklist.txt>]...".to_string()
}

fn load_table(path: &Path) -> Result<ComboTable, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    ComboTable::from_json_str(&raw)
        .map_err(|error| format!("could not load {}: {error}", path.display()))
}

fn load_deck<'db>(path: &Path, db: &'db CardDatabase) -> Result<Vec<&'db CardFace>, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let mut seen = BTreeSet::new();
    let mut faces = Vec::new();
    for (line_index, raw_line) in raw.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty()
            || line.starts_with('#')
            || line.starts_with("//")
            || matches!(
                line.to_ascii_lowercase().as_str(),
                "deck" | "commander" | "companion" | "sideboard" | "maybeboard"
            )
        {
            continue;
        }
        let name = strip_count(line);
        let Some(face) = db.get_face_by_name(name) else {
            return Err(format!(
                "{}:{}: unknown card {name:?}",
                path.display(),
                line_index + 1
            ));
        };
        if seen.insert(face.name.to_lowercase()) {
            faces.push(face);
        }
    }
    Ok(faces)
}

fn strip_count(line: &str) -> &str {
    let Some((first, rest)) = line.split_once(char::is_whitespace) else {
        return line;
    };
    let numeric = first.parse::<u32>().is_ok()
        || first
            .strip_suffix(['x', 'X'])
            .is_some_and(|count| count.parse::<u32>().is_ok());
    if numeric {
        rest.trim_start()
    } else {
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_assembly_smoke_covers_the_corpus() {
        let report = pair_audit_report(&CardDatabase::default(), None, &[]);
        assert_eq!(
            report.corpus_total,
            u32::try_from(engine::analysis::corpus_len()).unwrap()
        );
        assert!(report.max_scc_faces <= 2);
    }
}
