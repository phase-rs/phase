//! CR 603.2d — shared "X causes a triggered ability … to trigger" cause
//! classification for trigger doublers (`StaticMode::DoubleTriggers`) and
//! ability-triggering observers (`TriggerMode::AbilityTriggered`).
//!
//! Single authority for the `TriggerCause` axis: both call sites pass the
//! lowercased condition/line text and receive the classified cause, or `None`
//! when a printed cause clause matches no arm (the line stays a flagged gap).

use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::combinator::value;
use nom::sequence::{preceded, terminated};
use nom::Parser;

use super::oracle_nom::error::OracleError;
use super::oracle_nom::primitives as nom_primitives;
use crate::types::ability::PlayerRelation;
use crate::types::card_type::{CoreType, Supertype};
use crate::types::statics::{
    CausingObject, ControlledCreatureEvent, PlayerCauseAction, TriggerCause, ZoneChangeQualifier,
};

/// CR 603.2d: classify the cause phrase of a "[cause] causes a triggered
/// ability … to trigger" clause.
///
/// Cause classification by phrasing:
/// - "being dealt damage causes" / "dealt damage causes" — Wayta, Trainer
///   Prodigy (ControlledCreature { DealtDamage }); "becoming the target of"
///   — Valiant Emberkin (BecameTarget); "dealing combat damage to a player
///   causes" — Felix Five-Boots (DealsCombatDamageToPlayer).
/// - "attacking causes" — Isshin, Two Heavens as One (CreatureAttacking).
/// - "you gaining life causes" / "a player drawing a card causes" — Dr.
///   Beverly Crusher, Krang, the All-Powerful (PlayerAction); "turning a
///   face-down permanent face up causes" — Panoptic Projektor (TurnedFaceUp).
/// - "entering" / "enters the battlefield" / "enters" — Panharmonicon-class
///   (EntersBattlefield). Panharmonicon itself names "artifact or creature
///   entering", so both CoreTypes qualify; narrower wordings ("creature
///   entering") collapse to [Creature] only.
/// - Otherwise (e.g. "If a triggered ability ... triggers, it triggers an
///   additional time" — Roaming Throne, Strionic Resonator copies) use the
///   unrestricted `Any` cause; the doubler's `affected` filter narrows
///   which source's triggers qualify.
///
/// Returns `None` when a printed "causes a triggered ability" clause matches
/// no arm — the caller keeps the line a flagged gap instead of degrading to
/// the unrestricted `Any`.
pub(crate) fn classify_trigger_cause(lower: &str) -> Option<TriggerCause> {
    let cause = if let Some((_, spell_qualifier, _)) = nom_primitives::scan_preceded(lower, |i| {
        // CR 601.2 + CR 707.10: "you casting or copying <types> spell
        // causes" — Veyran, Voice of Duality class. The qualifier
        // between the verb pair and " spell causes" names the spell
        // types ("an instant or sorcery").
        preceded(
            tag::<_, _, OracleError<'_>>("you casting or copying "),
            take_until(" spell causes"),
        )
        .parse(i)
    }) {
        let mut core_types: Vec<CoreType> = Vec::new();
        if nom_primitives::scan_contains(spell_qualifier, "instant") {
            core_types.push(CoreType::Instant);
        }
        if nom_primitives::scan_contains(spell_qualifier, "sorcery") {
            core_types.push(CoreType::Sorcery);
        }
        if nom_primitives::scan_contains(spell_qualifier, "artifact") {
            core_types.push(CoreType::Artifact);
        }
        if nom_primitives::scan_contains(spell_qualifier, "creature") {
            core_types.push(CoreType::Creature);
        }
        if nom_primitives::scan_contains(spell_qualifier, "enchantment") {
            core_types.push(CoreType::Enchantment);
        }
        // An unqualified "a spell" leaves core_types empty = any spell.
        TriggerCause::ControllerCastOrCopiedSpell { core_types }
    } else if nom_primitives::scan_contains(lower, "becoming the target of") {
        // CR 603.2d + CR 115.1: "a creature you control becoming the target
        // of a spell or ability causes" (Valiant Emberkin).
        TriggerCause::ControlledCreature {
            event: ControlledCreatureEvent::BecameTarget,
        }
    } else if nom_primitives::scan_contains(
        lower,
        "a creature you control dealing combat damage to a player causes",
    ) {
        // CR 603.2d + CR 510.2: "a creature you control dealing combat
        // damage to a player causes" (Felix Five-Boots).
        TriggerCause::ControlledCreature {
            event: ControlledCreatureEvent::DealsCombatDamageToPlayer,
        }
    } else if nom_primitives::scan_contains(lower, "being dealt damage causes")
        || nom_primitives::scan_contains(lower, "dealt damage causes")
    {
        TriggerCause::ControlledCreature {
            event: ControlledCreatureEvent::DealtDamage,
        }
    } else if nom_primitives::scan_contains(lower, "attacking causes") {
        TriggerCause::CreatureAttacking
    } else if nom_primitives::scan_contains(lower, "dying causes") {
        TriggerCause::CreatureDying
    } else if let Some(cause) = parse_player_action_cause(lower) {
        cause
    } else if nom_primitives::scan_preceded(lower, |i| {
        tag::<_, _, OracleError<'_>>("turning a face-down permanent face up causes ").parse(i)
    })
    .is_some()
    {
        // CR 116.2b + CR 708.8: Panoptic Projektor.
        TriggerCause::TurnedFaceUp
    } else if let Some(cause) = parse_battlefield_transition_cause(lower) {
        cause
    } else if nom_primitives::scan_contains(lower, "entering")
        || nom_primitives::scan_contains(lower, "enters the battlefield")
    {
        // CR 603.6a: The entering-permanent's type is named in the
        // qualifier. "artifact or creature entering" = both; a bare
        // "creature entering" or "permanent entering" narrows
        // accordingly.
        let mut core_types: Vec<CoreType> = Vec::new();
        if nom_primitives::scan_contains(lower, "artifact") {
            core_types.push(CoreType::Artifact);
        }
        if nom_primitives::scan_contains(lower, "creature") {
            core_types.push(CoreType::Creature);
        }
        if nom_primitives::scan_contains(lower, "enchantment") {
            core_types.push(CoreType::Enchantment);
        }
        if nom_primitives::scan_contains(lower, "land") {
            core_types.push(CoreType::Land);
        }
        if nom_primitives::scan_contains(lower, "planeswalker") {
            core_types.push(CoreType::Planeswalker);
        }
        // Empty core_types (e.g. "a permanent entering") means any type.
        // CR 603.2d: "another permanent entering" (The Fish Brewer) is not
        // caused by the doubler's own entering.
        let entrant = if tag::<_, _, OracleError<'_>>("if another ")
            .parse(lower)
            .is_ok()
        {
            CausingObject::OtherThanDoubler
        } else {
            CausingObject::Any
        };
        TriggerCause::EntersBattlefield {
            core_types,
            entrant,
        }
    } else if nom_primitives::scan_preceded(lower, |i| {
        tag::<_, _, OracleError<'_>>("causes a triggered ability").parse(i)
    })
    .is_some()
    {
        // CR 603.2d: a printed cause clause no arm above types must not
        // degrade to the unrestricted `Any`, which would double triggers
        // the clause excludes (the doubler's own included). Decline so the
        // line stays a flagged gap.
        return None;
    } else {
        TriggerCause::Any
    };
    Some(cause)
}

/// CR 603.6a + CR 603.6c: Extract disjunctive qualifiers on the permanent whose
/// zone change caused a trigger doubler (Gandalf the White-class).
fn parse_zone_change_qualifiers(lower: &str) -> Vec<ZoneChangeQualifier> {
    let mut qualifiers = Vec::new();
    if nom_primitives::scan_contains(lower, "legendary") {
        qualifiers.push(ZoneChangeQualifier::Supertype(Supertype::Legendary));
    }
    if nom_primitives::scan_contains(lower, "artifact") {
        qualifiers.push(ZoneChangeQualifier::CoreType(CoreType::Artifact));
    }
    if nom_primitives::scan_contains(lower, "creature") {
        qualifiers.push(ZoneChangeQualifier::CoreType(CoreType::Creature));
    }
    if nom_primitives::scan_contains(lower, "enchantment") {
        qualifiers.push(ZoneChangeQualifier::CoreType(CoreType::Enchantment));
    }
    if nom_primitives::scan_contains(lower, "land") {
        qualifiers.push(ZoneChangeQualifier::CoreType(CoreType::Land));
    }
    if nom_primitives::scan_contains(lower, "planeswalker") {
        qualifiers.push(ZoneChangeQualifier::CoreType(CoreType::Planeswalker));
    }
    qualifiers
}

/// CR 603.2d: "<player> <action> causes" — a doubler's player-action cause:
/// "you gaining life causes" (Dr. Beverly Crusher, CR 119.3) or "a player
/// drawing a card causes" (Krang, the All-Powerful, CR 121.1). The player and
/// the action are independent axes.
fn parse_player_action_cause(lower: &str) -> Option<TriggerCause> {
    let (_, (player, action), _) = nom_primitives::scan_preceded(lower, |i| {
        terminated(
            (
                terminated(
                    alt((
                        value(
                            PlayerRelation::Controller,
                            tag::<_, _, OracleError<'_>>("you"),
                        ),
                        value(PlayerRelation::All, tag("a player")),
                    )),
                    tag(" "),
                ),
                alt((
                    value(PlayerCauseAction::GainedLife, tag("gaining life")),
                    value(PlayerCauseAction::DrewCard, tag("drawing a card")),
                )),
            ),
            tag(" causes "),
        )
        .parse(i)
    })?;
    Some(TriggerCause::PlayerAction { player, action })
}

/// CR 603.6a + CR 603.6c: Parse Gandalf-class battlefield transition causes.
/// Returns `None` for Panharmonicon-style enter-only lines without a legendary
/// supertype so the legacy `EntersBattlefield` path stays stable.
fn parse_battlefield_transition_cause(lower: &str) -> Option<TriggerCause> {
    let enter = nom_primitives::scan_contains(lower, "entering")
        || nom_primitives::scan_contains(lower, "enters the battlefield");
    let leave = nom_primitives::scan_contains(lower, "leaving")
        || nom_primitives::scan_contains(lower, "leaves the battlefield");
    if !enter && !leave {
        return None;
    }

    let has_legendary = nom_primitives::scan_contains(lower, "legendary");
    if enter && !leave && !has_legendary {
        return None;
    }

    Some(TriggerCause::BattlefieldTransition {
        enter,
        leave,
        qualifiers: parse_zone_change_qualifiers(lower),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observer_conditions_classify_to_their_causes() {
        // Firebender Ascension condition (verbatim, lowercased).
        assert_eq!(
            classify_trigger_cause(
                "whenever a creature you control attacking causes a triggered ability of that creature to trigger"
            ),
            Some(TriggerCause::CreatureAttacking)
        );
        // Aboleth Spawn condition: the entering arm reads the subject's type
        // word, so core_types is [Creature], not empty.
        assert_eq!(
            classify_trigger_cause(
                "whenever a creature entering under an opponent's control causes a triggered ability of that creature to trigger"
            ),
            Some(TriggerCause::EntersBattlefield {
                core_types: vec![CoreType::Creature],
                entrant: CausingObject::Any,
            })
        );
        // Strict Proctor condition: "permanent" names no core type.
        assert_eq!(
            classify_trigger_cause(
                "whenever a permanent entering causes a triggered ability to trigger"
            ),
            Some(TriggerCause::EntersBattlefield {
                core_types: vec![],
                entrant: CausingObject::Any,
            })
        );
    }

    #[test]
    fn unlisted_causes_stay_classified_for_their_own_callers() {
        // The life-gain clause classifies (the OBSERVER accept-set declines
        // it later — this pins the classifier half of that decline).
        assert_eq!(
            classify_trigger_cause(
                "whenever you gaining life causes a triggered ability to trigger"
            ),
            Some(TriggerCause::PlayerAction {
                player: PlayerRelation::Controller,
                action: PlayerCauseAction::GainedLife,
            })
        );
        // The Ninja refinement does not affect cause classification.
        assert_eq!(
            classify_trigger_cause(
                "whenever a creature you control attacking causes a triggered ability of a ninja creature you control to trigger"
            ),
            Some(TriggerCause::CreatureAttacking)
        );
    }

    #[test]
    fn untyped_cause_clause_declines() {
        // "dealing combat damage to a planeswalker" matches no arm (the Felix
        // arm requires "to a player"), so the decline arm fires.
        assert_eq!(
            classify_trigger_cause(
                "if a creature you control dealing combat damage to a planeswalker causes a triggered ability to trigger, that ability triggers an additional time"
            ),
            None
        );
    }
}
