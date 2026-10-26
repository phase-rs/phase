//! CR 603.12: support boundary for the "When <zone change> this way, <body>"
//! reflexive triggered ability.
//!
//! `conditions::strip_if_you_do_conditional_with_head` lowers that head to the
//! `WhenYouDo` creation gate guarded by `ZoneChangedThisWay` and reports the
//! head's typed verb and occurrence (`oracle_nom::condition::ThisWayHead`,
//! produced by the shared "… this way" combinators). The runtime creates the
//! reflexive after the zone change and chooses its targets as it is put on the
//! stack (CR 603.3d). That gate reads the zone-change ledger the instruction
//! just before it published, and the reflexive's body reads only referents the
//! stack object carries. This module is the semantic boundary: per clause it
//! decides whether both hold, and anything else fails closed.

use nom::bytes::complete::tag;
use nom::Parser;

use super::super::oracle_nom::condition::{ThisWayHead, ThisWayOccurrence, ThisWayVerb};
use super::super::oracle_nom::error::OracleError;
use super::super::oracle_nom::primitives as nom_primitives;
use super::{has_typed_target_widened, ClauseDisposition, ClauseIr};
use crate::types::ability::{AbilityCondition, Effect, PlayerFilter, QuantityExpr, TargetFilter};
use crate::types::zones::Zone;

/// CR 608.2c: whether `effect` is an instruction whose every resolution path
/// — the synchronous one and the resume after its player choice — publishes
/// the zone change `verb` names into the ledger the gate reads. A positive
/// allowlist of producers each covered by a runtime test on its paused path;
/// an unlisted producer (a spellbook draft, an exile-until loop, a mill, a
/// surveil, a mass move) fails closed.
fn publishes_event(effect: &Effect, verb: ThisWayVerb) -> bool {
    match verb {
        ThisWayVerb::Discard => matches!(effect, Effect::Discard { .. }),
        ThisWayVerb::Sacrifice => matches!(effect, Effect::Sacrifice { .. }),
        ThisWayVerb::Exile => {
            matches!(effect, Effect::ExileTop { .. } | Effect::ChangeZone { .. })
        }
        ThisWayVerb::EnterBattlefield => {
            matches!(effect, Effect::ChangeZone { .. } | Effect::Dig { .. })
        }
        ThisWayVerb::Mill
        | ThisWayVerb::PutIntoGraveyard
        | ThisWayVerb::Destroy
        | ThisWayVerb::Return
        | ThisWayVerb::Counter
        | ThisWayVerb::Die => false,
    }
}

/// CR 603.12a: a reflexive whose head counts ONE occurrence ("When you exile a
/// card this way") triggers once for each card the instruction moved, but the
/// creation gate creates one trigger. Supported only when the producer itself
/// moves at most one object: a fixed count of one, or a move whose own target
/// names one object. A target handed over by an earlier instruction (an
/// inherited parent target, "them", the cards exiled with this) carries no
/// cardinality the parser can prove at this boundary, so it fails closed.
fn producer_moves_at_most_one(producer: &ClauseIr) -> bool {
    let one = |count: &QuantityExpr| matches!(count, QuantityExpr::Fixed { value: 1 });
    let single_instruction = producer.multi_target.is_none()
        && producer.parsed.multi_target.is_none()
        && producer.repeat_for.is_none();
    single_instruction
        && match &producer.parsed.effect {
            Effect::Discard { count, .. }
            | Effect::ExileTop { count, .. }
            | Effect::Sacrifice { count, .. } => one(count),
            // `keep_count: None` keeps one card, except in the all-seen
            // library-to-library reorder mode.
            Effect::Dig {
                keep_count,
                keep_count_expr: None,
                destination,
                ..
            } => match keep_count {
                Some(kept) => *kept == 1,
                None => *destination != Some(Zone::Library),
            },
            Effect::ChangeZone { target, .. } => target_names_one_object(target),
            _ => false,
        }
}

/// CR 603.12a: whether a move's own target names one object. A referent handed
/// over by an earlier instruction (an inherited parent target, "those cards",
/// the cards exiled with this) or a pairing ("this creature and the creature
/// it's paired with") carries no cardinality of its own: the resolver moves
/// every member. Recurses through `And`/`Or` only; a filter property that
/// merely mentions a referent (`DistinctFrom { ParentTarget }`) excludes an
/// object, it doesn't move one.
fn target_names_one_object(target: &TargetFilter) -> bool {
    match target {
        // Populations, of any size.
        TargetFilter::ParentTarget
        | TargetFilter::ParentTargetSlot { .. }
        | TargetFilter::SourceOrPaired
        | TargetFilter::LastZoneChanged
        | TargetFilter::LastRevealed
        | TargetFilter::LastCreated
        | TargetFilter::CostPaidObject
        | TargetFilter::ExiledBySource
        | TargetFilter::TrackedSetFiltered { .. }
        | TargetFilter::ControllerAndControlledPermanents { .. } => false,
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().all(target_names_one_object)
        }
        // One object this instruction selects or names, a singular referent,
        // or no object population at all.
        TargetFilter::Typed(_)
        | TargetFilter::Not { .. }
        | TargetFilter::TrackedSet { .. }
        | TargetFilter::PlayerMatching { .. }
        | TargetFilter::ChosenDamageSource { .. }
        | TargetFilter::None
        | TargetFilter::Any
        | TargetFilter::Player
        | TargetFilter::Controller
        | TargetFilter::SourceController
        | TargetFilter::Opponent
        | TargetFilter::SelfRef
        | TargetFilter::GrantingObject { .. }
        | TargetFilter::StackAbility { .. }
        | TargetFilter::StackSpell
        | TargetFilter::SpecificObject { .. }
        | TargetFilter::SpecificPlayer { .. }
        | TargetFilter::PlayerWhoChoseLabel { .. }
        | TargetFilter::Neighbor { .. }
        | TargetFilter::ScopedPlayer
        | TargetFilter::AttachedTo
        | TargetFilter::AmassedArmy
        | TargetFilter::ChosenCard
        | TargetFilter::ExiledCardByIndex { .. }
        | TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSource
        | TargetFilter::EventTarget
        | TargetFilter::TriggeringSourceController
        | TargetFilter::EventTargetController
        | TargetFilter::DeclaredPlayer { .. }
        | TargetFilter::ParentTargetController
        | TargetFilter::ParentTargetOwner
        | TargetFilter::SourceChosenPlayer
        | TargetFilter::OriginalController
        | TargetFilter::OriginalSource
        | TargetFilter::PostReplacementSourceController
        | TargetFilter::PostReplacementDamageSource
        | TargetFilter::PostReplacementDamageTarget
        | TargetFilter::PostReplacementDamageTargetOwner
        | TargetFilter::DefendingPlayer
        | TargetFilter::AttackedPlayerOrPlaneswalker
        | TargetFilter::HasChosenName
        | TargetFilter::Named { .. }
        | TargetFilter::Owner
        | TargetFilter::AllPlayers => true,
    }
}

/// CR 608.2c: a play permission over the moved cards ("You may play that card
/// this turn") directly before the head is not the moving instruction; the
/// runtime leaves the ledger in place across that one node
/// (`permission_node_keeps_ledger_for_this_way_head`, the same single effect
/// kind).
fn grants_play_permission(effect: &Effect) -> bool {
    matches!(effect, Effect::GrantCastingPermission { .. })
}

/// CR 603.12 + CR 608.2c: why a clause gated by a "When <zone change> this
/// way" reflexive head cannot be supported, or `None` when it can.
///
/// * `zone_change_reflexive_event_source` — the instruction whose ledger the
///   gate reads (the nearest preceding instruction, skipping a play
///   permission) is not one that performs the head's zone change on every
///   resolution path; or it iterates over players ("each player exiles a
///   card"), which would create one reflexive per player.
/// * `zone_change_reflexive_occurrence` — the head counts one occurrence ("a
///   card") but the producer can move several (CR 603.12a).
/// * `zone_change_reflexive_scoped_body` — the body iterates over players
///   ("each opponent sacrifices that many"); such a body resolves inside the
///   creating resolution instead of as its own stack object.
/// * `zone_change_reflexive_unbound_referent` — the body names an object the
///   reflexive stack object cannot carry: a bare `ParentTarget` operand (its
///   target, or a fight's subject) while no earlier instruction announced a
///   target for it to inherit or while the body itself says "target"; or a
///   copy with no copy source.
pub(super) fn unsupported_zone_change_reflexive(
    condition: Option<&AbilityCondition>,
    head: Option<ThisWayHead>,
    effect: &Effect,
    body_lower: &str,
    body_player_scope: Option<&PlayerFilter>,
    clauses: &[ClauseIr],
) -> Option<&'static str> {
    let condition = condition?;
    if !condition.has_when_you_do_marker() {
        return None;
    }
    condition.zone_changed_this_way_head()?;
    let mut preceding = clauses
        .iter()
        .rev()
        .filter(|clause| !matches!(clause.disposition, ClauseDisposition::Continue { .. }));
    let producer = match preceding.next() {
        Some(permission) if grants_play_permission(&permission.parsed.effect) => preceding.next(),
        nearest => nearest,
    };
    let (Some(producer), Some(head)) = (producer, head) else {
        return Some("zone_change_reflexive_event_source");
    };
    if producer.player_scope.is_some() || !publishes_event(&producer.parsed.effect, head.verb) {
        return Some("zone_change_reflexive_event_source");
    }
    if head.occurrence == ThisWayOccurrence::Single && !producer_moves_at_most_one(producer) {
        return Some("zone_change_reflexive_occurrence");
    }
    if body_player_scope.is_some() {
        return Some("zone_change_reflexive_scoped_body");
    }
    let body_names_a_target = nom_primitives::scan_at_word_boundaries(body_lower, |input| {
        tag::<_, _, OracleError<'_>>("target ").parse(input)
    })
    .is_some();
    let parent_announced_a_target = has_typed_target_widened(&producer.parsed.effect);
    // The body's object operands: its target, plus the fighting creature of a
    // fight ("it fights ~").
    let fight_subject = match effect {
        Effect::Fight { subject, .. } => Some(subject),
        _ => None,
    };
    let reads_parent_target = effect
        .target_filter()
        .into_iter()
        .chain(fight_subject)
        .any(|operand| matches!(operand, TargetFilter::ParentTarget));
    if reads_parent_target && (body_names_a_target || !parent_announced_a_target) {
        return Some("zone_change_reflexive_unbound_referent");
    }
    // A copy whose source lost its referent ("a token that's a copy of that
    // Aura" parsed with no source) names no object the stack object can carry.
    if matches!(
        effect,
        Effect::CopyTokenOf {
            target: TargetFilter::None,
            ..
        }
    ) {
        return Some("zone_change_reflexive_unbound_referent");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::target_names_one_object;
    use crate::types::ability::{TargetFilter, TypedFilter};
    use crate::types::identifiers::TrackedSetId;

    /// CR 603.12a: a move's target names one object only when it selects or
    /// names one. Handed-over referents and a pairing ("this creature and the
    /// creature it's paired with", which the resolver expands to the source
    /// and its partner) are populations.
    #[test]
    fn handed_over_referents_and_pairings_are_populations() {
        for population in [
            TargetFilter::ParentTarget,
            TargetFilter::SourceOrPaired,
            TargetFilter::LastRevealed,
            TargetFilter::LastZoneChanged,
            TargetFilter::ExiledBySource,
        ] {
            assert!(!target_names_one_object(&population), "{population:?}");
        }
        let typed = TargetFilter::Typed(TypedFilter::default());
        assert!(target_names_one_object(&typed));
        assert!(target_names_one_object(&TargetFilter::TrackedSet {
            id: TrackedSetId(0)
        }));
        assert!(!target_names_one_object(&TargetFilter::And {
            filters: vec![typed, TargetFilter::ParentTarget],
        }));
    }
}
