//! Shared game-state predicate helpers used by multiple condition evaluators.
//!
//! Each function here eliminates duplication across two or more of:
//! `layers.rs` (StaticCondition), `triggers.rs` (TriggerCondition),
//! `effects/mod.rs` (AbilityCondition), and `replacement.rs` (ReplacementCondition).

use crate::game::combat::AttackTarget;
use crate::game::game_object::{AttachTarget, GameObject};
use crate::types::card_type::CoreType;
use crate::types::counter::CounterMatch;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::triggers::AttackTargetFilter;
use crate::types::zones::Zone;

/// CR 122.1: True when `obj` has at least `minimum` (and at most `maximum` if specified)
/// counters matching `counters`. `CounterMatch::Any` sums all counter types;
/// `CounterMatch::OfType(ct)` matches only that counter type.
/// Used for HasCounters evaluation in StaticCondition and TriggerCondition.
pub(crate) fn counter_condition_matches(
    obj: &GameObject,
    counters: &CounterMatch,
    minimum: u32,
    maximum: Option<u32>,
) -> bool {
    let count: u32 = match counters {
        CounterMatch::Any => obj.counters.values().sum(),
        CounterMatch::OfType(ct) => obj.counters.get(ct).copied().unwrap_or(0),
    };
    count >= minimum && maximum.is_none_or(|max| count <= max)
}

/// CR 110.5b + CR 110.5d: True when the source object is on the battlefield AND tapped.
/// Use in static-condition evaluation where CR 110.5d requires the zone guard
/// (cards not on the battlefield are neither tapped nor untapped).
pub(crate) fn eval_source_is_tapped_on_battlefield(state: &GameState, source_id: ObjectId) -> bool {
    state
        .objects
        .get(&source_id)
        .is_some_and(|obj| obj.zone == Zone::Battlefield && obj.tapped)
}

/// CR 110.5b: True when the source object is tapped, regardless of zone.
/// Use in trigger and ability evaluation where the source's zone is already
/// constrained by the functioning-abilities path.
pub(crate) fn eval_source_is_tapped(state: &GameState, source_id: ObjectId) -> bool {
    state.objects.get(&source_id).is_some_and(|obj| obj.tapped)
}

/// CR 614.12c + CR 607.2d: True when the source object's chosen label matches
/// the given anchor word, case-insensitively.
pub(crate) fn eval_chosen_label_is(state: &GameState, source_id: ObjectId, label: &str) -> bool {
    state
        .objects
        .get(&source_id)
        .and_then(|obj| obj.chosen_label())
        .is_some_and(|chosen| chosen.eq_ignore_ascii_case(label))
}

/// CR 716.2a: True when the source Class enchantment is at or above the given level.
/// Does NOT include a battlefield zone guard — callers that require the source to be
/// on the battlefield (e.g. `replacement.rs`) must apply the guard before calling.
pub(crate) fn eval_class_level_ge(state: &GameState, source_id: ObjectId, level: u8) -> bool {
    state
        .objects
        .get(&source_id)
        .and_then(|obj| obj.class_level)
        .is_some_and(|current| current >= level)
}

/// CR 113.6b: True when the source object is in the specified zone.
pub(crate) fn eval_source_in_zone(state: &GameState, source_id: ObjectId, zone: Zone) -> bool {
    state
        .objects
        .get(&source_id)
        .is_some_and(|obj| obj.zone == zone)
}

/// CR 508.1k: True when the source creature is currently an attacking creature.
pub(crate) fn eval_source_is_attacking(state: &GameState, source_id: ObjectId) -> bool {
    state
        .combat
        .as_ref()
        .is_some_and(|combat| combat.attackers.iter().any(|a| a.object_id == source_id))
}

/// CR 509.1b + CR 506.2 + CR 108.3: True when the recipient creature
/// (`attacker_id`, the per-object subject of the gating continuous effect) is
/// currently attacking a target permitted by `target`, evaluated relative to the
/// creature's OWNER (CR 108.3), not its controller. Realizes "attacking its
/// owner [or a permanent its owner controls]" for the conditional-evasion class
/// (e.g. Become the Pilot). Not attacking → false; no combat → false.
pub(crate) fn eval_recipient_attacking_owner_target(
    state: &GameState,
    attacker_id: ObjectId,
    target: &AttackTargetFilter,
) -> bool {
    let Some(attacker) = state.objects.get(&attacker_id) else {
        return false;
    };
    // CR 108.3: owner != controller — the donate/evasion class hinges on the
    // OWNER, who may not control the creature.
    let owner = attacker.owner;
    let Some(combat) = state.combat.as_ref() else {
        return false;
    };
    let Some(info) = combat.attackers.iter().find(|a| a.object_id == attacker_id) else {
        // Not an attacking creature → the exception cannot be met.
        return false;
    };

    // CR 506.2: a creature attacks a player, a planeswalker they control, or a
    // battle they protect. "Owner-controlled permanent" therefore resolves to a
    // planeswalker/battle whose controller is the owner.
    let attacks_owner_player =
        || matches!(info.attack_target, AttackTarget::Player(p) if p == owner);
    let attacks_owner_permanent = || match info.attack_target {
        AttackTarget::Planeswalker(pw) | AttackTarget::Battle(pw) => {
            state.objects.get(&pw).map(|o| o.controller) == Some(owner)
        }
        AttackTarget::Player(_) => false,
    };

    match target {
        // Bare "attacking its owner" = attacking the owning player directly.
        AttackTargetFilter::Owner => attacks_owner_player(),
        // "its owner or a permanent its owner controls".
        AttackTargetFilter::OwnerOrPlaneswalker => {
            attacks_owner_player() || attacks_owner_permanent()
        }
        // Not produced by the block-evasion parser path. Kept explicit (no
        // wildcard) so future widening is compiler-flagged.
        AttackTargetFilter::Player
        | AttackTargetFilter::Planeswalker
        | AttackTargetFilter::PlayerOrPlaneswalker
        | AttackTargetFilter::PlayerOrPermanents
        | AttackTargetFilter::Battle => false,
    }
}

/// CR 725.1: True when no player holds the monarch designation.
pub(crate) fn eval_no_monarch(state: &GameState) -> bool {
    state.monarch.is_none()
}

/// CR 725.1: True when the given player is the monarch.
pub(crate) fn eval_is_monarch(state: &GameState, controller: PlayerId) -> bool {
    state.monarch == Some(controller)
}

/// CR 726.3: True when the given player has the initiative.
pub(crate) fn eval_is_initiative(state: &GameState, controller: PlayerId) -> bool {
    state.initiative == Some(controller)
}

/// CR 702.131a + CR 702.131c: True when the given player has the city's blessing.
pub(crate) fn eval_has_city_blessing(state: &GameState, controller: PlayerId) -> bool {
    state.city_blessing.contains(&controller)
}

/// CR 400.7: True when the source permanent entered the battlefield this turn.
pub(crate) fn eval_source_entered_this_turn(state: &GameState, source_id: ObjectId) -> bool {
    state
        .objects
        .get(&source_id)
        .is_some_and(|obj| obj.entered_battlefield_turn == Some(state.turn_number))
}

/// CR 120.3 + CR 120.6 + CR 702.11b: True when the source permanent has actually
/// dealt damage (combat or noncombat) since it entered the battlefield. Backs the
/// `StaticCondition::SourceHasDealtDamage` predicate; the "hasn't dealt damage yet"
/// negation wraps this via `StaticCondition::Not`. The flag is set in
/// `deal_damage` on the first nonzero amount actually dealt and cleared on a
/// battlefield exit (`apply_zone_exit_cleanup`).
pub(crate) fn eval_source_has_dealt_damage(state: &GameState, source_id: ObjectId) -> bool {
    state.objects_that_dealt_damage.contains(&source_id)
}

/// CR 105.2 + CR 611.3a: True when the source permanent shares a color with the
/// most common color among all battlefield permanents — counting every color
/// tied for most common. Backs
/// `StaticCondition::SharesColorWithMostCommonColorAmongPermanents` (Heroic
/// Defiance, whose static gate wraps it in `Not` for the "unless" clause).
pub(crate) fn eval_shares_color_with_most_common_color(
    state: &GameState,
    source_id: ObjectId,
) -> bool {
    use crate::types::mana::ManaColor;
    use std::collections::HashMap;

    // CR 105.2a: only the five colors count; the histogram is over every colored
    // battlefield permanent (the source itself included).
    let mut counts: HashMap<ManaColor, usize> = HashMap::new();
    for &id in crate::game::targeting::zone_object_ids(state, Zone::Battlefield).iter() {
        if let Some(obj) = state.objects.get(&id) {
            for color in &obj.color {
                *counts.entry(*color).or_insert(0) += 1;
            }
        }
    }
    let Some(&max) = counts.values().max() else {
        return false; // no colored permanent — there is no "most common color"
    };
    // A source color is most-common (or tied) iff its board-wide count equals the
    // maximum; sharing any such color satisfies the predicate.
    state
        .objects
        .get(&source_id)
        .is_some_and(|source| source.color.iter().any(|c| counts.get(c) == Some(&max)))
}

/// CR 301.5 + CR 303.4: True when the source object is attached to a creature
/// controlled by `controller`. Returns false when the source has no host, when
/// the host is a player (Curse-style Aura), or when the host is not a creature
/// at the time of evaluation. Used by ability-resolution gates such as
/// Springheart Nantuko's optional landfall payment, which only resolves when
/// the bestowed Aura is currently attached to a creature its controller owns
/// (so the fallback Insect token branch can still run when the Aura is bare).
pub(crate) fn eval_source_attached_to_controlled_creature(
    state: &GameState,
    source_id: ObjectId,
    controller: PlayerId,
) -> bool {
    let Some(source) = state.objects.get(&source_id) else {
        return false;
    };
    let Some(AttachTarget::Object(host_id)) = source.attached_to else {
        return false;
    };
    state.objects.get(&host_id).is_some_and(|host| {
        host.controller == controller && host.card_types.core_types.contains(&CoreType::Creature)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::game_state::GameState;
    use crate::types::player::PlayerId;
    use crate::types::CardId;

    #[test]
    fn eval_is_initiative_matches_designation_holder() {
        let state = GameState {
            initiative: Some(PlayerId(0)),
            ..Default::default()
        };
        assert!(eval_is_initiative(&state, PlayerId(0)));
        assert!(!eval_is_initiative(&state, PlayerId(1)));
    }

    /// CR 110.5d: eval_source_is_tapped_on_battlefield must return false when the
    /// object is tapped but not on the battlefield (e.g. in the graveyard). The
    /// zone-guard-free eval_source_is_tapped must return true for the same object.
    #[test]
    fn tapped_zone_guard_distinguishes_battlefield_vs_graveyard() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test".to_string(),
            Zone::Graveyard,
        );
        state.objects.get_mut(&id).unwrap().tapped = true;

        // Zone guard: tapped but NOT on battlefield → false
        assert!(!eval_source_is_tapped_on_battlefield(&state, id));
        // No zone guard: tapped regardless of zone → true
        assert!(eval_source_is_tapped(&state, id));
    }

    #[test]
    fn tapped_on_battlefield_returns_true() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Test".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().tapped = true;

        assert!(eval_source_is_tapped_on_battlefield(&state, id));
        assert!(eval_source_is_tapped(&state, id));
    }

    /// CR 105.2 + CR 611.3a (Heroic Defiance): the most-common-color predicate is
    /// true only when a source color's board-wide count equals the maximum, and a
    /// color tied for most common still counts.
    #[test]
    fn shares_most_common_color_handles_majority_and_ties() {
        use crate::types::mana::ManaColor;

        let mut state = GameState::new_two_player(42);
        let mk = |state: &mut GameState, cid: u64, colors: Vec<ManaColor>| {
            let id = create_object(
                &mut *state,
                CardId(cid),
                PlayerId(0),
                "P".to_string(),
                Zone::Battlefield,
            );
            state.objects.get_mut(&id).unwrap().color = colors;
            id
        };

        let white = mk(&mut state, 1, vec![ManaColor::White]);
        mk(&mut state, 2, vec![ManaColor::White]);
        let red = mk(&mut state, 3, vec![ManaColor::Red]);

        // White is strictly most common (2 vs 1): a white source shares it; a
        // red-only source does not.
        assert!(eval_shares_color_with_most_common_color(&state, white));
        assert!(!eval_shares_color_with_most_common_color(&state, red));

        // Add a second red → White 2 / Red 2. CR 611.3a counts every tied color,
        // so the red source now shares a most-common color.
        mk(&mut state, 4, vec![ManaColor::Red]);
        assert!(eval_shares_color_with_most_common_color(&state, red));

        // A colorless source never shares a color, even at a tie.
        let colorless = mk(&mut state, 5, vec![]);
        assert!(!eval_shares_color_with_most_common_color(&state, colorless));
    }
}
