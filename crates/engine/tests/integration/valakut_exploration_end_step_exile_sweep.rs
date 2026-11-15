//! Valakut Exploration's end-step trigger sweeps every linked exiled card to
//! its owner's graveyard, then deals each opponent the sweep's total. The
//! tests drive the real Oracle parser and trigger-resolution pipeline.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    AggregateFunction, DamageChannel, Effect, PlayerFilter, QuantityExpr, QuantityRef,
};
use engine::types::game_state::{ExileLink, ExileLinkKind};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const VALAKUT_EXPLORATION_ORACLE: &str = "Landfall — Whenever a land you control enters, exile the top card of your library. You may play that card for as long as it remains exiled.\nAt the beginning of your end step, if there are cards exiled with this enchantment, put them into their owner's graveyard, then this enchantment deals that much damage to each opponent.";

/// Move an already-created object into the exile zone and link it to `source`
/// (the ordinary `TrackedBySource` link kind `ExileTop` records via
/// `push_tracked_by_source` — CR 607.2a + CR 406.6).
fn exile_and_link(
    runner: &mut engine::game::scenario::GameRunner,
    obj: ObjectId,
    source: ObjectId,
) {
    engine::game::zones::move_to_zone(runner.state_mut(), obj, Zone::Exile, &mut Vec::new());
    runner.state_mut().exile_links.push(ExileLink {
        exiled_id: obj,
        source_id: source,
        kind: ExileLinkKind::TrackedBySource,
    });
}

fn life_of(runner: &engine::game::scenario::GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .life
}

fn assert_queued_total_damage_continuation(
    runner: &engine::game::scenario::GameRunner,
    source: ObjectId,
) {
    let ability = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == source)
        .and_then(|entry| entry.ability())
        .expect("Valakut end-step trigger must be queued");
    let damage = ability
        .sub_ability
        .as_deref()
        .expect("Valakut damage continuation must be queued");
    assert!(
        matches!(
            &damage.effect,
            Effect::DamageEachPlayer {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::PreviousEffectAmount {
                        channel: DamageChannel::Total,
                        aggregate: AggregateFunction::Sum,
                    },
                },
                player_filter: PlayerFilter::Opponent,
            }
        ),
        "the queued damage continuation must read the completed sweep total, got {:?}",
        damage.effect
    );
}

/// T1 — the end-step trigger sweeps the WHOLE pool into the owners'
/// graveyards (CR 404.1: each card to its own owner's graveyard; CR 603.4:
/// the gate held), then deals the two-card total to every opponent.
#[test]
fn end_step_trigger_sweeps_pool_and_damages_each_opponent() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    // Start after combat so advancing to the end step does not halt at
    // DeclareAttackers.
    scenario.at_phase(Phase::PostCombatMain);
    let valakut = scenario
        .add_enchantment_from_oracle(P0, "Valakut Exploration", VALAKUT_EXPLORATION_ORACLE)
        .id();
    // Two pool members with DIFFERENT owners: CR 404.1 sends each to its own
    // owner's graveyard.
    let exiled_p0 = scenario.add_card_to_hand(P0, "Impulse One");
    let exiled_p1 = scenario.add_card_to_hand(P1, "Impulse Two");
    let mut runner = scenario.build();
    exile_and_link(&mut runner, exiled_p0, valakut);
    exile_and_link(&mut runner, exiled_p1, valakut);

    let p0_life = life_of(&runner, P0);
    let p1_life = life_of(&runner, P1);
    let p2_life = life_of(&runner, P2);

    runner.advance_to_end_step();
    assert_queued_total_damage_continuation(&runner, valakut);
    runner.advance_until_stack_empty();

    // CR 404.1: each swept card lands in its OWN owner's graveyard.
    assert_eq!(
        runner.state().objects[&exiled_p0].zone,
        Zone::Graveyard,
        "the P0-owned pool member must be swept to a graveyard"
    );
    assert_eq!(
        runner.state().objects[&exiled_p1].zone,
        Zone::Graveyard,
        "the P1-owned pool member must be swept to a graveyard"
    );
    assert!(
        runner.state().players[0].graveyard.contains(&exiled_p0),
        "P0's card must be in P0's graveyard"
    );
    assert!(
        runner.state().players[1].graveyard.contains(&exiled_p1),
        "P1's card must be in P1's graveyard"
    );

    assert_eq!(
        life_of(&runner, P1) - p1_life,
        -2,
        "each opponent must receive the two-card sweep total"
    );
    assert_eq!(
        life_of(&runner, P2) - p2_life,
        -2,
        "each opponent must receive the same two-card sweep total"
    );
    assert_eq!(
        life_of(&runner, P0) - p0_life,
        0,
        "the controller takes no damage"
    );
}

/// T2 — full-pipeline reachability guard: the landfall exile actually LINKS
/// (the fixed parse references `ExiledBySource`/`CardsExiledBySource`, so the
/// `LINKED_EXILE_CONSUMER_TAGS` scan turns tracking on and `ExileTop` records
/// the link), and the end-step sweep + damage then run end-to-end from a real
/// `PlayLand` action. Damage is exactly 1 (one card exiled by one landfall).
#[test]
fn landfall_exile_links_and_end_step_sweep_pipeline() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let valakut = scenario
        .add_enchantment_from_oracle(P0, "Valakut Exploration", VALAKUT_EXPLORATION_ORACLE)
        .id();
    scenario.with_library_top(P0, &["Impulse Hit"]);
    let forest = scenario.add_land_to_hand(P0, "Forest").id();
    let mut runner = scenario.build();

    let p1_life = life_of(&runner, P1);

    // Play the Forest — the landfall trigger exiles the library top.
    let card_id = runner.state().objects[&forest].card_id;
    runner
        .act(engine::types::actions::GameAction::PlayLand {
            object_id: forest,
            card_id,
        })
        .expect("should play Forest");
    runner.advance_until_stack_empty();

    // The landfall trigger's "You may play that card ..." grant parks an
    // optional offer for P0; decline it so the card REMAINS exiled for the
    // end-step sweep (CR 607.2a: the pool is the still-exiled linked cards).
    if matches!(
        runner.state().waiting_for,
        engine::types::game_state::WaitingFor::OptionalEffectChoice { .. }
    ) {
        runner
            .act(engine::types::actions::GameAction::DecideOptionalEffect { accept: false })
            .expect("decline the play-from-exile offer");
    }
    runner.advance_until_stack_empty();

    // The exiled card is in Exile and linked to the enchantment: the pool is
    // non-empty, so the end-step gate (CR 603.4) holds.
    let impulse = runner
        .state()
        .objects
        .values()
        .find(|o| o.name == "Impulse Hit")
        .expect("impulse card exists")
        .id;
    assert_eq!(
        runner.state().objects[&impulse].zone,
        Zone::Exile,
        "the landfall trigger must exile the library top"
    );
    assert!(
        runner
            .state()
            .exile_links
            .iter()
            .any(|link| link.exiled_id == impulse),
        "the fixed parse must turn consumer-tag link tracking on (CR 607.2a): {:?}",
        runner.state().exile_links
    );

    runner.advance_to_end_step();
    assert_queued_total_damage_continuation(&runner, valakut);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&impulse].zone,
        Zone::Graveyard,
        "the end-step sweep must move the linked card to its owner's graveyard"
    );
    assert_eq!(
        life_of(&runner, P1) - p1_life,
        -1,
        "the single-card sweep must deal one damage"
    );
}

/// T3 — CR 603.4 fire-time gate: with an EMPTY pool the trigger never goes on
/// the stack and no life changes. Paired positive reach-guard = T1/T2 (the
/// identical path with a non-empty pool sweeps linked cards), so this negative
/// cannot pass vacuously.
#[test]
fn end_step_trigger_does_not_fire_on_empty_pool() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PostCombatMain);
    scenario
        .add_enchantment_from_oracle(P0, "Valakut Exploration", VALAKUT_EXPLORATION_ORACLE)
        .id();
    let mut runner = scenario.build();

    let p0_life = life_of(&runner, P0);
    let p1_life = life_of(&runner, P1);
    let p2_life = life_of(&runner, P2);

    runner.advance_to_end_step();
    assert!(
        runner.stack_names().is_empty(),
        "with no cards exiled with the enchantment the trigger must not fire (CR 603.4); stack: {:?}",
        runner.stack_names()
    );
    runner.advance_until_stack_empty();

    assert_eq!(life_of(&runner, P0), p0_life, "no damage may be dealt");
    assert_eq!(life_of(&runner, P1), p1_life, "no damage may be dealt");
    assert_eq!(life_of(&runner, P2), p2_life, "no damage may be dealt");
}

/// T4 — multi-authority hostile fixture: the sweep and the count respect the
/// per-source link authority (`link.source_id == source_id` in
/// `linked_exile_cards_for_source`). A card linked to a DIFFERENT source is
/// neither swept nor counted: the foreign card stays in Exile while the
/// Valakut-linked card is swept.
#[test]
fn sweep_and_damage_respect_per_source_link_authority() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let valakut = scenario
        .add_enchantment_from_oracle(P0, "Valakut Exploration", VALAKUT_EXPLORATION_ORACLE)
        .id();
    // A second, unrelated source object with its own linked exile.
    let foreign_source = scenario.add_vanilla(P1, 2, 2);
    let mine = scenario.add_card_to_hand(P0, "Valakut Pool Card");
    let foreign = scenario.add_card_to_hand(P1, "Foreign Pool Card");
    let mut runner = scenario.build();
    exile_and_link(&mut runner, mine, valakut);
    exile_and_link(&mut runner, foreign, foreign_source);

    let p1_life = life_of(&runner, P1);

    runner.advance_to_end_step();
    assert_queued_total_damage_continuation(&runner, valakut);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&mine].zone,
        Zone::Graveyard,
        "the Valakut-linked card must be swept"
    );
    assert_eq!(
        runner.state().objects[&foreign].zone,
        Zone::Exile,
        "a card linked to a DIFFERENT source must not be swept (CR 607.2a per-source links)"
    );
    assert_eq!(
        life_of(&runner, P1) - p1_life,
        -1,
        "only the Valakut-linked card contributes to the sweep total"
    );
}
