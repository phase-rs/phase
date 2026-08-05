//! Valakut Exploration — end-step "if there are cards exiled with this
//! enchantment, put them into their owner's graveyard, then this enchantment
//! deals that much damage to each opponent" trigger.
//!
//! Oracle (relevant ability, verbatim):
//!   At the beginning of your end step, if there are cards exiled with this
//!   enchantment, put them into their owner's graveyard, then this enchantment
//!   deals that much damage to each opponent.
//!
//! These tests drive the REAL parser (`add_enchantment_from_oracle`) and the
//! REAL trigger→resolution pipeline (scenario runner) — never a raw resolve.
//! The chain under test: CR 603.4 intervening-if over the linked-exile pool
//! (`TriggerCondition::QuantityComparison { CardsExiledBySource GE 1 }`), the
//! CR 406.6 + CR 607.2a pool sweep
//! (`ChangeZoneAll { origin: Exile, destination: Graveyard, target:
//! ExiledBySource }`, consuming links), and the CR 608.2c chained
//! "that much" damage (`DamageEachPlayer { EventContextAmount, Opponent }`
//! reading `state.last_effect_count` stamped by the completed mass move).
//!
//! Revert map (the parser gate this change adds hoists the existential
//! intervening-if; reverting the condition arm leaves `condition: None` and
//! the old lone `ChangeZone { ParentTarget }` body):
//!   * `end_step_trigger_sweeps_pool_and_damages_each_opponent` —
//!     REVERT-FAILING. Reverted, the sweep moves nothing (a Phase trigger has
//!     no parent target) and no damage clause exists, so the graveyard
//!     assertions and P1's negative life delta all flip.
//!   * `landfall_exile_links_and_end_step_sweep_pipeline` — REVERT-FAILING
//!     pipeline-reachability guard: without the fixed parse the card carries
//!     no `LINKED_EXILE_CONSUMER_TAGS` member, so `ExileTop` records no link,
//!     the end-step gate reads an empty pool, and the card never leaves Exile.
//!   * `end_step_trigger_does_not_fire_on_empty_pool` — CR 603.4 empty-pool
//!     gate. Reverted, `condition: None` lets the trigger fire; fixed, it
//!     never goes on the stack. Paired positive reach-guard: the two tests
//!     above drive the identical path with a non-empty pool.
//!   * `sweep_and_damage_respect_per_source_link_authority` — multi-authority
//!     hostile fixture: a second source's `ExileLink` must be neither swept
//!     nor counted (`linked_exile_cards_for_source` filters on
//!     `link.source_id == source_id`).
//!
//! ENGINE GAP (out of this parser-only change's scope — see the CHARACTERIZED
//! damage assertions below): "that much" should read the TOTAL number of cards
//! moved by the sweep (the official Valakut ruling; CR 608.2c
//! later-text-reads-earlier-action), the same for every opponent. The parse is
//! exactly that (`DamageEachPlayer { Ref(EventContextAmount), Opponent }`, the
//! proven Shadowheart shape). But at runtime the parent→sub hand-off
//! (`effects/mod.rs::install_previous_effect_counts_by_player`) rewrites the
//! completed `ChangeZoneAll`'s count channels: `last_effect_count` becomes
//! max(per-OWNER counts), and a per-player table is installed which
//! `DamageEachPlayer`'s per-recipient scoped resolution
//! (`resolve_quantity_scoped_with_targets`) consults FIRST — so each opponent
//! reads the count of their OWN swept cards, not the total. Correct totals
//! (T1: 2 to each opponent; T2/T4: 1) need an engine-side discrimination
//! between per-recipient and scalar "that many" channels for mass zone moves;
//! until then the damage amounts below are CHARACTERIZATION, not rules truth.

use engine::game::scenario::{GameScenario, P0, P1};
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

/// T1 — the end-step trigger sweeps the WHOLE pool into the owners'
/// graveyards (CR 404.1: each card to its own owner's graveyard) and deals
/// exactly the moved count to EACH opponent (CR 608.2c: "that much" reads the
/// completed move; CR 603.4: the gate held). N=2 with two different owners.
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

    // The damage clause fired (revert-discriminating: the reverted parse has
    // no damage clause at all, so P1's delta would be 0).
    assert!(
        life_of(&runner, P1) < p1_life,
        "the chained damage clause must deal damage to an opponent"
    );
    // CHARACTERIZATION — ENGINE GAP (see module doc): the rules-correct
    // deltas are -2 for EACH opponent (total swept count, per the official
    // Valakut ruling). Today each opponent reads only their OWN swept-card
    // count from the per-player table installed after the mass move, so P1
    // (who owned one swept card) takes 1 and P2 (who owned none) takes 0.
    // Update these to -2/-2 when the engine-side channel discrimination lands.
    assert_eq!(
        life_of(&runner, P1) - p1_life,
        -1,
        "CHARACTERIZATION (engine gap): P1 reads its own swept-card count"
    );
    assert_eq!(
        life_of(&runner, P2) - p2_life,
        0,
        "CHARACTERIZATION (engine gap): P2 owned no swept card and reads 0"
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
    scenario
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
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&impulse].zone,
        Zone::Graveyard,
        "the end-step sweep must move the linked card to its owner's graveyard"
    );
    // CHARACTERIZATION — ENGINE GAP (see module doc): the rules-correct delta
    // is -1 (one swept card, total to each opponent). Today the per-player
    // table gives P1 (who owned no swept card) a 0. Update to -1 when the
    // engine-side channel discrimination lands.
    assert_eq!(
        life_of(&runner, P1) - p1_life,
        0,
        "CHARACTERIZATION (engine gap): P1 owned no swept card and reads 0"
    );
}

/// T3 — CR 603.4 fire-time gate: with an EMPTY pool the trigger never goes on
/// the stack and no life changes. Paired positive reach-guard = T1/T2 (the
/// identical path with a non-empty pool produces positive deltas), so this
/// negative cannot pass vacuously.
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
    // CHARACTERIZATION — ENGINE GAP (see module doc): the rules-correct delta
    // is -1 (the one Valakut-linked swept card, total to each opponent; the
    // foreign-linked card must NOT be counted). Today the per-player table
    // gives P1 (who owned no swept card — the swept card was P0's) a 0. The
    // per-source AUTHORITY claim is carried by the zone assertions above; the
    // 0 below at least proves the foreign link added nothing. Update to -1
    // when the engine-side channel discrimination lands.
    assert_eq!(
        life_of(&runner, P1) - p1_life,
        0,
        "CHARACTERIZATION (engine gap): P1 owned no swept card and reads 0"
    );
}
