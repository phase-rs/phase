//! Issue #6004: Azor's Gateway — "{1}, {T}: Draw a card, then exile a card
//! from your hand. If cards with five or more different mana values are
//! exiled with Azor's Gateway, you gain 5 life, untap Azor's Gateway, and
//! transform it."
//!
//! `oracle_tests.rs::azors_gateway_transform_condition_parses_with_zero_swallowed_clauses`
//! and the `condition.rs` unit tests already prove the PARSE shape (the
//! `ObjectCountDistinct[ManaValue](ExiledBySource) >= 5` condition gates all
//! three effects). This file proves the RUNTIME behavior end to end: the real
//! `ActivateAbility` pipeline draws, exiles via an interactive
//! `EffectZoneChoice`, and the engine's `should_track_exiled_by_source` scan
//! (CR 607.2a) links the exiled card to Azor's Gateway automatically — no
//! manual `ExileLink` wiring for the LIVE activation.
//!
//! Both tests pre-seed N-1 prior exiles as synthetic `TrackedBySource` links
//! (mirroring `mechtitan_core_return_exiled.rs`) so a single live activation
//! exercises the boundary crossing rather than five real taps. Each test also
//! sets Azor's Gateway's `back_face`, even in the below-threshold case, so
//! "did not transform" is never proven vacuously (CR 701.27a requires a
//! `back_face` for `Effect::Transform` to be anything but a no-op).
//!
//! Discriminator: reverting the parser fix (PR #6119) drops the whole `If`
//! clause, leaving an UNCONDITIONAL `GainLife` and no `Untap`/`Transform` sub-
//! abilities at all — every assertion in
//! `azors_gateway_below_threshold_does_not_transform` flips (life gained,
//! stays tapped either way) and `azors_gateway_at_threshold_transforms`'s
//! untap/transform assertions fail outright.

use engine::game::casting::activated_ability_definitions;
use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::{ExileLink, ExileLinkKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const AZORS_GATEWAY_ORACLE: &str = "{1}, {T}: Draw a card, then exile a card from your hand. If cards with five or more different mana values are exiled with Azor's Gateway, you gain 5 life, untap Azor's Gateway, and transform it.";

fn sanctum_of_the_sun_back_face() -> BackFaceData {
    BackFaceData {
        name: "Sanctum of the Sun".to_string(),
        power: None,
        toughness: None,
        loyalty: None,
        defense: None,
        card_types: CardType {
            supertypes: vec![engine::types::card_type::Supertype::Legendary],
            core_types: vec![CoreType::Land],
            subtypes: vec![],
        },
        mana_cost: ManaCost::default(),
        keywords: vec![],
        abilities: vec![],
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: vec![],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: vec![],
        casting_options: vec![],
        layout_kind: None,
    }
}

/// Pre-seed a synthetic card already exiled with `source`, carrying a
/// specific mana value, linked exactly as `push_tracked_by_source` would have
/// linked it during a prior real activation (CR 607.2a).
fn seed_prior_exile(
    state: &mut engine::types::game_state::GameState,
    player: PlayerId,
    source: ObjectId,
    name: &str,
    mana_value: u32,
) {
    let id = create_object(
        state,
        CardId(state.next_object_id),
        player,
        name.to_string(),
        engine::types::zones::Zone::Exile,
    );
    state.objects.get_mut(&id).unwrap().mana_cost = ManaCost::generic(mana_value);
    state.exile_links.push(ExileLink {
        exiled_id: id,
        source_id: source,
        kind: ExileLinkKind::TrackedBySource,
    });
}

/// Put a card with a specific mana value on top of the library so the LIVE
/// activation's "draw a card" pulls a card of known mana value.
fn seed_library_top(
    state: &mut engine::types::game_state::GameState,
    player: PlayerId,
    name: &str,
    mana_value: u32,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(state.next_object_id),
        player,
        name.to_string(),
        engine::types::zones::Zone::Library,
    );
    state.objects.get_mut(&id).unwrap().mana_cost = ManaCost::generic(mana_value);
    let player_state = state.players.iter_mut().find(|p| p.id == player).unwrap();
    player_state.library.retain(|&oid| oid != id);
    player_state.library.insert(0, id);
    id
}

fn draw_ability_index(runner: &engine::game::scenario::GameRunner, azor: ObjectId) -> usize {
    activated_ability_definitions(runner.state(), azor)
        .into_iter()
        .find(|(_, ability)| matches!(ability.effect.as_ref(), Effect::Draw { .. }))
        .expect("Azor's Gateway must carry a Draw activated ability")
        .0
}

/// Drive the live activation through announcement/payment, then answer the
/// "exile a card from your hand" `EffectZoneChoice` with the single card in
/// hand (the just-drawn card — hand was empty beforehand, so there is no
/// choice ambiguity), then drain the stack to resolve the conditional tail.
fn activate_and_exile_drawn_card(
    runner: &mut engine::game::scenario::GameRunner,
    azor: ObjectId,
    idx: usize,
) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: azor,
            ability_index: idx,
        })
        .expect("Azor's Gateway's ability must be payable ({1}, {T})");

    // Drive priority until the ability resolves and the Draw executes,
    // stopping at the interactive exile-from-hand prompt.
    for _ in 0..20 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::EffectZoneChoice { cards, .. } => {
                assert_eq!(
                    cards.len(),
                    1,
                    "hand was empty before the draw, so exactly the drawn card is offered"
                );
                let drawn = cards[0];
                runner
                    .act(GameAction::SelectCards { cards: vec![drawn] })
                    .expect("exile the drawn card from hand");
            }
            _ => break,
        }
    }

    // Drain any remaining priority passes so the conditional tail resolves.
    for _ in 0..20 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty()
        {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
}

fn setup(back_face: bool) -> (engine::game::scenario::GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let azor = scenario
        .add_creature(P0, "Azor's Gateway", 0, 0)
        .as_artifact()
        .from_oracle_text(AZORS_GATEWAY_ORACLE)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    if back_face {
        runner.state_mut().objects.get_mut(&azor).unwrap().back_face =
            Some(sanctum_of_the_sun_back_face());
    }
    (runner, azor)
}

/// Four distinct mana values exiled with Azor's Gateway (below the five
/// threshold): three pre-seeded (0, 1, 2) plus one live activation exiling a
/// mana-value-3 card. The condition must stay false — no life gain, no untap
/// (Azor's Gateway stays tapped from paying its own activation cost), no
/// transform.
#[test]
fn azors_gateway_below_threshold_does_not_transform() {
    let (mut runner, azor) = setup(true);
    seed_prior_exile(runner.state_mut(), P0, azor, "Prior MV0", 0);
    seed_prior_exile(runner.state_mut(), P0, azor, "Prior MV1", 1);
    seed_prior_exile(runner.state_mut(), P0, azor, "Prior MV2", 2);
    seed_library_top(runner.state_mut(), P0, "Live MV3", 3);

    let idx = draw_ability_index(&runner, azor);
    let life_before = runner.life(P0);
    activate_and_exile_drawn_card(&mut runner, azor, idx);

    assert_eq!(
        runner.life(P0),
        life_before,
        "four distinct mana values must NOT satisfy the five-value threshold"
    );
    assert!(
        runner.state().objects[&azor].tapped,
        "the conditional untap must not fire below the threshold — Azor's Gateway \
         stays tapped from its own {{T}} cost"
    );
    assert!(
        !runner.state().objects[&azor].transformed,
        "Azor's Gateway must not transform below the five-distinct-mana-value threshold"
    );
    assert_eq!(runner.state().objects[&azor].name, "Azor's Gateway");
}

/// Five distinct mana values exiled with Azor's Gateway (at the threshold):
/// four pre-seeded (0, 1, 2, 3) plus one live activation exiling a
/// mana-value-4 card. The condition must be true — the controller gains 5
/// life, Azor's Gateway untaps despite having just paid its own {T} cost, and
/// it transforms into Sanctum of the Sun.
#[test]
fn azors_gateway_at_threshold_transforms() {
    let (mut runner, azor) = setup(true);
    seed_prior_exile(runner.state_mut(), P0, azor, "Prior MV0", 0);
    seed_prior_exile(runner.state_mut(), P0, azor, "Prior MV1", 1);
    seed_prior_exile(runner.state_mut(), P0, azor, "Prior MV2", 2);
    seed_prior_exile(runner.state_mut(), P0, azor, "Prior MV3", 3);
    seed_library_top(runner.state_mut(), P0, "Live MV4", 4);

    let idx = draw_ability_index(&runner, azor);
    let life_before = runner.life(P0);
    activate_and_exile_drawn_card(&mut runner, azor, idx);

    assert_eq!(
        runner.life(P0),
        life_before + 5,
        "five distinct mana values must satisfy the threshold and gain 5 life"
    );
    assert!(
        !runner.state().objects[&azor].tapped,
        "the conditional untap must fire at the threshold, undoing the activation's own tap"
    );
    assert!(
        runner.state().objects[&azor].transformed,
        "Azor's Gateway must transform at the five-distinct-mana-value threshold"
    );
    assert_eq!(runner.state().objects[&azor].name, "Sanctum of the Sun");
}
