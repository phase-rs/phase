//! CR 702.50: Epic. When an Epic spell resolves, two linked effects begin and
//! last for the rest of the game:
//!
//! * CR 702.50b — its controller can't cast spells (but may still activate
//!   abilities, and effects may still put spell copies onto the stack);
//! * CR 702.50a — at the beginning of each of that player's upkeeps, they copy
//!   the spell except for its epic ability, optionally choosing new targets.
//!
//! This module mirrors Rebound (`game/effects/rebound.rs`): an on-resolution
//! arming hook records the effect and an upkeep-keyed trigger replays it. Two
//! differences make it Epic:
//!
//! 1. **Persistence.** Epic lasts "for the rest of the game", so the record is
//!    stored in the persistent `GameState::epic_effects` collection (never
//!    purged at cleanup, like `city_blessing`) rather than as a stored
//!    `DelayedTrigger` — those are removed at end-of-turn cleanup (`turns.rs`
//!    keeps only `one_shot && !WhenNextEvent`), which would delete a recurring
//!    rest-of-game trigger before its first upkeep ever arrived.
//! 2. **Synthesized firing.** At the beginning of each of the controller's
//!    upkeeps, [`epic_upkeep_trigger`] synthesizes an [`Effect::EpicCopy`]
//!    triggered ability from the stored snapshot, fired through the normal
//!    delayed-trigger path (`triggers::check_delayed_triggers`). Resolving that
//!    body puts a copy of the spell onto the stack (CR 707.10).
//!
//! The copy keeps the snapshot's declared targets: CR 702.50a's "you may choose
//! new targets" is an optional permission whose default — keeping the original
//! (still-legal) targets — is itself a legal choice.

use crate::game::effects::copy_spell::{open_copy_retarget_choice, set_resolved_source_recursive};
use crate::types::ability::{
    DelayedTriggerCondition, Effect, EffectError, EffectKind, ResolvedAbility,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    CastingVariant, DelayedTrigger, EpicEffect, GameState, StackEntry, StackEntryKind,
};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// CR 702.50b: Whether `player` controls a resolved Epic effect and is therefore
/// locked out of casting spells for the rest of the game. Derived from
/// `epic_effects` so the lock and the upkeep copies share one source of truth.
pub(crate) fn is_epic_locked(state: &GameState, player: PlayerId) -> bool {
    state.epic_effects.iter().any(|e| e.controller == player)
}

/// CR 702.50a-b: On-resolution arming hook for a spell carrying `Keyword::Epic`.
/// Called from `stack.rs::resolve_top` once it has confirmed the resolving
/// object is a non-token spell with `Keyword::Epic`. Records a rest-of-game
/// [`EpicEffect`]: its presence locks `controller` out of casting (CR 702.50b)
/// and drives the recurring upkeep copies (CR 702.50a). `source_id` is the
/// resolved Epic card whose characteristics each copy clones; `spell_ability`
/// is the snapshot each copy resolves.
pub(crate) fn arm_epic(
    state: &mut GameState,
    source_id: ObjectId,
    controller: PlayerId,
    spell_ability: ResolvedAbility,
) {
    state.epic_effects.push(EpicEffect {
        controller,
        prototype_id: source_id,
        spell: Box::new(spell_ability),
    });
}

/// CR 702.50a: Build the recurring upkeep trigger for `effect`, fired through
/// the normal delayed-trigger path. Keyed on the controller's upkeep with an
/// [`Effect::EpicCopy`] body carrying a fresh clone of the stored snapshot.
/// The returned trigger is synthesized on demand each upkeep — it is never
/// stored in `delayed_triggers`, so cleanup can't purge it.
pub(crate) fn epic_upkeep_trigger(effect: &EpicEffect) -> DelayedTrigger {
    DelayedTrigger {
        condition: DelayedTriggerCondition::AtNextPhaseForPlayer {
            phase: Phase::Upkeep,
            player: effect.controller,
        },
        ability: ResolvedAbility::new(
            Effect::EpicCopy {
                spell: effect.spell.clone(),
            },
            Vec::new(),
            effect.prototype_id,
            effect.controller,
        ),
        controller: effect.controller,
        source_id: effect.prototype_id,
        // Synthesized fresh each upkeep; the one-shot flag is irrelevant because
        // it is never stored — `epic_effects` is the persistent generator.
        one_shot: true,
    }
}

/// CR 702.50a + CR 707.10: Resolve [`Effect::EpicCopy`] — put a copy of the
/// snapshotted Epic spell onto the stack under its controller, excluding the
/// epic ability so the copy does not register a fresh Epic effect.
pub(crate) fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::EpicCopy { spell } = &ability.effect else {
        return Err(EffectError::MissingParam("EpicCopy".to_string()));
    };

    // The resolved Epic card supplies the copy's characteristics. If it has
    // left the game (no last-known object), the copy can't be built — resolve
    // as a no-op rather than fabricating a copy.
    let prototype_id = ability.source_id;
    let Some(source_obj) = state.objects.get(&prototype_id) else {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    };

    let controller = ability.controller;
    let copy_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;

    // CR 707.10 + CR 702.50a: clone the Epic card's characteristics, but strip
    // Epic ("except for its epic ability") so the copy's own resolution does
    // not arm a second Epic effect. The copy is a token spell on the stack.
    let mut copy_obj = source_obj.clone();
    copy_obj.id = copy_id;
    copy_obj.controller = controller;
    copy_obj.zone = Zone::Stack;
    copy_obj.is_token = true;
    copy_obj.additional_cost_payment_count = 0;
    copy_obj.kickers_paid.clear();
    copy_obj.keywords.retain(|k| !matches!(k, Keyword::Epic));
    let card_id = copy_obj.card_id;
    state.objects.insert(copy_id, copy_obj);

    // CR 707.10: the copy resolves the snapshotted ability, re-sourced to the
    // copy so every SelfRef resolves to the copy rather than the original.
    let mut copy_ability = (**spell).clone();
    set_resolved_source_recursive(&mut copy_ability, copy_id);

    state.stack.push_back(StackEntry {
        id: copy_id,
        source_id: copy_id,
        controller,
        kind: StackEntryKind::Spell {
            card_id,
            ability: Some(copy_ability.clone()),
            casting_variant: CastingVariant::default(),
            actual_mana_spent: 0,
        },
    });
    events.push(GameEvent::StackPushed { object_id: copy_id });

    // CR 707.10: a copy is put on the stack but not cast — `SpellCopied` (not
    // `SpellCast`) so copy-sensitive triggers fire without cast-only triggers.
    events.push(GameEvent::SpellCopied {
        card_id,
        controller,
        object_id: copy_id,
        original_id: prototype_id,
    });

    // CR 702.50a + CR 707.10c: Epic grants "you may choose new targets for the
    // copy." Reuse the same CopyRetarget choice path as other spell-copy
    // effects so the player may keep the old targets or choose legal new ones.
    let copy_targets = copy_ability.targets.clone();
    if !copy_targets.is_empty() {
        open_copy_retarget_choice(
            state,
            controller,
            copy_id,
            &copy_targets,
            &copy_ability,
            EffectKind::EpicCopy,
            ability.source_id,
        );
        // EffectResolved is deferred until the retarget choice completes.
        return Ok(());
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });
    Ok(())
}
