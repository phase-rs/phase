use std::cmp::Reverse;
use std::collections::BTreeSet;

use rand::Rng;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;

use engine::database::CardDatabase;
use engine::game::deck_validation::card_color_identity;
use engine::types::mana::ManaColor;

use crate::config::AiDifficulty;

use super::types::{canonical_constraints, tier_distance};
use super::{
    AiDeckCandidate, LabelProvenance, PodAssignment, PodConstraint, PodRelaxation, PodSeat,
    PodSelectionError, PodSelectionRequest, SeatAttribute, TierEnforcement,
};

/// The fixed order in which advisory constraints are dropped. Changing it
/// changes seeded output and is therefore a behavioural contract.
pub const RELAXATION_ORDER: [PodConstraint; 5] = [
    PodConstraint::LabelProvenance,
    PodConstraint::BracketDistance,
    PodConstraint::Archetype,
    PodConstraint::Distinct(SeatAttribute::ColorIdentity),
    PodConstraint::Distinct(SeatAttribute::Commander),
];

#[derive(Clone)]
struct ResolvedCandidate<'a> {
    candidate: &'a AiDeckCandidate,
    provenance: Option<LabelProvenance>,
    color_identity: Vec<ManaColor>,
}

/// Select one deck per AI seat for a whole table at once.
///
/// The only nondeterminism is `request.seed`, consumed by `ChaCha8Rng`; equal
/// inputs therefore produce a replayable assignment. Bracket constraints are
/// advisory except under `TierEnforcement::HardGate`, which refuses tier
/// shortfalls rather than relaxing them.
pub fn select_pod(
    candidates: &[AiDeckCandidate],
    request: &PodSelectionRequest,
    db: &CardDatabase,
) -> Result<PodAssignment, PodSelectionError> {
    if request.enforcement == TierEnforcement::HardGate && request.seats > 3 {
        return Err(PodSelectionError::TooManySeats {
            seats: request.seats,
        });
    }

    let resolved = resolve_candidates(candidates, db)?;
    let mut active = canonical_constraints(request.constraints.clone());

    let base_available = available_distinct_decks(&resolved, request, &active, false);
    if base_available < usize::from(request.seats) {
        return Err(PodSelectionError::InsufficientCandidates {
            requested: request.seats,
            available: count_as_u8(base_available),
        });
    }

    if request.enforcement == TierEnforcement::HardGate {
        let hard_available = available_distinct_decks(&resolved, request, &active, true);
        if hard_available < usize::from(request.seats) {
            return Err(PodSelectionError::HardGateUnsatisfiable {
                requested: request.seats,
                available: count_as_u8(hard_available),
            });
        }
    }

    let mut rng = ChaCha8Rng::seed_from_u64(request.seed);
    let mut seats = Vec::with_capacity(usize::from(request.seats));
    let mut selected_commanders: Vec<Vec<String>> = Vec::with_capacity(usize::from(request.seats));
    let mut relaxations = Vec::new();

    for seat_index in 0..request.seats {
        loop {
            let admissible: Vec<usize> = resolved
                .iter()
                .enumerate()
                .filter(|(_, candidate)| {
                    candidate_satisfies(candidate, request, &active, &seats, &selected_commanders)
                })
                .map(|(index, _)| index)
                .collect();

            if let Some(index) = choose_best(&resolved, &admissible, request, &mut rng) {
                let chosen = &resolved[index];
                let tier = chosen.candidate.label.as_ref().map(|label| label.tier);
                let difficulty = AiDifficulty::for_bracket(tier.unwrap_or_default());
                selected_commanders.push(chosen.candidate.commander.clone());
                seats.push(PodSeat {
                    seat_index,
                    candidate_id: chosen.candidate.id.clone(),
                    tier,
                    provenance: chosen.provenance,
                    difficulty,
                    color_identity: chosen.color_identity.clone(),
                });
                break;
            }

            if let Some(relaxed) = next_relaxation(&active, request.enforcement) {
                active.retain(|constraint| *constraint != relaxed);
                relaxations.push(PodRelaxation {
                    seat_index,
                    relaxed,
                });
                continue;
            }

            let error = match request.enforcement {
                TierEnforcement::Advisory => PodSelectionError::InsufficientCandidates {
                    requested: request.seats,
                    available: count_as_u8(seats.len()),
                },
                TierEnforcement::HardGate => PodSelectionError::HardGateUnsatisfiable {
                    requested: request.seats,
                    available: count_as_u8(seats.len()),
                },
            };
            return Err(error);
        }
    }

    Ok(PodAssignment { seats, relaxations })
}

fn resolve_candidates<'a>(
    candidates: &'a [AiDeckCandidate],
    db: &CardDatabase,
) -> Result<Vec<ResolvedCandidate<'a>>, PodSelectionError> {
    candidates
        .iter()
        .map(|candidate| {
            let colors = commander_color_identity(db, &candidate.commander, &candidate.id)?;

            let provenance = candidate.label.as_ref().map(|label| {
                if label.provenance == LabelProvenance::Declared
                    && label.data_version != db.bracket_lists_version()
                {
                    LabelProvenance::Estimated
                } else {
                    label.provenance
                }
            });

            Ok(ResolvedCandidate {
                candidate,
                provenance,
                color_identity: colors,
            })
        })
        .collect()
}

/// Resolves the combined identity of one or more commanders in canonical WUBRG
/// order. `candidate_id` is copied into [`PodSelectionError::UnknownCommander`]
/// so callers retain precise diagnostics for either candidates or occupied seats.
pub fn commander_color_identity(
    db: &CardDatabase,
    commander: &[String],
    candidate_id: &str,
) -> Result<Vec<ManaColor>, PodSelectionError> {
    let mut colors = Vec::new();
    for name in commander {
        let face =
            db.get_face_by_name(name)
                .ok_or_else(|| PodSelectionError::UnknownCommander {
                    candidate_id: candidate_id.to_string(),
                    name: name.clone(),
                })?;
        // CR 903.4: A commander's color identity includes mana symbols, color indicators, and characteristic-defining abilities.
        let identity = card_color_identity(face);
        for color in ManaColor::ALL {
            if identity.contains(&color) && !colors.contains(&color) {
                colors.push(color);
            }
        }
    }
    Ok(canonical_colors(&colors))
}

fn available_distinct_decks(
    candidates: &[ResolvedCandidate<'_>],
    request: &PodSelectionRequest,
    active: &[PodConstraint],
    enforce_hard_tier: bool,
) -> usize {
    candidates
        .iter()
        .filter(|candidate| passes_never_relaxed(candidate, request, active))
        .filter(|candidate| !enforce_hard_tier || passes_allowed_tier(candidate, request))
        .map(|candidate| candidate.candidate.id.as_str())
        .collect::<BTreeSet<_>>()
        .len()
}

/// Enforce constraints that never enter the relaxation ladder. Unmeasured
/// coverage is not a failure: the floor guards against a measured broken deck,
/// matching callers that admit candidates until a below-floor value is known.
fn passes_never_relaxed(
    candidate: &ResolvedCandidate<'_>,
    request: &PodSelectionRequest,
    active: &[PodConstraint],
) -> bool {
    if active.contains(&PodConstraint::CoverageFloor)
        && candidate
            .candidate
            .coverage_pct
            .is_some_and(|pct| pct < request.coverage_floor_pct)
    {
        return false;
    }
    if active.contains(&PodConstraint::Distinct(SeatAttribute::Deck))
        && request
            .occupied
            .iter()
            .any(|seat| seat.deck_id == candidate.candidate.id)
    {
        return false;
    }
    true
}

fn candidate_satisfies(
    candidate: &ResolvedCandidate<'_>,
    request: &PodSelectionRequest,
    active: &[PodConstraint],
    seats: &[PodSeat],
    selected_commanders: &[Vec<String>],
) -> bool {
    if !passes_never_relaxed(candidate, request, active) {
        return false;
    }
    if seats
        .iter()
        .any(|seat| seat.candidate_id == candidate.candidate.id)
    {
        return false;
    }
    if request.enforcement == TierEnforcement::HardGate && !passes_allowed_tier(candidate, request)
    {
        return false;
    }
    if active.contains(&PodConstraint::LabelProvenance)
        && candidate.provenance != Some(LabelProvenance::Declared)
    {
        return false;
    }
    if active.contains(&PodConstraint::BracketDistance) && !passes_allowed_tier(candidate, request)
    {
        return false;
    }
    if active.contains(&PodConstraint::Archetype)
        && request.archetype.is_some()
        && candidate.candidate.archetype != request.archetype
    {
        return false;
    }
    if active.contains(&PodConstraint::Distinct(SeatAttribute::Commander))
        && commander_collides(candidate, request, selected_commanders)
    {
        return false;
    }
    if active.contains(&PodConstraint::Distinct(SeatAttribute::ColorIdentity))
        && color_identity_collides(candidate, request, seats)
    {
        return false;
    }
    true
}

fn passes_allowed_tier(candidate: &ResolvedCandidate<'_>, request: &PodSelectionRequest) -> bool {
    request.allowed.is_empty()
        || candidate
            .candidate
            .label
            .as_ref()
            .is_some_and(|label| request.allowed.contains(&label.tier))
}

fn commander_collides(
    candidate: &ResolvedCandidate<'_>,
    request: &PodSelectionRequest,
    selected_commanders: &[Vec<String>],
) -> bool {
    candidate.candidate.commander.iter().any(|commander| {
        request
            .occupied
            .iter()
            .any(|seat| seat.commander.contains(commander))
            || selected_commanders
                .iter()
                .any(|commanders| commanders.contains(commander))
    })
}

fn color_identity_collides(
    candidate: &ResolvedCandidate<'_>,
    request: &PodSelectionRequest,
    seats: &[PodSeat],
) -> bool {
    request
        .occupied
        .iter()
        .any(|seat| canonical_colors(&seat.color_identity) == candidate.color_identity)
        || seats
            .iter()
            .any(|seat| seat.color_identity == candidate.color_identity)
}

fn canonical_colors(colors: &[ManaColor]) -> Vec<ManaColor> {
    ManaColor::ALL
        .into_iter()
        .filter(|color| colors.contains(color))
        .collect()
}

fn choose_best(
    candidates: &[ResolvedCandidate<'_>],
    admissible: &[usize],
    request: &PodSelectionRequest,
    rng: &mut ChaCha8Rng,
) -> Option<usize> {
    let best_rank = admissible
        .iter()
        .map(|index| candidate_rank(&candidates[*index], request))
        .min()?;
    let tied: Vec<usize> = admissible
        .iter()
        .copied()
        .filter(|index| candidate_rank(&candidates[*index], request) == best_rank)
        .collect();
    Some(tied[rng.random_range(0..tied.len())])
}

fn candidate_rank(
    candidate: &ResolvedCandidate<'_>,
    request: &PodSelectionRequest,
) -> (Reverse<Option<LabelProvenance>>, u8) {
    (
        provenance_rank(candidate.provenance),
        candidate
            .candidate
            .label
            .as_ref()
            .map_or(u8::MAX, |label| match request.prefer {
                Some(prefer) => tier_distance(label.tier, prefer),
                None if request.allowed.is_empty() || request.allowed.contains(&label.tier) => 0,
                None => u8::MAX,
            }),
    )
}

fn provenance_rank(provenance: Option<LabelProvenance>) -> Reverse<Option<LabelProvenance>> {
    Reverse(provenance)
}

fn next_relaxation(
    active: &[PodConstraint],
    enforcement: TierEnforcement,
) -> Option<PodConstraint> {
    RELAXATION_ORDER.iter().copied().find(|constraint| {
        active.contains(constraint)
            && !(enforcement == TierEnforcement::HardGate
                && matches!(
                    constraint,
                    PodConstraint::LabelProvenance | PodConstraint::BracketDistance
                ))
    })
}

fn count_as_u8(count: usize) -> u8 {
    u8::try_from(count).unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pod_selection::{BracketLabel, TierSet};
    use engine::game::bracket_estimate::CommanderBracketTier;

    #[test]
    fn empty_allowed_set_has_zero_tier_distance() {
        let candidate = AiDeckCandidate {
            id: "labelled".to_string(),
            commander: Vec::new(),
            label: Some(BracketLabel {
                tier: CommanderBracketTier::Optimized,
                provenance: LabelProvenance::Estimated,
                data_version: String::new(),
            }),
            coverage_pct: None,
            archetype: None,
        };
        let resolved = ResolvedCandidate {
            candidate: &candidate,
            provenance: Some(LabelProvenance::Estimated),
            color_identity: Vec::new(),
        };
        let request = PodSelectionRequest::new(
            TierSet::default(),
            None,
            TierEnforcement::Advisory,
            1,
            Vec::new(),
            0,
            None,
            0,
        );

        assert_eq!(candidate_rank(&resolved, &request).1, 0);
    }
}
