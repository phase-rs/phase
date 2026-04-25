//! CR 303.4 + CR 702.5d player-aura foundation tests.
//!
//! These tests exercise the building-block primitives Item 4 introduces:
//! `AttachTarget::Player`, the player branch of the Aura SBA (CR 303.4c via
//! CR 704.5m), the parser keyword arm for `Enchant player` / `Enchant
//! opponent`, and the cast-resolution path that routes a `TargetRef::Player`
//! into `attach_to_player`.
//!
//! Tests use direct game-state synthesis rather than full Oracle-text-driven
//! card construction so they can pin down exact CR-grounded semantics
//! without depending on parser arms that may not yet be wired (e.g. the
//! "enchanted player" self-ref in static/trigger sub-effects, which is
//! deliberately deferred).

use engine::game::effects::attach::{attach_to, attach_to_player};
use engine::game::game_object::AttachTarget;
use engine::game::sba::check_state_based_actions;
use engine::game::zones::create_object;
use engine::types::ability::{ControllerRef, TargetFilter, TypedFilter};
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::CardId;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

fn setup() -> GameState {
    GameState::new_two_player(42)
}

fn make_aura(
    state: &mut GameState,
    name: &str,
    controller: PlayerId,
) -> engine::types::identifiers::ObjectId {
    let id = create_object(
        state,
        CardId(state.next_object_id),
        controller,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Enchantment);
    obj.card_types.subtypes.push("Aura".to_string());
    id
}

fn make_creature(
    state: &mut GameState,
    name: &str,
    controller: PlayerId,
) -> engine::types::identifiers::ObjectId {
    let id = create_object(
        state,
        CardId(state.next_object_id),
        controller,
        name.to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&id)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);
    id
}

/// CR 303.4 + CR 702.5d: `attach_to_player` sets `attached_to` to the
/// `AttachTarget::Player` variant. This is the ground-floor invariant the
/// rest of the player-aura system rests on.
#[test]
fn attach_to_player_sets_player_variant_cr_303_4() {
    let mut state = setup();
    let aura = make_aura(&mut state, "Curse of Opulence", P0);

    attach_to_player(&mut state, aura, P1);

    assert_eq!(
        state.objects.get(&aura).unwrap().attached_to,
        Some(AttachTarget::Player(P1)),
        "CR 303.4: Aura attached to a player must hold the Player variant",
    );
}

/// CR 303.4 + CR 301.5d / CR 303.4e: An Aura's controller is independent of
/// the enchanted player. Curse-class Auras are typically controlled by the
/// caster while attached to an opponent — the variants must record both
/// roles distinctly. Probes the Curse-controller-divergence regression risk
/// flagged in the plan.
#[test]
fn curse_controller_diverges_from_enchanted_player_cr_303_4e() {
    let mut state = setup();
    // Aura is controlled by P0 ("you cast this Curse").
    let curse = make_aura(&mut state, "Curse of Opulence", P0);
    // Enchanted player is the opponent.
    attach_to_player(&mut state, curse, P1);

    assert_eq!(
        state.objects.get(&curse).unwrap().controller,
        P0,
        "CR 303.4e: Aura's controller is the caster (P0), not the enchanted player",
    );
    assert_eq!(
        state.objects.get(&curse).unwrap().attached_to,
        Some(AttachTarget::Player(P1)),
        "CR 303.4: enchanted player is the attach target (P1), not the controller",
    );
}

/// CR 704.5m + CR 303.4c: When the enchanted player has left the game (i.e.
/// is eliminated), the Aura is put into its owner's graveyard by SBA. This
/// is the player-axis analogue of "enchanted creature dies" cleanup.
#[test]
fn aura_falls_off_when_enchanted_player_loses_game_cr_704_5m() {
    let mut state = setup();
    let curse = make_aura(&mut state, "Curse of Opulence", P0);
    attach_to_player(&mut state, curse, P1);

    // Eliminate the enchanted player (mirrors the post-condition of CR 704.5a
    // / CR 704.5b — once flagged, SBA zone-cleanup proceeds).
    state.players[1].is_eliminated = true;

    let mut events = Vec::new();
    check_state_based_actions(&mut state, &mut events);

    assert!(
        !state.battlefield.contains(&curse),
        "CR 704.5m: Aura on an eliminated (left-game) player must leave the battlefield",
    );
    assert!(
        state.players[0].graveyard.contains(&curse),
        "CR 303.4c: Aura goes to its OWNER's graveyard (P0), not the enchanted player's",
    );
}

/// CR 704.5m + CR 303.4c: Multiplayer-exit case — symmetric to game-loss.
/// `is_eliminated` is the same flag that flips when a player concedes or
/// otherwise leaves a multiplayer game, so this tests the same SBA path
/// with the multiplayer-exit framing called out in the plan.
#[test]
fn aura_falls_off_when_enchanted_player_leaves_multiplayer_cr_704_5m() {
    let mut state = setup();
    let curse = make_aura(&mut state, "Cruel Reality", P0);
    attach_to_player(&mut state, curse, P1);

    // Player concedes / leaves the multiplayer game.
    state.players[1].is_eliminated = true;

    let mut events = Vec::new();
    check_state_based_actions(&mut state, &mut events);

    assert!(
        !state.battlefield.contains(&curse),
        "CR 303.4c + CR 704.5m: A player leaving the game makes the Aura's host illegal",
    );
}

/// CR 303.4 + CR 303.4i: An Aura that has been exiled and returned to the
/// battlefield with a fresh `Enchant` resolution gets a fresh attach target.
/// The key invariant is that `attached_to` cleanly transitions between
/// variants — Object → None → Player, and Player → None → Object — without
/// any prior variant "sticking" through a flicker.
#[test]
fn aura_flicker_preserves_attach_target_variant() {
    let mut state = setup();
    let curse = make_aura(&mut state, "Curse of Bounty", P0);
    let creature = make_creature(&mut state, "Bear", P1);

    // First attach to a player.
    attach_to_player(&mut state, curse, P1);
    assert_eq!(
        state.objects.get(&curse).unwrap().attached_to,
        Some(AttachTarget::Player(P1)),
    );

    // Simulate a flicker: clear, then attach to a creature.
    state.objects.get_mut(&curse).unwrap().attached_to = None;
    attach_to(&mut state, curse, creature);
    assert_eq!(
        state.objects.get(&curse).unwrap().attached_to,
        Some(AttachTarget::Object(creature)),
        "flicker → object: variant must update to Object",
    );

    // Reverse flicker: clear, then attach to a player again.
    state.objects.get_mut(&curse).unwrap().attached_to = None;
    attach_to_player(&mut state, curse, P1);
    assert_eq!(
        state.objects.get(&curse).unwrap().attached_to,
        Some(AttachTarget::Player(P1)),
        "flicker → player: variant must update to Player",
    );
}

/// CR 702.5: An Aura with `Enchant player` cannot be attached to a creature
/// at cast time. This is enforced by the targeting layer — `find_legal_targets`
/// against `TargetFilter::Player` returns only `TargetRef::Player(...)`, so
/// the Aura can never receive a creature target. This test pins that
/// behaviour at the keyword/targeting boundary.
#[test]
fn enchant_player_cannot_target_creature_cr_702_5() {
    use engine::game::targeting::find_legal_targets;
    use engine::types::ability::TargetRef;

    let mut state = setup();
    let curse = make_aura(&mut state, "Curse", P0);
    state
        .objects
        .get_mut(&curse)
        .unwrap()
        .keywords
        .push(Keyword::Enchant(TargetFilter::Player));
    let _bear = make_creature(&mut state, "Bear", P1);

    let legal = find_legal_targets(&state, &TargetFilter::Player, P0, curse);
    assert!(
        !legal.is_empty(),
        "Enchant player must surface at least one player target",
    );
    for t in &legal {
        assert!(
            matches!(t, TargetRef::Player(_)),
            "Enchant player must never include an object target; got {:?}",
            t,
        );
    }
}

/// CR 702.5d (Curse cycle): "Enchant opponent" produces a player-only filter
/// that excludes the controller. `find_legal_targets` must enumerate only
/// the opposing player(s).
#[test]
fn enchant_opponent_targets_only_opponents_cr_702_5d() {
    use engine::game::targeting::find_legal_targets;
    use engine::types::ability::TargetRef;

    let state = setup();
    // The "Enchant opponent" filter as parsed by `parse_enchant_target`.
    let filter = TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent));

    let legal = find_legal_targets(&state, &filter, P0, engine::types::identifiers::ObjectId(0));
    assert!(
        !legal.is_empty(),
        "Enchant opponent must surface the opposing player",
    );
    for t in &legal {
        match t {
            TargetRef::Player(pid) => assert_ne!(
                *pid, P0,
                "Enchant opponent must never include the controller as a target",
            ),
            TargetRef::Object(_) => {
                panic!("Enchant opponent (player-only filter) must never include an object target",)
            }
        }
    }
}

/// CR 303.4f + CR 702.5d: An Aura attached to a player is excluded from
/// creature-layer P/T aggregation. We verify that the "EnchantedBy" filter
/// prop, which surfaces in continuous P/T effects on auras, returns no
/// matching object when the source's host is a Player — there is no object
/// to apply +N/+N to.
#[test]
fn player_aura_does_not_match_enchanted_by_cr_303_4() {
    use engine::game::filter::matches_target_filter;
    use engine::game::filter::FilterContext;
    use engine::types::ability::FilterProp;

    let mut state = setup();
    let curse = make_aura(&mut state, "Curse", P0);
    attach_to_player(&mut state, curse, P1);

    let bear = make_creature(&mut state, "Bear", P0);
    let filter =
        TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]));
    let ctx = FilterContext::from_source(&state, curse);

    assert!(
        !matches_target_filter(&state, bear, &filter, &ctx),
        "CR 303.4: a Player-attached Aura must not match the 'enchanted creature' \
         filter against any creature — there is no object host",
    );
}
