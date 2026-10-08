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

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
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
use serde_json::Value;
use sha2::{Digest, Sha256};

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

const MARKERS: &[&str] = &[
    "all",
    "another",
    "each",
    "five",
    "four",
    "if",
    "its",
    "may",
    "nine",
    "noncreature",
    "nonland",
    "nontoken",
    "not",
    "one",
    "only",
    "opponent",
    "opponents",
    "other",
    "seven",
    "six",
    "ten",
    "their",
    "three",
    "two",
    "unless",
    "until",
    "you",
    "your",
];

type OmittedString = engine::parser::audit_projection::OmittedDefinitionDescription;

fn omit_definition_descriptions(
    value: &mut Value,
    pointer: &str,
    omitted: &mut Vec<OmittedString>,
) {
    omitted
        .extend(engine::parser::audit_projection::omit_definition_descriptions_at(value, pointer));
}

fn structural_projection(face: &CardFace) -> (Value, Vec<OmittedString>) {
    let mut value = serde_json::to_value(CanonicalFace::from_face(face))
        .expect("CanonicalFace is composed of serializable parser types");
    value
        .as_object_mut()
        .expect("CanonicalFace serializes as an object")
        .remove("oracle_text");
    let mut omitted = Vec::new();
    omit_definition_descriptions(&mut value, "", &mut omitted);
    omitted.sort();
    (value, omitted)
}

fn is_rules_bearing(projection: &Value) -> bool {
    projection.as_object().is_some_and(|map| {
        map.values().any(|value| match value {
            Value::Null => false,
            Value::Array(values) => !values.is_empty(),
            Value::Object(values) => !values.is_empty(),
            _ => true,
        })
    })
}

fn is_name_boundary(byte: Option<u8>) -> bool {
    byte.is_none_or(|b| !b.is_ascii_alphanumeric() && b != b'\'')
}

fn normalize_self_name(text: &str, name: &str) -> String {
    if name.is_empty() {
        return text.to_ascii_lowercase();
    }
    let lower = text.to_ascii_lowercase();
    let needle = name.to_ascii_lowercase();
    let mut result = String::with_capacity(lower.len());
    let mut offset = 0;
    while let Some(relative) = lower[offset..].find(&needle) {
        let start = offset + relative;
        let end = start + needle.len();
        if is_name_boundary(start.checked_sub(1).map(|i| lower.as_bytes()[i]))
            && is_name_boundary(lower.as_bytes().get(end).copied())
        {
            result.push_str(&lower[offset..start]);
            result.push('~');
            offset = end;
        } else {
            let next = start
                + lower[start..]
                    .chars()
                    .next()
                    .expect("find returned a character boundary")
                    .len_utf8();
            result.push_str(&lower[offset..next]);
            offset = next;
        }
    }
    result.push_str(&lower[offset..]);
    result
}

fn strip_balanced_reminder_text(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.char_indices().peekable();
    while let Some((start, character)) = chars.next() {
        if character == '(' {
            let mut depth = 1;
            let mut nested = false;
            let mut end = None;
            for (index, inner) in chars.by_ref() {
                match inner {
                    '(' => {
                        depth += 1;
                        nested = true;
                    }
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(index + inner.len_utf8());
                            break;
                        }
                    }
                    _ => {}
                }
            }
            match end {
                Some(end) if nested => result.push_str(&text[start..end]),
                Some(_) => {}
                None => {
                    result.push_str(&text[start..]);
                    break;
                }
            }
        } else {
            result.push(character);
        }
    }
    result
}

fn oracle_tokens(text: &str, name: &str) -> Vec<String> {
    let normalized = strip_balanced_reminder_text(&normalize_self_name(text, name));
    let bytes = normalized.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'~' {
            tokens.push("~".to_string());
            index += 1;
        } else if bytes[index].is_ascii_alphabetic() {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphabetic()
                    || (bytes[index] == b'\''
                        && index + 1 < bytes.len()
                        && bytes[index + 1].is_ascii_alphabetic()))
            {
                index += 1;
            }
            tokens.push(normalized[start..index].to_string());
        } else if bytes[index].is_ascii_digit() {
            let start = index;
            index += 1;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
            tokens.push(normalized[start..index].to_string());
        } else {
            index += 1;
        }
    }
    tokens
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct ChangedSpan {
    a_start: usize,
    a_end: usize,
    a_tokens: Vec<String>,
    b_start: usize,
    b_end: usize,
    b_tokens: Vec<String>,
}

fn lcs_spans(a: &[String], b: &[String]) -> (usize, Vec<ChangedSpan>) {
    let width = b.len() + 1;
    let mut lengths = vec![0usize; (a.len() + 1) * width];
    for ai in (0..a.len()).rev() {
        for bi in (0..b.len()).rev() {
            lengths[ai * width + bi] = if a[ai] == b[bi] {
                1 + lengths[(ai + 1) * width + bi + 1]
            } else {
                lengths[(ai + 1) * width + bi].max(lengths[ai * width + bi + 1])
            };
        }
    }
    let lcs = lengths[0];
    let (mut ai, mut bi) = (0, 0);
    let mut spans = Vec::new();
    while ai < a.len() || bi < b.len() {
        if ai < a.len() && bi < b.len() && a[ai] == b[bi] {
            ai += 1;
            bi += 1;
            continue;
        }
        let (a_start, b_start) = (ai, bi);
        while ai < a.len() || bi < b.len() {
            if ai < a.len() && bi < b.len() && a[ai] == b[bi] {
                break;
            }
            if ai == a.len() {
                bi += 1;
            } else if bi == b.len()
                || lengths[(ai + 1) * width + bi] >= lengths[ai * width + bi + 1]
            {
                ai += 1;
            } else {
                bi += 1;
            }
        }
        spans.push(ChangedSpan {
            a_start,
            a_end: ai,
            a_tokens: a[a_start..ai].to_vec(),
            b_start,
            b_end: bi,
            b_tokens: b[b_start..bi].to_vec(),
        });
    }
    (lcs, spans)
}

#[derive(Debug, Clone)]
struct CollisionMember {
    key: String,
    name: String,
    oracle_text: String,
    tokens: Vec<String>,
    omitted_strings: Vec<OmittedString>,
}

#[derive(Debug, Clone, Serialize)]
struct CandidateFace {
    key: String,
    name: String,
    oracle_text: String,
    omitted_strings: Vec<OmittedString>,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct Ratio {
    numerator: usize,
    denominator: usize,
}

#[derive(Debug, Clone, Serialize)]
struct CollisionCandidate {
    status: &'static str,
    a: CandidateFace,
    b: CandidateFace,
    identity_sensitive_omission: bool,
    similarity: Ratio,
    markers: Vec<String>,
    changed_spans: Vec<ChangedSpan>,
    natural_rebalance_pair: bool,
    #[serde(skip)]
    changed_token_count: usize,
}

#[derive(Serialize)]
struct ReportInput {
    path: &'static str,
    sha256: String,
}

#[derive(Serialize)]
struct ReportParameters {
    minimum_similarity: Ratio,
    markers_version: u8,
    projection_omits: [&'static str; 2],
    filter: String,
}

#[derive(Serialize)]
struct CollisionReport {
    schema_version: u8,
    algorithm_version: &'static str,
    result_kind: &'static str,
    input: ReportInput,
    parameters: ReportParameters,
    supported_cards: usize,
    rules_bearing_supported_cards: usize,
    identical_projected_groups: usize,
    largest_group_size: usize,
    pairs_examined: usize,
    pairs_rejected_by_length_bound: usize,
    candidate_pairs: usize,
    candidates: Vec<CollisionCandidate>,
}

fn natural_rebalance_pair(a: &str, b: &str) -> bool {
    fn without_prefix(name: &str) -> Option<&str> {
        name.get(..2)
            .filter(|prefix| prefix.eq_ignore_ascii_case("a-"))
            .map(|_| &name[2..])
    }
    without_prefix(a).is_some_and(|stripped| stripped.eq_ignore_ascii_case(b))
        || without_prefix(b).is_some_and(|stripped| stripped.eq_ignore_ascii_case(a))
}

fn candidate_face(member: &CollisionMember) -> CandidateFace {
    CandidateFace {
        key: member.key.clone(),
        name: member.name.clone(),
        oracle_text: member.oracle_text.clone(),
        omitted_strings: member.omitted_strings.clone(),
    }
}

fn compare_candidates(a: &CollisionCandidate, b: &CollisionCandidate) -> Ordering {
    let a_scaled = a.similarity.numerator * b.similarity.denominator;
    let b_scaled = b.similarity.numerator * a.similarity.denominator;
    b_scaled
        .cmp(&a_scaled)
        .then_with(|| a.changed_token_count.cmp(&b.changed_token_count))
        .then_with(|| a.a.key.cmp(&b.a.key))
        .then_with(|| a.b.key.cmp(&b.b.key))
        .then_with(|| a.a.name.cmp(&b.a.name))
        .then_with(|| a.b.name.cmp(&b.b.name))
}

fn build_collision_report(
    db: &CardDatabase,
    coverage: &[CardCoverageResult],
    selected_coverage: &[&CardCoverageResult],
    filter: String,
    input_digest: String,
) -> Result<CollisionReport, String> {
    let mut coverage_by_key = BTreeMap::new();
    for result in coverage {
        let key = result
            .card_face_key
            .as_deref()
            .ok_or_else(|| format!("coverage result has no face key: {}", result.card_name))?;
        if coverage_by_key.insert(key, result).is_some() {
            return Err(format!("duplicate coverage face key: {key}"));
        }
    }
    let selected: BTreeSet<String> = selected_coverage
        .iter()
        .map(|result| {
            result.card_face_key.clone().ok_or_else(|| {
                format!(
                    "selected coverage result has no face key: {}",
                    result.card_name
                )
            })
        })
        .collect::<Result<_, _>>()?;
    let mut supported_cards = 0;
    let mut rules_bearing_supported_cards = 0;
    let mut groups: BTreeMap<String, Vec<CollisionMember>> = BTreeMap::new();
    for (key, face) in db.face_iter() {
        if !selected.contains(key) {
            continue;
        }
        let coverage = coverage_by_key
            .get(key)
            .ok_or_else(|| format!("missing coverage result for face {key} ({})", face.name))?;
        if !coverage.supported {
            continue;
        }
        supported_cards += 1;
        let Some(oracle_text) = face
            .oracle_text
            .as_deref()
            .filter(|text| !text.trim().is_empty())
        else {
            continue;
        };
        let (projection, omitted_strings) = structural_projection(face);
        if !is_rules_bearing(&projection) {
            continue;
        }
        rules_bearing_supported_cards += 1;
        let projection_json = serde_json::to_string(&projection)
            .map_err(|error| format!("failed to serialize collision projection: {error}"))?;
        groups
            .entry(projection_json)
            .or_default()
            .push(CollisionMember {
                key: key.to_string(),
                name: face.name.clone(),
                oracle_text: oracle_text.to_string(),
                tokens: oracle_tokens(oracle_text, &face.name),
                omitted_strings,
            });
    }

    let mut identical_projected_groups = 0;
    let mut largest_group_size = 0;
    let mut pairs_examined = 0;
    let mut pairs_rejected_by_length_bound = 0;
    let marker_set: BTreeSet<&str> = MARKERS.iter().copied().collect();
    let mut candidates = Vec::new();
    for members in groups.values_mut().filter(|members| members.len() > 1) {
        identical_projected_groups += 1;
        largest_group_size = largest_group_size.max(members.len());
        members.sort_by(|a, b| a.key.cmp(&b.key).then_with(|| a.name.cmp(&b.name)));
        for ai in 0..members.len() {
            for bi in ai + 1..members.len() {
                pairs_examined += 1;
                let (a, b) = (&members[ai], &members[bi]);
                if a.tokens == b.tokens {
                    continue;
                }
                let total_len = a.tokens.len() + b.tokens.len();
                if 8 * a.tokens.len().min(b.tokens.len()) < 3 * total_len {
                    pairs_rejected_by_length_bound += 1;
                    continue;
                }
                let (lcs, changed_spans) = lcs_spans(&a.tokens, &b.tokens);
                if 8 * lcs < 3 * total_len {
                    continue;
                }
                let markers: Vec<String> = changed_spans
                    .iter()
                    .flat_map(|span| span.a_tokens.iter().chain(&span.b_tokens))
                    .filter(|token| marker_set.contains(token.as_str()))
                    .cloned()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect();
                if markers.is_empty() {
                    continue;
                }
                let identity_sensitive_omission = a
                    .omitted_strings
                    .iter()
                    .chain(&b.omitted_strings)
                    .any(|omission| omission.carrier == "TriggerDefinition");
                let changed_token_count = changed_spans
                    .iter()
                    .map(|span| span.a_tokens.len() + span.b_tokens.len())
                    .sum();
                candidates.push(CollisionCandidate {
                    status: "unverified_structural_collision_candidate",
                    a: candidate_face(a),
                    b: candidate_face(b),
                    identity_sensitive_omission,
                    similarity: Ratio {
                        numerator: 2 * lcs,
                        denominator: total_len,
                    },
                    markers,
                    changed_spans,
                    natural_rebalance_pair: natural_rebalance_pair(&a.name, &b.name),
                    changed_token_count,
                });
            }
        }
    }
    candidates.sort_by(compare_candidates);
    Ok(CollisionReport {
        schema_version: 1,
        algorithm_version: "structural-collision-v1",
        result_kind: "structural_collision_candidates",
        input: ReportInput {
            path: "card-data.json",
            sha256: input_digest,
        },
        parameters: ReportParameters {
            minimum_similarity: Ratio {
                numerator: 3,
                denominator: 4,
            },
            markers_version: 1,
            projection_omits: ["top_level_oracle_text", "definition_description"],
            filter,
        },
        supported_cards,
        rules_bearing_supported_cards,
        identical_projected_groups,
        largest_group_size,
        pairs_examined,
        pairs_rejected_by_length_bound,
        candidate_pairs: candidates.len(),
        candidates,
    })
}

/// CR 205.4c: Any land with the supertype "basic" is a basic land.
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

    fn write(&self, out: &mut dyn Write) -> std::io::Result<()> {
        let unsupported = self.total - self.supported;
        let pct = if self.total > 0 {
            (self.supported as f64 / self.total as f64) * 100.0
        } else {
            0.0
        };
        writeln!(out, "{}", self.label)?;
        writeln!(
            out,
            "  total: {}  supported: {}  unsupported: {}  ({pct:.1}%)",
            self.total, self.supported, unsupported
        )?;
        if !self.gap_freq.is_empty() {
            writeln!(out, "  gap breakdown:")?;
            let mut gaps: Vec<(&String, &usize)> = self.gap_freq.iter().collect();
            gaps.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
            for (handler, count) in gaps {
                writeln!(out, "    {count:>4}  {handler}")?;
            }
        }
        if !self.unsupported.is_empty() {
            writeln!(out, "  unsupported cards:")?;
            for card in &self.unsupported {
                writeln!(out, "    {} (gaps: {})", card.card_name, card.gap_count)?;
            }
        }
        Ok(())
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum Mode {
    Summary,
    Snapshot(String),
    Diff(String),
    StructuralCollisions,
}

enum ParseOutcome {
    Run(Options),
    Help,
}

struct Options {
    data_root: Option<String>,
    set: Option<String>,
    deck: Option<String>,
    mode: Mode,
    quiet: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            data_root: None,
            set: None,
            deck: None,
            mode: Mode::Summary,
            quiet: false,
        }
    }
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
    eprintln!("  --structural-collisions  Report deterministic structural collision candidates.");
    eprintln!("  --quiet             In --diff mode, print only the changed/regression counts.");
}

fn parse_args_from(args: impl IntoIterator<Item = String>) -> Result<ParseOutcome, String> {
    let mut opts = Options::default();
    let mut args = args.into_iter().peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--set" => opts.set = Some(args.next().ok_or("--set requires a value")?),
            "--deck" => opts.deck = Some(args.next().ok_or("--deck requires a value")?),
            "--snapshot" => {
                if opts.mode != Mode::Summary {
                    return Err(
                        "--snapshot, --diff, and --structural-collisions are mutually exclusive"
                            .to_string(),
                    );
                }
                opts.mode = Mode::Snapshot(args.next().ok_or("--snapshot requires a value")?);
            }
            "--diff" => {
                if opts.mode != Mode::Summary {
                    return Err(
                        "--snapshot, --diff, and --structural-collisions are mutually exclusive"
                            .to_string(),
                    );
                }
                opts.mode = Mode::Diff(args.next().ok_or("--diff requires a value")?);
            }
            "--structural-collisions" => {
                if opts.mode != Mode::Summary {
                    return Err(
                        "--snapshot, --diff, and --structural-collisions are mutually exclusive"
                            .to_string(),
                    );
                }
                opts.mode = Mode::StructuralCollisions;
            }
            "--quiet" => opts.quiet = true,
            "-h" | "--help" => return Ok(ParseOutcome::Help),
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
    Ok(ParseOutcome::Run(opts))
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
                        .find(|(face_key, face)| {
                            c.card_face_key.as_deref().map_or_else(
                                || face.name.eq_ignore_ascii_case(&c.card_name),
                                |coverage_key| *face_key == coverage_key,
                            )
                        })
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

fn run_with_io(opts: Options, out: &mut dyn Write, err: &mut dyn Write) -> Result<i32, String> {
    let data_root = resolve_data_root(opts.data_root.as_deref())
        .ok_or("could not locate card-data.json (pass DATA_ROOT or set PHASE_CARDS_PATH)")?;
    let export_path = data_root.join("card-data.json");
    let input_bytes = std::fs::read(&export_path)
        .map_err(|e| format!("failed to read {}: {e}", export_path.display()))?;
    let input_digest = (opts.mode == Mode::StructuralCollisions)
        .then(|| format!("{:x}", Sha256::digest(&input_bytes)));
    let db = CardDatabase::from_export_reader(input_bytes.as_slice())
        .map_err(|e| format!("failed to load {}: {e}", export_path.display()))?;
    drop(input_bytes);

    let summary = analyze_coverage(&db);
    let faces = face_by_key(&db);

    let (label, retained) = filter_cards(&db, &summary.cards, &opts)?;

    if opts.mode == Mode::StructuralCollisions {
        let report = build_collision_report(
            &db,
            &summary.cards,
            &retained,
            label,
            input_digest.expect("collision mode computes an input digest"),
        )?;
        serde_json::to_writer_pretty(&mut *out, &report)
            .map_err(|e| format!("failed to serialize structural collision report: {e}"))?;
        writeln!(out).map_err(|e| format!("failed to write report: {e}"))?;
        writeln!(
            err,
            "structural-collision-v1: {} candidates from {} supported rules-bearing cards",
            report.candidate_pairs, report.rules_bearing_supported_cards
        )
        .map_err(|e| format!("failed to write report summary: {e}"))?;
        return Ok(0);
    }

    // --diff: regression mode. Recompute hashes and compare to baseline.
    if let Mode::Diff(baseline_path) = &opts.mode {
        let baseline: BTreeMap<String, SnapshotEntry> = {
            let text = std::fs::read_to_string(baseline_path)
                .map_err(|e| format!("failed to read baseline {baseline_path}: {e}"))?;
            serde_json::from_str(&text)
                .map_err(|e| format!("failed to parse baseline {baseline_path}: {e}"))?
        };
        let current = build_snapshot(&faces, &retained);
        return run_diff(&baseline, &current, opts.quiet, out)
            .map_err(|e| format!("failed to write diff: {e}"));
    }

    // --snapshot: write baseline.
    if let Mode::Snapshot(snapshot_path) = &opts.mode {
        let snapshot = build_snapshot(&faces, &retained);
        let json = serde_json::to_string_pretty(&snapshot)
            .map_err(|e| format!("failed to serialize snapshot: {e}"))?;
        std::fs::write(snapshot_path, json)
            .map_err(|e| format!("failed to write snapshot {snapshot_path}: {e}"))?;
        writeln!(
            err,
            "Wrote snapshot of {} cards ({}) to {}",
            snapshot.len(),
            label,
            snapshot_path
        )
        .map_err(|e| format!("failed to write snapshot summary: {e}"))?;
        return Ok(0);
    }

    // Default: print the filtered coverage summary.
    Summary::build(label, &retained)
        .write(out)
        .map_err(|e| format!("failed to write summary: {e}"))?;
    Ok(0)
}

/// Compare two snapshots. Returns the exit code: non-zero if any `ast_hash`
/// changed (so CI can gate on it). Cards that were `supported: true` in the
/// baseline whose AST moved are flagged separately as regression suspects.
fn run_diff(
    baseline: &BTreeMap<String, SnapshotEntry>,
    current: &BTreeMap<String, SnapshotEntry>,
    quiet: bool,
    out: &mut dyn Write,
) -> std::io::Result<i32> {
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
        writeln!(
            out,
            "changed: {}  regression-suspects: {}  added: {}  removed: {}",
            changed.len(),
            regressions.len(),
            added.len(),
            removed.len()
        )?;
    } else {
        if changed.is_empty() && added.is_empty() && removed.is_empty() {
            writeln!(
                out,
                "No AST changes: {} cards match the baseline.",
                current.len()
            )?;
        }
        if !changed.is_empty() {
            writeln!(out, "Changed AST ({}):", changed.len())?;
            for (key, old, cur) in &changed {
                writeln!(
                    out,
                    "  {key}: hash {} -> {}  supported {} -> {}  gaps {} -> {}",
                    old.ast_hash,
                    cur.ast_hash,
                    old.supported,
                    cur.supported,
                    old.gap_count,
                    cur.gap_count
                )?;
            }
        }
        if !regressions.is_empty() {
            writeln!(
                out,
                "Regression suspects (baseline supported, AST moved) ({}):",
                regressions.len()
            )?;
            for (key, old, cur) in &regressions {
                writeln!(
                    out,
                    "  {key}: supported {} -> {}  gaps {} -> {}",
                    old.supported, cur.supported, old.gap_count, cur.gap_count
                )?;
            }
        }
        if !added.is_empty() {
            writeln!(out, "New cards not in baseline ({}):", added.len())?;
            for key in &added {
                writeln!(out, "  {key}")?;
            }
        }
        if !removed.is_empty() {
            writeln!(out, "Baseline cards missing now ({}):", removed.len())?;
            for key in &removed {
                writeln!(out, "  {key}")?;
            }
        }
    }

    // Exit non-zero if anything moved so scripts/CI can gate on a clean diff.
    Ok(i32::from(
        !changed.is_empty() || !added.is_empty() || !removed.is_empty(),
    ))
}

fn main() {
    match parse_args_from(std::env::args().skip(1)) {
        Ok(ParseOutcome::Help) => {
            print_usage();
            process::exit(0);
        }
        Ok(ParseOutcome::Run(opts)) => {
            match run_with_io(opts, &mut std::io::stdout(), &mut std::io::stderr()) {
                Ok(code) => process::exit(code),
                Err(msg) => {
                    eprintln!("set-check: {msg}");
                    print_usage();
                    process::exit(2);
                }
            }
        }
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
    use engine::types::replacements::ReplacementEvent;
    use engine::types::statics::StaticMode;
    use engine::types::triggers::TriggerMode;

    fn face_with_oracle(oracle: &str) -> CardFace {
        CardFace {
            oracle_text: Some(oracle.to_string()),
            ..CardFace::default()
        }
    }

    fn coverage_row(key: &str, name: &str, supported: bool, set: &str) -> CardCoverageResult {
        CardCoverageResult {
            card_face_key: Some(key.to_string()),
            card_name: name.to_string(),
            set_code: String::new(),
            supported,
            gap_details: Vec::new(),
            gap_count: usize::from(!supported),
            oracle_text: None,
            parse_details: Vec::new(),
            printings: vec![set.to_string()],
        }
    }

    fn database_from_faces(faces: BTreeMap<String, CardFace>) -> CardDatabase {
        CardDatabase::from_json_str(&serde_json::to_string(&faces).unwrap()).unwrap()
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
    fn sha256_digest_uses_exact_input_bytes() {
        assert_eq!(
            format!("{:x}", Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_ne!(Sha256::digest(b"abc"), Sha256::digest(b"abc\n"));
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
    fn projection_omits_only_recognized_definition_descriptions() {
        let mut ability = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::unimplemented("fixture", "raw effect description"),
        );
        ability.description = Some("ability description".to_string());
        let mut trigger = TriggerDefinition::new(TriggerMode::YouAttack);
        trigger.description = Some("trigger description".to_string());
        let mut static_definition = StaticDefinition::new(StaticMode::Continuous);
        static_definition.description = Some("static description".to_string());
        let mut replacement = ReplacementDefinition::new(ReplacementEvent::GainLife);
        replacement.description = Some("replacement description".to_string());
        let face = CardFace {
            oracle_text: Some("fixture".to_string()),
            abilities: vec![ability],
            triggers: vec![trigger],
            static_abilities: vec![static_definition],
            replacements: vec![replacement],
            ..CardFace::default()
        };

        let (projection, omitted) = structural_projection(&face);
        assert!(projection.get("oracle_text").is_none());
        assert_eq!(
            omitted
                .iter()
                .map(|item| (
                    item.json_pointer.as_str(),
                    item.value.as_str(),
                    item.carrier
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    "/abilities/0/description",
                    "ability description",
                    "AbilityDefinition"
                ),
                (
                    "/replacements/0/description",
                    "replacement description",
                    "ReplacementDefinition"
                ),
                (
                    "/static_abilities/0/description",
                    "static description",
                    "StaticDefinition"
                ),
                (
                    "/triggers/0/description",
                    "trigger description",
                    "TriggerDefinition"
                ),
            ]
        );
        let json = serde_json::to_string(&projection).unwrap();
        assert!(json.contains("raw effect description"));
    }

    #[test]
    fn projection_retains_raw_description_on_near_miss_and_unknown_key() {
        let mut near_miss = serde_json::json!({
            "kind": "Spell",
            "effect": {},
            "sub_ability": null,
            "duration": null,
            "description": "retain me",
        });
        near_miss["future_field"] = Value::Bool(true);
        let mut omitted = Vec::new();
        omit_definition_descriptions(&mut near_miss, "/fixture", &mut omitted);
        assert!(omitted.is_empty());
        assert_eq!(near_miss["description"], "retain me");

        let mut unrecognized = serde_json::json!({"description": "also retain me"});
        omit_definition_descriptions(&mut unrecognized, "/other", &mut omitted);
        assert_eq!(unrecognized["description"], "also retain me");
    }

    #[test]
    fn projection_keeps_every_rules_root_and_array_order() {
        let face = CardFace::default();
        let (projection, _) = structural_projection(&face);
        assert_eq!(
            projection
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "additional_cost",
                "abilities",
                "casting_options",
                "casting_restrictions",
                "cleave_variant",
                "deck_copy_limit",
                "keywords",
                "modal",
                "replacements",
                "solve_condition",
                "static_abilities",
                "strive_cost",
                "triggers",
            ])
        );

        let first = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::unimplemented("first", "first raw payload"),
        );
        let second = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::unimplemented("second", "second raw payload"),
        );
        let forward = CardFace {
            abilities: vec![first.clone(), second.clone()],
            ..CardFace::default()
        };
        let reverse = CardFace {
            abilities: vec![second, first],
            ..CardFace::default()
        };
        assert_ne!(
            structural_projection(&forward).0,
            structural_projection(&reverse).0
        );
    }

    #[test]
    fn collision_join_and_filter_use_exact_face_keys_for_same_name_rows() {
        let mut first = face_with_oracle("Flying.");
        first.name = "Same Name".to_string();
        first.keywords.push(Keyword::Flying);
        let mut second = first.clone();
        second.oracle_text = Some("Flying if able.".to_string());
        let db = database_from_faces(BTreeMap::from([
            ("same name".to_string(), first),
            ("same name [other]".to_string(), second),
        ]));
        let coverage = vec![
            coverage_row("same name", "Same Name", true, "ONE"),
            coverage_row("same name [other]", "Same Name", false, "TWO"),
        ];
        let opts = Options {
            set: Some("two".to_string()),
            ..Options::default()
        };
        let (_, selected) = filter_cards(&db, &coverage, &opts).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(
            selected[0].card_face_key.as_deref(),
            Some("same name [other]")
        );

        let all = coverage.iter().collect::<Vec<_>>();
        let report = build_collision_report(
            &db,
            &coverage,
            &all,
            "all cards".to_string(),
            "digest".to_string(),
        )
        .unwrap();
        assert_eq!(report.supported_cards, 1);
        assert_eq!(report.rules_bearing_supported_cards, 1);
    }

    #[test]
    fn report_builds_ranked_marker_candidate_and_is_deterministic() {
        let make_face = |name: &str, oracle: &str| {
            let mut face = face_with_oracle(oracle);
            face.name = name.to_string();
            face.keywords.push(Keyword::Flying);
            face
        };
        let db = database_from_faces(BTreeMap::from([
            (
                "alpha".to_string(),
                make_face("Alpha", "Flying flying flying."),
            ),
            (
                "beta".to_string(),
                make_face("Beta", "Flying flying if flying."),
            ),
        ]));
        let coverage = vec![
            coverage_row("beta", "Beta", true, "SET"),
            coverage_row("alpha", "Alpha", true, "SET"),
        ];
        let selected = coverage.iter().collect::<Vec<_>>();
        let first = build_collision_report(
            &db,
            &coverage,
            &selected,
            "all cards".to_string(),
            "digest".to_string(),
        )
        .unwrap();
        let second = build_collision_report(
            &db,
            &coverage,
            &selected,
            "all cards".to_string(),
            "digest".to_string(),
        )
        .unwrap();
        assert_eq!(first.candidate_pairs, 1);
        assert_eq!(first.candidates[0].a.key, "alpha");
        assert_eq!(first.candidates[0].markers, vec!["if"]);
        assert_eq!(
            serde_json::to_vec_pretty(&first).unwrap(),
            serde_json::to_vec_pretty(&second).unwrap()
        );
    }

    #[test]
    fn run_with_io_emits_reproducible_collision_json() {
        let mut face = face_with_oracle("Flying.");
        face.name = "Fixture".to_string();
        face.keywords.push(Keyword::Flying);
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("card-data.json"),
            serde_json::to_vec(&BTreeMap::from([("fixture".to_string(), face)])).unwrap(),
        )
        .unwrap();
        let options = || Options {
            data_root: Some(directory.path().display().to_string()),
            mode: Mode::StructuralCollisions,
            ..Options::default()
        };
        let (mut first_out, mut first_err) = (Vec::new(), Vec::new());
        assert_eq!(
            run_with_io(options(), &mut first_out, &mut first_err).unwrap(),
            0
        );
        let (mut second_out, mut second_err) = (Vec::new(), Vec::new());
        assert_eq!(
            run_with_io(options(), &mut second_out, &mut second_err).unwrap(),
            0
        );
        assert_eq!(first_out, second_out);
        assert_eq!(first_err, second_err);
        let report: Value = serde_json::from_slice(&first_out).unwrap();
        assert_eq!(report["algorithm_version"], "structural-collision-v1");
        assert_eq!(report["input"]["path"], "card-data.json");
        assert_eq!(report["candidate_pairs"], 0);
    }

    #[test]
    fn tokenizer_handles_names_reminders_and_hostile_larger_words() {
        assert_eq!(
            oracle_tokens(
                "Clone clones Clone's text. Clone attacks. (Reminder only.)",
                "Clone"
            ),
            vec!["~", "clones", "clone's", "text", "~", "attacks"]
        );
        assert_eq!(
            oracle_tokens("Test (nested (text) stays) one.", "Test"),
            vec!["~", "nested", "text", "stays", "one"]
        );
    }

    #[test]
    fn lcs_uses_a_first_ties_and_reports_half_open_spans() {
        let a = vec!["one", "x", "one"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>();
        let b = vec!["one", "one", "x"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>();
        let (length, spans) = lcs_spans(&a, &b);
        assert_eq!(length, 2);
        assert_eq!(
            spans,
            vec![
                ChangedSpan {
                    a_start: 1,
                    a_end: 2,
                    a_tokens: vec!["x".to_string()],
                    b_start: 1,
                    b_end: 1,
                    b_tokens: vec![],
                },
                ChangedSpan {
                    a_start: 3,
                    a_end: 3,
                    a_tokens: vec![],
                    b_start: 2,
                    b_end: 3,
                    b_tokens: vec!["x".to_string()],
                }
            ]
        );
    }

    #[test]
    fn argument_modes_are_mutually_exclusive() {
        let ParseOutcome::Run(opts) =
            parse_args_from(["--structural-collisions".to_string()]).unwrap()
        else {
            panic!("expected runnable options");
        };
        assert_eq!(opts.mode, Mode::StructuralCollisions);
        assert!(parse_args_from([
            "--structural-collisions".to_string(),
            "--snapshot".to_string(),
            "out.json".to_string(),
        ])
        .is_err());
        assert!(matches!(
            parse_args_from(["--help".to_string()]),
            Ok(ParseOutcome::Help)
        ));
        assert!(parse_args_from(["--unknown".to_string(), "--help".to_string()]).is_err());
        assert!(parse_args_from([
            "--snapshot".to_string(),
            "out.json".to_string(),
            "--diff".to_string(),
            "base.json".to_string(),
        ])
        .is_err());
    }

    #[test]
    fn rebalance_pair_requires_one_leading_prefix() {
        assert!(natural_rebalance_pair(
            "A-Sizzling Soloist",
            "Sizzling Soloist"
        ));
        assert!(!natural_rebalance_pair("A-A-Card", "Card"));
        assert!(!natural_rebalance_pair("Card", "Other Card"));
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

        let mut output = Vec::new();
        let code = run_diff(&baseline, &current, true, &mut output).unwrap();
        assert_eq!(code, 1, "a changed hash must yield a non-zero exit code");

        // Identical snapshots produce a clean (zero) exit.
        assert_eq!(
            run_diff(&baseline, &baseline.clone(), true, &mut Vec::new()).unwrap(),
            0
        );
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
