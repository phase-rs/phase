//! `set-check` — per-set / per-deck coverage filtering plus AST-hash
//! snapshot/diff regression detection for the Phase Oracle parser.
//!
//! This tool reuses [`engine::game::coverage::analyze_coverage`] for all
//! coverage logic (it never re-derives "is this card supported"). It adds
//! two capabilities on top:
//!
//! 1. **Filtering** — restrict the corpus to a single set (`--set`) or to a
//!    deck list (`--deck`) and print the same per-subset summary shape.
//! 2. **Regression detection** — `--snapshot` writes a stable baseline of
//!    `ast_hash`/`supported`/`gap_count` per card, and `--diff` recomputes the
//!    current hashes and reports exactly which cards' parses moved. This is the
//!    primary use: snapshot before a shared-parser change, regenerate card
//!    data, then diff to see which cards' parses changed (intended or not).
//!
//! ## ast_hash — canonical input definition
//!
//! `ast_hash` is a stable 16-hex-character FNV-1a (64-bit) hash of a
//! **canonical JSON serialization** of the card face's parse-relevant fields.
//! The canonical input is the [`CanonicalFace`] struct, serialized through
//! `serde_json` to a value tree and then to a string. Because this build of
//! `serde_json` has no `preserve_order` feature, its object maps are
//! `BTreeMap`-backed, so object keys serialize in sorted order — the string is
//! therefore deterministic across runs on the same code.
//!
//! The canonical input is, in field order (sorted by the JSON layer):
//! `oracle_text`, `keywords`, `abilities`, `triggers`, `static_abilities`,
//! `replacements`, `additional_cost`, `casting_restrictions`,
//! `casting_options`, `modal`, `solve_condition`, `strive_cost`,
//! `deck_copy_limit`, `cleave_variant`. These are exactly the fields produced
//! by the Oracle parser. Printing-only / legality-only / cosmetic fields
//! (`printings`, `rarities`, `color_identity`, `flavor_name`, `metadata`,
//! `scryfall_oracle_id`, `parse_warnings`) are deliberately excluded so the
//! hash changes **iff** the parsed representation or the source `oracle_text`
//! changes — and is stable otherwise.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process;

use engine::database::CardDatabase;
use engine::game::coverage::{analyze_coverage, CardCoverageResult, GapDetail};
use engine::types::ability::{
    AbilityDefinition, AdditionalCost, CastingRestriction, ModalChoice, ReplacementDefinition,
    SolveCondition, SpellCastingOption, StaticDefinition, TriggerDefinition,
};
use engine::types::card::{CardFace, CleaveVariant};
use engine::types::card_type::{CoreType, Supertype};
use engine::types::format::DeckCopyLimit;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use serde::Serialize;

/// Canonical, parse-only projection of a [`CardFace`] used as the `ast_hash`
/// input. See the module docs for the exact field contract. Fields borrow
/// from the face so building one is allocation-free.
#[derive(Serialize)]
struct CanonicalFace<'a> {
    oracle_text: &'a Option<String>,
    keywords: &'a [Keyword],
    abilities: &'a [AbilityDefinition],
    triggers: &'a [TriggerDefinition],
    static_abilities: &'a [StaticDefinition],
    replacements: &'a [ReplacementDefinition],
    additional_cost: &'a Option<AdditionalCost>,
    casting_restrictions: &'a [CastingRestriction],
    casting_options: &'a [SpellCastingOption],
    modal: &'a Option<ModalChoice>,
    solve_condition: &'a Option<SolveCondition>,
    strive_cost: &'a Option<ManaCost>,
    deck_copy_limit: &'a Option<DeckCopyLimit>,
    cleave_variant: &'a Option<CleaveVariant>,
}

impl<'a> CanonicalFace<'a> {
    fn from_face(face: &'a CardFace) -> Self {
        Self {
            oracle_text: &face.oracle_text,
            keywords: &face.keywords,
            abilities: &face.abilities,
            triggers: &face.triggers,
            static_abilities: &face.static_abilities,
            replacements: &face.replacements,
            additional_cost: &face.additional_cost,
            casting_restrictions: &face.casting_restrictions,
            casting_options: &face.casting_options,
            modal: &face.modal,
            solve_condition: &face.solve_condition,
            strive_cost: &face.strive_cost,
            deck_copy_limit: &face.deck_copy_limit,
            cleave_variant: &face.cleave_variant,
        }
    }
}

/// FNV-1a 64-bit. Self-contained (no external crate) so the algorithm — and
/// therefore the hash — is stable regardless of toolchain or std hasher
/// changes. Returns the low 16 hex characters of the 64-bit digest.
fn fnv1a_16hex(bytes: &[u8]) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for &byte in bytes {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

/// Compute the canonical `ast_hash` for a parsed card face. See module docs.
fn ast_hash(face: &CardFace) -> String {
    let canonical = CanonicalFace::from_face(face);
    // serde_json object maps are BTreeMap-backed in this build (no
    // `preserve_order` feature), so keys serialize sorted → deterministic.
    let json = serde_json::to_string(&canonical)
        .expect("CanonicalFace is composed of serializable parser types");
    fnv1a_16hex(json.as_bytes())
}

/// A single card's regression snapshot entry.
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq)]
struct SnapshotEntry {
    ast_hash: String,
    supported: bool,
    gap_count: usize,
}

/// CR 205.4b: A basic land has the Basic supertype and the Land card type.
/// Excluded from deck filtering since decks list them in bulk and they carry
/// no parser surface.
fn is_basic_land(face: &CardFace) -> bool {
    face.card_type.supertypes.contains(&Supertype::Basic)
        && face.card_type.core_types.contains(&CoreType::Land)
}

/// Parse a deck list into a set of lowercased card-name keys.
///
/// Accepts either a path to a file (one entry per line) or an inline
/// comma-separated list. Each entry tolerates the common decklist shape
/// `"4x Lightning Bolt (M11) 146"`: a leading `N` / `Nx` quantity and a
/// trailing `(SET) number` collector suffix are stripped. Blank lines and
/// lines beginning with `#`, `//`, or a sideboard marker are ignored.
fn parse_deck_list(spec: &str) -> Result<Vec<String>, String> {
    let raw = if Path::new(spec).is_file() {
        std::fs::read_to_string(spec).map_err(|e| format!("failed to read deck {spec}: {e}"))?
    } else {
        spec.replace(',', "\n")
    };

    let mut names = Vec::new();
    for line in raw.lines() {
        if let Some(name) = parse_deck_line(line) {
            names.push(name);
        }
    }
    if names.is_empty() {
        return Err(format!("no card names parsed from deck spec: {spec}"));
    }
    Ok(names)
}

/// Normalize one decklist line to a lowercased card name, or `None` if the
/// line is blank, a comment, or a section header.
fn parse_deck_line(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty()
        || line.starts_with('#')
        || line.starts_with("//")
        || line.eq_ignore_ascii_case("sideboard")
        || line.eq_ignore_ascii_case("deck")
        || line.eq_ignore_ascii_case("commander")
    {
        return None;
    }

    // Strip a leading quantity: "4 ", "4x ", "4X ".
    let mut rest = line;
    if let Some((head, tail)) = rest.split_once(char::is_whitespace) {
        let qty = head.strip_suffix(['x', 'X']).unwrap_or(head);
        if !qty.is_empty() && qty.chars().all(|c| c.is_ascii_digit()) {
            rest = tail.trim_start();
        }
    }

    // Strip a trailing collector suffix: "(SET) 123" or just "(SET)".
    if let Some(idx) = rest.find('(') {
        rest = rest[..idx].trim_end();
    }

    let name = rest.trim();
    (!name.is_empty()).then(|| name.to_lowercase())
}

/// Aggregate counts over a filtered slice of coverage results.
struct Summary<'a> {
    label: String,
    total: usize,
    supported: usize,
    unsupported: Vec<&'a CardCoverageResult>,
    gap_freq: BTreeMap<String, usize>,
}

impl<'a> Summary<'a> {
    fn build(label: String, cards: &[&'a CardCoverageResult]) -> Self {
        let total = cards.len();
        let mut supported = 0;
        let mut unsupported = Vec::new();
        let mut gap_freq: BTreeMap<String, usize> = BTreeMap::new();
        for card in cards {
            if card.supported {
                supported += 1;
            } else {
                unsupported.push(*card);
                for GapDetail { handler, .. } in &card.gap_details {
                    *gap_freq.entry(handler.clone()).or_default() += 1;
                }
            }
        }
        unsupported.sort_by(|a, b| a.card_name.cmp(&b.card_name));
        Self {
            label,
            total,
            supported,
            unsupported,
            gap_freq,
        }
    }

    fn print(&self) {
        let unsupported = self.total - self.supported;
        let pct = if self.total > 0 {
            (self.supported as f64 / self.total as f64) * 100.0
        } else {
            0.0
        };
        println!("{}", self.label);
        println!(
            "  total: {}  supported: {}  unsupported: {}  ({pct:.1}%)",
            self.total, self.supported, unsupported
        );
        if !self.gap_freq.is_empty() {
            println!("  gap breakdown:");
            let mut gaps: Vec<(&String, &usize)> = self.gap_freq.iter().collect();
            gaps.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
            for (handler, count) in gaps {
                println!("    {count:>4}  {handler}");
            }
        }
        if !self.unsupported.is_empty() {
            println!("  unsupported cards:");
            for card in &self.unsupported {
                println!("    {} (gaps: {})", card.card_name, card.gap_count);
            }
        }
    }
}

/// Resolve the directory that contains `card-data.json`. Mirrors
/// `coverage-report`: an explicit positional arg wins, then
/// `PHASE_CARDS_PATH`, then the common in-repo locations.
fn resolve_data_root(explicit: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return Some(PathBuf::from(p));
    }
    if let Ok(p) = std::env::var("PHASE_CARDS_PATH") {
        return Some(PathBuf::from(p));
    }
    for candidate in ["client/public", "data", "crates/engine/data"] {
        if Path::new(candidate).join("card-data.json").is_file() {
            return Some(PathBuf::from(candidate));
        }
    }
    None
}

#[derive(Default)]
struct Options {
    data_root: Option<String>,
    set: Option<String>,
    deck: Option<String>,
    snapshot: Option<String>,
    diff: Option<String>,
    quiet: bool,
}

fn print_usage() {
    eprintln!("Usage: set-check [DATA_ROOT] [OPTIONS]");
    eprintln!();
    eprintln!("Filters/regression-checks Oracle parser coverage. Reuses analyze_coverage.");
    eprintln!(
        "DATA_ROOT defaults to PHASE_CARDS_PATH, then client/public, data, crates/engine/data."
    );
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --set <CODE>        Restrict to cards printed in set CODE (case-insensitive).");
    eprintln!(
        "  --deck <PATH|LIST>  Restrict to a deck list (file path or comma-separated names)."
    );
    eprintln!("  --snapshot <FILE>   Write an ast_hash baseline JSON for the (filtered) corpus.");
    eprintln!(
        "  --diff <FILE>       Compare current ast_hashes to a baseline; exit 1 if any moved."
    );
    eprintln!("  --quiet             In --diff mode, print only the changed/regression counts.");
}

fn parse_args() -> Result<Options, String> {
    let mut opts = Options::default();
    let mut args = std::env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--set" => opts.set = Some(args.next().ok_or("--set requires a value")?),
            "--deck" => opts.deck = Some(args.next().ok_or("--deck requires a value")?),
            "--snapshot" => opts.snapshot = Some(args.next().ok_or("--snapshot requires a value")?),
            "--diff" => opts.diff = Some(args.next().ok_or("--diff requires a value")?),
            "--quiet" => opts.quiet = true,
            "-h" | "--help" => {
                print_usage();
                process::exit(0);
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            positional => {
                if opts.data_root.is_some() {
                    return Err(format!("unexpected positional argument: {positional}"));
                }
                opts.data_root = Some(positional.to_string());
            }
        }
    }
    Ok(opts)
}

/// Build the (lowercase-keyed) face lookup used to compute `ast_hash`.
fn face_by_key(db: &CardDatabase) -> BTreeMap<String, &CardFace> {
    db.face_iter().map(|(k, f)| (k.to_string(), f)).collect()
}

/// Apply the `--set` / `--deck` filter to the coverage results, returning the
/// retained cards. With no filter, returns every card.
fn filter_cards<'a>(
    db: &CardDatabase,
    cards: &'a [CardCoverageResult],
    opts: &Options,
) -> Result<(String, Vec<&'a CardCoverageResult>), String> {
    if let Some(set) = &opts.set {
        let set_upper = set.to_uppercase();
        let retained = cards
            .iter()
            .filter(|c| {
                c.printings
                    .iter()
                    .any(|p| p.eq_ignore_ascii_case(&set_upper))
            })
            .collect();
        return Ok((format!("set {set_upper}"), retained));
    }

    if let Some(deck) = &opts.deck {
        let wanted: std::collections::HashSet<String> =
            parse_deck_list(deck)?.into_iter().collect();
        let retained = cards
            .iter()
            .filter(|c| {
                let key = c.card_name.to_lowercase();
                wanted.contains(&key)
                    && db
                        .face_iter()
                        .find(|(_, f)| f.name.eq_ignore_ascii_case(&c.card_name))
                        .map(|(_, f)| !is_basic_land(f))
                        .unwrap_or(true)
            })
            .collect();
        return Ok((format!("deck ({} named cards)", wanted.len()), retained));
    }

    Ok(("all cards".to_string(), cards.iter().collect()))
}

/// Build the snapshot map for the retained cards.
fn build_snapshot(
    faces: &BTreeMap<String, &CardFace>,
    cards: &[&CardCoverageResult],
) -> BTreeMap<String, SnapshotEntry> {
    cards
        .iter()
        .map(|card| {
            let key = card.card_name.to_lowercase();
            let hash = faces
                .get(&key)
                .map(|face| ast_hash(face))
                .unwrap_or_default();
            (
                key,
                SnapshotEntry {
                    ast_hash: hash,
                    supported: card.supported,
                    gap_count: card.gap_count,
                },
            )
        })
        .collect()
}

fn run() -> Result<i32, String> {
    let opts = parse_args()?;

    let data_root = resolve_data_root(opts.data_root.as_deref())
        .ok_or("could not locate card-data.json (pass DATA_ROOT or set PHASE_CARDS_PATH)")?;
    let export_path = data_root.join("card-data.json");
    let db = CardDatabase::from_export(&export_path)
        .map_err(|e| format!("failed to load {}: {e}", export_path.display()))?;

    let summary = analyze_coverage(&db);
    let faces = face_by_key(&db);

    let (label, retained) = filter_cards(&db, &summary.cards, &opts)?;

    // --diff: regression mode. Recompute hashes and compare to baseline.
    if let Some(baseline_path) = &opts.diff {
        let baseline: BTreeMap<String, SnapshotEntry> = {
            let text = std::fs::read_to_string(baseline_path)
                .map_err(|e| format!("failed to read baseline {baseline_path}: {e}"))?;
            serde_json::from_str(&text)
                .map_err(|e| format!("failed to parse baseline {baseline_path}: {e}"))?
        };
        let current = build_snapshot(&faces, &retained);
        return Ok(run_diff(&baseline, &current, opts.quiet));
    }

    // --snapshot: write baseline.
    if let Some(snapshot_path) = &opts.snapshot {
        let snapshot = build_snapshot(&faces, &retained);
        let json = serde_json::to_string_pretty(&snapshot)
            .map_err(|e| format!("failed to serialize snapshot: {e}"))?;
        std::fs::write(snapshot_path, json)
            .map_err(|e| format!("failed to write snapshot {snapshot_path}: {e}"))?;
        eprintln!(
            "Wrote snapshot of {} cards ({}) to {}",
            snapshot.len(),
            label,
            snapshot_path
        );
        return Ok(0);
    }

    // Default: print the filtered coverage summary.
    Summary::build(label, &retained).print();
    Ok(0)
}

/// Compare two snapshots. Returns the exit code: non-zero if any `ast_hash`
/// changed (so CI can gate on it). Cards that were `supported: true` in the
/// baseline whose AST moved are flagged separately as regression suspects.
fn run_diff(
    baseline: &BTreeMap<String, SnapshotEntry>,
    current: &BTreeMap<String, SnapshotEntry>,
    quiet: bool,
) -> i32 {
    let mut changed: Vec<(&String, &SnapshotEntry, &SnapshotEntry)> = Vec::new();
    let mut added: Vec<&String> = Vec::new();
    let mut removed: Vec<&String> = Vec::new();

    for (key, cur) in current {
        match baseline.get(key) {
            Some(old) if old.ast_hash != cur.ast_hash => changed.push((key, old, cur)),
            Some(_) => {}
            None => added.push(key),
        }
    }
    for key in baseline.keys() {
        if !current.contains_key(key) {
            removed.push(key);
        }
    }

    changed.sort_by(|a, b| a.0.cmp(b.0));
    added.sort();
    removed.sort();

    let regressions: Vec<&(&String, &SnapshotEntry, &SnapshotEntry)> =
        changed.iter().filter(|(_, old, _)| old.supported).collect();

    if quiet {
        println!(
            "changed: {}  regression-suspects: {}  added: {}  removed: {}",
            changed.len(),
            regressions.len(),
            added.len(),
            removed.len()
        );
    } else {
        if changed.is_empty() && added.is_empty() && removed.is_empty() {
            println!(
                "No AST changes: {} cards match the baseline.",
                current.len()
            );
        }
        if !changed.is_empty() {
            println!("Changed AST ({}):", changed.len());
            for (key, old, cur) in &changed {
                println!(
                    "  {key}: hash {} -> {}  supported {} -> {}  gaps {} -> {}",
                    old.ast_hash,
                    cur.ast_hash,
                    old.supported,
                    cur.supported,
                    old.gap_count,
                    cur.gap_count
                );
            }
        }
        if !regressions.is_empty() {
            println!(
                "Regression suspects (baseline supported, AST moved) ({}):",
                regressions.len()
            );
            for (key, old, cur) in &regressions {
                println!(
                    "  {key}: supported {} -> {}  gaps {} -> {}",
                    old.supported, cur.supported, old.gap_count, cur.gap_count
                );
            }
        }
        if !added.is_empty() {
            println!("New cards not in baseline ({}):", added.len());
            for key in &added {
                println!("  {key}");
            }
        }
        if !removed.is_empty() {
            println!("Baseline cards missing now ({}):", removed.len());
            for key in &removed {
                println!("  {key}");
            }
        }
    }

    // Exit non-zero if anything moved so scripts/CI can gate on a clean diff.
    i32::from(!changed.is_empty() || !added.is_empty() || !removed.is_empty())
}

fn main() {
    match run() {
        Ok(code) => process::exit(code),
        Err(msg) => {
            eprintln!("set-check: {msg}");
            print_usage();
            process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::ability::{AbilityKind, Effect};

    fn face_with_oracle(oracle: &str) -> CardFace {
        CardFace {
            oracle_text: Some(oracle.to_string()),
            ..CardFace::default()
        }
    }

    #[test]
    fn deck_line_strips_quantity_and_collector_suffix() {
        assert_eq!(
            parse_deck_line("4 Lightning Bolt (M11) 146"),
            Some("lightning bolt".to_string())
        );
        assert_eq!(
            parse_deck_line("4x Lightning Bolt"),
            Some("lightning bolt".to_string())
        );
        assert_eq!(
            parse_deck_line("1X Sol Ring (CMR) 263"),
            Some("sol ring".to_string())
        );
        assert_eq!(
            parse_deck_line("Counterspell"),
            Some("counterspell".to_string())
        );
    }

    #[test]
    fn deck_line_ignores_headers_and_blanks() {
        assert_eq!(parse_deck_line(""), None);
        assert_eq!(parse_deck_line("   "), None);
        assert_eq!(parse_deck_line("# my deck"), None);
        assert_eq!(parse_deck_line("// notes"), None);
        assert_eq!(parse_deck_line("Sideboard"), None);
        assert_eq!(parse_deck_line("Deck"), None);
    }

    #[test]
    fn deck_line_does_not_strip_quantity_words() {
        // "Ancestral" must not be treated as a quantity even though it starts
        // with a letter; only all-digit (optionally x-suffixed) heads strip.
        assert_eq!(
            parse_deck_line("Ancestral Recall"),
            Some("ancestral recall".to_string())
        );
    }

    #[test]
    fn parse_deck_list_inline_comma_separated() {
        let names = parse_deck_list("Lightning Bolt, Counterspell ,Sol Ring").unwrap();
        assert_eq!(names, vec!["lightning bolt", "counterspell", "sol ring"]);
    }

    #[test]
    fn ast_hash_is_stable_across_runs() {
        let face = face_with_oracle("Flying");
        assert_eq!(ast_hash(&face), ast_hash(&face));
    }

    #[test]
    fn ast_hash_changes_when_oracle_text_changes() {
        let a = face_with_oracle("Flying");
        let b = face_with_oracle("Trample");
        assert_ne!(ast_hash(&a), ast_hash(&b));
    }

    #[test]
    fn ast_hash_changes_when_parsed_field_changes() {
        let mut a = face_with_oracle("Draw a card.");
        let mut b = a.clone();
        // Same oracle text, different parsed representation: the hash must move
        // because the parsed abilities are part of the canonical input.
        a.abilities = vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::unimplemented("a", "draw a card"),
        )];
        b.abilities = vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::unimplemented("b", "draw two cards"),
        )];
        assert_ne!(ast_hash(&a), ast_hash(&b));
    }

    #[test]
    fn diff_detects_changed_hash_and_flags_regression() {
        let mut baseline = BTreeMap::new();
        baseline.insert(
            "stable card".to_string(),
            SnapshotEntry {
                ast_hash: "aaaaaaaaaaaaaaaa".to_string(),
                supported: true,
                gap_count: 0,
            },
        );
        baseline.insert(
            "moved card".to_string(),
            SnapshotEntry {
                ast_hash: "bbbbbbbbbbbbbbbb".to_string(),
                supported: true,
                gap_count: 0,
            },
        );

        let mut current = baseline.clone();
        // Same hash for "stable card"; change the hash for "moved card".
        current.get_mut("moved card").unwrap().ast_hash = "cccccccccccccccc".to_string();
        current.get_mut("moved card").unwrap().supported = false;
        current.get_mut("moved card").unwrap().gap_count = 1;

        let code = run_diff(&baseline, &current, true);
        assert_eq!(code, 1, "a changed hash must yield a non-zero exit code");

        // Identical snapshots produce a clean (zero) exit.
        assert_eq!(run_diff(&baseline, &baseline.clone(), true), 0);
    }

    #[test]
    fn basic_land_detection() {
        let mut plains = CardFace::default();
        plains.card_type.supertypes.push(Supertype::Basic);
        plains.card_type.core_types.push(CoreType::Land);
        assert!(is_basic_land(&plains));

        // Nonbasic land is not excluded.
        let mut nonbasic = CardFace::default();
        nonbasic.card_type.core_types.push(CoreType::Land);
        assert!(!is_basic_land(&nonbasic));
    }
}
