//! Runtime regression for the SNC "graveyard mana-value diversity" condition:
//! "\[As long as\] there are five or more mana values among cards in your
//! graveyard" (Aven Heartstabber, Snooping Newsie, Syndicate Infiltrator,
//! Graveyard Shift, and their Alchemy variants).
//!
//! CR 202.3 (mana value) + CR 611.3a (continuous "as long as" static gate) +
//! CR 601.3d (conditional flash grant on a spell).
//!
//! Two DIFFERENT runtime paths are exercised — they share the same condition
//! (`StaticCondition`/`ParsedCondition::QuantityComparison` over
//! `QuantityRef::ObjectCountDistinct[ManaValue]`) but resolve through
//! different subsystems:
//!   1. A creature's own static P/T + keyword grant (`StaticCondition::
//!      QuantityComparison`, evaluated through the layer system like
//!      Delirium's `DistinctCardTypes` — see `add-static-ability`).
//!   2. A spell's OWN conditional Flash CASTING PERMISSION
//!      (`SpellCastingOption::AsThoughHadFlash`, evaluated directly at
//!      cast-announcement time via `restrictions::flash_timing_cost`).
//!      Despite its "This spell has flash" text also matching the generic
//!      "has " static-pattern arm, it must NOT lower to a continuous
//!      `AddKeyword(Flash)` static: CR 611.3b lets a static apply from
//!      whatever zone is "appropriate", but this engine's
//!      `for_each_static_effect_source` (an implementation choice, not
//!      itself a numbered rule) only indexes continuous-effect SOURCES from
//!      the battlefield and command zone, so a Hand-zone spell's own self-
//!      referential static would never fire — a "parses fine, does nothing"
//!      misparse.
//!      `oracle_classifier::is_self_conditional_flash_grant` defers the line
//!      past the static classifier so it reaches
//!      `oracle_casting::parse_self_has_flash_option` instead.
//!
//! Both paths count DISTINCT mana values, not total card count: the negative
//! case seeds MORE cards in the graveyard than the positive case, but with
//! only four distinct mana values among them, to prove the counting logic
//! dedupes by value rather than counting cards.

use engine::game::engine::EngineError;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const AVEN_HEARTSTABBER: &str = "Flying\n\
    As long as there are five or more mana values among cards in your graveyard, \
    this creature gets +2/+2 and has deathtouch.\n\
    When this creature dies, mill two cards, then draw a card.";

const GRAVEYARD_SHIFT: &str = "This spell has flash as long as there are five or \
    more mana values among cards in your graveyard.\n\
    Return target creature card from your graveyard to the battlefield.";

/// A printed generic-only mana cost of the given mana value.
fn generic_cost(mv: u32) -> ManaCost {
    ManaCost::Cost {
        shards: vec![],
        generic: mv,
    }
}

/// A printed single-black-pip cost of mana value `mv` (mv >= 1). Used so two
/// fillers can share the same numeric mana value via different pip shapes
/// while keeping the value itself identical (the axis under test is the
/// VALUE, not the shard shape).
fn black_cost(mv: u32) -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Black],
        generic: mv.saturating_sub(1),
    }
}

fn add_mana(
    runner: &mut engine::game::scenario::GameRunner,
    player: engine::types::player::PlayerId,
    mana: &[ManaType],
) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

// ---------------------------------------------------------------------------
// Path 1: static P/T + keyword grant (Aven Heartstabber)
// ---------------------------------------------------------------------------

#[test]
fn aven_heartstabber_gets_bonus_with_five_distinct_mana_values() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Printed 1/1 with flying; the +2/+2 and deathtouch must come ONLY from
    // the dynamic "as long as" static, never the printed values.
    let aven = scenario
        .add_creature(P0, "Aven Heartstabber", 1, 1)
        .from_oracle_text(AVEN_HEARTSTABBER)
        .id();

    // Five distinct mana values: 0, 1, 2, 3, 4.
    for mv in 0..5u32 {
        scenario
            .add_creature_to_graveyard(P0, &format!("Filler MV{mv}"), 1, 1)
            .with_mana_cost(generic_cost(mv));
    }

    let mut runner = scenario.build();
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    let obj = &runner.state().objects[&aven];
    assert_eq!(
        obj.power,
        Some(3),
        "1 (printed) + 2 (bonus) power with 5 distinct mana values in graveyard"
    );
    assert_eq!(
        obj.toughness,
        Some(3),
        "1 (printed) + 2 (bonus) toughness with 5 distinct mana values in graveyard"
    );
    assert!(
        obj.keywords.contains(&Keyword::Deathtouch),
        "deathtouch must be granted with 5+ distinct mana values in graveyard, got {:?}",
        obj.keywords
    );
    assert!(
        obj.keywords.contains(&Keyword::Flying),
        "printed Flying must still be present"
    );
}

#[test]
fn aven_heartstabber_no_bonus_with_four_distinct_values_despite_more_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let aven = scenario
        .add_creature(P0, "Aven Heartstabber", 1, 1)
        .from_oracle_text(AVEN_HEARTSTABBER)
        .id();

    // Six cards in the graveyard — MORE cards than the positive test above —
    // but only four DISTINCT mana values (0, 1, 2, 2, 3, 3). If the condition
    // were (mis-)implemented as a raw card-count check, six cards would
    // wrongly satisfy "five or more"; counting distinct values correctly
    // keeps it at four and the bonus must NOT apply.
    let mana_values = [0u32, 1, 2, 2, 3, 3];
    for (i, mv) in mana_values.iter().enumerate() {
        scenario
            .add_creature_to_graveyard(P0, &format!("Filler {i}"), 1, 1)
            .with_mana_cost(generic_cost(*mv));
    }

    // P1 has five distinct values, but "your graveyard" is relative to Aven's
    // controller (P0). A dropped controller filter would wrongly turn the bonus
    // on before P0 receives a fifth distinct value below.
    for mv in 0..5u32 {
        scenario
            .add_creature_to_graveyard(P1, &format!("Opponent Filler MV{mv}"), 1, 1)
            .with_mana_cost(generic_cost(mv));
    }

    let mut runner = scenario.build();
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    let obj = &runner.state().objects[&aven];
    assert_eq!(
        obj.power,
        Some(1),
        "printed power only — P0's 4 distinct mana values (despite P1 having 5) must not trigger the bonus"
    );
    assert_eq!(
        obj.toughness,
        Some(1),
        "printed toughness only — P0's 4 distinct mana values must not trigger the bonus"
    );
    assert!(
        !obj.keywords.contains(&Keyword::Deathtouch),
        "deathtouch must NOT be granted with only 4 distinct mana values, got {:?}",
        obj.keywords
    );

    // Positive reach-guard (card-test anti-pattern #6, vacuous negatives):
    // the assertions above must fail for the CONDITION being false, not
    // because the "as long as" static failed to parse at all (which would
    // produce the identical printed-1/1-no-deathtouch outcome). Add a 5th
    // distinct mana value to the SAME object/scenario and re-evaluate:
    // the bonus turning on proves the static parsed and its condition is
    // being read live, not that this creature can never get the bonus.
    scenario_add_creature_to_graveyard_post_build(&mut runner, P0, "Filler 5th", generic_cost(4));
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let obj = &runner.state().objects[&aven];
    assert_eq!(
        obj.power,
        Some(3),
        "reach-guard: a 5th distinct mana value must turn the bonus on, proving the prior \
         assertions exercised a false condition rather than a parse failure"
    );
    assert!(obj.keywords.contains(&Keyword::Deathtouch));
}

/// Owner-scoped graveyard regression (PR #8347 review, HIGH). CR 400.3 + CR
/// 109.5 + CR 108.4a: a graveyard's membership is keyed by OWNER, not
/// controller — "your graveyard" is an ownership claim even though the
/// parser represents "your" as `ControllerRef::You` (see
/// `game::filter::is_owner_scoped_zone`). A creature P0 OWNS but that was
/// under P1's control when it died (e.g. via a control-stealing effect like
/// Mind Control) leaves a stale `obj.controller = P1` behind
/// (`reset_for_battlefield_exit` does not reset `controller` back to the
/// owner on a non-battlefield exit) — yet the card still lands in ITS
/// OWNER's graveyard per CR 400.3 and must count toward P0's distinct-mana-
/// value query. Resolving `QuantityRef::ObjectCountDistinct` through the
/// plain controller-scoped `matches_target_filter` (instead of the zone-aware
/// `matches_target_filter_for_zone`) would wrongly exclude it, since its live
/// `controller` field reads P1, not P0.
#[test]
fn aven_heartstabber_counts_owned_creature_that_died_under_opponent_control() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let aven = scenario
        .add_creature(P0, "Aven Heartstabber", 1, 1)
        .from_oracle_text(AVEN_HEARTSTABBER)
        .id();

    // Four distinct mana values, cleanly owned AND controlled by P0.
    for mv in 0..4u32 {
        scenario
            .add_creature_to_graveyard(P0, &format!("Filler MV{mv}"), 1, 1)
            .with_mana_cost(generic_cost(mv));
    }

    let mut runner = scenario.build();
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&aven].power,
        Some(1),
        "only 4 distinct mana values before the owner-scoped 5th arrives"
    );

    // The 5th distinct mana value: a creature P0 OWNS, staged directly into
    // P0's graveyard, but whose live `controller` field is P1 — reproducing
    // the state a control-stealing effect (Mind Control) followed by that
    // creature's death would leave behind.
    let stolen = scenario_add_creature_to_graveyard_post_build(
        &mut runner,
        P0,
        "Stolen Filler",
        generic_cost(4),
    );
    assert_eq!(
        runner.state().objects[&stolen].owner,
        P0,
        "fixture: the card must be OWNED by P0 for this case to discriminate"
    );
    runner
        .state_mut()
        .objects
        .get_mut(&stolen)
        .unwrap()
        .controller = P1;
    assert_eq!(
        runner.state().objects[&stolen].controller,
        P1,
        "fixture: the stale controller must read P1 — an owner-keyed \
         implementation and a controller-keyed one must give DIFFERENT \
         answers here, or this fixture cannot discriminate"
    );

    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    let obj = &runner.state().objects[&aven];
    assert_eq!(
        obj.power,
        Some(3),
        "a creature P0 OWNS must count toward P0's 'your graveyard' distinct-mana-value \
         query even though its stale `controller` field reads P1 — ownership, not stale \
         controller, is authoritative for graveyard membership (CR 400.3)"
    );
    assert_eq!(obj.toughness, Some(3));
    assert!(
        obj.keywords.contains(&Keyword::Deathtouch),
        "deathtouch must be granted once the owner-scoped 5th distinct value is present, \
         got {:?}",
        obj.keywords
    );
}

#[test]
fn aven_heartstabber_bonus_tracks_live_as_graveyard_changes() {
    // CR 611.3a: the static re-evaluates continuously. Start below the
    // threshold, then mill a fifth distinct mana value in and confirm the
    // bonus turns on without re-building the scenario.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let aven = scenario
        .add_creature(P0, "Aven Heartstabber", 1, 1)
        .from_oracle_text(AVEN_HEARTSTABBER)
        .id();

    for mv in 0..4u32 {
        scenario
            .add_creature_to_graveyard(P0, &format!("Filler MV{mv}"), 1, 1)
            .with_mana_cost(generic_cost(mv));
    }

    let mut runner = scenario.build();
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&aven].power,
        Some(1),
        "only 4 distinct mana values — bonus must be off before the fifth arrives"
    );

    // A fifth distinct mana value lands in the graveyard.
    scenario_add_creature_to_graveyard_post_build(&mut runner, P0, "Filler MV4", black_cost(5));
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    let obj = &runner.state().objects[&aven];
    assert_eq!(
        obj.power,
        Some(3),
        "bonus must turn ON once a 5th distinct mana value enters the graveyard"
    );
    assert!(obj.keywords.contains(&Keyword::Deathtouch));
}

/// Minimal post-`build()` graveyard seeding helper (the fluent `CardBuilder`
/// only exists pre-build, on `GameScenario`), mirroring
/// `GameScenario::add_creature_to_graveyard` + `CardBuilder::with_mana_cost`
/// but usable after `.build()` has produced a `GameRunner`.
fn scenario_add_creature_to_graveyard_post_build(
    runner: &mut engine::game::scenario::GameRunner,
    player: engine::types::player::PlayerId,
    name: &str,
    mana_cost: ManaCost,
) -> ObjectId {
    let state = runner.state_mut();
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, player, name.to_string(), Zone::Graveyard);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(1);
    obj.toughness = Some(1);
    obj.base_power = Some(1);
    obj.base_toughness = Some(1);
    obj.mana_cost = mana_cost.clone();
    obj.base_mana_cost = mana_cost;
    id
}

// ---------------------------------------------------------------------------
// Path 2: conditional Flash on a spell (Graveyard Shift)
// ---------------------------------------------------------------------------

/// Build a scenario outside sorcery-speed timing (P1 is active, P0 holds
/// priority) with Graveyard Shift in P0's hand and a creature card in P0's
/// graveyard to target, mirroring `timely_ward_regression.rs`'s pattern for
/// isolating a conditional flash grant from ordinary sorcery-speed timing.
fn build_graveyard_shift_scenario(
    graveyard_mana_values: &[u32],
) -> (engine::game::scenario::GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);

    let shift = scenario
        .add_spell_to_hand_from_oracle(P0, "Graveyard Shift", false, GRAVEYARD_SHIFT)
        .with_mana_cost(generic_cost(4))
        .id();

    // The legal reanimation target: always present regardless of the mana
    // values under test.
    let target = scenario
        .add_creature_to_graveyard(P0, "Reanimation Target", 2, 2)
        .with_mana_cost(generic_cost(2))
        .id();

    for (i, mv) in graveyard_mana_values.iter().enumerate() {
        scenario
            .add_creature_to_graveyard(P0, &format!("Filler {i}"), 1, 1)
            .with_mana_cost(generic_cost(*mv));
    }

    let mut runner = scenario.build();
    // Outside P0's sorcery-speed window: P1 is active, P0 holds priority.
    runner.state_mut().active_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    add_mana(&mut runner, P0, &[ManaType::Black; 5]);

    (runner, shift, target)
}

#[test]
fn graveyard_shift_castable_at_instant_speed_with_five_distinct_mana_values() {
    // Reanimation target (mv 2) + 4 fillers (0, 1, 3, 4) = 5 distinct values
    // total (0, 1, 2, 3, 4). Driven entirely through the `SpellCast` fluent
    // driver (card-test canonical recipe): its internal `resolve()` asserts
    // the initial `CastSpell` announcement is accepted by the engine, so a
    // rejected instant-speed cast (flash condition unmet, or no flash at all)
    // would fail loudly here rather than silently — the announcement accept/
    // reject boundary IS the behavior under test.
    let (mut runner, shift, target) = build_graveyard_shift_scenario(&[0, 1, 3, 4]);

    let outcome = runner.cast(shift).target_object(target).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the conditional-flash cast must resolve cleanly, got {:?}",
        outcome.final_waiting_for()
    );
    outcome.assert_zone(&[target], Zone::Battlefield);
}

#[test]
fn graveyard_shift_not_castable_at_instant_speed_with_four_distinct_mana_values() {
    // Reanimation target (mv 2) + 3 fillers (0, 1, 3) = exactly 4 distinct
    // values total (0, 1, 2, 3) — one short of the "five or more" threshold.
    // Outside sorcery speed and with no flash granted, the cast must be
    // rejected outright.
    let (mut runner, shift, _target) = build_graveyard_shift_scenario(&[0, 1, 3]);
    let card_id = runner.state().objects[&shift].card_id;
    let before = runner.state().clone();

    let result = runner.act(GameAction::CastSpell {
        object_id: shift,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    });
    // Positive reach-guard (vacuous-negative check): assert the SPECIFIC
    // sorcery-speed-timing rejection reason, not just "any Err" — that rules
    // out an incidental setup failure (bad mana, bad target list) accidentally
    // making this assertion pass for the wrong reason regardless of whether
    // the flash condition is even wired up.
    match &result {
        Err(EngineError::ActionNotAllowed(msg)) => {
            assert!(
                msg.contains("Sorcery-speed spells can only be cast"),
                "expected the sorcery-speed timing rejection (no flash granted with only \
                 4 distinct mana values), got a different ActionNotAllowed: {msg:?}"
            );
        }
        other => panic!(
            "4 distinct mana values must be rejected at instant-speed timing \
             specifically, got {other:?}"
        ),
    }
    assert_eq!(
        runner.state(),
        &before,
        "a rejected cast announcement must not mutate game state"
    );
}
