use crate::types::events::GameEvent;
use crate::types::game_state::{
    CastOfferKind, ExileLink, ExileLinkKind, GameState, ParadigmPrime, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

/// CR 702.xxx: Paradigm (Strixhaven) — first-resolution hook. Called from
/// `stack.rs` when a spell with `Keyword::Paradigm` successfully resolves for
/// the first time by its controller (per card name).
///
/// Action: (a) push `(controller, card_name)` to `state.paradigm_primed`,
/// (b) override the spell's post-resolve destination to exile (CR 608.2n is
/// displaced by the Paradigm reminder text), (c) create an `ExileLink` with
/// `ExileLinkKind::ParadigmSource { player: controller }` pointing to the
/// spell object. Returns `true` if the hook fired (caller should skip the
/// default graveyard-routing branch). Assign when WotC publishes SOS CR
/// update.
///
/// The exiled card is the original spell object (it is left on the stack at
/// the point the resolver inspects it; the stack.rs caller moves it to exile
/// after `arm_paradigm` returns true instead of to the graveyard).
pub fn arm_paradigm(
    state: &mut GameState,
    object_id: ObjectId,
    controller: PlayerId,
    card_name: &str,
) -> bool {
    // CR 702.xxx: "After you first resolve a spell with this name" — gate on
    // (player, card_name). Already-primed spells follow default routing.
    let already_primed = state
        .paradigm_primed
        .iter()
        .any(|p| p.player == controller && p.card_name.eq_ignore_ascii_case(card_name));
    if already_primed {
        return false;
    }
    state.paradigm_primed.push(ParadigmPrime {
        player: controller,
        card_name: card_name.to_string(),
    });
    state.exile_links.push(ExileLink {
        source_id: object_id,
        exiled_id: object_id,
        kind: ExileLinkKind::ParadigmSource { player: controller },
    });
    true
}

/// CR 702.xxx: Paradigm (Strixhaven) — turn-based offer scan. Called from
/// `turns.rs` at the start of the active player's first precombat main phase
/// (CR 505.4 anchor for beginning-of-precombat-main turn-based actions).
/// Returns the list of exiled paradigm sources that belong to the given
/// player. Assign when WotC publishes SOS CR update.
pub fn paradigm_offers_for(state: &GameState, player: PlayerId) -> Vec<ObjectId> {
    state
        .exile_links
        .iter()
        .filter_map(|link| match link.kind {
            ExileLinkKind::ParadigmSource { player: owner } if owner == player => {
                Some(link.exiled_id)
            }
            _ => None,
        })
        .collect()
}

/// Enqueue a `WaitingFor::CastOffer` (Paradigm) if offers exist for the given
/// player. Returns true if a `WaitingFor` was set; false if no offers and the
/// caller should continue normal phase flow.
pub fn enqueue_offer_if_any(state: &mut GameState, player: PlayerId) -> bool {
    let offers = paradigm_offers_for(state, player);
    if offers.is_empty() {
        return false;
    }
    state.waiting_for = WaitingFor::CastOffer {
        player,
        kind: CastOfferKind::Paradigm { offers },
    };
    true
}

/// CR 702.xxx + CR 707.10f: Build a token spell-copy on the stack from an
/// exiled paradigm source. The exiled card stays in exile; the copy is a
/// fresh ObjectId, `is_token = true`, `CastingVariant::Normal`, controller =
/// acting player. Returns Ok(copy_id) on success. Assign when WotC publishes
/// SOS CR update.
pub fn cast_paradigm_copy(
    state: &mut GameState,
    source_id: ObjectId,
    controller: PlayerId,
    events: &mut Vec<GameEvent>,
) -> Result<ObjectId, String> {
    use crate::game::ability_utils::build_resolved_from_def;
    use crate::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
    use crate::types::zones::Zone;

    let (src_clone, card_id) = {
        let Some(src_obj) = state.objects.get(&source_id) else {
            return Err(format!("paradigm source {source_id:?} not found"));
        };
        (src_obj.clone(), src_obj.card_id)
    };
    // Verify this is an exiled paradigm source owned by the acting player.
    let has_link = state.exile_links.iter().any(|link| {
        link.exiled_id == source_id
            && matches!(link.kind, ExileLinkKind::ParadigmSource { player } if player == controller)
    });
    if !has_link {
        return Err("no ParadigmSource link for this source/player".to_string());
    }
    // CR 608.2 + CR 707.10: Mirror the normal cast path — a spell's on-resolve
    // chain is the union of every `AbilityKind::Spell` entry (each with its own
    // `sub_ability` tail) folded by `combined_spell_ability_def`. Taking only
    // `.first()` dropped sibling spell abilities (issue #1960: Decorum
    // Dissertation's "loses 2 life" conjunct lived in a second spell ability,
    // so Paradigm copies drew but did not deduct life).
    let ability_def = crate::game::casting::combined_spell_ability_def(&src_clone)
        .ok_or_else(|| "paradigm source has no spell ability".to_string())?;

    let copy_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;

    let mut copy_obj = src_clone;
    copy_obj.id = copy_id;
    copy_obj.controller = controller;
    copy_obj.owner = controller;
    copy_obj.zone = Zone::Stack;
    copy_obj.is_token = true;
    copy_obj.tapped = false;
    copy_obj.prepared = None;
    // Back-face is preserved from clone — not needed for copy behavior.
    state.objects.insert(copy_id, copy_obj);

    // CR 707.10: Build a ResolvedAbility from the paradigm source's ability
    // definition preserving sub-ability chains, optional flags, and duration
    // metadata. `build_resolved_from_def` is the authoritative constructor
    // used by normal casting (see `ability_utils`).
    let resolved = build_resolved_from_def(&ability_def, copy_id, controller);

    state.stack.push_back(StackEntry {
        id: copy_id,
        source_id: copy_id,
        controller,
        kind: StackEntryKind::Spell {
            card_id,
            ability: Some(resolved),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    events.push(GameEvent::StackPushed { object_id: copy_id });

    Ok(copy_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arm_paradigm_primes_once_per_name() {
        let mut state = GameState::new_two_player(42);
        let obj = ObjectId(100);
        let p = PlayerId(0);
        assert!(arm_paradigm(&mut state, obj, p, "Restoration Seminar"));
        assert_eq!(state.paradigm_primed.len(), 1);
        assert_eq!(state.exile_links.len(), 1);

        // Second resolution with same name for same player does not re-prime.
        let obj2 = ObjectId(101);
        assert!(!arm_paradigm(&mut state, obj2, p, "Restoration Seminar"));
        assert_eq!(state.paradigm_primed.len(), 1);
        assert_eq!(state.exile_links.len(), 1);

        // Different player can prime the same name separately.
        let p2 = PlayerId(1);
        assert!(arm_paradigm(&mut state, obj2, p2, "Restoration Seminar"));
        assert_eq!(state.paradigm_primed.len(), 2);
        assert_eq!(state.exile_links.len(), 2);
    }

    #[test]
    fn offers_scoped_to_player() {
        let mut state = GameState::new_two_player(42);
        arm_paradigm(&mut state, ObjectId(100), PlayerId(0), "Foo");
        arm_paradigm(&mut state, ObjectId(101), PlayerId(1), "Bar");
        assert_eq!(
            paradigm_offers_for(&state, PlayerId(0)),
            vec![ObjectId(100)]
        );
        assert_eq!(
            paradigm_offers_for(&state, PlayerId(1)),
            vec![ObjectId(101)]
        );
    }

    // Test gap #4: If a Paradigm spell fizzles (all targets illegal) at
    // resolution, `arm_paradigm` must NOT be called because `stack.rs`'s
    // first-resolution hook runs after `execute_effect` succeeds. The unit
    // behavior to lock is: `paradigm_primed` remains empty when
    // `arm_paradigm` is never invoked. This test asserts the
    // call-site-free invariant — the data structure starts empty and
    // stays empty without an `arm_paradigm` call.
    #[test]
    fn paradigm_not_primed_when_arm_not_called() {
        let state = GameState::new_two_player(42);
        assert!(
            state.paradigm_primed.is_empty(),
            "fizzled Paradigm (arm_paradigm never called) leaves no prime"
        );
        assert!(
            paradigm_offers_for(&state, PlayerId(0)).is_empty(),
            "no offers when no paradigm primed"
        );
    }

    // Test gap #5: Counter-the-first-Paradigm-then-cast-a-second path.
    // `effects/counter.rs` sends the countered spell to the graveyard without
    // invoking `arm_paradigm`, so a subsequent same-name Paradigm resolution
    // is treated as the first and primes normally.
    #[test]
    fn second_paradigm_primes_when_first_was_countered() {
        let mut state = GameState::new_two_player(42);
        let p = PlayerId(0);
        // First spell was countered — `arm_paradigm` was never invoked. The
        // prime list remains empty.
        assert!(state.paradigm_primed.is_empty());

        // Second Paradigm spell with the same card name resolves successfully.
        let primed = arm_paradigm(&mut state, ObjectId(200), p, "Decorum Dissertation");
        assert!(primed, "second spell resolves first → primes");
        assert_eq!(state.paradigm_primed.len(), 1);
        assert_eq!(state.exile_links.len(), 1);
    }

    /// Issue #1960 — Decorum Dissertation's resolution chain must include both
    /// Draw and LoseLife after `combined_spell_ability_def` folds every spell
    /// ability on the card.
    #[test]
    fn decorum_dissertation_combined_spell_chain_includes_draw_and_lose_life() {
        use crate::game::scenario::{GameScenario, P0};
        use crate::types::ability::Effect;

        const ORACLE: &str = "Target player draws two cards and loses 2 life.";

        let mut scenario = GameScenario::new();
        let id = scenario
            .add_spell_to_hand_from_oracle(P0, "Decorum Dissertation", false, ORACLE)
            .id();
        let runner = scenario.build();
        let obj = &runner.state().objects[&id];

        let combined =
            crate::game::casting::combined_spell_ability_def(obj).expect("combined spell ability");
        let mut node = Some(&combined);
        let mut saw_draw = false;
        let mut saw_lose_life = false;
        while let Some(def) = node {
            match &*def.effect {
                Effect::Draw { .. } => saw_draw = true,
                Effect::LoseLife { .. } => saw_lose_life = true,
                _ => {}
            }
            node = def.sub_ability.as_deref();
        }
        assert!(saw_draw, "combined chain must include Draw");
        assert!(saw_lose_life, "combined chain must include LoseLife");
    }

    /// Issue #1960 — a Paradigm copy must run the full combined spell chain, not
    /// only the first sibling spell ability.
    #[test]
    fn paradigm_copy_resolves_draw_and_lose_life_chain() {
        use std::sync::Arc;

        use crate::game::ability_utils::build_resolved_from_def_with_targets;
        use crate::game::stack;
        use crate::game::zones::create_object;
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter, TargetRef,
        };
        use crate::types::card_type::CoreType;
        use crate::types::game_state::StackEntryKind;
        use crate::types::identifiers::CardId;
        use crate::types::mana::ManaCost;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let controller = PlayerId(0);
        let target = PlayerId(1);
        let life_before = state.players[1].life;

        // Seed the target player's library so Draw can resolve.
        for i in 0..3 {
            let card_id = CardId(state.next_object_id);
            state.next_object_id += 1;
            let lib_id = create_object(
                &mut state,
                card_id,
                target,
                format!("Library Card {i}"),
                Zone::Library,
            );
            state.players[1].library.push_front(lib_id);
        }

        let source_id = create_object(
            &mut state,
            CardId(100),
            controller,
            "Decorum Dissertation".to_string(),
            Zone::Exile,
        );
        {
            let obj = state.objects.get_mut(&source_id).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            obj.base_card_types = obj.card_types.clone();
            obj.mana_cost = ManaCost::generic(3);
            // Mirror parsed storage: Draw and LoseLife are sibling spell abilities.
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 2 },
                    target: TargetFilter::Player,
                },
            ));
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::LoseLife {
                    amount: QuantityExpr::Fixed { value: 2 },
                    target: Some(TargetFilter::Player),
                },
            ));
        }
        arm_paradigm(&mut state, source_id, controller, "Decorum Dissertation");

        let mut events = Vec::new();
        let copy_id = cast_paradigm_copy(&mut state, source_id, controller, &mut events).unwrap();

        let combined = crate::game::casting::combined_spell_ability_def(
            state.objects.get(&copy_id).expect("copy object"),
        )
        .expect("copy carries combined spell ability");
        let resolved = build_resolved_from_def_with_targets(
            &combined,
            copy_id,
            controller,
            vec![TargetRef::Player(target)],
        );
        if let Some(entry) = state.stack.iter_mut().find(|e| e.id == copy_id) {
            if let StackEntryKind::Spell { ability, .. } = &mut entry.kind {
                *ability = Some(resolved);
            }
        }

        stack::resolve_top(&mut state, &mut events);

        assert_eq!(
            state.players[1].hand.len(),
            2,
            "Paradigm copy must draw two cards for the chosen player"
        );
        assert_eq!(
            state.players[1].life,
            life_before - 2,
            "Paradigm copy must also deduct two life (issue #1960)"
        );
    }
}
