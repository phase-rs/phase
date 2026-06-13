//! Integration tests for the Awaken keyword runtime (CR 702.113a).
//!
//! Awaken N—[cost] (CR 702.113a) is an alternative cost: "You may pay [cost]
//! rather than pay this spell's mana cost" plus a spell ability — "If this
//! spell's awaken cost was paid, put N +1/+1 counters on target land you
//! control. That land becomes a 0/0 Elemental creature with haste. It's still a
//! land." Per CR 702.113b the awaken target exists only if the awaken cost was
//! paid.
//!
//! These tests drive the real `apply` pipeline. The Awaken card is modeled on
//! Rush of Ice (Tap target creature. Awaken 3—{4}{U}) but is built inline so the
//! tests are independent of `card-data.json` (absent in CI). The printed effect
//! (`Tap target creature`) comes from the parser; the `Keyword::Awaken` and the
//! printed/awaken mana costs are stamped via the scenario CardBuilder.

use engine::game::keywords::object_has_effective_keyword_kind;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Duration, TargetRef};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{AlternativeCastKeyword, CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, KeywordKind};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const RUSH_OF_ICE_PRINTED: &str = "Tap target creature.";
const AWAKEN_COUNT: u32 = 3;

/// Printed cost {1}{U}; awaken cost {4}{U}.
fn printed_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 1,
    }
}

fn awaken_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 4,
    }
}

/// Build an Awaken instant in P0's hand: printed `Tap target creature` effect
/// plus `Keyword::Awaken { count: 3, cost: {4}{U} }` and printed cost {1}{U}.
fn add_rush_of_ice(scenario: &mut GameScenario) -> ObjectId {
    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Rush of Ice", true, RUSH_OF_ICE_PRINTED);
    builder
        .with_mana_cost(printed_cost())
        .with_keyword(Keyword::Awaken {
            count: AWAKEN_COUNT,
            cost: awaken_cost(),
        });
    builder.id()
}

/// {4}{U} — enough to pay either the awaken or printed cost.
fn awaken_pool() -> Vec<ManaType> {
    vec![
        ManaType::Blue,
        ManaType::Colorless,
        ManaType::Colorless,
        ManaType::Colorless,
        ManaType::Colorless,
    ]
}

fn add_pool(runner: &mut GameRunner, player: PlayerId, mana: &[ManaType]) {
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

/// True when the *active* target slot's legal targets include `id`.
fn legal_target_contains(runner: &GameRunner, id: ObjectId) -> bool {
    let want = TargetRef::Object(id);
    match &runner.state().waiting_for {
        WaitingFor::TargetSelection {
            target_slots,
            selection,
            ..
        } => target_slots
            .get(selection.current_slot)
            .map(|slot| slot.legal_targets.contains(&want))
            .unwrap_or(false),
        WaitingFor::TriggerTargetSelection { target_slots, .. } => target_slots
            .iter()
            .any(|slot| slot.legal_targets.contains(&want)),
        _ => false,
    }
}

/// Drive cast/resolve to a settled empty-stack Priority. For each object target
/// slot, pick the land when it is a legal target (the awaken rider slot),
/// otherwise pick the fallback creature (the printed Tap slot).
fn drive_to_settled(runner: &mut GameRunner, land: ObjectId, creature: ObjectId) {
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                let pick = if legal_target_contains(runner, land) {
                    land
                } else {
                    creature
                };
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(pick)],
                    })
                    .expect("select target");
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
                if runner.state().stack.is_empty()
                    && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                {
                    break;
                }
            }
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }
}

fn land_counters(runner: &GameRunner, land: ObjectId) -> u32 {
    runner.state().objects[&land]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn is_creature(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id]
        .card_types
        .core_types
        .contains(&CoreType::Creature)
}

fn is_land(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id]
        .card_types
        .core_types
        .contains(&CoreType::Land)
}

fn has_permanent_transient(runner: &GameRunner) -> bool {
    runner
        .state()
        .transient_continuous_effects
        .iter()
        .any(|e| matches!(e.duration, Duration::Permanent))
}

/// Cast Rush of Ice and opt into the awaken cost, driving target selection and
/// resolution. Asserts the awaken choice is offered first.
fn cast_with_awaken(runner: &mut GameRunner, spell: ObjectId, land: ObjectId, creature: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Rush of Ice");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::AlternativeCastChoice {
                keyword: AlternativeCastKeyword::Awaken,
                ..
            }
        ),
        "expected AlternativeCastChoice(Awaken), got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseAlternativeCast {
            choice: AlternativeCastDecision::Alternative,
        })
        .expect("opt into awaken");
    drive_to_settled(runner, land, creature);
}

// ---------------------------------------------------------------------------
// E1: Full awaken pipeline — counters + 0/0 Elemental creature with haste,
//     still a land, permanent animation.
// ---------------------------------------------------------------------------

#[test]
fn e1_awaken_full_pipeline_animates_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = add_rush_of_ice(&mut scenario);
    let land = scenario.add_basic_land(P0, ManaColor::Blue);
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    add_pool(&mut runner, P0, &awaken_pool());

    cast_with_awaken(&mut runner, spell, land, bear);

    // CR 702.113a: N +1/+1 counters on the land.
    assert_eq!(
        land_counters(&runner, land),
        AWAKEN_COUNT,
        "land must have {AWAKEN_COUNT} +1/+1 counters"
    );
    // CR 613.1d: it's a creature; CR 205.1b: still a land.
    assert!(is_creature(&runner, land), "land must be a creature");
    assert!(
        is_land(&runner, land),
        "land must still be a land (CR 205.1b)"
    );
    assert!(
        runner.state().objects[&land]
            .card_types
            .subtypes
            .iter()
            .any(|s| s == "Elemental"),
        "land must be an Elemental"
    );
    // CR 613.1f: has haste.
    assert!(
        object_has_effective_keyword_kind(runner.state(), land, KeywordKind::Haste),
        "animated land must have haste"
    );
    // CR 205.1b: the land retains its mana ability (intrinsic to the Island type).
    assert!(
        !runner.state().objects[&land].abilities.is_empty()
            || runner.state().objects[&land]
                .card_types
                .subtypes
                .iter()
                .any(|s| s == "Island"),
        "awakened land must retain its intrinsic land/mana identity"
    );
    // CR 611.2a: a permanent-duration animation transient exists.
    assert!(
        has_permanent_transient(&runner),
        "a Permanent-duration transient effect must exist for the awakened land"
    );
    // The printed effect still resolved — the bear is tapped.
    assert!(
        runner.state().objects[&bear].tapped,
        "the printed Tap effect must resolve alongside the awaken rider"
    );
}

// ---------------------------------------------------------------------------
// E2: Permanence across turns — the animation persists into the next turn even
//     though the awaken spell went to the graveyard.
// ---------------------------------------------------------------------------

#[test]
fn e2_awaken_animation_persists_across_turns() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = add_rush_of_ice(&mut scenario);
    let land = scenario.add_basic_land(P0, ManaColor::Blue);
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    add_pool(&mut runner, P0, &awaken_pool());

    cast_with_awaken(&mut runner, spell, land, bear);
    assert_eq!(land_counters(&runner, land), AWAKEN_COUNT);
    assert!(is_creature(&runner, land));

    // CR 702.113a: the awaken spell resolves to the graveyard (not exile).
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Graveyard,
        "awaken spell must go to the graveyard"
    );

    // Advance into a later turn by passing priority / declaring no combat
    // (bounded loop). The awakened land has haste, so it becomes a legal
    // attacker — declare no attackers so the turn can progress.
    let start_turn = runner.state().turn_number;
    for _ in 0..400 {
        if runner.state().turn_number > start_turn {
            break;
        }
        let action = match runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            _ => GameAction::PassPriority,
        };
        if runner.act(action).is_err() {
            break;
        }
    }
    assert!(
        runner.state().turn_number > start_turn,
        "the game must advance past the awaken turn (waiting={:?}, phase={:?})",
        runner.state().waiting_for,
        runner.state().phase,
    );

    // CR 611.2a: the Permanent animation must survive past cleanup.
    assert!(
        is_creature(&runner, land),
        "Permanent animation must persist across turns"
    );
    assert!(
        is_land(&runner, land),
        "still a land on the later turn (CR 205.1b)"
    );
    assert_eq!(
        land_counters(&runner, land),
        AWAKEN_COUNT,
        "+1/+1 counters persist (permanent objects, not duration-bound)"
    );
    assert!(
        has_permanent_transient(&runner),
        "the permanent animation transient must still be live"
    );
}

// ---------------------------------------------------------------------------
// E3: Land leaves → animation ends (prune_affected_object_left_effects).
// ---------------------------------------------------------------------------

#[test]
fn e3_awaken_animation_ends_when_land_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = add_rush_of_ice(&mut scenario);
    let land = scenario.add_basic_land(P0, ManaColor::Blue);
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    add_pool(&mut runner, P0, &awaken_pool());

    cast_with_awaken(&mut runner, spell, land, bear);
    assert!(is_creature(&runner, land));
    let before = runner.state().transient_continuous_effects.len();
    assert!(
        before >= 1,
        "animation effect present before the land leaves"
    );

    // Bounce the land off the battlefield. `move_to_zone` runs
    // `prune_affected_object_left_effects` for objects leaving the battlefield.
    engine::game::zones::move_to_zone(runner.state_mut(), land, Zone::Hand, &mut Vec::new());
    engine::game::layers::evaluate_layers(runner.state_mut());

    assert!(
        runner.state().transient_continuous_effects.len() < before,
        "animation effect must be pruned when the land leaves the battlefield (CR 611.2c)"
    );
    assert!(
        !has_permanent_transient(&runner),
        "no permanent animation transient remains once the land is gone"
    );
}

// ---------------------------------------------------------------------------
// E4 (discriminating negative): NORMAL cast does NOT awaken.
// ---------------------------------------------------------------------------

#[test]
fn e4_normal_cast_does_not_awaken() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = add_rush_of_ice(&mut scenario);
    let land = scenario.add_basic_land(P0, ManaColor::Blue);
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    add_pool(&mut runner, P0, &awaken_pool());

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::AlternativeCastChoice {
                keyword: AlternativeCastKeyword::Awaken,
                ..
            }
        ),
        "expected Awaken offer, got {:?}",
        runner.state().waiting_for
    );

    // Choose NORMAL — no rider, no land target.
    runner
        .act(GameAction::ChooseAlternativeCast {
            choice: AlternativeCastDecision::Normal,
        })
        .expect("cast normally");

    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                // CR 702.113b: a normal cast must never offer the land as a target.
                assert!(
                    !legal_target_contains(&runner, land),
                    "a normal (non-awaken) cast must not request the land target"
                );
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(bear)],
                    })
                    .expect("select bear for Tap");
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
                if runner.state().stack.is_empty()
                    && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                {
                    break;
                }
            }
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }

    // Discriminating: no counters, no animation, no permanent transient.
    assert_eq!(
        land_counters(&runner, land),
        0,
        "normal cast must not place +1/+1 counters"
    );
    assert!(
        !is_creature(&runner, land),
        "normal cast must not animate the land"
    );
    assert!(
        !has_permanent_transient(&runner),
        "normal cast must create no permanent animation transient"
    );
    // But the printed effect DID resolve.
    assert!(
        runner.state().objects[&bear].tapped,
        "the printed Tap effect must resolve on a normal cast"
    );
}

// ---------------------------------------------------------------------------
// E5: Offer suppressed without a legal land.
// ---------------------------------------------------------------------------

#[test]
fn e5_awaken_offer_suppressed_without_legal_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = add_rush_of_ice(&mut scenario);
    // No land controlled by P0; a creature is present for the printed Tap.
    let _bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    add_pool(&mut runner, P0, &awaken_pool());

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast");

    // CR 601.2c + CR 702.113b: with no land to awaken, the awaken option is not
    // offered — the cast falls through to the normal path.
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::AlternativeCastChoice { .. }
        ),
        "awaken must not be offered when there is no legal land, got {:?}",
        runner.state().waiting_for
    );
}
