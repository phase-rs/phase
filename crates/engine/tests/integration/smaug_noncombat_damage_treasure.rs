//! Smaug the Impenetrable — passive-voice noncombat damage-received trigger.
//!
//! CR 120.2b + CR 603.2c + CR 111.10a: "Whenever Smaug is dealt noncombat
//! damage, create that many Treasure tokens." Before the passive-voice damage
//! grammar was parameterized onto its axes, `"is dealt noncombat damage"` was an
//! unreachable cell — the eight enumerated `tag()` arms covered `"is dealt
//! damage"`, `"is dealt combat damage"` and the two excess forms, but never the
//! noncombat/total cell — so the trigger did not parse at all.
//!
//! Every test drives a REAL activated ability through the production pipeline
//! (`runner.activate(..).target_object(..).resolve()`), so the damage is a
//! genuine CR 120.2b noncombat `GameEvent::DamageDealt` raised by the
//! deal-damage resolver. Nothing is hand-injected. Reverting the parser change
//! leaves the trigger unparsed, so zero Treasures are created and the positive
//! assertions fail.

use super::rules::run_combat;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::Effect;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// Verbatim Oracle text (MTGJSON `AtomicCards.json`, name "Smaug the
/// Impenetrable"). A paraphrase could take a different parser branch, so the
/// printed text is used exactly as printed.
const SMAUG_ORACLE: &str = "Flying, indestructible, haste\n\
    Whenever Smaug is dealt noncombat damage, create that many Treasure tokens.";

/// CR 120.2b: a noncombat damage source — damage dealt as the effect of an
/// ability, not in the combat damage step.
const PINGER_3_ORACLE: &str = "{T}: Sear Drake deals 3 damage to any target.";
const PINGER_5_ORACLE: &str = "{T}: Ember Drake deals 5 damage to any target.";

/// CR 111.10a: a Treasure token is a Treasure artifact token, so the printed
/// subtype is the honest thing to count.
fn count_treasures(state: &GameState) -> usize {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| {
            obj.card_types
                .subtypes
                .iter()
                .any(|s| s.eq_ignore_ascii_case("Treasure"))
        })
        .count()
}

/// Index of a pinger's `{T}: … deals N damage to any target` activated ability.
fn tap_damage_index(runner: &GameRunner, pinger: ObjectId) -> usize {
    runner.state().objects[&pinger]
        .abilities
        .iter()
        .position(|a| matches!(a.effect.as_ref(), Effect::DealDamage { .. }))
        .expect("the damage source must carry a DealDamage ({T}) activated ability")
}

/// Give `player` priority in their own pre-combat main so an activated ability
/// can be declared (mirrors the other activated-ability integration tests).
fn hand_priority(runner: &mut GameRunner, player: PlayerId) {
    runner.state_mut().active_player = player;
    runner.state_mut().priority_player = player;
    runner.state_mut().waiting_for = WaitingFor::Priority { player };
}

/// Smaug, built from its verbatim Oracle text. The inline keyword line
/// "Flying, indestructible, haste" needs explicit keyword-name hints or it
/// parses to `Effect::Unimplemented`; indestructible also keeps Smaug alive
/// under the damage these tests deal to it.
fn add_smaug(scenario: &mut GameScenario) -> ObjectId {
    let mut builder = scenario.add_creature(P0, "Smaug the Impenetrable", 8, 7);
    builder.from_oracle_text_with_keywords(&["Flying", "Indestructible", "Haste"], SMAUG_ORACLE);
    builder.id()
}

#[test]
fn smaug_creates_treasure_equal_to_noncombat_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let smaug = add_smaug(&mut scenario);
    let pinger = scenario
        .add_creature_from_oracle(P0, "Sear Drake", 2, 2, PINGER_3_ORACLE)
        .id();

    let mut runner = scenario.build();
    hand_priority(&mut runner, P0);
    let idx = tap_damage_index(&runner, pinger);
    let before = count_treasures(runner.state());

    // CR 120.2b: the ability's resolution deals genuine noncombat damage to
    // Smaug, raising the `DamageDealt` event the trigger matches on.
    let _ = runner.activate(pinger, idx).target_object(smaug).resolve();

    // CR 603.2c + CR 111.10a: "that many" is the magnitude of THIS trigger's own
    // triggering event, so exactly three Treasure tokens are created.
    assert_eq!(
        count_treasures(runner.state()) - before,
        3,
        "3 noncombat damage to Smaug must create exactly 3 Treasure tokens"
    );
}

#[test]
fn smaug_ignores_combat_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let smaug = add_smaug(&mut scenario);
    let pinger = scenario
        .add_creature_from_oracle(P0, "Sear Drake", 2, 2, PINGER_3_ORACLE)
        .id();
    // A 3/3 blocker deals Smaug 3 COMBAT damage. Smaug is indestructible, so it
    // survives to be pinged by the reach-guard below.
    let blocker = scenario.add_creature(P1, "Stout Blocker", 3, 3).id();

    let mut runner = scenario.build();
    let before = count_treasures(runner.state());

    // CR 510 + CR 120.2a: combat damage is dealt in the combat damage step.
    // `damage_kind == NoncombatOnly` must reject it.
    run_combat(&mut runner, vec![smaug], vec![(blocker, smaug)]);

    assert_eq!(
        count_treasures(runner.state()),
        before,
        "combat damage must not fire a noncombat-only damage-received trigger"
    );

    // Paired positive reach-guard, same test: the trigger is NOT merely inert —
    // the very same Smaug creates Treasures from a noncombat hit. Without this,
    // the negative above would pass vacuously if the trigger never parsed.
    hand_priority(&mut runner, P0);
    let idx = tap_damage_index(&runner, pinger);
    let _ = runner.activate(pinger, idx).target_object(smaug).resolve();

    assert_eq!(
        count_treasures(runner.state()) - before,
        3,
        "the same Smaug must still create 3 Treasures from noncombat damage"
    );
}

#[test]
fn smaug_binds_each_damage_event_independently() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let smaug = add_smaug(&mut scenario);
    let pinger_3 = scenario
        .add_creature_from_oracle(P0, "Sear Drake", 2, 2, PINGER_3_ORACLE)
        .id();
    let pinger_5 = scenario
        .add_creature_from_oracle(P0, "Ember Drake", 2, 2, PINGER_5_ORACLE)
        .id();

    let mut runner = scenario.build();
    let before = count_treasures(runner.state());

    hand_priority(&mut runner, P0);
    let idx_3 = tap_damage_index(&runner, pinger_3);
    let _ = runner
        .activate(pinger_3, idx_3)
        .target_object(smaug)
        .resolve();
    assert_eq!(
        count_treasures(runner.state()) - before,
        3,
        "the first noncombat damage event binds its own amount (3)"
    );

    hand_priority(&mut runner, P0);
    let idx_5 = tap_damage_index(&runner, pinger_5);
    let _ = runner
        .activate(pinger_5, idx_5)
        .target_object(smaug)
        .resolve();

    // CR 603.2c: each trigger instance binds the magnitude of the event that
    // triggered IT. A shared or last-write-wins amount slot would yield 5+5=10
    // or 8+8=16 rather than 3+5=8.
    assert_eq!(
        count_treasures(runner.state()) - before,
        8,
        "each damage event must bind its own amount: 3 then 5, never 5+5 or 8+8"
    );
}

#[test]
fn smaug_ignores_noncombat_damage_to_another_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let smaug = add_smaug(&mut scenario);
    let bystander = scenario.add_creature(P0, "Idle Bystander", 4, 4).id();
    let pinger_a = scenario
        .add_creature_from_oracle(P0, "Sear Drake", 2, 2, PINGER_3_ORACLE)
        .id();
    let pinger_b = scenario
        .add_creature_from_oracle(P0, "Ember Drake", 2, 2, PINGER_3_ORACLE)
        .id();

    let mut runner = scenario.build();
    let before = count_treasures(runner.state());

    // The trigger's subject is Smaug itself (`valid_card: SelfRef`), so damage
    // to a different creature must not fire it.
    hand_priority(&mut runner, P0);
    let idx_a = tap_damage_index(&runner, pinger_a);
    let _ = runner
        .activate(pinger_a, idx_a)
        .target_object(bystander)
        .resolve();

    assert_eq!(
        count_treasures(runner.state()),
        before,
        "noncombat damage to another creature must not fire Smaug's trigger"
    );

    // Paired positive reach-guard, same test: the identical damage source aimed
    // at Smaug does fire it, so the negative is about the recipient and not
    // about the trigger being inert.
    hand_priority(&mut runner, P0);
    let idx_b = tap_damage_index(&runner, pinger_b);
    let _ = runner
        .activate(pinger_b, idx_b)
        .target_object(smaug)
        .resolve();

    assert_eq!(
        count_treasures(runner.state()) - before,
        3,
        "the same damage aimed at Smaug must create 3 Treasures"
    );
}
