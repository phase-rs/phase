use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process;
use std::thread;

use serde::{Deserialize, Serialize};

use engine::database::legality::{legalities_to_export_map, normalize_legalities};
use engine::database::mtgjson::{
    load_atomic_cards, load_card_types, AtomicCard, Ruling, SetCard, SetFile,
};
use engine::database::removed_cards::is_removed_offensive_card;
use engine::database::set_catalog::load_set_catalog;
use engine::database::synthesis::{
    build_oracle_face, build_oracle_face_multi, layout_faces, map_layout, LayoutKind,
};
use engine::database::{set_gating, BracketLists, BracketSignals, CardDatabase};
use engine::game::coverage::{
    audit_semantic, card_face_has_unimplemented_parts, format_semantic_audit_markdown,
};
use engine::types::card::{CardFace, CardLayout, Rarity};

#[derive(Debug, Clone, Serialize)]
struct CardExportEntry {
    #[serde(flatten)]
    face: CardFace,
    #[serde(default)]
    legalities: BTreeMap<String, String>,
    /// MTGJSON layout string for multi-face cards (e.g. "modal_dfc", "transform",
    /// "adventure"). Enables the runtime card database to determine the correct
    /// `LayoutKind` when loading from the export (where `CardRules` is not available).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    layout: Option<String>,
    /// Original zero-based position of this face within MTGJSON's multi-face
    /// record. Used by runtime loaders to choose the card's front face
    /// deterministically after flattening the JSON object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    face_index: Option<usize>,
    /// Set codes the card has been printed in (from MTGJSON `printings`).
    /// Used by the coverage dashboard to group supported/gap cards by set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    printings: Vec<String>,
    /// Official WotC rulings for the card. MTGJSON duplicates the same rulings
    /// across every face of a multi-face card; we attach them to the front
    /// face only (index 0). Back faces receive an empty vec. Rulings describe
    /// the card as a whole, not a specific face, so no information is lost.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    rulings: Vec<Ruling>,
    /// All rarities this card has been printed at across all sets.
    /// Populated by scanning per-set MTGJSON files in `data/mtgjson/sets/`.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    rarities: BTreeSet<Rarity>,
    /// Bracket-axis signals stamped during export. Omitted when all four
    /// flags are false to keep card-data.json compact.
    #[serde(default, skip_serializing_if = "is_clean_signals")]
    bracket_signals: BracketSignals,
}

fn is_clean_signals(sig: &BracketSignals) -> bool {
    sig.is_clean()
}

/// A localized card face for the per-language content-i18n sidecars. Only display
/// fields are carried (name/oracle text/type line); the engine never consumes
/// these — they're overlaid at the frontend display layer. Fields are omitted
/// when absent so the consumer falls back to English per-field.
#[derive(Debug, Clone, Serialize, Default)]
struct LocalizedFace {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    oracle_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    type_line: Option<String>,
}

impl LocalizedFace {
    fn is_empty(&self) -> bool {
        self.name.is_none() && self.oracle_text.is_none() && self.type_line.is_none()
    }
}

/// Map an MTGJSON `foreignData.language` (full English language name) to the
/// supported UI locale code, or `None` for languages we don't ship.
fn locale_code(language: &str) -> Option<&'static str> {
    match language {
        "Spanish" => Some("es"),
        "French" => Some("fr"),
        "German" => Some("de"),
        "Italian" => Some("it"),
        "Portuguese (Brazil)" => Some("pt"),
        _ => None,
    }
}

/// Collect a single-faced card's localized printings into the per-locale sidecar
/// maps, keyed by the same lowercased name used for the English `face_index` so
/// the frontend overlay is a direct lookup.
///
/// Single-faced only, by design. MTGJSON `foreignData` is *card-level*: a
/// multi-face card exposes one combined `"A // B"` `name` and one combined
/// `text`, with no reliable per-face split. Writing that combined name under a
/// single face's key produces wrong data (e.g. "Emerita des Konflikts //
/// Lightning Bolt" as the German name of Lightning Bolt), and — because a
/// multi-face back face can share a key with a classic standalone card (see
/// `insert_face`) — would clobber the canonical card the way `insert_face`
/// specifically prevents for the English index. Restricting collection to
/// single-faced cards keeps the sidecar consistent with `face_index`'s winners:
/// every key holds either the correct single-faced localization or nothing (→
/// English fallback), never a wrong value. Multi-face localization is deferred
/// until per-face localized text can be sourced and integrated into
/// `insert_face`'s single winner decision.
fn collect_localized(
    sidecars: &mut BTreeMap<&'static str, BTreeMap<String, LocalizedFace>>,
    key: &str,
    source: &AtomicCard,
) {
    for fd in &source.foreign_data {
        let Some(code) = locale_code(&fd.language) else {
            continue;
        };
        let localized = LocalizedFace {
            name: fd.name.clone(),
            oracle_text: fd.text.clone(),
            type_line: fd.type_line.clone(),
        };
        if localized.is_empty() {
            continue;
        }
        sidecars
            .entry(code)
            .or_default()
            .entry(key.to_string())
            .or_insert(localized);
    }
}

/// Atomically write a localized sidecar to `<dir>/card-data.<code>.json` (tmp +
/// rename, since Tilt's card-data resource may run this concurrently).
fn write_sidecar(dir: &Path, code: &str, map: &BTreeMap<String, LocalizedFace>) {
    let final_path = dir.join(format!("card-data.{code}.json"));
    let tmp_path = dir.join(format!("card-data.{code}.json.tmp"));
    let json = serde_json::to_string(map).expect("Failed to serialize localized sidecar");
    std::fs::write(&tmp_path, json)
        .unwrap_or_else(|e| panic!("Failed to write {}: {e}", tmp_path.display()));
    std::fs::rename(&tmp_path, &final_path)
        .unwrap_or_else(|e| panic!("Failed to promote {}: {e}", final_path.display()));
}

/// MTGJSON sometimes groups unrelated single-faced cards that share a printed
/// name under one atomic key (homonyms). Example: MKM's *Pick Your Poison*
/// and a Mystery Booster playtest card with the same name. These are not true
/// multi-face cards — each face has `layout: normal` and, when MTGJSON provides
/// IDs, no duplicate oracle id. Missing oracle IDs stay on this conservative
/// all-single path rather than being reconstructed as a bogus multi-face card.
fn is_homonym_atomic_group(faces: &[AtomicCard]) -> bool {
    if faces.len() < 2 {
        return false;
    }
    if !faces
        .iter()
        .all(|face| map_layout(&face.layout) == LayoutKind::Single)
    {
        return false;
    }
    let mut oracle_ids = HashSet::new();
    for face in faces {
        let Some(oracle_id) = face.identifiers.scryfall_oracle_id.as_ref() else {
            continue;
        };
        if !oracle_ids.insert(oracle_id) {
            return false;
        }
    }
    true
}

fn legality_export_score(legalities: &BTreeMap<String, String>) -> u32 {
    legalities
        .values()
        .filter(|status| status.as_str() == "legal")
        .count() as u32
}

/// Within the same structural class (both standalone or both multi-face), pick
/// the entry with more printings; on a tie, prefer the one legal in more
/// formats so homonyms like *Pick Your Poison* resolve to the paper card.
fn same_class_face_priority(existing: &CardExportEntry, new: &CardExportEntry) -> bool {
    let new_printings = new.printings.len();
    let existing_printings = existing.printings.len();
    if new_printings != existing_printings {
        return new_printings > existing_printings;
    }
    legality_export_score(&new.legalities) > legality_export_score(&existing.legalities)
}

/// Homonym groups are already known to be unrelated standalone cards with the
/// same printed name. In that path, constructed-format legality is the semantic
/// signal for the canonical tournament card; printing count is only a fallback.
fn homonym_face_priority(existing: &CardExportEntry, new: &CardExportEntry) -> bool {
    let new_legalities = legality_export_score(&new.legalities);
    let existing_legalities = legality_export_score(&existing.legalities);
    if new_legalities != existing_legalities {
        return new_legalities > existing_legalities;
    }
    same_class_face_priority(existing, new)
}

fn hidden_multiface_key(key: &str, entry: &CardExportEntry) -> Option<String> {
    let oracle_id = entry.face.scryfall_oracle_id.as_ref()?;
    entry.layout.as_ref()?;
    Some(format!("{key} [{oracle_id}]"))
}

fn insert_hidden_multiface(
    face_index: &mut BTreeMap<String, CardExportEntry>,
    key: &str,
    entry: CardExportEntry,
) {
    if let Some(hidden_key) = hidden_multiface_key(key, &entry) {
        face_index.entry(hidden_key).or_insert(entry);
    }
}

fn bracket_signals_for_face(
    lists: &BracketLists,
    face: &CardFace,
    source: &AtomicCard,
) -> BracketSignals {
    let mut signals = lists.signals_for(&face.name);
    signals.game_changer = source.is_game_changer;
    signals
}

/// Insert a card face under its short-name key, resolving collisions when
/// multiple MTGJSON entries map to the same lowercased face name. This happens
/// when a new DFC has a back face whose name matches a classic standalone card
/// — e.g. the Secrets of Strixhaven card `"Emeritus of Truce // Swords to
/// Plowshares"` whose back-face name collides with the iconic paper
/// `"Swords to Plowshares"`.
///
/// Winner selection is structural first, then by the default same-class priority:
/// 1. An entry from a standalone MTGJSON key (`entry.layout.is_none()`) beats
///    one from a multi-face `" // "` key. The canonical paper card always wins
///    over a component of a compound card — not by popularity, but by origin.
/// 2. Within the same structural class, the entry with more printings wins.
/// 3. On a printings tie, the entry legal in more formats wins.
/// 4. On an exact tie, the first-inserted entry is kept (iteration is sorted by
///    MTGJSON key, so "first" is deterministic across machines).
///
/// Collisions are logged at `debug` level so a full card-data export does not
/// flood stderr with deterministic, already-resolved face-name overlaps.
/// `mtgjson_key` is the source key (e.g. `"Start // Fire"`) for diagnostics
/// when running with debug logging enabled.
fn insert_face(
    face_index: &mut BTreeMap<String, CardExportEntry>,
    mtgjson_key: &str,
    key: String,
    entry: CardExportEntry,
) {
    insert_face_with_priority(
        face_index,
        mtgjson_key,
        key,
        entry,
        same_class_face_priority,
    );
}

fn insert_face_with_priority(
    face_index: &mut BTreeMap<String, CardExportEntry>,
    mtgjson_key: &str,
    key: String,
    entry: CardExportEntry,
    same_class_priority: fn(&CardExportEntry, &CardExportEntry) -> bool,
) {
    let Some(existing) = face_index.get(&key) else {
        face_index.insert(key, entry);
        return;
    };

    let new_standalone = entry.layout.is_none();
    let existing_standalone = existing.layout.is_none();
    let new_wins = match (existing_standalone, new_standalone) {
        (false, true) => true,
        (true, false) => false,
        _ => same_class_priority(existing, &entry),
    };

    let existing_oracle = existing.face.scryfall_oracle_id.as_deref();
    let new_oracle = entry.face.scryfall_oracle_id.as_deref();
    let existing_printings = existing.printings.len();
    let new_printings = entry.printings.len();

    if new_wins {
        let existing = existing.clone();
        tracing::debug!(
            "Face collision on '{key}': replacing prior entry ({existing_oracle:?}, \
             {existing_printings} printings) with entry from MTGJSON key '{mtgjson_key}' \
             ({new_oracle:?}, {new_printings} printings)"
        );
        face_index.insert(key.clone(), entry);
        insert_hidden_multiface(face_index, &key, existing);
    } else {
        tracing::debug!(
            "Face collision on '{key}': keeping prior entry ({existing_oracle:?}, \
             {existing_printings} printings) over entry from MTGJSON key '{mtgjson_key}' \
             ({new_oracle:?}, {new_printings} printings)"
        );
        insert_hidden_multiface(face_index, &key, entry);
    }
}

fn build_export_layout(
    faces: &[AtomicCard],
    oracle_id: Option<String>,
    layout_kind: LayoutKind,
) -> CardLayout {
    if faces.len() >= 2 {
        let face_a = build_oracle_face_multi(&faces[0], oracle_id.clone());
        let face_b = build_oracle_face_multi(&faces[1], oracle_id.clone());
        match layout_kind {
            LayoutKind::Split => CardLayout::Split(face_a, face_b),
            LayoutKind::Flip => CardLayout::Flip(face_a, face_b),
            LayoutKind::Transform => CardLayout::Transform(face_a, face_b),
            LayoutKind::Meld => CardLayout::Meld(face_a, face_b),
            LayoutKind::Adventure => CardLayout::Adventure(face_a, face_b),
            LayoutKind::Modal => CardLayout::Modal(face_a, face_b),
            // CR 702.xxx: Prepare (Strixhaven) — Adventure-family frame layout.
            LayoutKind::Prepare => CardLayout::Prepare(face_a, face_b),
            LayoutKind::Specialize => {
                let mut variant_faces = vec![face_b];
                for extra in faces.iter().skip(2) {
                    variant_faces.push(build_oracle_face_multi(extra, oracle_id.clone()));
                }
                CardLayout::Specialize(face_a, variant_faces)
            }
            LayoutKind::Single => CardLayout::Single(face_a),
        }
    } else {
        CardLayout::Single(build_oracle_face(&faces[0], oracle_id))
    }
}

/// Which `face_index` collision policy a pending face uses. Homonym groups
/// (distinct cards sharing one printed name) resolve collisions differently
/// from ordinary faces — see `homonym_face_priority` / `same_class_face_priority`.
enum FacePriority {
    SameClass,
    Homonym,
}

/// Read-only inputs shared by every worker thread of the per-card parse pass.
struct CardWorkCtx<'a> {
    filter_names: &'a [String],
    token_source_metadata: &'a HashMap<TokenSourceMetadataKey, TokenSourceMetadata>,
    rarity_map: &'a HashMap<String, BTreeSet<Rarity>>,
    bracket_lists: &'a BracketLists,
    #[cfg(feature = "forge")]
    forge_index: Option<&'a engine::database::forge::ForgeIndex>,
}

/// One card's fully-parsed export payload. Produced off-thread by
/// `build_card_work` (Oracle-text parsing dominates the pass at ~5ms/card) and
/// merged into the order-sensitive shared indexes afterwards, in input order.
struct CardWork<'a> {
    mtgjson_key: &'a str,
    /// Faces to insert into `face_index`, in the order the sequential loop emitted them.
    entries: Vec<(String, CardExportEntry, FacePriority)>,
    /// `(face key, source)` pairs destined for the localized sidecars.
    localized: Vec<(String, &'a AtomicCard)>,
    /// Number of `cards_with_unimplemented` increments this card contributes.
    /// A homonym group increments once per unimplemented face, so this is a
    /// count rather than a flag. Always computed (the AST scan is trivial next
    /// to the parse); `--stats` only gates the printout.
    unimplemented_faces: u32,
}

/// Parse one MTGJSON atomic group into its export payload. Pure: reads only
/// `ctx` and `faces`, touches no shared mutable state, so it is safe to run
/// concurrently across cards. Returns `None` for cards the export skips.
fn build_card_work<'a>(
    ctx: &CardWorkCtx<'_>,
    mtgjson_key: &'a str,
    faces: &'a [AtomicCard],
) -> Option<CardWork<'a>> {
    // Drop officially-removed offensive cards before any other handling,
    // so they never enter the card database (and not even an explicit
    // --filter can resurrect them).
    if faces
        .first()
        .is_some_and(|f| is_removed_offensive_card(&f.name))
    {
        return None;
    }
    // --filter: skip cards not matching any filter name
    if !ctx.filter_names.is_empty() {
        let card_name = faces
            .first()
            .map(|f| f.name.to_lowercase())
            .unwrap_or_default();
        if !ctx.filter_names.iter().any(|n| card_name.contains(n)) {
            return None;
        }
    }

    let mut work = CardWork {
        mtgjson_key,
        entries: Vec::new(),
        localized: Vec::new(),
        unimplemented_faces: 0,
    };

    let oracle_id = faces
        .first()
        .and_then(|f| f.identifiers.scryfall_oracle_id.clone());

    let layout_kind = map_layout(&faces[0].layout);

    if is_homonym_atomic_group(faces) {
        for source in faces.iter() {
            let oracle_id = source.identifiers.scryfall_oracle_id.clone();
            let mut face = build_oracle_face(source, oracle_id);
            #[cfg(feature = "forge")]
            if let Some(fi) = ctx.forge_index {
                engine::database::forge::apply_forge_fallback(&mut face, fi);
            }
            stamp_token_source_metadata(&mut face, source, ctx.token_source_metadata);
            let key = face.name.to_lowercase();
            let legalities = legalities_to_export_map(&normalize_legalities(&source.legalities));

            if card_face_has_unimplemented_parts(&face) {
                work.unimplemented_faces += 1;
            }

            let rarities = ctx
                .rarity_map
                .get(&face.name.to_lowercase())
                .cloned()
                .unwrap_or_default();

            let bracket_signals = bracket_signals_for_face(ctx.bracket_lists, &face, source);
            work.localized.push((key.clone(), source));
            work.entries.push((
                key,
                CardExportEntry {
                    face,
                    legalities,
                    layout: None,
                    face_index: None,
                    printings: source.printings.clone(),
                    rulings: source.rulings.clone(),
                    rarities,
                    bracket_signals,
                },
                FacePriority::Homonym,
            ));
        }
    } else if faces.len() >= 2 {
        let mut legalities_by_face = BTreeMap::new();
        let layout = build_export_layout(faces, oracle_id, layout_kind);
        for (face, source) in layout_faces(&layout).iter().zip(faces.iter()) {
            legalities_by_face.insert(
                face.name.to_lowercase(),
                legalities_to_export_map(&normalize_legalities(&source.legalities)),
            );
        }

        if layout_faces(&layout)
            .iter()
            .any(|f| card_face_has_unimplemented_parts(f))
        {
            work.unimplemented_faces += 1;
        }

        for (face_idx, (face_ref, source)) in layout_faces(&layout)
            .into_iter()
            .zip(faces.iter())
            .enumerate()
        {
            let key = face_ref.name.to_lowercase();
            let legalities = legalities_by_face.remove(&key).unwrap_or_default();
            let mut face = face_ref.clone();
            #[cfg(feature = "forge")]
            if let Some(fi) = ctx.forge_index {
                engine::database::forge::apply_forge_fallback(&mut face, fi);
            }
            stamp_token_source_metadata(&mut face, source, ctx.token_source_metadata);
            let layout_str = match layout_kind {
                LayoutKind::Single => None,
                _ => Some(faces[0].layout.clone()),
            };
            // Front face (index 0) owns the rulings; back faces get an empty vec.
            // MTGJSON duplicates rulings across faces; this dedups at export time.
            let rulings = if face_idx == 0 {
                faces[0].rulings.clone()
            } else {
                Vec::new()
            };
            let rarities = ctx
                .rarity_map
                .get(&face.name.to_lowercase())
                .cloned()
                .unwrap_or_default();
            let bracket_signals = bracket_signals_for_face(ctx.bracket_lists, &face, source);
            // Localized sidecars cover single-faced cards only — see
            // `collect_localized`. Multi-face `foreignData` is a combined
            // "A // B" name with no reliable per-face split, so these faces
            // fall back to English at the display layer.
            work.entries.push((
                key,
                CardExportEntry {
                    face,
                    legalities,
                    layout: layout_str,
                    face_index: Some(face_idx),
                    printings: faces[0].printings.clone(),
                    rulings,
                    rarities,
                    bracket_signals,
                },
                FacePriority::SameClass,
            ));
        }
    } else {
        let mut face = build_oracle_face(&faces[0], oracle_id);
        #[cfg(feature = "forge")]
        if let Some(fi) = ctx.forge_index {
            engine::database::forge::apply_forge_fallback(&mut face, fi);
        }
        stamp_token_source_metadata(&mut face, &faces[0], ctx.token_source_metadata);
        let key = face.name.to_lowercase();
        let legalities = legalities_to_export_map(&normalize_legalities(&faces[0].legalities));

        if card_face_has_unimplemented_parts(&face) {
            work.unimplemented_faces += 1;
        }

        let rarities = ctx
            .rarity_map
            .get(&face.name.to_lowercase())
            .cloned()
            .unwrap_or_default();

        let bracket_signals = bracket_signals_for_face(ctx.bracket_lists, &face, &faces[0]);
        work.localized.push((key.clone(), &faces[0]));
        work.entries.push((
            key,
            CardExportEntry {
                face,
                legalities,
                layout: None,
                face_index: None,
                printings: faces[0].printings.clone(),
                rulings: faces[0].rulings.clone(),
                rarities,
                bracket_signals,
            },
            FacePriority::SameClass,
        ));
    }

    Some(work)
}

/// Write parser-authoritative creature subtypes: CardTypes.json ∪ corroborated
/// AtomicCards harvest (token-only + newer card-printed types).
///
/// Runs only under `--write-subtypes` (see `main`), and takes `CardTypesFile` by
/// reference rather than `Option` so a partial source set cannot reach it.
fn write_oracle_subtypes(
    card_types: &engine::database::mtgjson::CardTypesFile,
    atomic: &engine::database::mtgjson::AtomicCardsFile,
) {
    use engine::database::subtype_vocab::build_creature_subtype_vocabulary;

    let list: Vec<String> = build_creature_subtype_vocabulary(card_types, atomic)
        .into_iter()
        .collect();
    let out_path = PathBuf::from("crates/engine/data/oracle-subtypes.json");
    // `serde_json::to_string_pretty` does not emit a trailing newline; append one
    // so the committed generated file stays POSIX-compliant (no "\ No newline at
    // end of file" diff churn on every regeneration).
    //
    // Write only when the bytes actually change. The engine lib pulls this file
    // in with `include_str!` (see `parser/oracle_util.rs`), so an unconditional
    // `fs::write` bumps its mtime, dirties the engine crate's dep-info
    // fingerprint, and forces a full engine recompile on the next cargo
    // invocation — a byte-identical file costing ~220s of CI rebuild. This
    // mirrors the `cmp`/`mv` mtime-preservation dance `scripts/gen-card-data.sh`
    // performs on `known-tokens.toml` for exactly the same reason.
    match serde_json::to_string_pretty(&list)
        .map_err(|e| e.to_string())
        .and_then(|json| {
            let contents = format!("{json}\n");
            if std::fs::read_to_string(&out_path).is_ok_and(|prev| prev == contents) {
                return Ok(false);
            }
            std::fs::write(&out_path, contents)
                .map(|()| true)
                .map_err(|e| e.to_string())
        }) {
        Ok(true) => eprintln!(
            "Wrote {} creature subtypes to {}",
            list.len(),
            out_path.display()
        ),
        Ok(false) => eprintln!(
            "Skipped write of {} creature subtypes to {} (unchanged)",
            list.len(),
            out_path.display()
        ),
        Err(e) => eprintln!("warning: failed to write {}: {e}", out_path.display()),
    }
}

fn build_rarity_map(mtgjson_path: &std::path::Path) -> HashMap<String, BTreeSet<Rarity>> {
    let sets_dir = mtgjson_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("sets");

    if !sets_dir.exists() {
        tracing::warn!(
            "Sets directory {} not found — rarities will be empty",
            sets_dir.display()
        );
        return HashMap::new();
    }

    let mut map: HashMap<String, BTreeSet<Rarity>> = HashMap::new();
    let mut set_count: usize = 0;

    let entries = match std::fs::read_dir(&sets_dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("Failed to read sets directory {}: {e}", sets_dir.display());
            return HashMap::new();
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }

        let data = match std::fs::read_to_string(&path) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("Failed to read {}: {e}", path.display());
                continue;
            }
        };

        let set_file: SetFile = match serde_json::from_str(&data) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Failed to parse {}: {e}", path.display());
                continue;
            }
        };

        set_count += 1;
        for card in set_file.data.cards {
            let rarity = match card.rarity.as_str() {
                "common" => Rarity::Common,
                "uncommon" => Rarity::Uncommon,
                "rare" => Rarity::Rare,
                "mythic" => Rarity::Mythic,
                "special" => Rarity::Special,
                "bonus" => Rarity::Bonus,
                _ => continue,
            };
            let key = card
                .face_name
                .as_deref()
                .unwrap_or(&card.name)
                .to_lowercase();
            map.entry(key).or_default().insert(rarity);
        }
    }

    tracing::info!(
        "Scanned {set_count} set files, {} cards with rarity data",
        map.len()
    );

    map
}

#[derive(Default, Clone)]
struct TokenSourceMetadata {
    related_token_ids: BTreeSet<String>,
    source_printing_ids: BTreeSet<String>,
    /// Alchemy spellbook list (order-preserving; MTGJSON lists are already sorted
    /// by the source). A `Vec` rather than a set so the presented order is stable.
    spellbook: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum TokenSourceMetadataKey {
    Oracle {
        oracle_id: String,
        face_name: String,
    },
    Name {
        card_name: String,
        face_name: String,
    },
}

impl TokenSourceMetadataKey {
    fn from_set_card(card: &SetCard) -> Self {
        let face_name = normalized_source_face_name(&card.name, card.face_name.as_deref());
        if let Some(oracle_id) = card.identifiers.scryfall_oracle_id.as_deref() {
            Self::Oracle {
                oracle_id: oracle_id.to_string(),
                face_name,
            }
        } else {
            Self::Name {
                card_name: card.name.to_lowercase(),
                face_name,
            }
        }
    }

    fn candidates_for_atomic(source: &AtomicCard) -> Vec<Self> {
        let face_name = normalized_source_face_name(&source.name, source.face_name.as_deref());
        let mut candidates = Vec::new();
        if let Some(oracle_id) = source.identifiers.scryfall_oracle_id.as_deref() {
            candidates.push(Self::Oracle {
                oracle_id: oracle_id.to_string(),
                face_name: face_name.clone(),
            });
        }
        candidates.push(Self::Name {
            card_name: source.name.to_lowercase(),
            face_name,
        });
        candidates
    }
}

fn normalized_source_face_name(card_name: &str, face_name: Option<&str>) -> String {
    face_name.unwrap_or(card_name).to_lowercase()
}

fn build_token_source_metadata(
    mtgjson_path: &std::path::Path,
    atomic: &engine::database::mtgjson::AtomicCardsFile,
) -> HashMap<TokenSourceMetadataKey, TokenSourceMetadata> {
    let sets_dir = mtgjson_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("sets");

    let mut map: HashMap<TokenSourceMetadataKey, TokenSourceMetadata> = HashMap::new();

    // Per-set token/spellbook metadata requires local set files, so guard the
    // set loop on the directory. It must NOT early-return the whole function —
    // the Alchemy spellbook harvest below runs unconditionally so the
    // Effect::DraftFromSpellbook faces are populated even when only
    // AtomicCards.json is present locally.
    if sets_dir.exists() {
        merge_set_token_metadata(&sets_dir, &mut map);
    }

    // Revive Effect::DraftFromSpellbook: source each face's Alchemy spellbook
    // list from the already-loaded AtomicCards.json (relatedCards.spellbook),
    // which serde previously dropped for lack of a capturing field.
    merge_atomic_spellbooks(&mut map, atomic);

    map
}

fn merge_set_token_metadata(
    sets_dir: &std::path::Path,
    map: &mut HashMap<TokenSourceMetadataKey, TokenSourceMetadata>,
) {
    let entries = match std::fs::read_dir(sets_dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(data) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(set_file) = serde_json::from_str::<SetFile>(&data) else {
            continue;
        };
        for card in set_file.data.cards {
            if card.related_cards.tokens.is_empty()
                && card.related_cards.spellbook.is_empty()
                && card.identifiers.scryfall_id.is_none()
            {
                continue;
            }
            let key = TokenSourceMetadataKey::from_set_card(&card);
            let entry = map.entry(key).or_default();
            entry.related_token_ids.extend(card.related_cards.tokens);
            // Alchemy spellbook: keep the first non-empty list seen for the face.
            if entry.spellbook.is_empty() && !card.related_cards.spellbook.is_empty() {
                entry.spellbook = card.related_cards.spellbook;
            }
            if let Some(id) = card.identifiers.scryfall_id {
                entry.source_printing_ids.insert(id);
            }
        }
    }
}

/// Harvest each card's Alchemy spellbook (`relatedCards.spellbook`) from the
/// already-loaded AtomicCards data and merge it into the token-source map.
///
/// This is the data-pipeline fix that revives `Effect::DraftFromSpellbook`:
/// the spellbook faces are absent from the local per-set files, and serde
/// previously dropped the nested `relatedCards`, so the map was empty and every
/// DraftFromSpellbook face drafted from an empty list (a runtime no-op).
///
/// The key mirrors the set-file loop's derivation — `faceName` when present,
/// otherwise `name`, lowercased — NOT `faceName` alone: several spellbook
/// sources (e.g. Tome of Gadwick, Boseiju Pathlighter) have `faceName: null`,
/// so keying by face name alone would leave them inert. The "first non-empty
/// list wins" guard matches the set-file loop so a set-file spellbook, if any,
/// is not clobbered.
fn merge_atomic_spellbooks(
    map: &mut HashMap<TokenSourceMetadataKey, TokenSourceMetadata>,
    atomic: &engine::database::mtgjson::AtomicCardsFile,
) {
    for faces in atomic.data.values() {
        for card in faces {
            if card.related_cards.spellbook.is_empty() {
                continue;
            }
            // Mirror the set-file loop's key derivation (oracle id when present,
            // else card name; qualified by face name) so an atomic-sourced spellbook
            // merges into the same entry a set file would populate.
            let key = TokenSourceMetadataKey::candidates_for_atomic(card)
                .into_iter()
                .next()
                .expect("candidates_for_atomic always yields at least the Name key");
            let entry = map.entry(key).or_default();
            if entry.spellbook.is_empty() {
                entry.spellbook = card.related_cards.spellbook.clone();
            }
        }
    }
}

fn stamp_token_source_metadata(
    face: &mut CardFace,
    source: &AtomicCard,
    map: &HashMap<TokenSourceMetadataKey, TokenSourceMetadata>,
) {
    if let Some(metadata) = TokenSourceMetadataKey::candidates_for_atomic(source)
        .iter()
        .find_map(|key| map.get(key))
    {
        face.metadata.related_token_ids = metadata.related_token_ids.iter().cloned().collect();
        face.metadata.source_printing_ids = metadata.source_printing_ids.iter().cloned().collect();
        face.metadata.spellbook = metadata.spellbook.clone();
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Check for semantic-audit subcommand before normal parsing
    if args.get(1).map(|s| s.as_str()) == Some("semantic-audit") {
        run_semantic_audit(&args[2..]);
        return;
    }

    if args.get(1).map(|s| s.as_str()) == Some("rulings") {
        run_rulings(&args[2..]);
        return;
    }

    if args.get(1).map(|s| s.as_str()) == Some("set-list") {
        run_set_list(&args[2..]);
        return;
    }

    if args.get(1).map(|s| s.as_str()) == Some("decks") {
        run_decks(&args[2..]);
        return;
    }

    let mut data_dir: Option<PathBuf> = None;
    let mut mtgjson_override: Option<PathBuf> = None;
    let mut names_out: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut sidecar_dir: Option<PathBuf> = None;
    let mut stats = false;
    let mut write_subtypes = false;
    let mut filter_names: Vec<String> = Vec::new();
    #[cfg(feature = "forge")]
    let mut forge_path: Option<PathBuf> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--mtgjson" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Error: --mtgjson requires a path argument");
                    process::exit(1);
                }
                mtgjson_override = Some(PathBuf::from(&args[i]));
            }
            "--names-out" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Error: --names-out requires a path argument");
                    process::exit(1);
                }
                names_out = Some(PathBuf::from(&args[i]));
            }
            "--output" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Error: --output requires a path argument");
                    process::exit(1);
                }
                output = Some(PathBuf::from(&args[i]));
            }
            "--sidecar-dir" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Error: --sidecar-dir requires a path argument");
                    process::exit(1);
                }
                sidecar_dir = Some(PathBuf::from(&args[i]));
            }
            "--stats" => {
                stats = true;
            }
            "--write-subtypes" => {
                write_subtypes = true;
            }
            "--filter" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Error: --filter requires card name(s) separated by |");
                    process::exit(1);
                }
                filter_names = args[i]
                    .split('|')
                    .map(|s| s.trim().to_lowercase())
                    .collect();
            }
            #[cfg(feature = "forge")]
            "--forge" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Error: --forge requires a path to Forge cardsfolder/");
                    process::exit(1);
                }
                forge_path = Some(PathBuf::from(&args[i]));
            }
            _ if data_dir.is_none() && !args[i].starts_with('-') => {
                data_dir = Some(PathBuf::from(&args[i]));
            }
            other => {
                eprintln!("Unknown argument: {other}");
                process::exit(1);
            }
        }
        i += 1;
    }

    let data_dir = data_dir.or_else(|| std::env::var("PHASE_DATA_DIR").ok().map(PathBuf::from));

    let mtgjson_path = match mtgjson_override {
        Some(p) => p,
        None => match &data_dir {
            Some(d) => d.join("mtgjson/AtomicCards.json"),
            None => {
                eprintln!(
                    "Usage: oracle-gen <data-dir> [--mtgjson <path>] [--stats] [--output <path>]"
                );
                eprintln!("  Parses Oracle text from MTGJSON and outputs card-data export JSON");
                eprintln!("  --output <path>  Write the export to a file instead of stdout");
                eprintln!(
                    "  --write-subtypes Regenerate the committed creature-subtype vocabulary\n\
                     \x20                 (crates/engine/data/oracle-subtypes.json). Requires\n\
                     \x20                 CardTypes.json alongside AtomicCards.json. Without this\n\
                     \x20                 flag the export never writes to the tracked tree."
                );
                process::exit(1);
            }
        },
    };

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "oracle_gen=info,engine=info".parse().unwrap()),
        )
        .init();

    if !mtgjson_path.exists() {
        eprintln!("Error: {} not found", mtgjson_path.display());
        process::exit(1);
    }

    let atomic = match load_atomic_cards(&mtgjson_path) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Error loading MTGJSON: {e}");
            process::exit(1);
        }
    };

    // The creature-subtype vocabulary is a COMMITTED parser input: the engine
    // pulls `crates/engine/data/oracle-subtypes.json` in with `include_str!`.
    // Regenerating it is therefore a deliberate data-pipeline act, not a side
    // effect of exporting cards, and it happens only under `--write-subtypes`
    // (passed by `scripts/gen-card-data.sh`, the one caller that fetches the
    // MTGJSON sidecars). A plain export is a pure read: it must not mutate the
    // tracked tree it is measuring, and it must not bump the mtime of a file the
    // engine compiles in — that rebuilds the parser underneath the very export
    // whose output is being compared.
    //
    // When the refresh IS requested, CardTypes.json is REQUIRED. It is the sole
    // source of the token-only creature subtypes (Army, Servo, Pentavite,
    // Sculpture, Tentacle, …), which are printed on no card face and so cannot
    // be recovered from the AtomicCards harvest. Regenerating without it silently
    // deletes all 26 and degrades every later parse, so a missing sidecar is a
    // hard failure rather than a quiet downgrade.
    if write_subtypes {
        let card_types_path = mtgjson_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("CardTypes.json");
        match load_card_types(&card_types_path) {
            Ok(card_types) => write_oracle_subtypes(&card_types, &atomic),
            Err(e) => {
                eprintln!(
                    "Error: --write-subtypes requires {}: {e}",
                    card_types_path.display()
                );
                eprintln!(
                    "  Run scripts/gen-card-data.sh, which downloads the MTGJSON sidecars, \
                     or drop --write-subtypes to export without refreshing the vocabulary."
                );
                process::exit(1);
            }
        }
    }

    // Scan per-set MTGJSON files to build a card name → rarities map.
    let rarity_map = build_rarity_map(&mtgjson_path);
    let token_source_metadata = build_token_source_metadata(&mtgjson_path, &atomic);

    let set_catalog = data_dir
        .as_ref()
        .map(|d| load_set_catalog(d))
        .unwrap_or_default();

    // Load non-MTGJSON bracket lists for signal stamping. Game Changers come
    // directly from MTGJSON `isGameChanger`; this file covers policy axes that
    // MTGJSON does not expose.
    let bracket_lists_path = data_dir
        .as_ref()
        .map(|d| d.join("bracket_lists.json"))
        .unwrap_or_else(|| PathBuf::from("data/bracket_lists.json"));
    let bracket_lists = if bracket_lists_path.exists() {
        BracketLists::from_json_path(&bracket_lists_path).unwrap_or_else(|e| {
            eprintln!(
                "warning: failed to load {}: {e}; non-MTGJSON bracket signals will be all-false",
                bracket_lists_path.display()
            );
            BracketLists::default()
        })
    } else {
        eprintln!(
            "warning: {} not found; non-MTGJSON bracket signals will be all-false",
            bracket_lists_path.display()
        );
        BracketLists::default()
    };

    // Build Forge index: --forge flag > PHASE_FORGE_PATH env var > data/forge-cardsfolder/ default.
    #[cfg(feature = "forge")]
    let forge_index = {
        let explicit = forge_path.is_some() || std::env::var("PHASE_FORGE_PATH").is_ok();
        let default_path = data_dir
            .as_ref()
            .map(|d| d.join("forge-cardsfolder"))
            .unwrap_or_else(|| PathBuf::from("data/forge-cardsfolder"));
        let path = forge_path
            .or_else(|| std::env::var("PHASE_FORGE_PATH").ok().map(PathBuf::from))
            .unwrap_or(default_path);
        if path.exists() {
            eprintln!("Building Forge index from: {}", path.display());
            let idx = engine::database::forge::ForgeIndex::scan(&path);
            eprintln!("Forge index: {} face names", idx.len());
            Some(idx)
        } else if explicit {
            // Only warn if the user explicitly requested a path that doesn't exist.
            eprintln!("warning: Forge path {} not found, skipping", path.display());
            None
        } else {
            None
        }
    };

    let mut face_index: BTreeMap<String, CardExportEntry> = BTreeMap::new();
    // Per-locale localized face data for content-i18n sidecars, keyed by the same
    // lowercased face name as `face_index`.
    let mut sidecars: BTreeMap<&'static str, BTreeMap<String, LocalizedFace>> = BTreeMap::new();
    let mut total_cards = 0u32;
    let mut cards_with_unimplemented = 0u32;

    // Sort MTGJSON keys for deterministic iteration. `atomic.data` is a
    // `HashMap`, so raw `.values()` order is per-process random — that is
    // the root cause behind flaky face-name collision outcomes (e.g. paper
    // Brainstorm vs. the SOS DFC back-face Brainstorm picking different
    // winners across builds). Deterministic iteration lets `insert_face`'s
    // tiebreakers produce the same winner every time.
    let mut atomic_keys: Vec<&String> = atomic.data.keys().collect();
    atomic_keys.sort_unstable();

    let ctx = CardWorkCtx {
        filter_names: &filter_names,
        token_source_metadata: &token_source_metadata,
        rarity_map: &rarity_map,
        bracket_lists: &bracket_lists,
        #[cfg(feature = "forge")]
        forge_index: forge_index.as_ref(),
    };

    // Parse pass: Oracle-text parsing dominates this loop (~5ms/card over
    // ~35k cards) and `build_card_work` is pure, so fan it out across cores.
    // Results are collected index-addressed — each worker owns one contiguous
    // chunk of the sorted key list and the chunks are concatenated in order —
    // so the merge below observes exactly the sequential iteration order. That
    // determinism is load-bearing: `insert_face`'s collision tiebreakers are
    // order-sensitive, and CI caches card-data.json by content hash.
    let worker_count = thread::available_parallelism().map_or(1, |n| n.get());
    let chunk_len = atomic_keys.len().div_ceil(worker_count).max(1);
    let ctx = &ctx;
    let atomic_data = &atomic.data;
    let card_work: Vec<CardWork> = thread::scope(|scope| {
        let handles: Vec<_> = atomic_keys
            .chunks(chunk_len)
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .filter_map(|key| build_card_work(ctx, key.as_str(), &atomic_data[*key]))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("card parse worker panicked"))
            .collect()
    });

    // Merge pass: single-threaded and in sorted-key order, because every
    // mutation here is order-sensitive (`insert_face` collision resolution,
    // sidecar first-writer-wins) or an accumulator.
    for card in card_work {
        total_cards += 1;
        cards_with_unimplemented += card.unimplemented_faces;
        for (key, source) in &card.localized {
            collect_localized(&mut sidecars, key, source);
        }
        for (key, entry, priority) in card.entries {
            match priority {
                FacePriority::SameClass => {
                    insert_face(&mut face_index, card.mtgjson_key, key, entry)
                }
                FacePriority::Homonym => insert_face_with_priority(
                    &mut face_index,
                    card.mtgjson_key,
                    key,
                    entry,
                    homonym_face_priority,
                ),
            }
        }
    }

    // Warn for any bracket list entry that didn't match any exported card.
    let known_names: std::collections::HashSet<String> = face_index
        .values()
        .map(|e| e.face.name.to_lowercase())
        .collect();
    for list_entry in bracket_lists.all_names() {
        if !known_names.contains(list_entry) {
            eprintln!(
                "warning: bracket_lists.json entry \"{list_entry}\" does not match any exported card"
            );
        }
    }

    // Release-gate (hybrid): keep cards available ONLY through gated sets in
    // card-data so they stay browsable, but mark them Banned in every format so
    // they are excluded from every format-scoped deck-builder pool. The gated
    // sets are separately hidden from the draft/picker/deck-builder UIs below
    // (the `is_set_gated` filter on the set list), so the sets remain
    // un-draftable. Reprint-aware. Sets past their MTGJSON release date are
    // auto-unlocked even when still listed in `GATED_SETS`. See
    // `database::set_gating`.
    let gated_sets = set_gating::resolve_gated_sets(&set_catalog);
    if !gated_sets.is_empty() {
        let banned = legalities_to_export_map(&set_gating::all_formats_banned());
        let mut gated_count = 0usize;
        for entry in face_index.values_mut() {
            if set_gating::is_card_gated(&entry.printings, &gated_sets) {
                entry.legalities = banned.clone();
                gated_count += 1;
            }
        }
        eprintln!(
            "Set gating active ({}): marked {} card face(s) Banned in all formats (available only via gated sets)",
            {
                let mut codes: Vec<&str> = gated_sets.iter().map(String::as_str).collect();
                codes.sort_unstable();
                codes.join(",")
            },
            gated_count
        );
    }

    let json = serde_json::to_string(&face_index).expect("Failed to serialize card data");
    if let Some(ref out_path) = output {
        std::fs::write(out_path, &json)
            .unwrap_or_else(|e| panic!("Failed to write {}: {e}", out_path.display()));
    } else {
        println!("{json}");
    }

    // Emit per-locale content-i18n sidecars (card-data.<code>.json) into the
    // sidecar dir, independent of whether the main export went to stdout or a file.
    if let Some(ref dir) = sidecar_dir {
        for (code, map) in &sidecars {
            write_sidecar(dir, code, map);
        }
        eprintln!(
            "Localized sidecars written: {} locales ({})",
            sidecars.len(),
            sidecars
                .iter()
                .map(|(c, m)| format!("{c}:{}", m.len()))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }

    if let Some(names_path) = names_out {
        let mut names: Vec<&str> = face_index.values().map(|e| e.face.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        let names_json = serde_json::to_string(&names).expect("Failed to serialize card names");
        std::fs::write(&names_path, names_json)
            .unwrap_or_else(|e| panic!("Failed to write {}: {e}", names_path.display()));
        eprintln!("Card names written: {} names", names.len());
    }

    if stats {
        eprintln!("Total cards: {total_cards}");
        eprintln!("Faces indexed: {}", face_index.len());
        eprintln!("Cards with unimplemented effects: {cards_with_unimplemented}");
        let implemented = total_cards.saturating_sub(cards_with_unimplemented);
        let pct = if total_cards > 0 {
            (implemented as f64 / total_cards as f64) * 100.0
        } else {
            0.0
        };
        eprintln!("Fully implemented: {implemented}/{total_cards} ({pct:.1}%)");
    }
}

fn run_semantic_audit(remaining_args: &[String]) {
    // Parse optional data dir from remaining args
    let card_data_path = if let Some(dir) = remaining_args.first() {
        PathBuf::from(dir).join("card-data.json")
    } else {
        // Default: try PHASE_DATA_DIR, then client/public/card-data.json
        std::env::var("PHASE_CARDS_PATH")
            .map(|p| PathBuf::from(p).join("card-data.json"))
            .or_else(|_| {
                std::env::var("PHASE_DATA_DIR").map(|d| PathBuf::from(d).join("card-data.json"))
            })
            .unwrap_or_else(|_| PathBuf::from("client/public/card-data.json"))
    };

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "oracle_gen=info,engine=info".parse().unwrap()),
        )
        .init();

    if !card_data_path.exists() {
        eprintln!(
            "Error: card-data.json not found at {}",
            card_data_path.display()
        );
        eprintln!("Run ./scripts/gen-card-data.sh first, or pass a data directory.");
        process::exit(1);
    }

    eprintln!("Loading card database from {}...", card_data_path.display());
    let card_db = match CardDatabase::from_export(&card_data_path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Error loading card database: {e}");
            process::exit(1);
        }
    };

    eprintln!("Running semantic audit...");
    let summary = audit_semantic(&card_db);

    eprintln!(
        "Audit complete: {} cards audited, {} with findings",
        summary.total_supported_audited, summary.cards_with_findings
    );

    // Write JSON output
    let json_path = PathBuf::from("data/semantic-audit.json");
    let json_str =
        serde_json::to_string_pretty(&summary).expect("Failed to serialize audit summary");
    std::fs::write(&json_path, &json_str)
        .unwrap_or_else(|e| panic!("Failed to write {}: {e}", json_path.display()));
    eprintln!("JSON written to {}", json_path.display());

    // Write markdown output
    let md_path = PathBuf::from("data/semantic-audit.md");
    let md_str = format_semantic_audit_markdown(&summary);
    std::fs::write(&md_path, &md_str)
        .unwrap_or_else(|e| panic!("Failed to write {}: {e}", md_path.display()));
    eprintln!("Markdown written to {}", md_path.display());

    // Print summary to stdout
    for (category, count) in &summary.finding_counts {
        eprintln!("  {category}: {count}");
    }
}

/// Pretty-print the WotC rulings for a card. Useful during parser authoring
/// to verify parsed AbilityDefinitions don't contradict official rulings.
///
/// Usage: `cargo run --bin oracle-gen -- rulings "<card name>"`
fn run_rulings(remaining_args: &[String]) {
    let Some(card_name) = remaining_args.first() else {
        eprintln!("Usage: oracle-gen rulings <card name> [data-dir]");
        process::exit(1);
    };

    let card_data_path = if let Some(dir) = remaining_args.get(1) {
        PathBuf::from(dir).join("card-data.json")
    } else {
        std::env::var("PHASE_CARDS_PATH")
            .map(|p| PathBuf::from(p).join("card-data.json"))
            .or_else(|_| {
                std::env::var("PHASE_DATA_DIR").map(|d| PathBuf::from(d).join("card-data.json"))
            })
            .unwrap_or_else(|_| PathBuf::from("client/public/card-data.json"))
    };

    if !card_data_path.exists() {
        eprintln!(
            "Error: card-data.json not found at {}",
            card_data_path.display()
        );
        eprintln!("Run ./scripts/gen-card-data.sh first, or pass a data directory.");
        process::exit(1);
    }

    let card_db = match CardDatabase::from_export(&card_data_path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Error loading card database: {e}");
            process::exit(1);
        }
    };

    let rulings = card_db.rulings_for(card_name);
    if rulings.is_empty() {
        eprintln!("No rulings found for '{card_name}'.");
        eprintln!("(Note: rulings are attached to the front face of multi-face cards.)");
        return;
    }

    println!("Rulings for {card_name}:");
    for ruling in rulings {
        println!("  [{}] {}", ruling.date, ruling.text);
    }
}

/// Top-level wrapper for MTGJSON's `SetList.json` file.
#[derive(Deserialize)]
struct SetListFile {
    data: Vec<SetListRawEntry>,
}

/// Raw SetList entry — only the fields we forward to the frontend.
/// Fields we ignore (decks, sealedProduct, translations, keyruneCode, languages,
/// mcm/mtgo/tcgplayer metadata, isFoilOnly, totalSetSize, block) would bloat the
/// sidecar by ~10x with no current consumer.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetListRawEntry {
    code: String,
    name: String,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default, rename = "type")]
    set_type: Option<String>,
    #[serde(default)]
    is_online_only: bool,
    #[serde(default)]
    base_set_size: Option<u32>,
    #[serde(default)]
    parent_code: Option<String>,
}

/// Projected SetList entry written to `client/public/set-list.json`. Keys are
/// camelCase so the frontend can use them verbatim without renaming.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SetListEntry {
    code: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    release_date: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    set_type: Option<String>,
    is_online_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_set_size: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_code: Option<String>,
}

/// Project MTGJSON's `SetList.json` down to the fields the frontend actually
/// needs (~10% of the source file's size). Input is `<data-dir>/mtgjson/SetList.json`
/// by default; output goes to `<data-dir>/set-list.json` (or stdout if no data dir).
///
/// Usage: `cargo run --bin oracle-gen -- set-list <data-dir> [output-path]`
fn run_set_list(remaining_args: &[String]) {
    let Some(data_dir) = remaining_args.first() else {
        eprintln!("Usage: oracle-gen set-list <data-dir> [output-path]");
        process::exit(1);
    };
    let input = PathBuf::from(data_dir).join("mtgjson").join("SetList.json");
    let output = remaining_args
        .get(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("client/public/set-list.json"));

    if !input.exists() {
        eprintln!(
            "Error: SetList.json not found at {}. Run ./scripts/gen-card-data.sh first.",
            input.display()
        );
        process::exit(1);
    }

    let contents = std::fs::read_to_string(&input)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", input.display()));
    let raw: SetListFile = serde_json::from_str(&contents)
        .unwrap_or_else(|e| panic!("Failed to parse {}: {e}", input.display()));

    // Release-gate: hide gated sets from the picker / draft / deck-builder UIs.
    // Sets past their release date are auto-unlocked. See `database::set_gating`.
    let set_catalog = load_set_catalog(Path::new(data_dir));
    let gated_sets = set_gating::resolve_gated_sets(&set_catalog);
    let projected: BTreeMap<String, SetListEntry> = raw
        .data
        .into_iter()
        .filter(|s| !set_gating::is_set_gated(&s.code, &gated_sets))
        .map(|s| {
            (
                s.code.clone(),
                SetListEntry {
                    code: s.code,
                    name: s.name,
                    release_date: s.release_date,
                    set_type: s.set_type,
                    is_online_only: s.is_online_only,
                    base_set_size: s.base_set_size,
                    parent_code: s.parent_code,
                },
            )
        })
        .collect();

    let json = serde_json::to_string(&projected).expect("SetListEntry serialization cannot fail");
    std::fs::write(&output, &json)
        .unwrap_or_else(|e| panic!("Failed to write {}: {e}", output.display()));
    eprintln!(
        "Projected {} sets to {} ({} bytes)",
        projected.len(),
        output.display(),
        json.len()
    );
}

/// MTGJSON per-deck file wrapper: `{ "meta": {...}, "data": { ... } }`.
#[derive(Deserialize)]
struct DeckFile {
    data: DeckRaw,
}

/// Raw deck payload. MTGJSON deck entries carry ~200 fields per card
/// (localized names, purchase URLs, printings, identifiers); we only need
/// name + count, so everything else is dropped via `#[serde(default)]` +
/// ignoring unknown fields.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeckRaw {
    code: String,
    name: String,
    #[serde(rename = "type")]
    deck_type: String,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    main_board: Vec<DeckCardRaw>,
    #[serde(default)]
    side_board: Vec<DeckCardRaw>,
    #[serde(default)]
    commander: Vec<DeckCardRaw>,
}

#[derive(Deserialize)]
struct DeckCardRaw {
    name: String,
    #[serde(default = "default_count")]
    count: u32,
}

fn default_count() -> u32 {
    1
}

/// Projected deck entry written to `client/public/decks.json`.
///
/// `coverage_pct` is the percentage of mainboard+commander cards (counting
/// duplicates) the engine can currently play; `unsupported` lists the unique
/// card names that fall short. Both are surfaced to the precon picker so the
/// UI can show the same coverage-floor slider used by the AI deck picker —
/// the user picks any deck they want, and the slider just controls how much
/// of the catalog is visible by default.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeckEntry {
    code: String,
    name: String,
    #[serde(rename = "type")]
    deck_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    release_date: Option<String>,
    coverage_pct: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unsupported: Vec<String>,
    main_board: Vec<DeckCardEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    side_board: Vec<DeckCardEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    commander: Vec<DeckCardEntry>,
}

#[derive(Serialize)]
struct DeckCardEntry {
    name: String,
    count: u32,
}

fn project_deck_cards(raw: &[DeckCardRaw]) -> Vec<DeckCardEntry> {
    raw.iter()
        .map(|c| DeckCardEntry {
            name: c.name.clone(),
            count: c.count,
        })
        .collect()
}

/// Total physical cards in main + commander (the "is this actually a deck?"
/// yardstick). Sideboard is excluded because many non-decks (Secret Lair,
/// Jumpstart, sample product) list their whole content under mainBoard.
fn deck_card_total(raw: &DeckRaw) -> u32 {
    raw.main_board.iter().map(|c| c.count).sum::<u32>()
        + raw.commander.iter().map(|c| c.count).sum::<u32>()
}

/// Minimum card count to qualify as a "deck" (MTG Limited-format minimum,
/// CR 100.2a). Anything smaller is a product — Secret Lair Drops, half-
/// jumpstart packs, welcome boosters, sample decks, toolkits, etc. MTGJSON
/// ships all of these in AllDeckFiles and distinguishing them by `type`
/// alone is brittle (every new product line invents a new type string).
const MIN_DECK_CARDS: u32 = 40;

/// Per-deck engine coverage. `unsupported` is the deduped list of card names
/// the parser/runtime can't currently play (preserves first-seen order so the
/// list reads in deck order). `coverage_pct` is the share of mainboard +
/// commander *copies* (counting duplicates) that ARE playable, rounded to
/// the nearest percent — by-count rather than by-unique so a 30-Forest deck
/// with one missing card scores ~96%, matching the share of gameplay that
/// works. Sideboard is excluded from the percentage because the sideboard is
/// optional and skews the score for products that pile flavor text into it.
struct DeckCoverage {
    coverage_pct: u32,
    unsupported: Vec<String>,
}

fn compute_coverage(raw: &DeckRaw, db: &CardDatabase) -> DeckCoverage {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut unsupported: Vec<String> = Vec::new();
    let mut unsupported_lc: std::collections::HashSet<String> = std::collections::HashSet::new();
    let sections = [&raw.main_board, &raw.side_board, &raw.commander];
    for section in sections {
        for card in section.iter() {
            let lc = card.name.to_lowercase();
            if seen.insert(lc.clone()) && !engine::database::is_card_playable(db, &card.name) {
                unsupported.push(card.name.clone());
                unsupported_lc.insert(lc);
            }
        }
    }

    // Counted copies in main + commander (sideboard excluded — see doc above).
    let mut total: u32 = 0;
    let mut playable: u32 = 0;
    for section in [&raw.main_board, &raw.commander] {
        for card in section.iter() {
            total += card.count;
            if !unsupported_lc.contains(&card.name.to_lowercase()) {
                playable += card.count;
            }
        }
    }
    let coverage_pct = if total == 0 {
        0
    } else {
        ((playable as f64 / total as f64) * 100.0).round() as u32
    };

    DeckCoverage {
        coverage_pct,
        unsupported,
    }
}

/// Ingest MTGJSON's `AllDeckFiles` (one JSON per deck extracted under
/// `<data-dir>/mtgjson/decks/`) and project every deck above `MIN_DECK_CARDS`
/// into a flat map keyed by deck filename stem. Each entry carries a
/// `coveragePct` and `unsupported` list so the precon picker can surface a
/// coverage-floor slider rather than dropping decks at build time. Coverage
/// is informational; the user is allowed to pick any deck.
///
/// `--emit-skipped` is accepted but is now a no-op (the previous build of
/// this command emitted a `decks-skipped.json` sidecar — that data is now
/// inline on every entry that has unsupported cards, so the sidecar would
/// be redundant). The flag is retained so existing scripts continue working.
///
/// Usage: `cargo run --bin oracle-gen -- decks <data-dir> [output-path] [--emit-skipped]`
fn run_decks(remaining_args: &[String]) {
    let mut positional: Vec<&String> = Vec::new();
    for arg in remaining_args {
        if arg == "--emit-skipped" {
            // Accepted for backward compatibility; coverage data is always
            // inline now (see doc comment above).
        } else {
            positional.push(arg);
        }
    }

    let Some(data_dir) = positional.first() else {
        eprintln!("Usage: oracle-gen decks <data-dir> [output-path] [--emit-skipped]");
        process::exit(1);
    };
    let decks_dir = PathBuf::from(data_dir).join("mtgjson").join("decks");
    let output = positional
        .get(1)
        .map(|s| PathBuf::from(s.as_str()))
        .unwrap_or_else(|| PathBuf::from("client/public/decks.json"));

    if !decks_dir.is_dir() {
        eprintln!(
            "Error: decks directory not found at {}. Extract AllDeckFiles.tar.gz first.",
            decks_dir.display()
        );
        process::exit(1);
    }

    let card_data_path = PathBuf::from(data_dir).join("../client/public/card-data.json");
    let card_data_path = if card_data_path.exists() {
        card_data_path
    } else {
        PathBuf::from("client/public/card-data.json")
    };
    if !card_data_path.exists() {
        eprintln!(
            "Error: card-data.json not found at {}. Run oracle-gen card export first.",
            card_data_path.display()
        );
        process::exit(1);
    }

    let card_db = match CardDatabase::from_export(&card_data_path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Error loading card database: {e}");
            process::exit(1);
        }
    };

    let mut included: BTreeMap<String, DeckEntry> = BTreeMap::new();
    let mut fully_playable: u32 = 0;
    let mut partially_playable: u32 = 0;
    let mut too_small: u32 = 0;
    let mut read_errors: u32 = 0;

    let entries = std::fs::read_dir(&decks_dir)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", decks_dir.display()));
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let contents = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => {
                read_errors += 1;
                continue;
            }
        };
        let parsed: DeckFile = match serde_json::from_str(&contents) {
            Ok(d) => d,
            Err(_) => {
                read_errors += 1;
                continue;
            }
        };
        let deck_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        let mut raw = parsed.data;
        // Strip officially-removed cards from every section at ingestion, so
        // they never reach decks.json, coverage, or the deck-size threshold.
        // Single authority shared with the card-export path above.
        for section in [&mut raw.main_board, &mut raw.side_board, &mut raw.commander] {
            section.retain(|c| !is_removed_offensive_card(&c.name));
        }
        if deck_card_total(&raw) < MIN_DECK_CARDS {
            too_small += 1;
            continue;
        }
        let coverage = compute_coverage(&raw, &card_db);
        if coverage.unsupported.is_empty() {
            fully_playable += 1;
        } else {
            partially_playable += 1;
        }
        included.insert(
            deck_id,
            DeckEntry {
                code: raw.code,
                name: raw.name,
                deck_type: raw.deck_type,
                release_date: raw.release_date,
                coverage_pct: coverage.coverage_pct,
                unsupported: coverage.unsupported,
                main_board: project_deck_cards(&raw.main_board),
                side_board: project_deck_cards(&raw.side_board),
                commander: project_deck_cards(&raw.commander),
            },
        );
    }

    let json = serde_json::to_string(&included).expect("DeckEntry serialization cannot fail");
    std::fs::write(&output, &json)
        .unwrap_or_else(|e| panic!("Failed to write {}: {e}", output.display()));
    eprintln!(
        "Wrote {} decks to {} ({} bytes; {} fully playable, {} partially playable, {} dropped as non-decks (<{} cards), {} read errors)",
        included.len(),
        output.display(),
        json.len(),
        fully_playable,
        partially_playable,
        too_small,
        MIN_DECK_CARDS,
        read_errors
    );
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::OnceLock;

    use engine::database::mtgjson::{
        load_atomic_cards, AtomicCard, AtomicCardsFile, AtomicIdentifiers, SetIdentifiers,
        SetRelatedCards,
    };
    use engine::types::ability::TargetFilter;
    use engine::types::card::CardFace;
    use engine::types::keywords::Keyword;
    use serde_json::json;

    use super::*;

    fn make_entry(oracle_id: &str, printings: &[&str], layout: Option<&str>) -> CardExportEntry {
        make_entry_with_legalities(oracle_id, printings, layout, &[])
    }

    fn make_entry_with_legalities(
        oracle_id: &str,
        printings: &[&str],
        layout: Option<&str>,
        legalities: &[(&str, &str)],
    ) -> CardExportEntry {
        CardExportEntry {
            face: CardFace {
                scryfall_oracle_id: Some(oracle_id.to_string()),
                ..Default::default()
            },
            legalities: legalities
                .iter()
                .map(|(format, status)| (format.to_string(), status.to_string()))
                .collect(),
            layout: layout.map(|s| s.to_string()),
            face_index: None,
            printings: printings.iter().map(|s| s.to_string()).collect(),
            rulings: Vec::new(),
            rarities: BTreeSet::new(),
            bracket_signals: BracketSignals::default(),
        }
    }

    #[test]
    fn card_export_entry_serializes_multiface_face_index() {
        let mut entry = make_entry("ordered-oracle", &["TST"], Some("transform"));
        entry.face.name = "Back Face".to_string();
        entry.face_index = Some(1);

        let value = serde_json::to_value(&entry).expect("entry should serialize");

        assert_eq!(value["face_index"], json!(1));
    }

    fn atomic_single(name: &str, oracle_id: Option<&str>) -> AtomicCard {
        AtomicCard {
            name: name.to_string(),
            mana_cost: None,
            colors: Vec::new(),
            color_identity: Vec::new(),
            power: None,
            toughness: None,
            loyalty: None,
            defense: None,
            text: None,
            layout: "normal".to_string(),
            type_line: None,
            types: Vec::new(),
            subtypes: Vec::new(),
            supertypes: Vec::new(),
            keywords: None,
            side: None,
            face_name: None,
            mana_value: 0.0,
            legalities: HashMap::new(),
            leadership_skills: None,
            printings: Vec::new(),
            rulings: Vec::new(),
            is_game_changer: false,
            identifiers: AtomicIdentifiers {
                scryfall_id: None,
                scryfall_oracle_id: oracle_id.map(str::to_string),
            },
            foreign_data: Vec::new(),
            related_cards: SetRelatedCards::default(),
        }
    }

    fn set_card_with_metadata(
        name: &str,
        face_name: Option<&str>,
        oracle_id: Option<&str>,
        printing_id: Option<&str>,
        tokens: &[&str],
        spellbook: &[&str],
    ) -> SetCard {
        SetCard {
            uuid: format!("{name}-uuid"),
            name: name.to_string(),
            face_name: face_name.map(str::to_string),
            rarity: "rare".to_string(),
            identifiers: SetIdentifiers {
                scryfall_id: printing_id.map(str::to_string),
                scryfall_oracle_id: oracle_id.map(str::to_string),
            },
            related_cards: SetRelatedCards {
                tokens: tokens.iter().map(|token| token.to_string()).collect(),
                reverse_related: Vec::new(),
                spellbook: spellbook.iter().map(|card| card.to_string()).collect(),
            },
        }
    }

    #[test]
    fn is_homonym_atomic_group_detects_distinct_single_faced_oracle_ids() {
        let faces = vec![
            atomic_single("Shared Name", Some("paper-oracle")),
            atomic_single("Shared Name", Some("playtest-oracle")),
        ];
        assert!(is_homonym_atomic_group(&faces));
    }

    #[test]
    fn is_homonym_atomic_group_rejects_true_multiface_cards() {
        let atomic = load_atomic_fixture();
        let faces = atomic
            .data
            .get("Aang, Swift Savior // Aang and La, Ocean's Fury")
            .expect("Aang faces should exist");
        assert!(
            !is_homonym_atomic_group(faces),
            "true multi-face cards must not be treated as homonyms"
        );
    }

    #[test]
    fn is_homonym_atomic_group_does_not_fall_back_to_multiface_for_missing_oracle_id() {
        let faces = vec![
            atomic_single("Shared Name", Some("known-oracle")),
            atomic_single("Shared Name", None),
        ];
        assert!(
            is_homonym_atomic_group(&faces),
            "all-single groups with missing oracle ids should stay in standalone collision resolution"
        );
    }

    #[test]
    fn is_homonym_atomic_group_rejects_duplicate_known_oracle_ids() {
        let faces = vec![
            atomic_single("Shared Name", Some("same-oracle")),
            atomic_single("Shared Name", Some("same-oracle")),
        ];
        assert!(!is_homonym_atomic_group(&faces));
    }

    #[test]
    fn token_source_metadata_preserves_spellbook_and_printing_ids() {
        let set_card = set_card_with_metadata(
            "Spellbook Source",
            None,
            Some("spellbook-oracle"),
            Some("spellbook-printing"),
            &["token-id"],
            &["Draft Pick"],
        );
        let mut map = HashMap::new();
        let mut metadata = TokenSourceMetadata::default();
        metadata
            .related_token_ids
            .extend(set_card.related_cards.tokens.clone());
        metadata
            .source_printing_ids
            .insert(set_card.identifiers.scryfall_id.clone().unwrap());
        metadata.spellbook = set_card.related_cards.spellbook.clone();
        map.insert(TokenSourceMetadataKey::from_set_card(&set_card), metadata);

        let source = atomic_single("Spellbook Source", Some("spellbook-oracle"));
        let mut face = CardFace {
            name: "Spellbook Source".to_string(),
            scryfall_oracle_id: Some("spellbook-oracle".to_string()),
            ..Default::default()
        };
        stamp_token_source_metadata(&mut face, &source, &map);

        assert_eq!(
            face.metadata.related_token_ids,
            vec!["token-id".to_string()]
        );
        assert_eq!(
            face.metadata.source_printing_ids,
            vec!["spellbook-printing".to_string()]
        );
        assert_eq!(face.metadata.spellbook, vec!["Draft Pick".to_string()]);
    }

    #[test]
    fn token_source_metadata_disambiguates_homonymous_oracle_ids() {
        let first = set_card_with_metadata(
            "Shared Name",
            None,
            Some("first-oracle"),
            Some("first-printing"),
            &["first-token"],
            &[],
        );
        let second = set_card_with_metadata(
            "Shared Name",
            None,
            Some("second-oracle"),
            Some("second-printing"),
            &["second-token"],
            &[],
        );
        let mut map = HashMap::new();
        for set_card in [&first, &second] {
            let mut metadata = TokenSourceMetadata::default();
            metadata
                .related_token_ids
                .extend(set_card.related_cards.tokens.clone());
            metadata
                .source_printing_ids
                .insert(set_card.identifiers.scryfall_id.clone().unwrap());
            map.insert(TokenSourceMetadataKey::from_set_card(set_card), metadata);
        }

        let source = atomic_single("Shared Name", Some("second-oracle"));
        let mut face = CardFace {
            name: "Shared Name".to_string(),
            scryfall_oracle_id: Some("second-oracle".to_string()),
            ..Default::default()
        };
        stamp_token_source_metadata(&mut face, &source, &map);

        assert_eq!(
            face.metadata.related_token_ids,
            vec!["second-token".to_string()]
        );
        assert_eq!(
            face.metadata.source_printing_ids,
            vec!["second-printing".to_string()]
        );
    }

    #[test]
    fn homonym_insert_prefers_legalities_before_printings() {
        let mut map = BTreeMap::new();
        insert_face_with_priority(
            &mut map,
            "Pick Your Poison",
            "pick your poison".to_string(),
            make_entry_with_legalities("playtest-oracle", &["CMB1", "CMB2", "MB2"], None, &[]),
            homonym_face_priority,
        );
        insert_face_with_priority(
            &mut map,
            "Pick Your Poison",
            "pick your poison".to_string(),
            make_entry_with_legalities(
                "mkm-oracle",
                &["MKM"],
                None,
                &[("modern", "legal"), ("pioneer", "legal")],
            ),
            homonym_face_priority,
        );
        assert_eq!(
            map["pick your poison"].face.scryfall_oracle_id.as_deref(),
            Some("mkm-oracle"),
            "homonym paper card with format legalities must beat a playtest card with more printings"
        );
        assert_eq!(
            map["pick your poison"]
                .legalities
                .get("modern")
                .map(String::as_str),
            Some("legal"),
        );
    }

    #[test]
    fn ordinary_same_class_insert_keeps_printings_before_legalities() {
        let mut map = BTreeMap::new();
        insert_face(
            &mut map,
            "Shared",
            "shared".to_string(),
            make_entry_with_legalities("many-printings", &["A", "B", "C"], None, &[]),
        );
        insert_face(
            &mut map,
            "Shared",
            "shared".to_string(),
            make_entry_with_legalities("legal-card", &["D"], None, &[("modern", "legal")]),
        );
        assert_eq!(
            map["shared"].face.scryfall_oracle_id.as_deref(),
            Some("many-printings"),
            "ordinary same-class collisions keep the existing printings-first policy"
        );
    }

    #[test]
    fn insert_face_preserves_losing_multiface_entry_under_hidden_key() {
        let mut map = BTreeMap::new();
        insert_face(
            &mut map,
            "Emeritus of Truce // Swords to Plowshares",
            "swords to plowshares".to_string(),
            make_entry("sos-oracle", &["SOS"], Some("prepare")),
        );
        insert_face(
            &mut map,
            "Swords to Plowshares",
            "swords to plowshares".to_string(),
            make_entry("paper-oracle", &["2ED", "ICE", "MMA"], None),
        );

        assert_eq!(
            map["swords to plowshares"]
                .face
                .scryfall_oracle_id
                .as_deref(),
            Some("paper-oracle"),
            "canonical face-name lookup still prefers the standalone card"
        );
        assert_eq!(
            map["swords to plowshares [sos-oracle]"]
                .face
                .scryfall_oracle_id
                .as_deref(),
            Some("sos-oracle"),
            "printed-card rehydration must retain the prepare back face"
        );
    }

    #[test]
    fn insert_face_standalone_beats_multiface_even_with_fewer_printings() {
        let mut map = BTreeMap::new();
        // Insert the SOS DFC back-face first (wrong winner if we only looked at
        // printings order of insertion).
        insert_face(
            &mut map,
            "Emeritus of Truce // Swords to Plowshares",
            "swords to plowshares".to_string(),
            make_entry("sos-oracle", &["SOS"], Some("prepare")),
        );
        // Then insert paper Swords to Plowshares as a standalone.
        insert_face(
            &mut map,
            "Swords to Plowshares",
            "swords to plowshares".to_string(),
            make_entry("paper-oracle", &["2ED", "ICE", "MMA"], None),
        );
        assert_eq!(
            map["swords to plowshares"]
                .face
                .scryfall_oracle_id
                .as_deref(),
            Some("paper-oracle"),
            "standalone MTGJSON entry must win over a multi-face back face"
        );
    }

    #[test]
    fn insert_face_standalone_insertion_order_does_not_matter() {
        // Reverse order vs. the test above — paper first, then DFC.
        let mut map = BTreeMap::new();
        insert_face(
            &mut map,
            "Swords to Plowshares",
            "swords to plowshares".to_string(),
            make_entry("paper-oracle", &["2ED", "ICE", "MMA"], None),
        );
        insert_face(
            &mut map,
            "Emeritus of Truce // Swords to Plowshares",
            "swords to plowshares".to_string(),
            make_entry("sos-oracle", &["SOS"], Some("prepare")),
        );
        assert_eq!(
            map["swords to plowshares"]
                .face
                .scryfall_oracle_id
                .as_deref(),
            Some("paper-oracle"),
        );
    }

    #[test]
    fn insert_face_within_same_class_more_printings_wins() {
        // Both entries are multi-face (e.g., two unrelated split cards sharing
        // a face name). Structural tiebreaker is a draw; printings count decides.
        let mut map = BTreeMap::new();
        insert_face(
            &mut map,
            "A // Shared",
            "shared".to_string(),
            make_entry("older", &["INV"], Some("split")),
        );
        insert_face(
            &mut map,
            "B // Shared",
            "shared".to_string(),
            make_entry("newer", &["MH1", "MH3"], Some("split")),
        );
        assert_eq!(
            map["shared"].face.scryfall_oracle_id.as_deref(),
            Some("newer"),
        );
    }

    #[test]
    fn insert_face_tied_printings_keep_first_inserted() {
        // Iteration order of the caller is sorted, so "first-inserted" is
        // deterministic. The Start // Finish vs. Start // Fire case.
        let mut map = BTreeMap::new();
        insert_face(
            &mut map,
            "Start // Finish",
            "start".to_string(),
            make_entry("finish-oracle", &["AKH", "PLST"], Some("aftermath")),
        );
        insert_face(
            &mut map,
            "Start // Fire",
            "start".to_string(),
            make_entry("fire-oracle", &["SOS", "PLST"], Some("split")),
        );
        assert_eq!(
            map["start"].face.scryfall_oracle_id.as_deref(),
            Some("finish-oracle"),
            "on a tie, first-inserted wins"
        );
    }

    /// DATA-PIPELINE guard for the Alchemy spellbook fix. Reverting the
    /// unconditional `merge_atomic_spellbooks` fold (or the `related_cards`
    /// capture on `AtomicCard`) empties the map and flips this test red. The
    /// path deliberately has NO `sets/` subdir, so ONLY the AtomicCards harvest
    /// can populate the spellbook — exercising the fold in isolation.
    #[test]
    fn build_token_source_metadata_harvests_spellbook_from_atomic() {
        // Drafting source with a 12-name Alchemy spellbook.
        let spellbook: Vec<String> = (1..=12).map(|i| format!("Spell {i}")).collect();
        let mut source = atomic_single("Tome Test", Some("tome-oracle"));
        source.related_cards.spellbook = spellbook.clone();

        // Reach-guard: a second card WITH a spellbook must get its own entry.
        let mut other = atomic_single("Second Source", Some("second-oracle"));
        other.related_cards.spellbook = vec!["A".to_string(), "B".to_string()];

        // Negative: a card with an EMPTY spellbook must create no entry.
        let empty = atomic_single("No Spellbook", Some("none-oracle"));

        let mut data: HashMap<String, Vec<AtomicCard>> = HashMap::new();
        data.insert("Tome Test".to_string(), vec![source]);
        data.insert("Second Source".to_string(), vec![other]);
        data.insert("No Spellbook".to_string(), vec![empty]);
        let atomic = AtomicCardsFile { data };

        // Temp path whose parent has no `sets/` subdir → set-file loop skipped.
        let dir =
            std::env::temp_dir().join(format!("phase-spellbook-harvest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir should be creatable");
        let mtgjson_path = dir.join("AtomicCards.json");

        let map = build_token_source_metadata(&mtgjson_path, &atomic);

        // Atomic spellbooks key the same way the set-file loop does: oracle id when
        // present, face name (here == lowercased name, as face_name is None) as qualifier.
        let oracle_key = |oracle: &str, name: &str| TokenSourceMetadataKey::Oracle {
            oracle_id: oracle.to_string(),
            face_name: name.to_lowercase(),
        };

        assert_eq!(
            map[&oracle_key("tome-oracle", "Tome Test")].spellbook,
            spellbook,
            "12-name spellbook must be harvested from AtomicCards even with no set files"
        );
        assert_eq!(
            map[&oracle_key("tome-oracle", "Tome Test")].spellbook.len(),
            12
        );
        assert_eq!(
            map[&oracle_key("second-oracle", "Second Source")].spellbook,
            vec!["A".to_string(), "B".to_string()],
            "a second spellbook source must yield its own populated entry"
        );
        assert!(
            !map.contains_key(&oracle_key("none-oracle", "No Spellbook")),
            "a card with an empty spellbook must not create a map entry"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn load_atomic_fixture() -> &'static AtomicCardsFile {
        static ATOMIC: OnceLock<AtomicCardsFile> = OnceLock::new();
        ATOMIC.get_or_init(|| {
            let path =
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/mtgjson/AtomicCards.json");
            load_atomic_cards(&path).expect("AtomicCards.json should load")
        })
    }

    #[test]
    fn export_layout_keeps_aang_front_face_keywords_face_local() {
        let atomic = load_atomic_fixture();
        let faces = atomic
            .data
            .get("Aang, Swift Savior // Aang and La, Ocean's Fury")
            .expect("Aang faces should exist");
        let oracle_id = faces[0].identifiers.scryfall_oracle_id.clone();
        let layout = build_export_layout(faces, oracle_id, map_layout(&faces[0].layout));
        let layout_face_refs = layout_faces(&layout);
        let front = layout_face_refs
            .iter()
            .find(|face| face.name == "Aang, Swift Savior")
            .expect("front face should exist");

        assert!(front.keywords.contains(&Keyword::Flash));
        assert!(front.keywords.contains(&Keyword::Flying));
        assert!(!front.keywords.contains(&Keyword::Reach));
        assert!(!front.keywords.contains(&Keyword::Trample));
    }

    #[test]
    fn export_layout_keeps_floodpits_etb_counter_on_parent_target() {
        let atomic = load_atomic_fixture();
        let faces = atomic
            .data
            .get("Floodpits Drowner")
            .expect("Floodpits should exist");
        let oracle_id = faces[0].identifiers.scryfall_oracle_id.clone();
        let layout = build_export_layout(faces, oracle_id, map_layout(&faces[0].layout));
        let face = match layout {
            CardLayout::Single(face) => face,
            other => panic!("expected single-face layout, got {other:?}"),
        };
        let trigger = face.triggers.first().expect("ETB trigger should exist");
        let sub = trigger
            .execute
            .as_ref()
            .and_then(|ability| ability.sub_ability.as_ref())
            .expect("ETB should chain into PutCounter");

        match &*sub.effect {
            engine::types::ability::Effect::PutCounter { target, .. } => {
                assert!(matches!(target, TargetFilter::ParentTarget));
            }
            other => panic!("expected PutCounter sub-ability, got {other:?}"),
        }
    }
}
