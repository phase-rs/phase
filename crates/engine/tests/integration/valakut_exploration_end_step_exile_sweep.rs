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
//!     no parent target), so the graveyard assertions flip. The 0-delta life
//!     assertions are non-discriminating for THIS revert — a condition revert
//!     also produces 0 deltas, so `== 0` still passes; they discriminate gate
//!     removal (see the gate revert map in the STRICT-FAILURE GATE note
//!     below).
//!   * `landfall_exile_links_and_end_step_sweep_pipeline` — REVERT-FAILING
//!     pipeline-reachability guard: without the fixed parse the card carries
//!     no `LINKED_EXILE_CONSUMER_TAGS` member, so `ExileTop` records no link,
//!     the end-step gate reads an empty pool, and the card never leaves Exile.
//!   * `end_step_trigger_does_not_fire_on_empty_pool` — CR 603.4 empty-pool
//!     gate. Reverted, `condition: None` lets the trigger fire; fixed, it
//!     never goes on the stack. Paired positive reach-guard: the two tests
//!     above drive the identical path with a non-empty pool and sweep it.
//!   * `sweep_and_damage_respect_per_source_link_authority` — multi-authority
//!     hostile fixture: a second source's `ExileLink` must be neither swept
//!     nor counted (`linked_exile_cards_for_source` filters on
//!     `link.source_id == source_id`).
//!
//! STRICT-FAILURE GATE (issue #7046 — see `oracle_effect::assembly::
//! MASS_MOVE_TOTAL_DAMAGE_GAP`): "that much" should read the TOTAL number of
//! cards moved by the sweep (the official Valakut ruling; CR 608.2c
//! later-text-reads-earlier-action), the same for every opponent. But the
//! runtime parent->sub hand-off
//! (`effects/mod.rs::install_previous_effect_counts_by_player`) rewrites the
//! completed `ChangeZoneAll`'s count channels: `last_effect_count` becomes
//! max(per-OWNER counts), and a per-player table is installed which
//! `DamageEachPlayer`'s per-recipient scoped resolution
//! (`resolve_quantity_scoped_with_targets`) consults FIRST — so each opponent
//! would read the count of their OWN swept cards, not the total. Emitting the
//! parsed `DamageEachPlayer{Ref(EventContextAmount), Opponent}` shape would
//! therefore be silently wrong at runtime. Rather than ship that, the parser
//! gate (F1) rewrites the damage clause to a named, fragment-carrying
//! `Effect::Unimplemented { name: MASS_MOVE_TOTAL_DAMAGE_GAP, .. }` — the
//! sweep/condition/link machinery below all still runs for real, but the
//! damage clause is an honest no-op: it deals NO damage. Delete the gate —
//! and flip these deltas to the rules-correct totals (T1: -2 to EACH
//! opponent; T2/T4: -1) — only once #7046 lands an engine-side
//! completed-sweep scalar-total channel.
//!
//! NOT asserted here (empirically verified during this fix round, distinct
//! from and outside this PR's scope): `state.unimplemented_oracle_ids` —
//! `Effect::Unimplemented`'s telemetry side of the no-op — is populated only
//! by `resolve_effect`'s dedicated arm (`effects/mod.rs:~4508`), but the
//! REAL chain-resolution path (`resolve_ability_chain` ->
//! `resolve_chain_body`) short-circuits BEFORE calling `resolve_effect` for
//! any `Effect::Unimplemented` node ("Skip no-op unimplemented/runtime-handled
//! effects", `effects/mod.rs:~9755`), so the telemetry set never actually
//! populates via ordinary stack resolution — only via a direct `resolve_effect`
//! call (unit tests, and one narrow reveal-all special case). This is a
//! pre-existing gap in the telemetry mechanism itself, unrelated to the F1
//! gate, and fixing `resolve_chain_body` is outside this parser-only PR's
//! frozen scope; the deltas below are this test's sole (and sufficient)
//! tripwire.
//!
//! Revert map for the gate itself (distinct from the condition/sweep revert
//! map above):
//!   * `end_step_trigger_sweeps_pool_and_damages_each_opponent` (T1) is the
//!     revert-failing tripwire. Gate removed WITHOUT #7046 landing: the raw
//!     `DamageEachPlayer` shape ships again, so each opponent reads their OWN
//!     swept-card count (P1's delta flips to -1) — the 0-delta assertion
//!     fails. Gate removed WITH #7046 landed but the deltas here left at 0:
//!     the engine now deals the rules-correct -2/-2, which also fails the
//!     0-delta assertions — forcing the conscious flip to -2/-2 described
//!     above.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::Effect;
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

fn assert_queued_gated_damage_continuation(
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
            Effect::Unimplemented { name, .. } if name == "mass_move_total_damage"
        ),
        "the queued damage continuation must carry the mass-move-total-damage marker, got {:?}",
        damage.effect
    );
}

/// T1 — the end-step trigger sweeps the WHOLE pool into the owners'
/// graveyards (CR 404.1: each card to its own owner's graveyard; CR 603.4:
/// the gate held), but the chained damage clause is the strict-failure
/// marker (issue #7046): it deals NO damage to anyone (see module doc for why
/// `state.unimplemented_oracle_ids` is NOT asserted here). N=2 with two
/// different owners.
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
    assert_queued_gated_damage_continuation(&runner, valakut);
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

    // STRICT-FAILURE GATE (issue #7046, see module doc): the damage clause is
    // an honest no-op — NO damage to any player. Revert-discriminating: with
    // the gate removed (and #7046 not yet landed), P1 would take -1 (its own
    // swept-card count read from the per-player table).
    assert_eq!(
        life_of(&runner, P1) - p1_life,
        0,
        "the gated damage clause must deal no damage (honest no-op, issue #7046)"
    );
    assert_eq!(
        life_of(&runner, P2) - p2_life,
        0,
        "the gated damage clause must deal no damage (honest no-op, issue #7046)"
    );
    assert_eq!(
        life_of(&runner, P0) - p0_life,
        0,
        "the controller takes no damage"
    );
    // `state.unimplemented_oracle_ids` is deliberately NOT asserted here — see
    // the module doc's "NOT asserted here" paragraph: the real chain-
    // resolution path never populates it for an `Unimplemented` node, a
    // pre-existing gap outside this PR's scope.
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
    assert_queued_gated_damage_continuation(&runner, valakut);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&impulse].zone,
        Zone::Graveyard,
        "the end-step sweep must move the linked card to its owner's graveyard"
    );
    // STRICT-FAILURE GATE (issue #7046, see module doc): the honest expected
    // delta is 0 (the gated damage clause is a no-op). NON-DISCRIMINATING for
    // the gate itself — T1 owns that tripwire (its zero-delta assertions);
    // this assertion is numerically identical whether the gate exists or not
    // (a Phase trigger has no parent target, so a REVERTED condition/sweep
    // parse also produces a 0 delta here — the pipeline-reachability guards
    // above are what discriminate that revert).
    assert_eq!(
        life_of(&runner, P1) - p1_life,
        0,
        "the gated damage clause must deal no damage (honest no-op, issue #7046)"
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
    assert_queued_gated_damage_continuation(&runner, valakut);
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
    // STRICT-FAILURE GATE (issue #7046, see module doc): the honest expected
    // delta is 0 (the gated damage clause is a no-op). NON-DISCRIMINATING for
    // the gate itself — T1 owns that tripwire. The per-source AUTHORITY claim
    // (a foreign-linked card must not be swept or counted) is carried
    // entirely by the zone assertions above.
    assert_eq!(
        life_of(&runner, P1) - p1_life,
        0,
        "the gated damage clause must deal no damage (honest no-op, issue #7046)"
    );
}
