//! CR 508.3a — the attack-target scope on attack triggers, driven through the
//! real combat pipeline (`GameScenario` → `declare_attackers` → `apply()`).
//!
//! `parse_attack_target` used to be a flat `alt` of full-string `tag`s plus
//! three post-hoc `tag()` re-probes that re-derived the defending-player axis by
//! rescanning text the first parse had already discarded. Because it enumerated
//! the product instead of composing the axes, phrases it did not literally list
//! collapsed onto whichever shorter arm matched first — or onto nothing at all:
//!
//!   - **Mila, Crafty Companion** ("attacks one or more planeswalkers you
//!     control") matched no arm, so BOTH axes were dropped. With
//!     `attack_target_filter == None` and `valid_target == None`,
//!     `attack_target_matches` returns unconditionally true and the trigger
//!     fired on EVERY attack any opponent declared.
//!   - **Oath of Kaya** ("attacks a planeswalker you control with one or more
//!     creatures") kept the type axis but lost the " you control" controller
//!     relation, so it fired on attacks against a THIRD player's planeswalker.
//!   - **Gahiji, Honored One** ("attacks one of your opponents or a planeswalker
//!     an opponent controls") matched the player leaf first and never saw the
//!     planeswalker disjunct, so the pump was silently lost on planeswalker
//!     attacks.
//!
//! The fix composes the grammar by dimension (player leaf × disjunction ×
//! planeswalker noun/plurality × controller relation) and returns both axes from
//! one parse. Nothing in `game/` changed — the runtime matcher was already
//! correct.
//!
//! CR references:
//!   - CR 508.1b: the active player announces which player, planeswalker, or
//!     battle each chosen creature is attacking.
//!   - CR 508.3a: "Whenever [a creature] attacks [a player, planeswalker, or
//!     battle]" triggers only when that player or permanent is the attacked
//!     target. **This is the authorizing rule for the whole change.**
//!   - CR 508.3d: "Whenever [a player] attacks" triggers once per attack
//!     declaration, not once per attacker.
//!   - CR 506.2: the defending player is the player being attacked; that
//!     player's planeswalkers may be attacked in their stead.
//!   - CR 109.4: control of the attacked planeswalker is read live, so the
//!     defending-player relation is never snapshotted at parse time.
//!   - CR 306.5: loyalty is a characteristic only planeswalkers have.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);

const MILA_ORACLE: &str = "Whenever an opponent attacks one or more planeswalkers you control, \
                           put a loyalty counter on each planeswalker you control.";

const OATH_OF_KAYA_ORACLE: &str =
    "Whenever an opponent attacks a planeswalker you control with one or more creatures, \
     Oath of Kaya deals 2 damage to that player and you gain 2 life.";

const GAHIJI_ORACLE: &str =
    "Whenever a creature attacks one of your opponents or a planeswalker an opponent controls, \
     that creature gets +2/+0 until end of turn.";

const BLOOD_RECKONING_ORACLE: &str =
    "Whenever a creature attacks you or a planeswalker you control, \
     that creature's controller loses 1 life.";

/// Count stack entries sourced from `source`.
fn stack_triggers_from(runner: &GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|e| e.source_id == source)
        .count()
}

/// Reach guard (never optional on a negative row): prove the declaration the
/// test claims to have made actually reached `state.combat`. A bare "the trigger
/// did not fire" assertion is vacuous if the attack never happened or the card
/// never parsed.
fn assert_attack_reached_combat(
    runner: &GameRunner,
    attacker: ObjectId,
    expected: AttackTarget,
    context: &str,
) {
    let combat = runner
        .state()
        .combat
        .as_ref()
        .unwrap_or_else(|| panic!("{context}: no combat state after declare_attackers"));
    let info = combat
        .attackers
        .iter()
        .find(|a| a.object_id == attacker)
        .unwrap_or_else(|| {
            panic!("{context}: attacker {attacker:?} never reached state.combat.attackers")
        });
    assert_eq!(
        info.attack_target, expected,
        "{context}: the attack was declared against the wrong target"
    );
}

/// Hand the turn to `player` so an opponent of P0 can be the attacking player.
fn hand_turn_to(runner: &mut GameRunner, player: PlayerId) {
    runner.state_mut().active_player = player;
    runner.state_mut().priority_player = player;
    runner.state_mut().waiting_for = WaitingFor::Priority { player };
}

// ---------------------------------------------------------------------------
// Mila, Crafty Companion — the primary fix (both axes were dropped)
// ---------------------------------------------------------------------------

/// Positive: an opponent attacks a planeswalker P0 controls → Mila fires.
///
/// This is the reach-guard for every Mila negative below: it proves the card
/// parsed, the trigger is indexed, and the pipeline reaches it.
#[test]
fn mila_fires_when_opponent_attacks_your_planeswalker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mila = scenario
        .add_creature_from_oracle(P0, "Mila, Crafty Companion", 2, 2, MILA_ORACLE)
        .id();
    // CR 306.5: a planeswalker fixture with printed loyalty.
    let walker = scenario
        .add_creature(P0, "Jace Fixture", 0, 0)
        .as_planeswalker_with_loyalty("Jace", 5)
        .id();
    let attacker = scenario.add_creature(P1, "Raider", 2, 2).id();

    let mut runner = scenario.build();
    hand_turn_to(&mut runner, P1);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Planeswalker(walker))])
        .expect("DeclareAttackers must succeed");

    assert_attack_reached_combat(
        &runner,
        attacker,
        AttackTarget::Planeswalker(walker),
        "Mila positive",
    );
    assert_eq!(
        stack_triggers_from(&runner, mila),
        1,
        "an opponent attacking a planeswalker you control must fire Mila exactly once, \
         got stack {:?}",
        runner.stack_names()
    );
}

/// Negative + reach guard: the opponent attacks THEIR OWN planeswalker's
/// controller — i.e. a third player's planeswalker. Mila must not fire.
///
/// On the unfixed parser Mila carried `attack_target_filter: null` AND
/// `valid_target: null`, so `attack_target_matches` returned unconditionally
/// true and this fired. **This assertion is the revert canary.**
#[test]
fn mila_does_not_fire_when_opponent_attacks_another_players_planeswalker() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let mila = scenario
        .add_creature_from_oracle(P0, "Mila, Crafty Companion", 2, 2, MILA_ORACLE)
        .id();
    let own_walker = scenario
        .add_creature(P0, "Jace Fixture", 0, 0)
        .as_planeswalker_with_loyalty("Jace", 5)
        .id();
    // The attacked planeswalker belongs to P2, not to Mila's controller.
    let foreign_walker = scenario
        .add_creature(P2, "Liliana Fixture", 0, 0)
        .as_planeswalker_with_loyalty("Liliana", 5)
        .id();
    let attacker = scenario.add_creature(P1, "Raider", 2, 2).id();

    let mut runner = scenario.build();
    hand_turn_to(&mut runner, P1);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Planeswalker(foreign_walker))])
        .expect("DeclareAttackers must succeed");

    // Reach guard: the attack really happened, against a planeswalker.
    assert_attack_reached_combat(
        &runner,
        attacker,
        AttackTarget::Planeswalker(foreign_walker),
        "Mila foreign-planeswalker negative",
    );
    assert_eq!(
        stack_triggers_from(&runner, mila),
        0,
        "CR 506.2: Mila must not fire when the attacked planeswalker is controlled by \
         another player, got stack {:?}",
        runner.stack_names()
    );
    assert_eq!(
        runner.state().objects[&own_walker]
            .counters
            .get(&engine::types::counter::CounterType::Loyalty)
            .copied()
            .unwrap_or(0),
        5,
        "no loyalty counter may be added when the trigger must not fire"
    );
}

/// Negative: the opponent attacks P0 directly (no planeswalker involved). The
/// type axis (`Planeswalker`) must reject a `Player` attack target.
#[test]
fn mila_does_not_fire_when_opponent_attacks_you_directly() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mila = scenario
        .add_creature_from_oracle(P0, "Mila, Crafty Companion", 2, 2, MILA_ORACLE)
        .id();
    scenario
        .add_creature(P0, "Jace Fixture", 0, 0)
        .as_planeswalker_with_loyalty("Jace", 5);
    let attacker = scenario.add_creature(P1, "Raider", 2, 2).id();

    let mut runner = scenario.build();
    hand_turn_to(&mut runner, P1);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P0))])
        .expect("DeclareAttackers must succeed");

    assert_attack_reached_combat(
        &runner,
        attacker,
        AttackTarget::Player(P0),
        "Mila direct-attack negative",
    );
    assert_eq!(
        stack_triggers_from(&runner, mila),
        0,
        "CR 508.3a: a Planeswalker-scoped attack trigger must not fire on a player attack, \
         got stack {:?}",
        runner.stack_names()
    );
}

/// CR 508.3d + CR 508.3b: two attackers declared against TWO different
/// planeswalkers P0 controls must still produce exactly ONE Mila trigger —
/// "an opponent attacks …" fires once per attack declaration, not once per
/// attacker. Both attacks resolve to the same defending player (P0, via
/// `attack_target_defending_player`), so the `seen_defending_players` dedup in
/// `matching_attack_events` collapses them.
#[test]
fn mila_fires_exactly_once_for_two_attackers_on_two_planeswalkers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mila = scenario
        .add_creature_from_oracle(P0, "Mila, Crafty Companion", 2, 2, MILA_ORACLE)
        .id();
    let walker_a = scenario
        .add_creature(P0, "Jace Fixture", 0, 0)
        .as_planeswalker_with_loyalty("Jace", 5)
        .id();
    let walker_b = scenario
        .add_creature(P0, "Chandra Fixture", 0, 0)
        .as_planeswalker_with_loyalty("Chandra", 4)
        .id();
    let attacker_a = scenario.add_creature(P1, "Raider A", 2, 2).id();
    let attacker_b = scenario.add_creature(P1, "Raider B", 2, 2).id();

    let mut runner = scenario.build();
    hand_turn_to(&mut runner, P1);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (attacker_a, AttackTarget::Planeswalker(walker_a)),
            (attacker_b, AttackTarget::Planeswalker(walker_b)),
        ])
        .expect("DeclareAttackers must succeed");

    assert_attack_reached_combat(
        &runner,
        attacker_a,
        AttackTarget::Planeswalker(walker_a),
        "Mila multi-attacker row (A)",
    );
    assert_attack_reached_combat(
        &runner,
        attacker_b,
        AttackTarget::Planeswalker(walker_b),
        "Mila multi-attacker row (B)",
    );
    assert_eq!(
        stack_triggers_from(&runner, mila),
        1,
        "CR 508.3d: two attackers against two of your planeswalkers is ONE attack \
         declaration and must fire Mila exactly once, got stack {:?}",
        runner.stack_names()
    );
}

// ---------------------------------------------------------------------------
// Oath of Kaya — the controller relation on the planeswalker noun was dropped
// ---------------------------------------------------------------------------

/// Positive reach-guard for the Oath negative below.
#[test]
fn oath_of_kaya_fires_when_opponent_attacks_your_planeswalker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let oath = scenario
        .add_enchantment_from_oracle(P0, "Oath of Kaya", OATH_OF_KAYA_ORACLE)
        .id();
    let walker = scenario
        .add_creature(P0, "Jace Fixture", 0, 0)
        .as_planeswalker_with_loyalty("Jace", 5)
        .id();
    let attacker = scenario.add_creature(P1, "Raider", 2, 2).id();

    let mut runner = scenario.build();
    hand_turn_to(&mut runner, P1);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Planeswalker(walker))])
        .expect("DeclareAttackers must succeed");

    assert_attack_reached_combat(
        &runner,
        attacker,
        AttackTarget::Planeswalker(walker),
        "Oath of Kaya positive",
    );
    assert_eq!(
        stack_triggers_from(&runner, oath),
        1,
        "Oath of Kaya must fire when an opponent attacks a planeswalker you control, \
         got stack {:?}",
        runner.stack_names()
    );
}

/// Revert canary: with `valid_target` dropped, Oath of Kaya fired when one
/// opponent attacked a DIFFERENT opponent's planeswalker.
#[test]
fn oath_of_kaya_does_not_fire_on_another_players_planeswalker() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let oath = scenario
        .add_enchantment_from_oracle(P0, "Oath of Kaya", OATH_OF_KAYA_ORACLE)
        .id();
    scenario
        .add_creature(P0, "Jace Fixture", 0, 0)
        .as_planeswalker_with_loyalty("Jace", 5);
    let foreign_walker = scenario
        .add_creature(P2, "Liliana Fixture", 0, 0)
        .as_planeswalker_with_loyalty("Liliana", 5)
        .id();
    let attacker = scenario.add_creature(P1, "Raider", 2, 2).id();

    let mut runner = scenario.build();
    hand_turn_to(&mut runner, P1);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Planeswalker(foreign_walker))])
        .expect("DeclareAttackers must succeed");

    assert_attack_reached_combat(
        &runner,
        attacker,
        AttackTarget::Planeswalker(foreign_walker),
        "Oath of Kaya foreign-planeswalker negative",
    );
    assert_eq!(
        stack_triggers_from(&runner, oath),
        0,
        "CR 506.2: 'a planeswalker you control' must not match another player's \
         planeswalker, got stack {:?}",
        runner.stack_names()
    );
}

// ---------------------------------------------------------------------------
// Gahiji, Honored One — the or-disjunct collapsed to the player leaf
// ---------------------------------------------------------------------------

/// Revert canary for the disjunction: on the unfixed parser Gahiji's
/// `attack_target_filter` stayed `Player`, so `attack_target_type_matches`
/// rejected a `Planeswalker` attack target and the trigger never fired.
#[test]
fn gahiji_fires_when_creature_attacks_an_opponents_planeswalker() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let gahiji = scenario
        .add_creature_from_oracle(P0, "Gahiji, Honored One", 3, 4, GAHIJI_ORACLE)
        .id();
    let foreign_walker = scenario
        .add_creature(P2, "Liliana Fixture", 0, 0)
        .as_planeswalker_with_loyalty("Liliana", 5)
        .id();
    let attacker = scenario.add_creature(P0, "Raider", 2, 2).id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Planeswalker(foreign_walker))])
        .expect("DeclareAttackers must succeed");

    assert_attack_reached_combat(
        &runner,
        attacker,
        AttackTarget::Planeswalker(foreign_walker),
        "Gahiji planeswalker canary",
    );
    assert_eq!(
        stack_triggers_from(&runner, gahiji),
        1,
        "CR 508.3a: the 'or a planeswalker an opponent controls' disjunct must widen the \
         TYPE axis so the pump fires, got stack {:?}",
        runner.stack_names()
    );
}

/// Sibling positive: the player leaf still works after the grammar was
/// recomposed.
#[test]
fn gahiji_still_fires_when_creature_attacks_an_opponent() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let gahiji = scenario
        .add_creature_from_oracle(P0, "Gahiji, Honored One", 3, 4, GAHIJI_ORACLE)
        .id();
    let attacker = scenario.add_creature(P0, "Raider", 2, 2).id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("DeclareAttackers must succeed");

    assert_attack_reached_combat(
        &runner,
        attacker,
        AttackTarget::Player(P1),
        "Gahiji player leg",
    );
    assert_eq!(
        stack_triggers_from(&runner, gahiji),
        1,
        "the player leaf must still fire, got stack {:?}",
        runner.stack_names()
    );
}

/// The classic wrong-fix guard: the disjunct must widen the attacked-object
/// TYPE axis, NOT the defending-PLAYER axis. Gahiji says "one of your
/// opponents", so an attack against Gahiji's own controller's planeswalker must
/// not fire.
#[test]
fn gahiji_does_not_fire_on_its_own_controllers_planeswalker() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let gahiji = scenario
        .add_creature_from_oracle(P0, "Gahiji, Honored One", 3, 4, GAHIJI_ORACLE)
        .id();
    let own_walker = scenario
        .add_creature(P0, "Jace Fixture", 0, 0)
        .as_planeswalker_with_loyalty("Jace", 5)
        .id();
    let attacker = scenario.add_creature(P1, "Raider", 2, 2).id();

    let mut runner = scenario.build();
    hand_turn_to(&mut runner, P1);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Planeswalker(own_walker))])
        .expect("DeclareAttackers must succeed");

    assert_attack_reached_combat(
        &runner,
        attacker,
        AttackTarget::Planeswalker(own_walker),
        "Gahiji own-controller negative",
    );
    assert_eq!(
        stack_triggers_from(&runner, gahiji),
        0,
        "CR 506.2: 'one of your opponents' keeps the defending-player axis opponent-relative, \
         got stack {:?}",
        runner.stack_names()
    );
}

// ---------------------------------------------------------------------------
// Blood Reckoning — the cheap regression lock (correct before AND after)
// ---------------------------------------------------------------------------

/// Blood Reckoning parses to `PlayerOrPlaneswalker` + `Controller` today and
/// must keep firing on an attack against a planeswalker its controller
/// controls.
#[test]
fn blood_reckoning_still_fires_on_planeswalker_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let reckoning = scenario
        .add_enchantment_from_oracle(P0, "Blood Reckoning", BLOOD_RECKONING_ORACLE)
        .id();
    let walker = scenario
        .add_creature(P0, "Jace Fixture", 0, 0)
        .as_planeswalker_with_loyalty("Jace", 5)
        .id();
    let attacker = scenario.add_creature(P1, "Raider", 2, 2).id();

    let mut runner = scenario.build();
    hand_turn_to(&mut runner, P1);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Planeswalker(walker))])
        .expect("DeclareAttackers must succeed");

    assert_attack_reached_combat(
        &runner,
        attacker,
        AttackTarget::Planeswalker(walker),
        "Blood Reckoning planeswalker leg",
    );
    assert_eq!(
        stack_triggers_from(&runner, reckoning),
        1,
        "Blood Reckoning must still fire on a planeswalker attack, got stack {:?}",
        runner.stack_names()
    );
}

/// The other half of the same regression lock: the direct player attack.
#[test]
fn blood_reckoning_still_fires_on_player_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let reckoning = scenario
        .add_enchantment_from_oracle(P0, "Blood Reckoning", BLOOD_RECKONING_ORACLE)
        .id();
    let attacker = scenario.add_creature(P1, "Raider", 2, 2).id();

    let mut runner = scenario.build();
    hand_turn_to(&mut runner, P1);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P0))])
        .expect("DeclareAttackers must succeed");

    assert_attack_reached_combat(
        &runner,
        attacker,
        AttackTarget::Player(P0),
        "Blood Reckoning player leg",
    );
    assert_eq!(
        stack_triggers_from(&runner, reckoning),
        1,
        "Blood Reckoning must still fire on a direct player attack, got stack {:?}",
        runner.stack_names()
    );
}

/// Multi-authority: 3-player game, P1 attacks P2's planeswalker. Blood
/// Reckoning is P0's, and its defending-player relation is `Controller` — so it
/// must not fire for an attack that concerns neither P0 nor P0's permanents.
#[test]
fn blood_reckoning_does_not_fire_on_a_third_players_planeswalker() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let reckoning = scenario
        .add_enchantment_from_oracle(P0, "Blood Reckoning", BLOOD_RECKONING_ORACLE)
        .id();
    scenario
        .add_creature(P0, "Jace Fixture", 0, 0)
        .as_planeswalker_with_loyalty("Jace", 5);
    let foreign_walker = scenario
        .add_creature(P2, "Liliana Fixture", 0, 0)
        .as_planeswalker_with_loyalty("Liliana", 5)
        .id();
    let attacker = scenario.add_creature(P1, "Raider", 2, 2).id();

    let mut runner = scenario.build();
    hand_turn_to(&mut runner, P1);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Planeswalker(foreign_walker))])
        .expect("DeclareAttackers must succeed");

    assert_attack_reached_combat(
        &runner,
        attacker,
        AttackTarget::Planeswalker(foreign_walker),
        "Blood Reckoning multi-authority negative",
    );
    assert_eq!(
        stack_triggers_from(&runner, reckoning),
        0,
        "CR 109.4 + CR 506.2: the defender relation is source-controller-relative, \
         got stack {:?}",
        runner.stack_names()
    );
}
