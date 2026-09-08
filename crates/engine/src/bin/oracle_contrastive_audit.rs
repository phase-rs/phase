//! Deterministic semantic-collision audit for controlled Oracle mutations.

use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process;

use engine::database::mtgjson::AtomicCardsFile;
use engine::database::synthesis::build_oracle_face;
use engine::game::coverage::card_face_gaps;
use engine::parser::oracle_ir::diagnostic::OracleDiagnostic;
use engine::types::ability::{ControllerRef, Effect, FilterProp, QuantityExpr, TargetFilter};
use engine::types::ability_visit::visit_ability_def;
use engine::types::card::CardFace;
use nom::branch::alt;
use nom::bytes::complete::{tag, take_until, take_while};
use nom::character::complete::digit1;
use nom::combinator::{all_consuming, map_res, recognize as nom_recognize};
use nom::sequence::delimited;
use nom::Parser;
use serde::Serialize;
use sha2::{Digest, Sha256};

const TOOL: &str = "oracle_contrastive_audit";
const FAMILIES: [&str; 3] = [
    "life_recipient_v1",
    "token_count_article_two_v1",
    "destroy_nontoken_v1",
];

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    BothUnsupported,
    BaseUnsupported,
    MutantUnsupported,
    BothDiagnostic,
    BaseDiagnostic,
    MutantDiagnostic,
    UncheckableCarrierCount,
    SemanticCollision,
    ProjectionChangedElsewhere,
    ChangedAsRequired,
}

impl Status {
    fn reason_code(self) -> &'static str {
        match self {
            Self::BothUnsupported => "both_unsupported",
            Self::BaseUnsupported => "base_unsupported",
            Self::MutantUnsupported => "mutant_unsupported",
            Self::BothDiagnostic => "both_diagnostic",
            Self::BaseDiagnostic => "base_diagnostic",
            Self::MutantDiagnostic => "mutant_diagnostic",
            Self::UncheckableCarrierCount => "uncheckable_carrier_count",
            Self::SemanticCollision => "semantic_collision",
            Self::ProjectionChangedElsewhere => "projection_changed_elsewhere",
            Self::ChangedAsRequired => "changed_as_required",
        }
    }
}

#[derive(Clone, Debug)]
struct Mutation {
    kind: MutationKind,
    text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MutationKind {
    LifeYouToTargetOpponent,
    LifeTargetOpponentToYou,
    TokenOneToTwo,
    TokenTwoToOne,
    DestroyCreatureToNontoken,
    DestroyNontokenToCreature,
}

impl MutationKind {
    fn family(self) -> &'static str {
        match self {
            Self::LifeYouToTargetOpponent | Self::LifeTargetOpponentToYou => FAMILIES[0],
            Self::TokenOneToTwo | Self::TokenTwoToOne => FAMILIES[1],
            Self::DestroyCreatureToNontoken | Self::DestroyNontokenToCreature => FAMILIES[2],
        }
    }

    fn direction(self) -> &'static str {
        match self {
            Self::LifeYouToTargetOpponent => "you_to_target_opponent",
            Self::LifeTargetOpponentToYou => "target_opponent_to_you",
            Self::TokenOneToTwo => "one_to_two",
            Self::TokenTwoToOne => "two_to_one",
            Self::DestroyCreatureToNontoken => "creature_to_nontoken",
            Self::DestroyNontokenToCreature => "nontoken_to_creature",
        }
    }
}

#[derive(Default, Serialize)]
struct FamilyCensus {
    faces_scanned: usize,
    grammar_matches: usize,
    attempted: usize,
    rejected_before_synthesis: BTreeMap<String, usize>,
}

#[derive(Serialize)]
struct ProjectionPair {
    base_carrier_count: usize,
    mutant_carrier_count: usize,
    base: Option<Effect>,
    mutant: Option<Effect>,
}

#[derive(Serialize)]
struct AuditResult {
    atomic_key: String,
    face_name: String,
    oracle_id: Option<String>,
    family: &'static str,
    direction: &'static str,
    original_text: String,
    mutated_text: String,
    byte_span: ByteSpan,
    base_handler_gaps: Vec<String>,
    mutant_handler_gaps: Vec<String>,
    base_warnings: Vec<OracleDiagnostic>,
    mutant_warnings: Vec<OracleDiagnostic>,
    projections: ProjectionPair,
    status: Status,
    reason_code: &'static str,
}

#[derive(Serialize)]
struct ByteSpan {
    start: usize,
    end: usize,
}

#[derive(Serialize)]
struct Report {
    schema_version: u32,
    tool: &'static str,
    support_authority: &'static str,
    input_label: String,
    input_sha256: String,
    families: [&'static str; 3],
    census: BTreeMap<&'static str, FamilyCensus>,
    status_counts: BTreeMap<Status, usize>,
    results: Vec<AuditResult>,
}

fn decimal(input: &str) -> nom::IResult<&str, i32> {
    map_res(digit1, str::parse::<i32>).parse(input)
}

fn recognize_life(text: &str) -> Option<Mutation> {
    if let Ok((_, amount)) =
        all_consuming(delimited(tag("You gain "), decimal, tag(" life."))).parse(text)
    {
        return Some(Mutation {
            kind: MutationKind::LifeYouToTargetOpponent,
            text: format!("Target opponent gains {amount} life."),
        });
    }
    let (_, amount) = all_consuming(delimited(
        tag("Target opponent gains "),
        decimal,
        tag(" life."),
    ))
    .parse(text)
    .ok()?;
    Some(Mutation {
        kind: MutationKind::LifeTargetOpponentToYou,
        text: format!("You gain {amount} life."),
    })
}

fn description_is_single_token(description: &str) -> bool {
    let structurally_valid = all_consuming(nom_recognize((
        digit1::<&str, nom::error::Error<&str>>,
        tag("/"),
        digit1,
        take_while(|c: char| !matches!(c, '\n' | '\r' | '"' | '.' | '—' | '(' | ')' | ';' | ':')),
    )))
    .parse(description)
    .is_ok();
    if !structurally_valid {
        return false;
    }
    let mut forbidden = alt((tag::<_, _, nom::error::Error<_>>(" token"), tag(" and a ")));
    let mut remaining = description;
    while !remaining.is_empty() {
        if forbidden.parse(remaining).is_ok() {
            return false;
        }
        let Some((offset, ch)) = remaining.char_indices().next() else {
            break;
        };
        remaining = &remaining[offset + ch.len_utf8()..];
    }
    true
}

fn recognize_token(text: &str) -> Option<Mutation> {
    fn singular(input: &str) -> nom::IResult<&str, &str> {
        all_consuming(delimited(
            tag("Create a "),
            nom_recognize((digit1, tag("/"), digit1, take_until(" token."))),
            tag(" token."),
        ))
        .parse(input)
    }

    fn plural(input: &str) -> nom::IResult<&str, &str> {
        all_consuming(delimited(
            tag("Create two "),
            nom_recognize((digit1, tag("/"), digit1, take_until(" tokens."))),
            tag(" tokens."),
        ))
        .parse(input)
    }

    if let Ok((_, description)) = singular(text) {
        if description_is_single_token(description) {
            return Some(Mutation {
                kind: MutationKind::TokenOneToTwo,
                text: format!("Create two {description} tokens."),
            });
        }
        return None;
    }
    let (_, description) = plural(text).ok()?;
    description_is_single_token(description).then(|| Mutation {
        kind: MutationKind::TokenTwoToOne,
        text: format!("Create a {description} token."),
    })
}

fn recognize_destroy(text: &str) -> Option<Mutation> {
    fn sentence(input: &str) -> nom::IResult<&str, &str> {
        alt((
            tag("Destroy target creature."),
            tag("Destroy target nontoken creature."),
        ))
        .parse(input)
    }

    let (_, qualified) = all_consuming(sentence).parse(text).ok()?;
    if qualified == "Destroy target creature." {
        Some(Mutation {
            kind: MutationKind::DestroyCreatureToNontoken,
            text: "Destroy target nontoken creature.".to_string(),
        })
    } else {
        Some(Mutation {
            kind: MutationKind::DestroyNontokenToCreature,
            text: "Destroy target creature.".to_string(),
        })
    }
}

fn recognize_mutation(text: &str) -> Option<Mutation> {
    recognize_life(text)
        .or_else(|| recognize_token(text))
        .or_else(|| recognize_destroy(text))
}

fn carriers(face: &CardFace, kind: MutationKind) -> Vec<Effect> {
    let mut found = Vec::new();
    for ability in &face.abilities {
        let _ = visit_ability_def(ability, &mut |effect| {
            let relevant = match kind {
                MutationKind::LifeYouToTargetOpponent | MutationKind::LifeTargetOpponentToYou => {
                    matches!(effect, Effect::GainLife { .. })
                }
                MutationKind::TokenOneToTwo | MutationKind::TokenTwoToOne => {
                    matches!(effect, Effect::Token { .. })
                }
                MutationKind::DestroyCreatureToNontoken
                | MutationKind::DestroyNontokenToCreature => {
                    matches!(effect, Effect::Destroy { .. })
                }
            };
            if relevant {
                found.push(effect.clone());
            }
            ControlFlow::Continue(())
        });
    }
    found
}

fn compare_life(base: &Effect, mutant: &Effect, kind: MutationKind) -> Status {
    let (
        Effect::GainLife {
            amount: base_amount,
            player: base_player,
        },
        Effect::GainLife {
            amount: mutant_amount,
            player: mutant_player,
        },
    ) = (base, mutant)
    else {
        return Status::UncheckableCarrierCount;
    };

    fn recipient(filter: &TargetFilter) -> Option<bool> {
        match filter {
            TargetFilter::Controller => Some(false),
            TargetFilter::Typed(typed)
                if typed.type_filters.is_empty()
                    && typed.controller == Some(ControllerRef::Opponent)
                    && typed.properties.is_empty() =>
            {
                Some(true)
            }
            _ => None,
        }
    }

    if base_player == mutant_player {
        return Status::SemanticCollision;
    }
    if !matches!(base_amount, QuantityExpr::Fixed { .. })
        || !matches!(mutant_amount, QuantityExpr::Fixed { .. })
    {
        return Status::UncheckableCarrierCount;
    }
    let (Some(base_is_opponent), Some(mutant_is_opponent)) =
        (recipient(base_player), recipient(mutant_player))
    else {
        return Status::UncheckableCarrierCount;
    };
    let expected = match kind {
        MutationKind::LifeYouToTargetOpponent => !base_is_opponent && mutant_is_opponent,
        MutationKind::LifeTargetOpponentToYou => base_is_opponent && !mutant_is_opponent,
        MutationKind::TokenOneToTwo
        | MutationKind::TokenTwoToOne
        | MutationKind::DestroyCreatureToNontoken
        | MutationKind::DestroyNontokenToCreature => {
            unreachable!("classify dispatches only life mutations to compare_life")
        }
    };
    if !expected {
        return Status::ProjectionChangedElsewhere;
    }
    let mut normalized_base = base.clone();
    let mut normalized_mutant = mutant.clone();
    if let Effect::GainLife { player, .. } = &mut normalized_base {
        *player = TargetFilter::Controller;
    }
    if let Effect::GainLife { player, .. } = &mut normalized_mutant {
        *player = TargetFilter::Controller;
    }
    if normalized_base == normalized_mutant {
        Status::ChangedAsRequired
    } else {
        Status::ProjectionChangedElsewhere
    }
}

fn compare_token(base: &Effect, mutant: &Effect, kind: MutationKind) -> Status {
    let (
        Effect::Token {
            count: base_count, ..
        },
        Effect::Token {
            count: mutant_count,
            ..
        },
    ) = (base, mutant)
    else {
        return Status::SemanticCollision;
    };
    if base_count == mutant_count {
        return Status::SemanticCollision;
    }
    let (
        QuantityExpr::Fixed { value: base_value },
        QuantityExpr::Fixed {
            value: mutant_value,
        },
    ) = (base_count, mutant_count)
    else {
        return Status::UncheckableCarrierCount;
    };
    let (base_expected, mutant_expected) = match kind {
        MutationKind::TokenOneToTwo => (1, 2),
        MutationKind::TokenTwoToOne => (2, 1),
        MutationKind::LifeYouToTargetOpponent
        | MutationKind::LifeTargetOpponentToYou
        | MutationKind::DestroyCreatureToNontoken
        | MutationKind::DestroyNontokenToCreature => {
            unreachable!("classify dispatches only token mutations to compare_token")
        }
    };
    if *base_value != base_expected || *mutant_value != mutant_expected {
        return Status::ProjectionChangedElsewhere;
    }
    let mut normalized_base = base.clone();
    let mut normalized_mutant = mutant.clone();
    if let Effect::Token { count, .. } = &mut normalized_base {
        *count = QuantityExpr::Fixed { value: 1 };
    }
    if let Effect::Token { count, .. } = &mut normalized_mutant {
        *count = QuantityExpr::Fixed { value: 1 };
    }
    if normalized_base == normalized_mutant {
        Status::ChangedAsRequired
    } else {
        Status::ProjectionChangedElsewhere
    }
}

fn compare_destroy(base: &Effect, mutant: &Effect, kind: MutationKind) -> Status {
    let (
        Effect::Destroy {
            target: base_target,
            ..
        },
        Effect::Destroy {
            target: mutant_target,
            ..
        },
    ) = (base, mutant)
    else {
        return Status::SemanticCollision;
    };
    let (TargetFilter::Typed(base_filter), TargetFilter::Typed(mutant_filter)) =
        (base_target, mutant_target)
    else {
        return Status::UncheckableCarrierCount;
    };
    let base_non_tokens = base_filter
        .properties
        .iter()
        .filter(|property| matches!(property, FilterProp::NonToken))
        .count();
    let mutant_non_tokens = mutant_filter
        .properties
        .iter()
        .filter(|property| matches!(property, FilterProp::NonToken))
        .count();
    if base_non_tokens > 1 || mutant_non_tokens > 1 {
        return Status::UncheckableCarrierCount;
    }
    if base_target == mutant_target {
        return Status::SemanticCollision;
    }
    let expected = match kind {
        MutationKind::DestroyCreatureToNontoken => base_non_tokens == 0 && mutant_non_tokens == 1,
        MutationKind::DestroyNontokenToCreature => base_non_tokens == 1 && mutant_non_tokens == 0,
        MutationKind::LifeYouToTargetOpponent
        | MutationKind::LifeTargetOpponentToYou
        | MutationKind::TokenOneToTwo
        | MutationKind::TokenTwoToOne => {
            unreachable!("classify dispatches only destroy mutations to compare_destroy")
        }
    };
    if !expected {
        return Status::ProjectionChangedElsewhere;
    }
    let mut normalized_base = base.clone();
    let mut normalized_mutant = mutant.clone();
    for effect in [&mut normalized_base, &mut normalized_mutant] {
        if let Effect::Destroy {
            target: TargetFilter::Typed(filter),
            ..
        } = effect
        {
            filter
                .properties
                .retain(|property| !matches!(property, FilterProp::NonToken));
        }
    }
    if normalized_base == normalized_mutant {
        Status::ChangedAsRequired
    } else {
        Status::ProjectionChangedElsewhere
    }
}

fn classify(
    base_face: &CardFace,
    mutant_face: &CardFace,
    kind: MutationKind,
) -> (Status, Vec<String>, Vec<String>, ProjectionPair) {
    let base_gaps = card_face_gaps(base_face);
    let mutant_gaps = card_face_gaps(mutant_face);
    let status = match (base_gaps.is_empty(), mutant_gaps.is_empty()) {
        (false, false) => Some(Status::BothUnsupported),
        (false, true) => Some(Status::BaseUnsupported),
        (true, false) => Some(Status::MutantUnsupported),
        (true, true) => None,
    };
    let status = status.or(
        match (
            base_face.parse_warnings.is_empty(),
            mutant_face.parse_warnings.is_empty(),
        ) {
            (false, false) => Some(Status::BothDiagnostic),
            (false, true) => Some(Status::BaseDiagnostic),
            (true, false) => Some(Status::MutantDiagnostic),
            (true, true) => None,
        },
    );
    let base = carriers(base_face, kind);
    let mutant = carriers(mutant_face, kind);
    let projections = ProjectionPair {
        base_carrier_count: base.len(),
        mutant_carrier_count: mutant.len(),
        base: base.first().cloned(),
        mutant: mutant.first().cloned(),
    };
    let status = status.unwrap_or_else(|| {
        if base.len() != 1 || mutant.len() != 1 {
            return Status::UncheckableCarrierCount;
        }
        match kind {
            MutationKind::LifeYouToTargetOpponent | MutationKind::LifeTargetOpponentToYou => {
                compare_life(&base[0], &mutant[0], kind)
            }
            MutationKind::TokenOneToTwo | MutationKind::TokenTwoToOne => {
                compare_token(&base[0], &mutant[0], kind)
            }
            MutationKind::DestroyCreatureToNontoken | MutationKind::DestroyNontokenToCreature => {
                compare_destroy(&base[0], &mutant[0], kind)
            }
        }
    });
    (status, base_gaps, mutant_gaps, projections)
}

fn audit(input: &Path, input_label: String) -> Result<Report, Box<dyn Error>> {
    let bytes = fs::read(input)?;
    // Digest and deserialize the same read so a concurrent corpus refresh
    // cannot make the provenance describe different bytes than the results.
    let cards: AtomicCardsFile = serde_json::from_slice(&bytes)?;
    let mut census: BTreeMap<_, _> = FAMILIES
        .into_iter()
        .map(|family| (family, FamilyCensus::default()))
        .collect();
    let mut results = Vec::new();
    let mut keys: Vec<_> = cards.data.into_iter().collect();
    keys.sort_by(|left, right| left.0.cmp(&right.0));
    for (atomic_key, mut faces) in keys {
        faces.sort_by(|left, right| {
            let left_identity = (
                left.face_name.as_deref().unwrap_or(&left.name),
                left.identifiers.scryfall_oracle_id.as_deref(),
            );
            let right_identity = (
                right.face_name.as_deref().unwrap_or(&right.name),
                right.identifiers.scryfall_oracle_id.as_deref(),
            );
            left_identity.cmp(&right_identity)
        });
        for face in faces {
            for family in FAMILIES {
                census.get_mut(family).unwrap().faces_scanned += 1;
            }
            let Some(text) = face.text.as_deref() else {
                for family in FAMILIES {
                    *census
                        .get_mut(family)
                        .unwrap()
                        .rejected_before_synthesis
                        .entry("missing_text".to_string())
                        .or_default() += 1;
                }
                continue;
            };
            let Some(mutation) = recognize_mutation(text) else {
                for family in FAMILIES {
                    *census
                        .get_mut(family)
                        .unwrap()
                        .rejected_before_synthesis
                        .entry("grammar_mismatch".to_string())
                        .or_default() += 1;
                }
                continue;
            };
            for family in FAMILIES {
                if family != mutation.kind.family() {
                    *census
                        .get_mut(family)
                        .unwrap()
                        .rejected_before_synthesis
                        .entry("grammar_mismatch".to_string())
                        .or_default() += 1;
                }
            }
            let family_census = census.get_mut(mutation.kind.family()).unwrap();
            family_census.grammar_matches += 1;
            let oracle_id = face.identifiers.scryfall_oracle_id.clone();
            let base_face = build_oracle_face(&face, oracle_id.clone());
            let mut mutated_card = face.clone();
            mutated_card.text = Some(mutation.text.clone());
            let mutant_face = build_oracle_face(&mutated_card, oracle_id.clone());
            family_census.attempted += 1;
            let (status, base_gaps, mutant_gaps, projections) =
                classify(&base_face, &mutant_face, mutation.kind);
            results.push(AuditResult {
                atomic_key: atomic_key.clone(),
                face_name: face.face_name.clone().unwrap_or_else(|| face.name.clone()),
                oracle_id,
                family: mutation.kind.family(),
                direction: mutation.kind.direction(),
                original_text: text.to_string(),
                mutated_text: mutation.text,
                byte_span: ByteSpan {
                    start: 0,
                    end: text.len(),
                },
                base_handler_gaps: base_gaps,
                mutant_handler_gaps: mutant_gaps,
                base_warnings: base_face.parse_warnings,
                mutant_warnings: mutant_face.parse_warnings,
                projections,
                status,
                reason_code: status.reason_code(),
            });
        }
    }
    results.sort_by(|left, right| {
        (
            &left.atomic_key,
            &left.face_name,
            &left.oracle_id,
            left.family,
            left.direction,
            &left.mutated_text,
        )
            .cmp(&(
                &right.atomic_key,
                &right.face_name,
                &right.oracle_id,
                right.family,
                right.direction,
                &right.mutated_text,
            ))
    });
    let mut status_counts = BTreeMap::new();
    for result in &results {
        *status_counts.entry(result.status).or_default() += 1;
    }
    Ok(Report {
        schema_version: 1,
        tool: TOOL,
        support_authority: "card_face_gaps_per_face_handler_coverage",
        input_label,
        input_sha256: format!("{:x}", Sha256::digest(bytes)),
        families: FAMILIES,
        census,
        status_counts,
        results,
    })
}

fn arguments_from(
    args: impl IntoIterator<Item = String>,
) -> Result<(PathBuf, String, Option<PathBuf>), String> {
    let mut positional = None;
    let mut output = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--output" {
            output = Some(PathBuf::from(
                args.next().ok_or("--output requires a path")?,
            ));
        } else if positional.replace(arg).is_some() {
            return Err("usage: oracle_contrastive_audit [data-dir] [--output path]".to_string());
        }
    }
    let spelling = positional.unwrap_or_else(|| "data".to_string());
    let (input, input_label) = resolve_input(&spelling);
    Ok((input, input_label, output))
}

fn resolve_input(spelling: &str) -> (PathBuf, String) {
    let supplied = PathBuf::from(&spelling);
    let input = if supplied.file_name().and_then(|name| name.to_str()) == Some("AtomicCards.json") {
        supplied.clone()
    } else {
        supplied.join("mtgjson/AtomicCards.json")
    };
    let input_label = if spelling == "data" || supplied.is_absolute() {
        "mtgjson/AtomicCards.json".to_string()
    } else {
        spelling.to_string()
    };
    (input, input_label)
}

fn paths_refer_to_same_file(left: &Path, right: &Path) -> bool {
    if let Some(same) = same_file_identity(left, right) {
        return same;
    }

    fn resolved(path: &Path) -> Option<PathBuf> {
        if let Ok(canonical) = fs::canonicalize(path) {
            return Some(canonical);
        }
        let parent = match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        Some(fs::canonicalize(parent).ok()?.join(path.file_name()?))
    }

    matches!((resolved(left), resolved(right)), (Some(left), Some(right)) if left == right)
}

fn write_report_atomic(path: &Path, rendered: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "output has no file name")
    })?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        file_name.to_string_lossy(),
        process::id()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(rendered)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
fn same_file_identity(left: &Path, right: &Path) -> Option<bool> {
    use std::os::unix::fs::MetadataExt;

    let left = fs::metadata(left).ok()?;
    let right = fs::metadata(right).ok()?;
    Some(left.dev() == right.dev() && left.ino() == right.ino())
}

#[cfg(windows)]
fn same_file_identity(left: &Path, right: &Path) -> Option<bool> {
    same_file::is_same_file(left, right).ok()
}

#[cfg(not(any(unix, windows)))]
fn same_file_identity(_left: &Path, _right: &Path) -> Option<bool> {
    None
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut stdout = std::io::stdout();
    run_with_args(env::args().skip(1), &mut stdout)
}

fn run_with_args(
    args: impl IntoIterator<Item = String>,
    stdout: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let (input, input_label, output) = arguments_from(args).map_err(std::io::Error::other)?;
    if output
        .as_deref()
        .is_some_and(|path| paths_refer_to_same_file(&input, path))
    {
        return Err("output path must not overwrite the input corpus".into());
    }
    let rendered = render_report(&audit(&input, input_label)?)?;
    if let Some(path) = output {
        write_report_atomic(&path, &rendered)?;
    } else {
        stdout.write_all(&rendered)?;
    }
    Ok(())
}

fn render_report(report: &Report) -> Result<Vec<u8>, serde_json::Error> {
    let mut rendered = serde_json::to_vec_pretty(report)?;
    rendered.push(b'\n');
    Ok(rendered)
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{TOOL}: {error}");
        process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::database::mtgjson::AtomicCard;
    use engine::types::ability::{AbilityDefinition, AbilityKind, TypedFilter};

    fn raw_card(text: &str) -> AtomicCard {
        serde_json::from_value(serde_json::json!({
            "name": "Fixture",
            "colors": ["W"],
            "colorIdentity": ["W"],
            "layout": "normal",
            "type": "Instant",
            "types": ["Instant"],
            "subtypes": [],
            "supertypes": [],
            "text": text,
            "manaValue": 1.0,
            "legalities": {},
            "identifiers": { "scryfallOracleId": "fixture-oracle-id" }
        }))
        .unwrap()
    }

    #[test]
    fn life_grammar_is_reciprocal_and_all_consuming() {
        let mutation = recognize_life("You gain 12 life.").unwrap();
        assert_eq!(mutation.text, "Target opponent gains 12 life.");
        assert_eq!(
            recognize_life(&mutation.text).unwrap().text,
            "You gain 12 life."
        );
        assert!(recognize_life("When You gain 12 life.").is_none());
        assert!(recognize_life("You gain 12 life. Draw a card.").is_none());
        assert!(recognize_life("You gain 12 life.\n").is_none());
    }

    #[test]
    fn token_grammar_is_reciprocal_and_rejects_ambiguous_descriptions() {
        let mutation = recognize_token("Create two 1/1 white Soldier creature tokens.").unwrap();
        assert_eq!(mutation.text, "Create a 1/1 white Soldier creature token.");
        assert_eq!(
            recognize_token(&mutation.text).unwrap().text,
            "Create two 1/1 white Soldier creature tokens."
        );
        assert!(recognize_token("Create an 1/1 white Spirit creature token.").is_none());
        assert!(recognize_token("Create a Food token.").is_none());
        assert!(recognize_token("Create a 1/1 token and a Food token.").is_none());
        assert!(recognize_token("Create a 1/1 white \"Soldier\" creature token.").is_none());
        assert!(recognize_token("Create a 1/1 white Soldier\ncreature token.").is_none());
        assert!(recognize_token("Create a 1/1 white Soldier (armed) creature token.").is_none());
        assert!(
            recognize_token("Create a 1/1 white Soldier creature token. Draw a card.").is_none()
        );
    }

    #[test]
    fn destroy_grammar_is_reciprocal_and_all_consuming() {
        assert_eq!(
            recognize_destroy("Destroy target creature.").unwrap().text,
            "Destroy target nontoken creature."
        );
        assert_eq!(
            recognize_destroy("Destroy target nontoken creature.")
                .unwrap()
                .text,
            "Destroy target creature."
        );
        assert!(recognize_destroy("Destroy target creature. Draw a card.").is_none());
    }

    #[test]
    fn mutation_kinds_preserve_report_labels() {
        let cases = [
            (
                MutationKind::LifeYouToTargetOpponent,
                "life_recipient_v1",
                "you_to_target_opponent",
            ),
            (
                MutationKind::LifeTargetOpponentToYou,
                "life_recipient_v1",
                "target_opponent_to_you",
            ),
            (
                MutationKind::TokenOneToTwo,
                "token_count_article_two_v1",
                "one_to_two",
            ),
            (
                MutationKind::TokenTwoToOne,
                "token_count_article_two_v1",
                "two_to_one",
            ),
            (
                MutationKind::DestroyCreatureToNontoken,
                "destroy_nontoken_v1",
                "creature_to_nontoken",
            ),
            (
                MutationKind::DestroyNontokenToCreature,
                "destroy_nontoken_v1",
                "nontoken_to_creature",
            ),
        ];

        for (kind, family, direction) in cases {
            assert_eq!(kind.family(), family);
            assert_eq!(kind.direction(), direction);
        }
    }

    #[test]
    fn production_angels_mercy_reaches_an_exact_life_projection() {
        let mut base = raw_card("You gain 7 life.");
        base.name = "Angel's Mercy".to_string();
        let mut mutant = base.clone();
        mutant.text = Some("Target opponent gains 7 life.".to_string());
        assert_eq!(base.name, mutant.name);
        assert_eq!(base.types, mutant.types);
        assert_eq!(base.mana_value, mutant.mana_value);
        assert_eq!(
            base.identifiers.scryfall_oracle_id,
            mutant.identifiers.scryfall_oracle_id
        );
        let base_face = build_oracle_face(&base, Some("fixture-oracle-id".to_string()));
        let mutant_face = build_oracle_face(&mutant, Some("fixture-oracle-id".to_string()));
        let (status, _, _, projections) = classify(
            &base_face,
            &mutant_face,
            MutationKind::LifeYouToTargetOpponent,
        );
        assert_eq!(status, Status::ChangedAsRequired);
        assert!(projections.base.is_some());
        assert!(projections.mutant.is_some());
    }

    fn production_status(text: &str) -> Status {
        let base = raw_card(text);
        let mutation = recognize_mutation(text).expect("fixture must reach a controlled family");
        let mut mutant = base.clone();
        mutant.text = Some(mutation.text);
        let base_face = build_oracle_face(&base, Some("fixture-oracle-id".to_string()));
        let mutant_face = build_oracle_face(&mutant, Some("fixture-oracle-id".to_string()));
        classify(&base_face, &mutant_face, mutation.kind).0
    }

    #[test]
    fn production_synthesis_reaches_exact_token_and_destroy_projections() {
        assert_eq!(
            production_status("Create two 1/1 white Soldier creature tokens."),
            Status::ChangedAsRequired
        );
        assert_eq!(
            production_status("Destroy target creature."),
            Status::ChangedAsRequired
        );
    }

    #[test]
    fn support_and_diagnostics_precede_carrier_projection() {
        let clean = build_oracle_face(
            &raw_card("You gain 7 life."),
            Some("fixture-oracle-id".to_string()),
        );
        let mut unsupported = clean.clone();
        unsupported.abilities = vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::unimplemented("fixture", "unsupported"),
        )];
        assert_eq!(
            classify(
                &unsupported,
                &unsupported,
                MutationKind::LifeYouToTargetOpponent,
            )
            .0,
            Status::BothUnsupported
        );
        assert_eq!(
            classify(&unsupported, &clean, MutationKind::LifeYouToTargetOpponent,).0,
            Status::BaseUnsupported
        );
        assert_eq!(
            classify(&clean, &unsupported, MutationKind::LifeYouToTargetOpponent,).0,
            Status::MutantUnsupported
        );

        let warning = OracleDiagnostic::TargetFallback {
            context: "fixture".to_string(),
            text: "fixture".to_string(),
            line_index: 0,
        };
        let mut diagnostic = clean.clone();
        diagnostic.parse_warnings.push(warning);
        assert_eq!(
            classify(
                &diagnostic,
                &diagnostic,
                MutationKind::LifeYouToTargetOpponent,
            )
            .0,
            Status::BothDiagnostic
        );
        assert_eq!(
            classify(&diagnostic, &clean, MutationKind::LifeYouToTargetOpponent,).0,
            Status::BaseDiagnostic
        );
        assert_eq!(
            classify(&clean, &diagnostic, MutationKind::LifeYouToTargetOpponent,).0,
            Status::MutantDiagnostic
        );
    }

    #[test]
    fn comparison_statuses_distinguish_collision_extra_delta_and_exact_delta() {
        let controller = Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 3 },
            player: TargetFilter::Controller,
        };
        let opponent = Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 3 },
            player: TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
        };
        let opponent_wrong_amount = Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 4 },
            player: TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
        };
        assert_eq!(
            compare_life(
                &controller,
                &controller,
                MutationKind::LifeYouToTargetOpponent,
            ),
            Status::SemanticCollision
        );
        assert_eq!(
            compare_life(
                &controller,
                &opponent,
                MutationKind::LifeYouToTargetOpponent,
            ),
            Status::ChangedAsRequired
        );

        assert_eq!(
            compare_life(
                &controller,
                &opponent_wrong_amount,
                MutationKind::LifeYouToTargetOpponent,
            ),
            Status::ProjectionChangedElsewhere
        );
        let unsupported_opponent_shape = Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 3 },
            player: TargetFilter::Opponent,
        };
        assert_eq!(
            compare_life(
                &controller,
                &unsupported_opponent_shape,
                MutationKind::LifeYouToTargetOpponent,
            ),
            Status::UncheckableCarrierCount
        );
    }

    #[test]
    fn zero_and_multiple_carriers_are_uncheckable_after_clean_gates() {
        let empty = CardFace::default();
        assert_eq!(
            classify(&empty, &empty, MutationKind::LifeYouToTargetOpponent).0,
            Status::UncheckableCarrierCount
        );
        let mut multiple = empty.clone();
        multiple.abilities = vec![
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            ),
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            ),
        ];
        assert_eq!(
            classify(&multiple, &multiple, MutationKind::LifeYouToTargetOpponent,).0,
            Status::UncheckableCarrierCount
        );
    }

    #[test]
    fn duplicate_nontoken_properties_are_uncheckable() {
        let destroy = |properties| Effect::Destroy {
            target: TargetFilter::Typed(TypedFilter {
                properties,
                ..TypedFilter::creature()
            }),
            cant_regenerate: false,
        };
        let unqualified = destroy(vec![]);
        let duplicate = destroy(vec![FilterProp::NonToken, FilterProp::NonToken]);
        assert_eq!(
            compare_destroy(
                &unqualified,
                &duplicate,
                MutationKind::DestroyCreatureToNontoken,
            ),
            Status::UncheckableCarrierCount
        );
    }

    #[test]
    fn carrier_count_includes_siblings_and_nested_sub_abilities() {
        let gain = || Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        };
        let face = CardFace {
            abilities: vec![
                AbilityDefinition::new(AbilityKind::Spell, gain())
                    .sub_ability(AbilityDefinition::new(AbilityKind::Spell, gain())),
                AbilityDefinition::new(AbilityKind::Spell, gain()),
            ],
            ..CardFace::default()
        };
        assert_eq!(
            carriers(&face, MutationKind::LifeYouToTargetOpponent).len(),
            3
        );
    }

    #[test]
    fn report_order_and_rendering_are_deterministic() {
        fn raw_value(name: &str, text: &str) -> serde_json::Value {
            serde_json::json!({
                "name": name,
                "colors": ["W"],
                "colorIdentity": ["W"],
                "layout": "normal",
                "type": "Instant",
                "types": ["Instant"],
                "subtypes": [],
                "supertypes": [],
                "text": text,
                "manaValue": 1.0,
                "legalities": {},
                "identifiers": { "scryfallOracleId": name }
            })
        }

        let first = serde_json::json!({
            "data": {
                "Zulu": [raw_value("Zulu", "Destroy target creature.")],
                "Alpha": [raw_value("Alpha", "You gain 3 life.")]
            }
        });
        let second = serde_json::json!({
            "data": {
                "Alpha": [raw_value("Alpha", "You gain 3 life.")],
                "Zulu": [raw_value("Zulu", "Destroy target creature.")]
            }
        });
        let directory = env::temp_dir();
        let first_path = directory.join(format!("oracle-contrastive-{}-a.json", process::id()));
        let second_path = directory.join(format!("oracle-contrastive-{}-b.json", process::id()));
        fs::write(&first_path, serde_json::to_vec(&first).unwrap()).unwrap();
        fs::write(&second_path, serde_json::to_vec(&second).unwrap()).unwrap();
        let mut first_report = audit(&first_path, "fixture".to_string()).unwrap();
        let mut second_report = audit(&second_path, "fixture".to_string()).unwrap();
        fs::remove_file(first_path).unwrap();
        fs::remove_file(second_path).unwrap();

        // Input hashes truthfully differ when JSON member order differs. Once
        // that provenance field is held constant, result bytes must not depend
        // on the source map's insertion order.
        second_report.input_sha256 = first_report.input_sha256.clone();
        let first_bytes = render_report(&first_report).unwrap();
        let second_bytes = render_report(&second_report).unwrap();
        assert_eq!(first_bytes, second_bytes);
        assert_eq!(first_bytes.last(), Some(&b'\n'));
        let rendered = String::from_utf8(first_bytes).unwrap();
        assert!(!rendered.contains(directory.to_string_lossy().as_ref()));
        assert!(!rendered.contains("timestamp"));
        first_report.results.reverse();
        assert_ne!(render_report(&first_report).unwrap(), second_bytes);
    }

    #[test]
    fn output_aliases_are_rejected_without_modifying_the_corpus() {
        let directory = tempfile::tempdir().unwrap();
        let corpus = directory.path().join("AtomicCards.json");
        let original = br#"{"data":{}}"#;
        fs::write(&corpus, original).unwrap();

        let run_with_output = |output: &Path| {
            let mut unused_stdout = Vec::new();
            run_with_args(
                vec![
                    corpus.to_string_lossy().into_owned(),
                    "--output".to_string(),
                    output.to_string_lossy().into_owned(),
                ],
                &mut unused_stdout,
            )
            .unwrap_err()
            .to_string()
        };

        assert!(run_with_output(&corpus).contains("must not overwrite the input corpus"));
        assert_eq!(fs::read(&corpus).unwrap(), original);

        let nested = directory.path().join("nested");
        fs::create_dir(&nested).unwrap();
        let dot_dot_alias = nested.join("..").join("AtomicCards.json");
        assert!(run_with_output(&dot_dot_alias).contains("must not overwrite the input corpus"));
        assert_eq!(fs::read(&corpus).unwrap(), original);

        #[cfg(unix)]
        {
            let symlink = directory.path().join("symlink.json");
            std::os::unix::fs::symlink(&corpus, &symlink).unwrap();
            assert!(run_with_output(&symlink).contains("must not overwrite the input corpus"));
            assert_eq!(fs::read(&corpus).unwrap(), original);

            let hard_link = directory.path().join("hard-link.json");
            fs::hard_link(&corpus, &hard_link).unwrap();
            assert!(run_with_output(&hard_link).contains("must not overwrite the input corpus"));
            assert_eq!(fs::read(&corpus).unwrap(), original);
        }

        assert!(!paths_refer_to_same_file(
            &corpus,
            &directory.path().join("new-report.json")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn atomic_output_replaces_a_raced_symlink_without_modifying_the_corpus() {
        let directory = tempfile::tempdir().unwrap();
        let corpus = directory.path().join("AtomicCards.json");
        let output = directory.path().join("report.json");
        let original = br#"{"data":{}}"#;
        fs::write(&corpus, original).unwrap();
        fs::write(&output, b"old report").unwrap();

        assert!(!paths_refer_to_same_file(&corpus, &output));
        fs::remove_file(&output).unwrap();
        std::os::unix::fs::symlink(&corpus, &output).unwrap();

        write_report_atomic(&output, b"new report\n").unwrap();

        assert_eq!(fs::read(&corpus).unwrap(), original);
        assert_eq!(fs::read(&output).unwrap(), b"new report\n");
        assert!(!fs::symlink_metadata(&output)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn absolute_input_and_output_locations_do_not_enter_report_labels() {
        let directory = env::temp_dir().join(format!("oracle-contrastive-cli-{}", process::id()));
        let corpus_directory = directory.join("mtgjson");
        let corpus = corpus_directory.join("AtomicCards.json");
        fs::create_dir_all(&corpus_directory).unwrap();
        let fixture = serde_json::json!({
            "data": {
                "Fixture": [{
                    "name": "Fixture",
                    "colors": ["W"],
                    "colorIdentity": ["W"],
                    "layout": "normal",
                    "type": "Instant",
                    "types": ["Instant"],
                    "subtypes": [],
                    "supertypes": [],
                    "text": "You gain 3 life.",
                    "manaValue": 1.0,
                    "legalities": {},
                    "identifiers": { "scryfallOracleId": "fixture" }
                }]
            }
        });
        fs::write(&corpus, serde_json::to_vec(&fixture).unwrap()).unwrap();
        let spelling = corpus.to_string_lossy();
        let (resolved, label) = resolve_input(&spelling);
        assert_eq!(resolved, corpus);
        assert_eq!(label, "mtgjson/AtomicCards.json");

        let output = directory.join("report.json");
        fs::write(&output, b"must be overwritten").unwrap();
        let mut unused_stdout = Vec::new();
        run_with_args(
            vec![
                spelling.to_string(),
                "--output".to_string(),
                output.to_string_lossy().into_owned(),
            ],
            &mut unused_stdout,
        )
        .unwrap();
        assert!(unused_stdout.is_empty());
        let file_bytes = fs::read(&output).unwrap();
        let mut stdout_bytes = Vec::new();
        run_with_args(vec![spelling.to_string()], &mut stdout_bytes).unwrap();
        assert_eq!(file_bytes, stdout_bytes);
        let rendered = String::from_utf8(file_bytes).unwrap();
        assert!(!rendered.contains(directory.to_string_lossy().as_ref()));
        assert!(rendered.contains("\"input_label\": \"mtgjson/AtomicCards.json\""));
        assert!(rendered.contains("\"family\": \"life_recipient_v1\""));
        fs::remove_file(output).unwrap();
        fs::remove_file(corpus).unwrap();
        fs::remove_dir(corpus_directory).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
