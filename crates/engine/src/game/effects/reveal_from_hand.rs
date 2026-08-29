//! CR 701.20a: Optional self-reveal from the controller's hand.
//!
//! Backs `Effect::RevealFromHand { filter, on_decline }`. Used by any "you may
//! reveal a [FILTER] card from your hand" pattern — notably the reveal-land
//! cycle (Port Town, Gilt-Leaf Palace, Temple of ...) where the alternative is
//! "[source] enters tapped." The primitive is not land-specific: the
//! `on_decline` ability is any composable chain, so symmetric "if you do,
//! [effect]" variants and future hand-reveal gated effects reuse it directly.

use crate::game::filter::{matches_target_filter, FilterContext};
use crate::types::ability::{
    AbilityDefinition, Effect, EffectError, EffectKind, ResolvedAbility, TargetRef,
};
#[cfg(test)]
use crate::types::ability::{EffectScope, TapStateChange};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingContinuation, WaitingFor};

use crate::game::ability_utils::build_resolved_from_def;

/// CR 701.20a: Resolve `Effect::RevealFromHand`.
///
/// Flow:
/// 1. Gather matching cards in the controller's hand.
/// 2. If none are eligible, immediately queue `on_decline` as the next
///    continuation and let the standard drain step execute it. The controller
///    made no choice — the source's "if you don't" branch fires automatically.
/// 3. Otherwise, mark the eligible set as revealed, emit `CardsRevealed`, and
///    set `WaitingFor::RevealChoice { optional: true, ... }`. The choice
///    handler in `engine_resolution_choices` routes either the chosen card or
///    an empty selection (decline) back into the continuation chain:
///      - Pick → normal sub-ability chain (typically empty for reveal-lands;
///        accepting = "do nothing more, don't tap").
///      - Decline → the stashed `on_decline` chain runs (e.g., `Tap SelfRef`).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (card_filter, on_decline) = match &ability.effect {
        Effect::RevealFromHand { filter, on_decline } => (filter.clone(), on_decline.clone()),
        _ => return Ok(()),
    };

    let controller = ability.controller;
    let source_id = ability.source_id;

    let hand: Vec<_> = state
        .players
        .iter()
        .find(|p| p.id == controller)
        .map(|p| p.hand.iter().copied().collect())
        .unwrap_or_default();

    let eligible: Vec<_> = if matches!(card_filter, crate::types::ability::TargetFilter::Any) {
        hand
    } else {
        let ctx = FilterContext::from_ability(ability);
        hand.into_iter()
            .filter(|&id| matches_target_filter(state, id, &card_filter, &ctx))
            .collect()
    };

    // CR 701.20a: No eligible card to reveal → the "if you don't" branch fires.
    // This is equivalent to the player declining the optional reveal.
    if eligible.is_empty() {
        run_on_decline_now(state, on_decline.as_deref(), controller, source_id, events);
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::Reveal,
            source_id,
            subject: None,
        });
        return Ok(());
    }

    // Stash the decline branch as a pending continuation BEFORE setting the
    // WaitingFor, so an empty `SelectCards` payload drains straight into it.
    // A successful pick overwrites or supersedes this continuation via the
    // normal RevealChoice handler path.
    //
    // CR 107.3 + CR 611.2: Seed the continuation's targets with the source so
    // `Effect::SetTapState { target: SelfRef, scope: Single, state: Tap }` (the
    // reveal-land decline branch)
    // resolves correctly when drained. Mirrors `apply_post_replacement_effect`'s
    // convention of threading the source object as the default target.
    if let Some(def) = on_decline {
        let mut resolved = build_resolved_from_def(&def, source_id, controller);
        if resolved.targets.is_empty() {
            resolved.targets.push(TargetRef::Object(source_id));
        }
        state.park_ability_continuation(PendingContinuation::new(Box::new(resolved), state));
    } else {
        let _ = state
            .take_active_ability_continuation()
            .expect("reveal continuation must be the active frame");
    }

    state.waiting_for = WaitingFor::RevealChoice {
        player: controller,
        cards: eligible,
        filter: card_filter,
        optional: true,
        decline_runs_continuation: true,
    };

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Reveal,
        source_id,
        subject: None,
    });

    Ok(())
}

/// Run the decline ability inline when no eligible hand card exists. Bypasses
/// the WaitingFor path because no player choice is needed — CR 701.20a treats
/// the "can't reveal" case identically to "chose not to reveal."
fn run_on_decline_now(
    state: &mut GameState,
    on_decline: Option<&AbilityDefinition>,
    controller: crate::types::player::PlayerId,
    source_id: crate::types::identifiers::ObjectId,
    events: &mut Vec<GameEvent>,
) {
    let Some(def) = on_decline else {
        return;
    };
    // CR 107.3 + CR 611.2: Seed the resolved ability with the source as its
    // object target so SelfRef-targeted effects (Tap SelfRef for reveal-lands)
    // tap the source land. Post-replacement effects from other paths go through
    // `apply_post_replacement_effect`, which threads the ETB'd object in the same
    // way — we mirror that convention here because this path runs immediately
    // (no player prompt) when the hand has no eligible card.
    //
    // CR 608.2c: Route through `resolve_ability_chain` (not `resolve_effect`) so
    // any condition on the on_decline ability is evaluated. Reveal-tribal lands
    // (Fortified Beachhead, Temple of the Dragon Queen) gate the on_decline Tap
    // on `AbilityCondition::ControllerControlsMatching { negated: true }` — the
    // Tap fires only when the controller doesn't already control a [filter]
    // permanent. Bypassing the chain would skip that check.
    let mut resolved = build_resolved_from_def(def, source_id, controller);
    if resolved.targets.is_empty() {
        resolved.targets.push(TargetRef::Object(source_id));
    }
    let _ = super::resolve_ability_chain(state, &resolved, events, 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, TargetFilter, TypeFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn elf_filter() -> TargetFilter {
        let mut typed = TypedFilter::card();
        typed
            .type_filters
            .push(TypeFilter::Subtype("Elf".to_string()));
        TargetFilter::Typed(typed)
    }

    fn tap_self_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
        )
    }

    fn reveal_ability(source_id: ObjectId, controller: PlayerId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::RevealFromHand {
                filter: elf_filter(),
                on_decline: Some(Box::new(tap_self_ability())),
            },
            Vec::new(),
            source_id,
            controller,
        )
    }

    #[test]
    fn empty_eligible_hand_runs_on_decline_and_taps_source() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Gilt-Leaf Palace".to_string(),
            Zone::Battlefield,
        );
        // Non-Elf card in hand: not eligible.
        create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Mountain".to_string(),
            Zone::Hand,
        );

        let ability = reveal_ability(source, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // No RevealChoice prompt; decline path tapped the source in-place.
        assert!(
            !matches!(state.waiting_for, WaitingFor::RevealChoice { .. }),
            "expected no prompt for empty eligible set, got {:?}",
            state.waiting_for
        );
        assert!(
            state.objects.get(&source).unwrap().tapped,
            "on_decline Tap SelfRef should have tapped the source land"
        );
    }

    #[test]
    fn eligible_hand_sets_optional_reveal_choice() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Gilt-Leaf Palace".to_string(),
            Zone::Battlefield,
        );
        // Give the controller an Elf card.
        let elf = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Llanowar Elves".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&elf).unwrap();
            obj.card_types.core_types = vec![CoreType::Creature];
            obj.card_types.subtypes = vec!["Elf".to_string()];
        }

        let ability = reveal_ability(source, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::RevealChoice {
                player,
                cards,
                optional,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert!(*optional, "reveal-land prompt must be optional");
                assert_eq!(cards, &vec![elf]);
            }
            other => panic!("expected optional RevealChoice, got {:?}", other),
        }
        // The decline branch is stashed for the empty-pick path.
        assert!(
            state.active_ability_continuation().is_some(),
            "on_decline should be stashed as pending continuation"
        );
    }

    /// CR 608.2c + CR 614.1d: Tarkir reveal-tribal land — when controller has
    /// no Soldier card in hand AND no Soldier on battlefield, the on_decline's
    /// `ControllerControlsMatching{negated:true}` condition is met (no
    /// matching permanent) → Tap fires → land enters tapped.
    #[test]
    fn tarkir_reveal_land_taps_when_no_match_in_hand_or_battlefield() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Fortified Beachhead".to_string(),
            Zone::Battlefield,
        );
        // Non-Soldier card in hand: not eligible for the reveal.
        create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Mountain".to_string(),
            Zone::Hand,
        );

        let mut soldier_filter = TypedFilter::card();
        soldier_filter
            .type_filters
            .push(TypeFilter::Subtype("Soldier".to_string()));
        let reveal_filter = TargetFilter::Typed(soldier_filter.clone());
        let cond_filter = TargetFilter::Typed(
            soldier_filter.controller(crate::types::ability::ControllerRef::You),
        );
        let conditional_tap = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
        )
        .condition(crate::types::ability::AbilityCondition::Not {
            condition: Box::new(
                crate::types::ability::AbilityCondition::ControllerControlsMatching {
                    filter: cond_filter,
                },
            ),
        });

        let ability = ResolvedAbility::new(
            Effect::RevealFromHand {
                filter: reveal_filter,
                on_decline: Some(Box::new(conditional_tap)),
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Empty eligible hand → on_decline runs immediately. No Soldier on
        // battlefield → ControllerControlsMatching{negated:true} is satisfied → Tap.
        assert!(
            state.objects.get(&source).unwrap().tapped,
            "should tap when no Soldier card in hand and no Soldier on battlefield"
        );
    }

    /// CR 608.2c + CR 614.1d: Mirror of the above — controller has no Soldier
    /// in hand BUT controls a Soldier on the battlefield → on_decline's
    /// condition fails (controls_any = true, negated → false) → Tap suppressed
    /// → land enters untapped via the disjunction's second arm.
    #[test]
    fn tarkir_reveal_land_skips_tap_when_controls_matching() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Fortified Beachhead".to_string(),
            Zone::Battlefield,
        );
        // Non-Soldier card in hand: not eligible for the reveal.
        create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Mountain".to_string(),
            Zone::Hand,
        );
        // Soldier creature on battlefield → satisfies the OR's second arm.
        let soldier = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Squire".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&soldier).unwrap();
            obj.card_types.core_types = vec![CoreType::Creature];
            obj.card_types.subtypes = vec!["Soldier".to_string()];
        }

        let mut soldier_filter = TypedFilter::card();
        soldier_filter
            .type_filters
            .push(TypeFilter::Subtype("Soldier".to_string()));
        let reveal_filter = TargetFilter::Typed(soldier_filter.clone());
        let cond_filter = TargetFilter::Typed(
            soldier_filter.controller(crate::types::ability::ControllerRef::You),
        );
        let conditional_tap = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
        )
        .condition(crate::types::ability::AbilityCondition::Not {
            condition: Box::new(
                crate::types::ability::AbilityCondition::ControllerControlsMatching {
                    filter: cond_filter,
                },
            ),
        });

        let ability = ResolvedAbility::new(
            Effect::RevealFromHand {
                filter: reveal_filter,
                on_decline: Some(Box::new(conditional_tap)),
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Empty eligible hand → on_decline runs immediately. Controls Soldier
        // → ControllerControlsMatching{negated:true} fails → Tap skipped.
        assert!(
            !state.objects.get(&source).unwrap().tapped,
            "should NOT tap when controller already controls a Soldier"
        );
    }

    /// The eligible set is drawn from the CONTROLLER'S HAND, and only then filtered — the seam
    /// the block-(3) `execute` firewall relief
    /// (`analysis::resource::reveal_from_hand_execute_provably_excludes_class`) rests its
    /// universe argument on: a growing-class member that is a battlefield object (CR 110.1) can
    /// never be in the eligible set, WHATEVER the filter says, because the pool is a hand.
    /// Widening it to `state.objects` would make the relief unsound while every filter-level
    /// test stayed green, so the pool is pinned here, at its own seam.
    ///
    /// The three decoys discriminate: each MATCHES the filter (asserted below, so the row cannot
    /// pass because they were filtered out) and each is excluded by the POOL alone. The controller
    /// is `PlayerId(1)`, so "the controller's hand" is distinguishable from "player zero's hand".
    ///
    /// REVERT / MUTATION PROBE: widen `resolve`'s universe to `state.objects` in any zone ⇒
    /// **this row FAILS** (the prompt offers all three matches instead of the one).
    #[test]
    fn reveal_from_hand_eligible_set_is_a_subset_of_the_controllers_hand() {
        let mut state = GameState::new_two_player(42);
        let controller = PlayerId(1);

        let source = create_object(
            &mut state,
            CardId(1),
            controller,
            "Gilt-Leaf Palace".to_string(),
            Zone::Battlefield,
        );

        let make_elf = |state: &mut GameState, card: u64, owner: PlayerId, zone: Zone| {
            let id = create_object(
                state,
                CardId(card),
                owner,
                "Llanowar Elves".to_string(),
                zone,
            );
            let obj = state.objects.get_mut(&id).expect("just created");
            obj.card_types.core_types = vec![CoreType::Creature];
            obj.card_types.subtypes = vec!["Elf".to_string()];
            id
        };

        // (i) a MATCHING battlefield permanent, (ii) a MATCHING card in the OTHER player's
        // hand, (iii) the one matching card in the controller's own hand.
        let battlefield_elf = make_elf(&mut state, 2, controller, Zone::Battlefield);
        let other_hand_elf = make_elf(&mut state, 3, PlayerId(0), Zone::Hand);
        let controller_hand_elf = make_elf(&mut state, 4, controller, Zone::Hand);

        let ability = reveal_ability(source, controller);

        // ⟨G⟩ reach-guard: every decoy MATCHES the filter, so each one's absence from the
        // eligible set below is attributable to the POOL and to nothing else. Without this
        // the row passes for a filter that happened to reject them.
        let ctx = FilterContext::from_ability(&ability);
        for (label, id) in [
            ("battlefield permanent", battlefield_elf),
            ("other player's hand", other_hand_elf),
            ("controller's hand", controller_hand_elf),
        ] {
            assert!(
                matches_target_filter(&state, id, &elf_filter(), &ctx),
                "⟨G⟩ reach-guard: the {label} decoy must MATCH the reveal filter, else its \
                 exclusion below proves nothing about the subject pool"
            );
        }
        // ⟨G⟩ reach-guard: the two hands are genuinely distinct, so "controller's hand" is
        // not accidentally satisfied by "player zero's hand".
        assert_ne!(controller, PlayerId(0));

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::RevealChoice { player, cards, .. } => {
                assert_eq!(*player, controller);
                assert_eq!(
                    cards,
                    &vec![controller_hand_elf],
                    "S6-P2a: the eligible set is EXACTLY the controller's hand, filtered. \
                     A matching battlefield permanent and a matching card in another \
                     player's hand are both out of the pool. Widening the universe to \
                     `state.objects` makes this FAIL"
                );
            }
            other => panic!("expected optional RevealChoice, got {:?}", other),
        }
    }
}
