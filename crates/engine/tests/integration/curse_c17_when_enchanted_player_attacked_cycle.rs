//! Commander 2017 "whenever enchanted player is attacked" curse cycle.
//!
//! Five curses share the same trigger structure:
//!   - **Curse of Bounty** (1G) — untap all nonland permanents you control
//!   - **Curse of Disturbance** (2B) — create a 2/2 black Zombie creature token
//!   - **Curse of Opulence** (R) — create a Gold token
//!   - **Curse of Verbosity** (2U) — draw a card
//!   - **Curse of Vitality** (2W) — you gain 2 life
//!
//! Each also has "Each opponent attacking that player does the same." Per
//! CR 508.6 a player is "attacking [a player]" iff it controls a creature
//! attacking that player, so the rider fans out only to the curse controller's
//! opponents who, when the trigger resolves, control a creature attacking the
//! ENCHANTED player itself (not its planeswalker or battle) — never the
//! enchanted defending player themselves, and never a non-attacking opponent.
//! In a two-player game the controller's only opponent IS the enchanted
//! defending player, who cannot attack themselves, so the rider is a no-op.
//!
//! CR references:
//!   - CR 508.3b: "Whenever [a player] is attacked" triggers when one or more
//!     creatures are declared as attackers attacking that player.
//!   - CR 508.6: a player is "attacking [a player]" iff it controls a creature
//!     attacking that player; it "has attacked" iff it declared such a creature.
//!   - CR 102.2: opponents are measured relative to the curse controller.
//!   - CR 303.4b: An Aura that enchants a player is attached to that player.
//!   - CR 508.1a: The active player chooses which creatures will attack.
//!   - CR 506.4: a creature removed from combat stops being an attacking
//!     creature, so its controller is no longer "attacking" that player.

use engine::game::combat::{apply_resolved_combat_membership, AttackerInfo};
use engine::game::effects::attach::attach_to_player;
use engine::game::effects::remove_from_combat::remove_object_from_combat;
use engine::game::game_object::AttachTarget;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::game::zones::move_to_zone;
use engine::types::ability::{
    AbilityCost, AbilityKind, ContinuousModification, Effect, FilterProp, ManaProduction,
    StaticDefinition, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, LayersDirty, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolved_commands::{
    ResolvedCombatMembershipCommand, ResolvedCombatMembershipEdit, ResolvedRulesCommand,
};
use engine::types::zones::Zone;

use super::rules::AttackTarget;

/// Convenience constants for the third+ players (the scenario module only
/// exports `P0`/`P1`).
const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);
const P4: PlayerId = PlayerId(4);
const P5: PlayerId = PlayerId(5);

// ─── Oracle text constants ───────────────────────────────────────────────────

const CURSE_OF_BOUNTY_ORACLE: &str = "Enchant player\n\
     Whenever enchanted player is attacked, untap all nonland permanents you control. \
     Each opponent attacking that player untaps all nonland permanents they control.";

const CURSE_OF_DISTURBANCE_ORACLE: &str = "Enchant player\n\
     Whenever enchanted player is attacked, create a 2/2 black Zombie creature token. \
     Each opponent attacking that player does the same.";

const CURSE_OF_OPULENCE_ORACLE: &str = "Enchant player\n\
     Whenever enchanted player is attacked, create a Gold token. \
     Each opponent attacking that player does the same. \
     (A Gold token is an artifact with \"Sacrifice this token: Add one mana of any color.\")";

const CURSE_OF_VERBOSITY_ORACLE: &str = "Enchant player\n\
     Whenever enchanted player is attacked, draw a card. \
     Each opponent attacking that player draws a card.";

const CURSE_OF_VITALITY_ORACLE: &str = "Enchant player\n\
     Whenever enchanted player is attacked, you gain 2 life. \
     Each opponent attacking that player gains 2 life.";

/// Verbatim Oracle text (Scryfall), same constant as `derived_target_carriers.rs`.
const ACT_OF_AGGRESSION_ORACLE: &str = "({R/P} can be paid with either {R} or 2 life.)\nGain control of target creature an opponent controls until end of turn. Untap that creature. It gains haste until end of turn.";

/// The genuine "does the same" phrasing (Curse of Vitality's printed rider) —
/// exercises the `try_parse_scoped_does_the_same` fan-out path this PR fixes,
/// as opposed to the explicit-verb `CURSE_OF_VITALITY_ORACLE` above.
const CURSE_OF_VITALITY_DOES_THE_SAME_ORACLE: &str = "Enchant player\n\
     Whenever enchanted player is attacked, you gain 2 life. \
     Each opponent attacking that player does the same.";

// ─── Shared helpers ──────────────────────────────────────────────────────────

/// Set up a scenario with a curse attached to P1 (enchanted player), controlled
/// by P0. P0 has a creature to attack with. Returns `(runner, curse_id, attacker_id)`.
fn setup_curse(oracle: &str, name: &str) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let curse_id = {
        let mut builder = scenario.add_creature(P0, name, 0, 0);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.from_oracle_text(oracle);
        builder.id()
    };

    let attacker_id = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    // Library padding so advance_until_stack_empty doesn't deck anyone.
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();
    attach_to_player(runner.state_mut(), curse_id, P1);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse_id);

    (runner, curse_id, attacker_id)
}

/// Count triggered abilities on the stack sourced from `source`.
fn stack_triggers_from(runner: &GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|e| e.source_id == source)
        .count()
}

/// Declare P0's creature as attacking P1 (the enchanted player).
fn attack_enchanted_player(runner: &mut GameRunner, attacker: ObjectId) {
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("DeclareAttackers should succeed");
}

// ─── Curse of Vitality ───────────────────────────────────────────────────────

/// CR 508.3b: Trigger fires when enchanted player is attacked; curse controller
/// gains 2 life.
#[test]
fn curse_of_vitality_fires_and_gains_life() {
    let (mut runner, curse_id, attacker) =
        setup_curse(CURSE_OF_VITALITY_ORACLE, "Curse of Vitality");

    let life_before = runner.life(P0);
    attack_enchanted_player(&mut runner, attacker);

    assert!(
        stack_triggers_from(&runner, curse_id) >= 1,
        "Curse of Vitality must trigger when enchanted player is attacked"
    );

    runner.advance_until_stack_empty();

    // In a 2-player game, P0 is both the curse controller ("you gain 2 life")
    // and the only opponent attacking that player ("each opponent attacking that
    // player gains 2 life"), so P0 gains 2 + 2 = 4 life total.
    let life_after = runner.life(P0);
    assert!(
        life_after > life_before,
        "Curse of Vitality: P0 must gain life (before={life_before}, after={life_after})"
    );
}

/// CR 508.3b: Trigger does NOT fire when a non-enchanted player is attacked.
#[test]
fn curse_of_vitality_does_not_fire_when_non_enchanted_player_attacked() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let curse_id = {
        let mut builder = scenario.add_creature(P0, "Curse of Vitality", 0, 0);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.from_oracle_text(CURSE_OF_VITALITY_ORACLE);
        builder.id()
    };

    let attacker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();
    // Attach curse to P1 — so P1 is the enchanted player.
    attach_to_player(runner.state_mut(), curse_id, P1);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse_id);

    // P1 attacks P0 (NOT the enchanted player).
    runner.state_mut().active_player = P1;
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P0))])
        .expect("DeclareAttackers should succeed");

    assert_eq!(
        stack_triggers_from(&runner, curse_id),
        0,
        "Curse of Vitality must NOT trigger when non-enchanted player (P0) is attacked"
    );
}

/// CR 508.6 + CR 102.2: In a TWO-PLAYER game the "Each opponent attacking that
/// player does the same." rider is a NO-OP. The curse controller's only
/// opponent is the enchanted defending player (P1), and per CR 508.6 P1 is not
/// "attacking that player" — a player cannot attack themselves — so P1 must NOT
/// gain life from the rider. Only the base "you gain 2 life" fires, for the
/// controller (P0).
///
/// Fail-on-revert: this is the exact test that previously ENCODED the bug — it
/// asserted P1 (the defender) gained life under the old blanket `Opponent`
/// scope. The scoped `OpponentAttackingEnchantedPlayer` fix makes P1's life
/// unchanged; a regression back to the blanket scope re-inflates P1's life and
/// fails here.
#[test]
fn curse_of_vitality_does_the_same_rider_is_noop_for_two_player_defender() {
    let (mut runner, curse_id, attacker) =
        setup_curse(CURSE_OF_VITALITY_DOES_THE_SAME_ORACLE, "Curse of Vitality");

    let p0_before = runner.life(P0);
    let p1_before = runner.life(P1);
    attack_enchanted_player(&mut runner, attacker);

    assert!(
        stack_triggers_from(&runner, curse_id) >= 1,
        "Curse of Vitality must trigger when enchanted player is attacked"
    );

    runner.advance_until_stack_empty();

    // Base "you gain 2 life" → the curse controller (P0).
    assert_eq!(
        runner.life(P0),
        p0_before + 2,
        "controller must gain exactly 2 life from the base effect"
    );
    // Rider fans out over `OpponentAttackingEnchantedPlayer`. P0's only opponent
    // is the enchanted defender P1, who is not attacking themselves (CR 508.6),
    // so the rider affects nobody. P1's life must be UNCHANGED (the old blanket
    // `Opponent` scope wrongly gave P1 +2 here — the bug this PR fixes).
    assert_eq!(
        runner.life(P1),
        p1_before,
        "the enchanted defending player must NOT gain life from the rider in a \
         two-player game (they are not attacking that player)"
    );
}

// ─── Curse of Verbosity ──────────────────────────────────────────────────────

/// CR 508.3b: Trigger fires when enchanted player is attacked; curse controller
/// draws a card.
#[test]
fn curse_of_verbosity_fires_and_draws_card() {
    let (mut runner, curse_id, attacker) =
        setup_curse(CURSE_OF_VERBOSITY_ORACLE, "Curse of Verbosity");

    let hand_before = runner.state().players[P0.0 as usize].hand.len();
    attack_enchanted_player(&mut runner, attacker);

    assert!(
        stack_triggers_from(&runner, curse_id) >= 1,
        "Curse of Verbosity must trigger when enchanted player is attacked"
    );

    runner.advance_until_stack_empty();

    let hand_after = runner.state().players[P0.0 as usize].hand.len();
    assert!(
        hand_after > hand_before,
        "Curse of Verbosity: P0 must draw cards (before={hand_before}, after={hand_after})"
    );
}

// ─── Curse of Disturbance ────────────────────────────────────────────────────

/// CR 508.3b: Trigger fires when enchanted player is attacked; curse controller
/// creates a 2/2 black Zombie creature token.
#[test]
fn curse_of_disturbance_fires_and_creates_zombie_token() {
    let (mut runner, curse_id, attacker) =
        setup_curse(CURSE_OF_DISTURBANCE_ORACLE, "Curse of Disturbance");

    attack_enchanted_player(&mut runner, attacker);

    assert!(
        stack_triggers_from(&runner, curse_id) >= 1,
        "Curse of Disturbance must trigger when enchanted player is attacked"
    );

    runner.advance_until_stack_empty();

    // P0 should have at least one 2/2 black Zombie token.
    let zombie_count = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner.state().objects.get(id).is_some_and(|obj| {
                obj.zone == Zone::Battlefield
                    && obj.controller == P0
                    && obj.is_token
                    && obj.power == Some(2)
                    && obj.toughness == Some(2)
                    && obj.card_types.subtypes.iter().any(|s| s == "Zombie")
                    && obj.color.contains(&ManaColor::Black)
            })
        })
        .count();

    assert!(
        zombie_count >= 1,
        "Curse of Disturbance: P0 must have at least one 2/2 black Zombie token, got {zombie_count}"
    );
}

// ─── Curse of Opulence ───────────────────────────────────────────────────────

/// Gold tokens on the battlefield that `player` both owns and controls.
fn gold_tokens(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            runner.state().objects.get(id).is_some_and(|obj| {
                obj.zone == Zone::Battlefield
                    && obj.is_token
                    && obj.owner == player
                    && obj.controller == player
                    && obj.card_types.subtypes.iter().any(|s| s == "Gold")
            })
        })
        .collect()
}

/// CR 508.3b + CR 111.10c: Trigger fires when enchanted player is attacked; the
/// curse controller creates exactly one Gold token ("Sacrifice this token: Add
/// one mana of any color."). In a two-player game the rider is a no-op: the
/// controller's only opponent is the enchanted player, who is not attacking.
#[test]
fn curse_of_opulence_fires_and_creates_gold_token() {
    let (mut runner, curse_id, attacker) =
        setup_curse(CURSE_OF_OPULENCE_ORACLE, "Curse of Opulence");

    attack_enchanted_player(&mut runner, attacker);

    assert!(
        stack_triggers_from(&runner, curse_id) >= 1,
        "Curse of Opulence must trigger when enchanted player is attacked"
    );

    runner.advance_until_stack_empty();

    let p0_gold = gold_tokens(&runner, P0);
    assert_eq!(
        p0_gold.len(),
        1,
        "Curse of Opulence: P0 must have exactly one Gold token"
    );
    let gold = &runner.state().objects[&p0_gold[0]];
    assert!(
        gold.abilities.iter().any(|ability| {
            ability.kind == AbilityKind::Activated
                && matches!(
                    *ability.effect,
                    Effect::Mana {
                        produced: ManaProduction::AnyOneColor { .. },
                        ..
                    }
                )
                && matches!(ability.cost, Some(AbilityCost::Sacrifice(_)))
        }),
        "the Gold token must have \"Sacrifice this token: Add one mana of any color.\""
    );
    assert!(
        gold_tokens(&runner, P1).is_empty(),
        "the enchanted player P1 is not attacking and gets no Gold"
    );
}

// ─── Curse of Opulence: multiplayer "is attacking" (CR 508.6) ────────────────

/// Build an `n`-player table with Curse of Opulence controlled by P0 and
/// enchanting `enchanted`, plus one creature for each entry of `creatures`.
/// Returns `(runner, curse_id, creature_ids)` with ids in `creatures` order.
fn opulence_table(
    n: u8,
    enchanted: PlayerId,
    creatures: &[PlayerId],
) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let (scenario, curse_id, creature_ids) = opulence_scenario(n, creatures);
    let runner = finish_opulence(scenario, curse_id, enchanted);
    (runner, curse_id, creature_ids)
}

/// The unbuilt half of [`opulence_table`]: the scenario with the curse (not yet
/// attached), the creatures and library padding, so a caller can still add
/// cards before building.
fn opulence_scenario(n: u8, creatures: &[PlayerId]) -> (GameScenario, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new_n_player(n, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let curse_id = {
        let mut builder = scenario.add_creature(P0, "Curse of Opulence", 0, 0);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.from_oracle_text(CURSE_OF_OPULENCE_ORACLE);
        builder.id()
    };

    let creature_ids = creatures
        .iter()
        .map(|&player| scenario.add_creature(player, "Grizzly Bears", 2, 2).id())
        .collect();

    for pid in 0..n {
        for _ in 0..10 {
            scenario.add_card_to_library_top(PlayerId(pid), "Plains");
        }
    }

    (scenario, curse_id, creature_ids)
}

/// The built half of [`opulence_table`]: build, attach the curse to
/// `enchanted` (CR 303.4b) and index its trigger.
fn finish_opulence(scenario: GameScenario, curse_id: ObjectId, enchanted: PlayerId) -> GameRunner {
    let mut runner = scenario.build();
    attach_to_player(runner.state_mut(), curse_id, enchanted);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse_id);
    runner
}

/// CR 508.6 + CR 506.4: an opponent whose attacker is removed from combat
/// before the trigger resolves is no longer "attacking that player" and gets no
/// Gold. The controller still gets the base Gold.
///
/// Revert-failing: the old declaration-ledger read still counted P2.
#[test]
fn curse_of_opulence_attacker_removed_from_combat_gets_no_gold() {
    let (mut runner, curse_id, creatures) = opulence_table(4, P1, &[P2]);
    let p2_attacker = creatures[0];

    hand_turn_to(&mut runner, P2);
    runner
        .declare_attackers(&[(p2_attacker, AttackTarget::Player(P1))])
        .expect("P2 declares an attacker against the enchanted player P1");
    assert!(
        stack_triggers_from(&runner, curse_id) >= 1,
        "curse must trigger when the enchanted player P1 is attacked"
    );

    remove_object_from_combat(runner.state_mut(), p2_attacker);
    runner.advance_until_stack_empty();

    assert_eq!(
        gold_tokens(&runner, P0).len(),
        1,
        "controller P0 gets exactly one Gold (the trigger resolved)"
    );
    assert!(
        gold_tokens(&runner, P2).is_empty(),
        "P2's attacker was removed from combat: P2 is not attacking P1"
    );
    assert!(
        gold_tokens(&runner, P1).is_empty(),
        "the enchanted player gets no Gold"
    );
    assert!(
        gold_tokens(&runner, P3).is_empty(),
        "a non-attacking opponent gets no Gold"
    );
}

/// CR 508.6 + CR 603.2c: an opponent with two attackers, one of them removed
/// from combat, is still attacking that player and gets exactly one Gold.
///
/// Regression guard (not revert-failing): the old ledger also yields one Gold
/// here; this pins that the live predicate neither doubles nor drops a
/// surviving attacker.
#[test]
fn curse_of_opulence_surviving_attacker_gets_exactly_one_gold() {
    let (mut runner, curse_id, creatures) = opulence_table(4, P1, &[P2, P2]);

    hand_turn_to(&mut runner, P2);
    runner
        .declare_attackers(&[
            (creatures[0], AttackTarget::Player(P1)),
            (creatures[1], AttackTarget::Player(P1)),
        ])
        .expect("P2 declares two attackers against the enchanted player P1");
    assert!(
        stack_triggers_from(&runner, curse_id) >= 1,
        "curse must trigger when the enchanted player P1 is attacked"
    );

    remove_object_from_combat(runner.state_mut(), creatures[0]);
    runner.advance_until_stack_empty();

    assert_eq!(
        gold_tokens(&runner, P2).len(),
        1,
        "P2 is still attacking P1 and gets exactly one Gold"
    );
    assert_eq!(
        gold_tokens(&runner, P0).len(),
        1,
        "controller P0 gets exactly one Gold"
    );
    assert!(
        gold_tokens(&runner, P1).is_empty(),
        "enchanted P1 gets no Gold"
    );
    assert!(
        gold_tokens(&runner, P3).is_empty(),
        "non-attacking P3 gets no Gold"
    );
}

/// CR 508.6: the rider is set-valued over the opponents attacking that player
/// when it resolves. P2 attacks P1 and P3 is also attacking P1; P2's attacker
/// is then removed from combat. Only P3 is still attacking, so only P3 (and the
/// controller) get Gold.
///
/// Revert-failing: the old ledger read gave P2 a Gold (declared, then removed)
/// and gave P3 none (never written to the ledger).
#[test]
fn curse_of_opulence_gold_goes_only_to_opponents_still_attacking() {
    let (mut runner, curse_id, creatures) = opulence_table(5, P1, &[P2, P3, P4]);
    let (p2_attacker, p3_attacker) = (creatures[0], creatures[1]);

    hand_turn_to(&mut runner, P2);
    runner
        .declare_attackers(&[(p2_attacker, AttackTarget::Player(P1))])
        .expect("P2 declares an attacker against the enchanted player P1");
    assert!(
        stack_triggers_from(&runner, curse_id) >= 1,
        "curse must trigger when the enchanted player P1 is attacked"
    );

    // A second attacking controller: P3's creature is attacking P1 as well
    // (only the active player declares, so it is placed in combat directly).
    runner
        .state_mut()
        .combat
        .as_mut()
        .expect("combat is active after declaring attackers")
        .attackers
        .push(AttackerInfo::attacking_player(p3_attacker, P1));
    remove_object_from_combat(runner.state_mut(), p2_attacker);

    runner.advance_until_stack_empty();

    assert_eq!(
        gold_tokens(&runner, P3).len(),
        1,
        "P3 is still attacking P1 and gets exactly one Gold"
    );
    assert!(
        gold_tokens(&runner, P2).is_empty(),
        "P2's attacker was removed from combat: P2 gets no Gold"
    );
    assert_eq!(
        gold_tokens(&runner, P0).len(),
        1,
        "controller P0 gets exactly one Gold"
    );
    assert!(
        gold_tokens(&runner, P1).is_empty(),
        "enchanted P1 gets no Gold"
    );
    assert!(
        gold_tokens(&runner, P4).is_empty(),
        "idle opponent P4 gets no Gold"
    );
}

/// Declare P2's attacker at `target` on a 4-player Opulence table where P1
/// (the enchanted player) controls a planeswalker. Returns the runner, the
/// curse, and the planeswalker.
fn opulence_table_with_enchanted_planeswalker(
    target: impl FnOnce(ObjectId) -> AttackTarget,
) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let curse_id = {
        let mut builder = scenario.add_creature(P0, "Curse of Opulence", 0, 0);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.from_oracle_text(CURSE_OF_OPULENCE_ORACLE);
        builder.id()
    };
    let walker = scenario
        .add_planeswalker_from_oracle(P1, "Test Walker", "Test", 3, "+1: You gain 1 life.")
        .id();
    let p2_attacker = scenario.add_creature(P2, "Grizzly Bears", 2, 2).id();

    for pid in 0..4u8 {
        for _ in 0..10 {
            scenario.add_card_to_library_top(PlayerId(pid), "Plains");
        }
    }

    let mut runner = scenario.build();
    attach_to_player(runner.state_mut(), curse_id, P1);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse_id);

    hand_turn_to(&mut runner, P2);
    runner
        .declare_attackers(&[(p2_attacker, target(walker))])
        .expect("P2 declares its attacker");
    (runner, curse_id)
}

/// CR 508.3b: attacking only the enchanted player's planeswalker does not
/// trigger "whenever enchanted player is attacked", so nobody gets Gold. The
/// same table with the creature attacking P1 directly triggers and awards Gold.
#[test]
fn curse_of_opulence_planeswalker_only_attack_does_not_trigger() {
    let (mut runner, curse_id) =
        opulence_table_with_enchanted_planeswalker(AttackTarget::Planeswalker);
    assert_eq!(
        stack_triggers_from(&runner, curse_id),
        0,
        "attacking P1's planeswalker is not attacking P1 (CR 508.3b)"
    );
    runner.advance_until_stack_empty();
    for player in [P0, P1, P2, P3] {
        assert!(
            gold_tokens(&runner, player).is_empty(),
            "no Gold for {player:?} without a trigger"
        );
    }

    // Reach-guard: the same table attacking P1 directly triggers.
    let (mut runner, curse_id) =
        opulence_table_with_enchanted_planeswalker(|_| AttackTarget::Player(P1));
    assert!(
        stack_triggers_from(&runner, curse_id) >= 1,
        "attacking P1 directly triggers the curse"
    );
    runner.advance_until_stack_empty();
    assert_eq!(gold_tokens(&runner, P0).len(), 1, "controller gets Gold");
    assert_eq!(
        gold_tokens(&runner, P2).len(),
        1,
        "P2 is attacking P1 and gets Gold"
    );
}

/// Ruling: a player may enchant themselves with the curse. P0 enchants P0 and
/// P1 attacks P0: P0 gets the base Gold and P1, an opponent attacking that
/// player, gets the rider's Gold.
#[test]
fn curse_of_opulence_self_enchanted_controller_and_attacker_each_get_gold() {
    let (mut runner, curse_id, creatures) = opulence_table(2, P0, &[P1]);

    hand_turn_to(&mut runner, P1);
    runner
        .declare_attackers(&[(creatures[0], AttackTarget::Player(P0))])
        .expect("P1 declares an attacker against the enchanted player P0");
    assert!(
        stack_triggers_from(&runner, curse_id) >= 1,
        "curse must trigger when the enchanted player P0 is attacked"
    );

    runner.advance_until_stack_empty();

    assert_eq!(
        gold_tokens(&runner, P0).len(),
        1,
        "controller P0 gets exactly one Gold (base effect)"
    );
    assert_eq!(
        gold_tokens(&runner, P1).len(),
        1,
        "P1 is an opponent attacking P0 and gets exactly one Gold (rider)"
    );
}

/// CR 608.2h + CR 303.4b: if the curse leaves the battlefield before its
/// trigger resolves, "that player" is still the player it enchanted (last
/// known information), so the controller and the attacking opponent still get
/// Gold.
#[test]
fn curse_of_opulence_resolves_after_curse_leaves_battlefield() {
    let (mut runner, curse_id, creatures) = opulence_table(4, P1, &[P2]);

    hand_turn_to(&mut runner, P2);
    runner
        .declare_attackers(&[(creatures[0], AttackTarget::Player(P1))])
        .expect("P2 declares an attacker against the enchanted player P1");
    assert!(
        stack_triggers_from(&runner, curse_id) >= 1,
        "curse must trigger when the enchanted player P1 is attacked"
    );

    // Reach guard: the live curse enchants P1 before it leaves.
    assert_eq!(
        runner.state().objects[&curse_id].attached_to,
        Some(AttachTarget::Player(P1)),
        "before the move the curse is attached to the enchanted player P1"
    );

    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), curse_id, Zone::Graveyard, &mut events);

    // The live attachment is severed on leaving the battlefield, so "that
    // player" can only come from last known information: the zone-change
    // record keeps the enchanted player, and that record is the anchor the
    // resolution reads (`zone_changes_this_turn`).
    let curse = &runner.state().objects[&curse_id];
    assert_eq!(curse.zone, Zone::Graveyard);
    assert!(
        curse.attached_to.is_none(),
        "the curse in the graveyard is no longer attached to anything"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged { object_id, record, .. }
                if *object_id == curse_id
                    && record.attached_to == Some(AttachTarget::Player(P1))
        )),
        "the curse's zone-change event records the enchanted player P1"
    );
    assert_eq!(
        runner
            .state()
            .zone_changes_this_turn
            .iter()
            .rev()
            .find(|record| record.object_id == curse_id
                && record.from_zone == Some(Zone::Battlefield))
            .and_then(|record| record.attached_to),
        Some(AttachTarget::Player(P1)),
        "the last-known-information record names the enchanted player P1"
    );

    runner.advance_until_stack_empty();

    assert_eq!(
        gold_tokens(&runner, P0).len(),
        1,
        "controller P0 gets exactly one Gold"
    );
    assert_eq!(
        gold_tokens(&runner, P2).len(),
        1,
        "attacking opponent P2 gets exactly one Gold"
    );
    assert!(
        gold_tokens(&runner, P1).is_empty(),
        "enchanted P1 gets no Gold"
    );
    assert!(
        gold_tokens(&runner, P3).is_empty(),
        "non-attacking P3 gets no Gold"
    );
}

// ─── Curse of Opulence: control change removes from combat (CR 506.4) ────────

/// The 4-player Opulence table (curse controlled by P0, enchanting P1) with a
/// free Act of Aggression in P3's hand. Returns `(runner, curse_id,
/// creature_ids, act_id)`.
fn opulence_table_with_act(
    creatures: &[PlayerId],
) -> (GameRunner, ObjectId, Vec<ObjectId>, ObjectId) {
    let (mut scenario, curse_id, creature_ids) = opulence_scenario(4, creatures);
    let act = scenario
        .add_spell_to_hand_from_oracle(P3, "Act of Aggression", true, ACT_OF_AGGRESSION_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let runner = finish_opulence(scenario, curse_id, P1);
    (runner, curse_id, creature_ids, act)
}

/// Pass priority until `player` holds it.
fn pass_priority_to(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..8 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { player: p } if p == player) {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority toward the caster");
    }
    panic!("{player:?} never received priority");
}

/// `caster` casts Act of Aggression targeting `target` through the real cast
/// pipeline, then every player passes until the spell (and only it) resolves.
fn cast_act_of_aggression(
    runner: &mut GameRunner,
    act: ObjectId,
    caster: PlayerId,
    target: ObjectId,
) {
    pass_priority_to(runner, caster);
    runner
        .act(GameAction::CastSpell {
            object_id: act,
            card_id: runner.state().objects[&act].card_id,
            targets: vec![],
            payment_mode: Default::default(),
        })
        .expect("cast Act of Aggression");
    // With several legal targets the caster chooses; with exactly one the
    // engine announces it directly. Either way the stack entry is checked below.
    if let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for {
        assert!(
            target_slots[0]
                .legal_targets
                .contains(&TargetRef::Object(target)),
            "the creature is a legal Act of Aggression target"
        );
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .expect("choose the Act of Aggression target");
    }
    let announced = runner
        .state()
        .stack
        .last()
        .and_then(|entry| entry.ability())
        .expect("reach guard: Act of Aggression is on the stack");
    assert_eq!(announced.source_id, act, "reach guard: Act is on top");
    assert_eq!(
        announced.targets,
        vec![TargetRef::Object(target)],
        "reach guard: Act of Aggression targets the intended creature"
    );

    let depth = runner.state().stack.len();
    for _ in 0..8 {
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to resolve Act of Aggression");
        if runner.state().stack.len() < depth {
            return;
        }
    }
    panic!("Act of Aggression never resolved");
}

/// The attack target of `creature` while it is an attacking creature.
fn attack_target_of(runner: &GameRunner, creature: ObjectId) -> Option<AttackTarget> {
    runner.state().combat.as_ref().and_then(|combat| {
        combat
            .attackers
            .iter()
            .find(|info| info.object_id == creature)
            .map(|info| info.attack_target)
    })
}

/// CR 506.4 + CR 508.6: P2 attacks the enchanted player P1 and the curse
/// triggers; P3 responds with Act of Aggression on the attacker. When the
/// spell resolves the creature's controller changes, so it is removed from
/// combat: when the curse trigger then resolves neither P2 (controls no
/// attacking creature) nor P3 (controls the creature, but it is not attacking)
/// is attacking P1. Only the controller P0 gets Gold.
///
/// Revert-failing: without the CR 506.4 control-change removal the creature
/// stays listed as attacking P1 under its new controller P3, who gets a Gold.
#[test]
fn curse_of_opulence_attacker_stolen_by_responder_is_not_attacking() {
    let (mut runner, curse_id, creatures, act) = opulence_table_with_act(&[P2]);
    let stolen = creatures[0];

    hand_turn_to(&mut runner, P2);
    runner
        .declare_attackers(&[(stolen, AttackTarget::Player(P1))])
        .expect("P2 declares an attacker against the enchanted player P1");
    assert!(
        stack_triggers_from(&runner, curse_id) >= 1,
        "reach guard: the curse triggers when the enchanted player P1 is attacked"
    );
    assert_eq!(
        attack_target_of(&runner, stolen),
        Some(AttackTarget::Player(P1)),
        "reach guard: the creature is attacking P1"
    );
    assert_eq!(runner.state().objects[&stolen].controller, P2);

    cast_act_of_aggression(&mut runner, act, P3, stolen);

    assert!(
        stack_triggers_from(&runner, curse_id) >= 1,
        "reach guard: Act resolved above the still-waiting curse trigger"
    );
    let creature = &runner.state().objects[&stolen];
    assert_eq!(creature.zone, Zone::Battlefield);
    assert_eq!(
        creature.controller, P3,
        "Act of Aggression transferred control"
    );
    assert_eq!(creature.owner, P2);
    assert_eq!(
        attack_target_of(&runner, stolen),
        None,
        "CR 506.4: a creature whose controller changes is removed from combat"
    );
    // CR 508.6: "has attacked" is declaration history and is unaffected.
    assert!(runner.state().player_attacked_player_this_combat(P2, P1));
    assert!(!runner.state().player_attacked_player_this_combat(P3, P1));

    runner.advance_until_stack_empty();

    assert_eq!(
        gold_tokens(&runner, P0).len(),
        1,
        "controller P0 gets exactly one Gold (the trigger resolved)"
    );
    assert!(
        gold_tokens(&runner, P3).is_empty(),
        "P3 controls the creature, but it is not attacking: no Gold"
    );
    assert!(
        gold_tokens(&runner, P2).is_empty(),
        "P2 no longer controls an attacking creature: no Gold"
    );
    assert!(
        gold_tokens(&runner, P1).is_empty(),
        "enchanted P1 gets no Gold"
    );
}

/// CR 506.4 removes only the permanent whose controller changed. P3 steals
/// P2's creature that is NOT attacking; P2's attacker stays in combat and P2
/// still gets its Gold.
///
/// Paired control (not revert-failing): unchanged control of the attacker
/// prunes nothing.
#[test]
fn curse_of_opulence_stealing_a_non_attacker_leaves_the_attack_intact() {
    let (mut runner, curse_id, creatures, act) = opulence_table_with_act(&[P2, P2]);
    let (attacker, idle) = (creatures[0], creatures[1]);

    hand_turn_to(&mut runner, P2);
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("P2 declares one attacker against the enchanted player P1");
    assert!(
        stack_triggers_from(&runner, curse_id) >= 1,
        "reach guard: the curse triggers"
    );

    cast_act_of_aggression(&mut runner, act, P3, idle);

    assert_eq!(
        runner.state().objects[&idle].controller,
        P3,
        "reach guard: the control change happened"
    );
    assert_eq!(attack_target_of(&runner, idle), None);
    assert_eq!(
        attack_target_of(&runner, attacker),
        Some(AttackTarget::Player(P1)),
        "P2's attacker keeps attacking P1"
    );

    runner.advance_until_stack_empty();

    assert_eq!(
        gold_tokens(&runner, P2).len(),
        1,
        "P2 is still attacking P1 and gets exactly one Gold"
    );
    assert!(
        gold_tokens(&runner, P3).is_empty(),
        "P3's stolen creature never attacked: no Gold"
    );
    assert_eq!(gold_tokens(&runner, P0).len(), 1, "controller gets Gold");
}

/// CR 506.4: P2 attacks P1 with two creatures and P3 steals one of them. The
/// stolen one leaves combat; the other keeps attacking, so P2 still gets
/// exactly one Gold and P3 gets none.
///
/// Revert-failing: without the removal P3 gets a Gold for the stolen creature.
#[test]
fn curse_of_opulence_other_attacker_survives_when_sibling_is_stolen() {
    let (mut runner, curse_id, creatures, act) = opulence_table_with_act(&[P2, P2]);
    let (stolen, survivor) = (creatures[0], creatures[1]);

    hand_turn_to(&mut runner, P2);
    runner
        .declare_attackers(&[
            (stolen, AttackTarget::Player(P1)),
            (survivor, AttackTarget::Player(P1)),
        ])
        .expect("P2 declares two attackers against the enchanted player P1");
    assert!(
        stack_triggers_from(&runner, curse_id) >= 1,
        "reach guard: the curse triggers"
    );
    let survivor_before = runner
        .state()
        .combat
        .as_ref()
        .and_then(|combat| {
            combat
                .attackers
                .iter()
                .find(|info| info.object_id == survivor)
                .cloned()
        })
        .expect("reach guard: the survivor is attacking");

    cast_act_of_aggression(&mut runner, act, P3, stolen);

    assert_eq!(
        runner.state().objects[&stolen].controller,
        P3,
        "reach guard: the control change happened"
    );
    assert_eq!(
        attack_target_of(&runner, stolen),
        None,
        "CR 506.4: the stolen creature is removed from combat"
    );
    assert_eq!(
        runner.state().combat.as_ref().and_then(|combat| combat
            .attackers
            .iter()
            .find(|info| info.object_id == survivor)
            .cloned()),
        Some(survivor_before),
        "the sibling attacker's combat entry is untouched"
    );

    runner.advance_until_stack_empty();

    assert_eq!(
        gold_tokens(&runner, P2).len(),
        1,
        "P2 is still attacking P1 with the survivor: exactly one Gold"
    );
    assert!(
        gold_tokens(&runner, P3).is_empty(),
        "P3's stolen creature is not attacking: no Gold"
    );
    assert_eq!(gold_tokens(&runner, P0).len(), 1, "controller gets Gold");
}

/// CR 506.4 removes a creature only when its controller changes while it is
/// in combat. P3 steals P2's creature in its main phase (Act grants haste) and
/// then attacks P1 with it: the creature joins combat under P3, later layer
/// passes see no control change, and P3 is the opponent attacking P1.
///
/// Paired positive control: the live-controller read still attributes a
/// stolen creature that attacks for its new controller.
#[test]
fn curse_of_opulence_creature_stolen_before_attack_attacks_for_new_controller() {
    let (mut runner, curse_id, creatures, act) = opulence_table_with_act(&[P2]);
    let stolen = creatures[0];

    {
        let state = runner.state_mut();
        state.active_player = P3;
        state.priority_player = P3;
        state.waiting_for = WaitingFor::Priority { player: P3 };
    }
    cast_act_of_aggression(&mut runner, act, P3, stolen);
    assert_eq!(
        runner.state().objects[&stolen].controller,
        P3,
        "reach guard: P3 controls the creature before attacking"
    );

    hand_turn_to(&mut runner, P3);
    runner
        .declare_attackers(&[(stolen, AttackTarget::Player(P1))])
        .expect("P3 attacks P1 with the hasty stolen creature");
    assert!(
        stack_triggers_from(&runner, curse_id) >= 1,
        "reach guard: the curse triggers"
    );

    runner.advance_until_stack_empty();

    assert_eq!(
        attack_target_of(&runner, stolen),
        Some(AttackTarget::Player(P1)),
        "the creature is still attacking P1 after the trigger resolved"
    );
    assert_eq!(runner.state().objects[&stolen].controller, P3);
    assert_eq!(
        gold_tokens(&runner, P3).len(),
        1,
        "P3 is attacking P1 and gets exactly one Gold"
    );
    assert!(
        gold_tokens(&runner, P2).is_empty(),
        "P2 owns the creature but is not attacking: no Gold"
    );
    assert_eq!(gold_tokens(&runner, P0).len(), 1, "controller gets Gold");
}

// ─── Blocking-creature membership survives its attacker (CR 509.1g) ─────────

/// Verbatim Oracle text (Scryfall / MTGJSON).
const INTREPID_ACE_ORACLE: &str =
    "This creature gets +2/+0 as long as it isn't attacking or blocking.";

/// Controller-agnostic Layer 6 grant keyed on `FilterProp::Blocking`:
/// "blocking creatures have deathtouch". Built typed because the static parser
/// reads a bare "Blocking creatures have …" subject as a creature subtype, and
/// no printed card grants a keyword to blocking creatures this way.
fn blocking_deathtouch() -> StaticDefinition {
    StaticDefinition::continuous()
        .affected(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::Blocking]),
        ))
        .modifications(vec![ContinuousModification::AddKeyword {
            keyword: Keyword::Deathtouch,
        }])
}

/// The 4-player Opulence table (curse controlled by P0, enchanting P1) with
/// one creature per entry of `attackers`, P1's Intrepid Ace, a P0 enchantment
/// granting deathtouch to blocking creatures, and a free Act of Aggression in
/// `act_holder`'s hand. Returns `(runner, curse_id, creature_ids, ace, act)`.
fn opulence_table_with_ace_and_act(
    attackers: &[PlayerId],
    act_holder: PlayerId,
) -> (GameRunner, ObjectId, Vec<ObjectId>, ObjectId, ObjectId) {
    let (mut scenario, curse_id, creature_ids) = opulence_scenario(4, attackers);
    let ace = scenario
        .add_creature_from_oracle(P1, "Intrepid Ace", 2, 1, INTREPID_ACE_ORACLE)
        .id();
    scenario
        .add_creature(P0, "Blocking Grant", 0, 0)
        .as_enchantment()
        .with_static_definition(blocking_deathtouch());
    let act = scenario
        .add_spell_to_hand_from_oracle(
            act_holder,
            "Act of Aggression",
            true,
            ACT_OF_AGGRESSION_ORACLE,
        )
        .with_mana_cost(ManaCost::zero())
        .id();
    let runner = finish_opulence(scenario, curse_id, P1);
    (runner, curse_id, creature_ids, ace, act)
}

fn power_of(runner: &GameRunner, id: ObjectId) -> Option<i32> {
    runner.state().objects[&id].power
}

fn has_deathtouch(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].has_keyword(&Keyword::Deathtouch)
}

/// The attackers `blocker` is blocking, `None` when it is not a blocking
/// creature (CR 509.1g).
fn blocking_list(runner: &GameRunner, blocker: ObjectId) -> Option<Vec<ObjectId>> {
    runner
        .state()
        .combat
        .as_ref()
        .and_then(|combat| combat.blocker_to_attacker.get(&blocker).cloned())
}

/// Combat-membership commands journaled since entry `start`.
fn combat_commands_since(
    runner: &GameRunner,
    start: usize,
) -> Vec<ResolvedCombatMembershipCommand> {
    runner
        .state()
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(start)
        .filter_map(|entry| match &entry.command {
            Some(ResolvedRulesCommand::CombatMembership(command)) => Some(command.clone()),
            _ => None,
        })
        .collect()
}

/// Replays `commands` onto the `before` snapshot and asserts the replayed
/// combat equals the live one.
fn assert_combat_replay(
    before: &GameState,
    runner: &GameRunner,
    commands: &[ResolvedCombatMembershipCommand],
) {
    let mut replay = before.clone();
    for command in commands {
        apply_resolved_combat_membership(&mut replay, command)
            .expect("the journaled removal replays against its predecessor");
    }
    assert_eq!(
        replay.combat,
        runner.state().combat,
        "replaying the journal reproduces the live combat"
    );
}

/// Drives the shared prelude: P0 declares `target` and `survivor` attacking
/// the enchanted P1, the curse trigger resolves, and P1 declares Intrepid Ace
/// blocking `target` only. Asserts the reach guards along the way.
fn ace_blocks_target(runner: &mut GameRunner, ace: ObjectId, target: ObjectId, survivor: ObjectId) {
    hand_turn_to(runner, P0);
    runner
        .declare_attackers(&[
            (target, AttackTarget::Player(P1)),
            (survivor, AttackTarget::Player(P1)),
        ])
        .expect("P0 declares two attackers against the enchanted player P1");
    runner.advance_until_stack_empty();
    for _ in 0..8 {
        if runner.waiting_for_kind() == "DeclareBlockers" {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority toward the declare-blockers step");
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::DeclareBlockers { player, .. } if player == P1),
        "reach guard: P1 declares blockers"
    );
    assert_eq!(
        power_of(runner, ace),
        Some(4),
        "reach guard: Intrepid Ace gets +2/+0 while not attacking or blocking"
    );
    assert!(!has_deathtouch(runner, ace));

    runner
        .declare_blockers(&[(ace, target)])
        .expect("P1 blocks the target with Intrepid Ace");

    assert_eq!(blocking_list(runner, ace), Some(vec![target]));
    assert_eq!(
        power_of(runner, ace),
        Some(2),
        "reach guard: a blocking Intrepid Ace loses its bonus"
    );
    assert!(
        has_deathtouch(runner, ace),
        "reach guard: the blocking grant reaches Intrepid Ace"
    );
}

/// CR 509.1g + CR 506.4 + CR 510.1d: P1's Intrepid Ace blocks one of P0's two
/// attackers; P1 then steals that attacker with Act of Aggression. The
/// attacker leaves combat (its controller changed), but Ace — whose own
/// control never changed — remains a blocking creature, now blocking no
/// creature: it keeps "isn't attacking or blocking" false (power 2) and the
/// Blocking grant, journals nothing, and assigns no combat damage.
///
/// Revert-failing: with the old prune the emptied key was dropped, so Ace read
/// as not blocking (power 4, no deathtouch); without the CR 510.1d guard the
/// damage step indexed Ace's empty attacker list.
#[test]
fn intrepid_ace_stays_a_blocker_when_its_attacker_is_stolen() {
    let (mut runner, _curse, creatures, ace, act) = opulence_table_with_ace_and_act(&[P0, P0], P1);
    let (target, survivor) = (creatures[0], creatures[1]);
    ace_blocks_target(&mut runner, ace, target, survivor);
    let before = runner.state().clone();
    let start = runner.state().resolved_rules_journal.entries().len();

    cast_act_of_aggression(&mut runner, act, P1, target);

    assert_eq!(runner.state().objects[&target].controller, P1);
    assert_eq!(
        attack_target_of(&runner, target),
        None,
        "CR 506.4: the stolen attacker is removed from combat"
    );
    assert_eq!(
        attack_target_of(&runner, survivor),
        Some(AttackTarget::Player(P1)),
        "the other attacker keeps attacking"
    );
    assert_eq!(runner.state().objects[&ace].controller, P1);
    assert_eq!(runner.state().objects[&ace].zone, Zone::Battlefield);
    assert_eq!(
        blocking_list(&runner, ace),
        Some(vec![]),
        "CR 509.1g: Intrepid Ace remains a blocking creature with no attacker assigned"
    );
    assert_eq!(
        power_of(&runner, ace),
        Some(2),
        "Intrepid Ace is still blocking, so it does not get +2/+0"
    );
    assert!(
        has_deathtouch(&runner, ace),
        "FilterProp::Blocking still matches Intrepid Ace"
    );
    assert_eq!(runner.state().layers_dirty, LayersDirty::Clean);

    let commands = combat_commands_since(&runner, start);
    let for_target: Vec<_> = commands
        .iter()
        .filter(|command| command.object.object_id == target)
        .collect();
    assert_eq!(for_target.len(), 1, "exactly one removal for the target");
    let ResolvedCombatMembershipEdit::Remove {
        expected_participation,
    } = &for_target[0].edit
    else {
        panic!("the target's command is a removal");
    };
    assert_eq!(expected_participation.blocked_by, vec![ace]);
    assert_eq!(expected_participation.blocking, None);
    assert!(
        commands
            .iter()
            .all(|command| command.object.object_id != ace),
        "Intrepid Ace was not removed, so nothing is journaled for it"
    );
    assert_combat_replay(&before, &runner, &commands);

    let life_before = runner.life(P1);
    let survivor_power = power_of(&runner, survivor).unwrap();
    let outcome = runner.combat_damage();
    assert_eq!(
        runner.life(P1),
        life_before - survivor_power,
        "reach guard: the damage step ran and the unblocked survivor hit P1"
    );
    let damage_sources: Vec<ObjectId> = outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::DamageDealt {
                source_id,
                is_combat: true,
                ..
            } => Some(*source_id),
            _ => None,
        })
        .collect();
    assert!(
        !damage_sources.contains(&ace),
        "CR 510.1d: a blocking creature blocking no creature assigns no combat damage"
    );
    assert_eq!(runner.state().objects[&ace].damage_marked, 0);
    assert_eq!(runner.state().objects[&target].damage_marked, 0);
}

/// CR 506.4 + CR 509.1h: the paired case — P3 steals Intrepid Ace itself while
/// it blocks. Ace's blocking membership ends (power 4, Blocking grant gone,
/// one removal journaled with its exact assignment), while the attacker it
/// blocked stays attacking and blocked, so it deals no combat damage.
#[test]
fn intrepid_ace_stolen_while_blocking_leaves_combat() {
    let (mut runner, _curse, creatures, ace, act) = opulence_table_with_ace_and_act(&[P0, P0], P3);
    let (target, survivor) = (creatures[0], creatures[1]);
    ace_blocks_target(&mut runner, ace, target, survivor);
    let before = runner.state().clone();
    let start = runner.state().resolved_rules_journal.entries().len();

    cast_act_of_aggression(&mut runner, act, P3, ace);

    assert_eq!(runner.state().objects[&ace].controller, P3);
    assert_eq!(
        blocking_list(&runner, ace),
        None,
        "CR 506.4: Intrepid Ace is removed from combat"
    );
    let combat = runner.state().combat.as_ref().unwrap();
    let info = combat
        .attackers
        .iter()
        .find(|info| info.object_id == target)
        .expect("the target keeps attacking");
    assert!(info.blocked, "CR 509.1h: the target remains blocked");
    assert!(combat.blocker_assignments[&target].is_empty());
    assert_eq!(
        power_of(&runner, ace),
        Some(4),
        "Intrepid Ace is no longer attacking or blocking: +2/+0"
    );
    assert!(
        !has_deathtouch(&runner, ace),
        "the Blocking grant no longer matches Intrepid Ace"
    );

    let commands = combat_commands_since(&runner, start);
    assert_eq!(commands.len(), 1, "exactly one combat removal");
    assert_eq!(commands[0].object.object_id, ace);
    let ResolvedCombatMembershipEdit::Remove {
        expected_participation,
    } = &commands[0].edit
    else {
        panic!("Intrepid Ace's command is a removal");
    };
    assert_eq!(expected_participation.blocking, Some(vec![target]));
    assert_combat_replay(&before, &runner, &commands);

    let life_before = runner.life(P1);
    let survivor_power = power_of(&runner, survivor).unwrap();
    let outcome = runner.combat_damage();
    assert_eq!(
        runner.life(P1),
        life_before - survivor_power,
        "reach guard: only the unblocked survivor hits P1"
    );
    let damage_sources: Vec<ObjectId> = outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::DamageDealt {
                source_id,
                is_combat: true,
                ..
            } => Some(*source_id),
            _ => None,
        })
        .collect();
    assert!(
        !damage_sources.contains(&target),
        "CR 510.1c: a blocked creature with no blocker assigns no combat damage"
    );
    assert!(!damage_sources.contains(&ace));
}

/// CR 506.4 removes only the permanent whose controller changed: P3 steals
/// the attacker Intrepid Ace is NOT blocking. Ace keeps blocking its attacker
/// (power 2) and nothing is journaled for it.
///
/// Paired control for the stolen-attacker case.
#[test]
fn intrepid_ace_unaffected_when_the_other_attacker_is_stolen() {
    let (mut runner, _curse, creatures, ace, act) = opulence_table_with_ace_and_act(&[P0, P0], P3);
    let (target, survivor) = (creatures[0], creatures[1]);
    ace_blocks_target(&mut runner, ace, target, survivor);
    let start = runner.state().resolved_rules_journal.entries().len();

    cast_act_of_aggression(&mut runner, act, P3, survivor);

    assert_eq!(
        runner.state().objects[&survivor].controller,
        P3,
        "reach guard: the control change happened"
    );
    assert_eq!(attack_target_of(&runner, survivor), None);
    assert_eq!(blocking_list(&runner, ace), Some(vec![target]));
    assert_eq!(power_of(&runner, ace), Some(2));
    assert!(has_deathtouch(&runner, ace));
    let commands = combat_commands_since(&runner, start);
    assert_eq!(commands.len(), 1, "only the stolen attacker is removed");
    assert_eq!(commands[0].object.object_id, survivor);
}

// ─── Curse of Bounty ─────────────────────────────────────────────────────────

/// CR 508.3b: Trigger fires when enchanted player is attacked; curse controller's
/// tapped nonland permanents become untapped.
#[test]
fn curse_of_bounty_fires_and_untaps_nonland_permanents() {
    let (mut runner, curse_id, attacker) = setup_curse(CURSE_OF_BOUNTY_ORACLE, "Curse of Bounty");

    // Create a tapped nonland permanent under P0's control.
    let tapped_artifact = {
        let state = runner.state_mut();
        let card_id = engine::types::identifiers::CardId(state.next_object_id);
        let id = engine::game::zones::create_object(
            state,
            card_id,
            P0,
            "Sol Ring".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types
            .core_types
            .push(engine::types::card_type::CoreType::Artifact);
        obj.base_card_types = obj.card_types.clone();
        obj.tapped = true;
        id
    };

    assert!(
        runner.state().objects[&tapped_artifact].tapped,
        "precondition: artifact must be tapped"
    );

    attack_enchanted_player(&mut runner, attacker);

    assert!(
        stack_triggers_from(&runner, curse_id) >= 1,
        "Curse of Bounty must trigger when enchanted player is attacked"
    );

    runner.advance_until_stack_empty();

    assert!(
        !runner.state().objects[&tapped_artifact].tapped,
        "Curse of Bounty: P0's tapped nonland permanent must be untapped after trigger resolves"
    );
}

// ─── Deduplication ──────────────────────────────────────────────────────────

/// CR 508.3b: "Whenever [player] is attacked" triggers only ONCE per combat,
/// even when multiple creatures attack the same enchanted player.
#[test]
fn curse_triggers_once_when_multiple_creatures_attack_enchanted_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let curse_id = {
        let mut builder = scenario.add_creature(P0, "Curse of Vitality", 0, 0);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.from_oracle_text(CURSE_OF_VITALITY_ORACLE);
        builder.id()
    };

    let attacker1 = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let attacker2 = scenario.add_creature(P0, "Hill Giant", 3, 3).id();

    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();
    attach_to_player(runner.state_mut(), curse_id, P1);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse_id);

    // Both creatures attack the enchanted player.
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (attacker1, AttackTarget::Player(P1)),
            (attacker2, AttackTarget::Player(P1)),
        ])
        .expect("DeclareAttackers should succeed");

    let trigger_count = stack_triggers_from(&runner, curse_id);
    assert_eq!(
        trigger_count, 1,
        "CR 508.3b: 'whenever enchanted player is attacked' triggers once, not per creature (got {trigger_count})"
    );
}

// ─── Multiplayer "does the same" fan-out matrix ──────────────────────────────

/// Make `attacker` the active player and pass priority around the table until
/// the engine waits for their attacker declaration. Mirrors the multiplayer
/// idiom in `suppressor_skyguard_prevent_2924.rs` (only the active player may
/// declare attackers, so the opponent whose attack we want must take the turn).
fn hand_turn_to(runner: &mut GameRunner, attacker: PlayerId) {
    runner.state_mut().active_player = attacker;
    runner.state_mut().priority_player = attacker;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: attacker };

    for _ in 0..40 {
        if runner.waiting_for_kind() == "DeclareAttackers" {
            return;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            return;
        }
    }
}

/// CR 508.6 + CR 102.2 + CR 508.1b: MULTIPLAYER matrix. P0 controls the curse
/// enchanting P1. P2 (an opponent of the controller) attacks the enchanted
/// player P1; P3 (another opponent) has a creature but does NOT attack. Only the
/// attacking opponent P2 — plus the controller P0 via the base "you gain 2 life"
/// — gain life. The enchanted DEFENDER P1 and the NON-ATTACKING opponent P3 gain
/// nothing. This is the maintainer-required matrix: only the players actually
/// attacking the enchanted player receive the copied effect.
#[test]
fn curse_of_vitality_rider_fans_out_only_to_attacking_opponents() {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let curse_id = {
        let mut builder = scenario.add_creature(P0, "Curse of Vitality", 0, 0);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.from_oracle_text(CURSE_OF_VITALITY_DOES_THE_SAME_ORACLE);
        builder.id()
    };

    // P2 (opponent of the controller) will attack the enchanted player P1.
    let p2_attacker = scenario.add_creature(P2, "Grizzly Bears", 2, 2).id();
    // P3 (another opponent) has a creature but will NOT attack this combat.
    let _p3_idle = scenario.add_creature(P3, "Hill Giant", 3, 3).id();

    for pid in 0..4u8 {
        for _ in 0..10 {
            scenario.add_card_to_library_top(PlayerId(pid), "Plains");
        }
    }

    let mut runner = scenario.build();
    attach_to_player(runner.state_mut(), curse_id, P1);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse_id);

    let l0 = runner.life(P0);
    let l1 = runner.life(P1);
    let l2 = runner.life(P2);
    let l3 = runner.life(P3);

    hand_turn_to(&mut runner, P2);
    runner
        .declare_attackers(&[(p2_attacker, AttackTarget::Player(P1))])
        .expect("P2 declares an attacker against the enchanted player P1");

    assert!(
        stack_triggers_from(&runner, curse_id) >= 1,
        "curse must trigger when the enchanted player P1 is attacked"
    );

    runner.advance_until_stack_empty();

    // Controller gains from the base "you gain 2 life".
    assert_eq!(
        runner.life(P0),
        l0 + 2,
        "controller P0 must gain 2 life from the base effect"
    );
    // Attacking opponent gains from the "does the same" rider.
    assert_eq!(
        runner.life(P2),
        l2 + 2,
        "attacking opponent P2 must gain 2 life from the does-the-same rider"
    );
    // Enchanted defender is not attacking themselves (CR 508.6) → no rider.
    assert_eq!(
        runner.life(P1),
        l1,
        "enchanted defender P1 must NOT gain life from the rider"
    );
    // Non-attacking opponent → no rider.
    assert_eq!(
        runner.life(P3),
        l3,
        "non-attacking opponent P3 must NOT gain life from the rider"
    );
}

/// CR 508.6: the rider is SET-VALUED — it fans out to EVERY opponent attacking
/// the enchanted player, not merely the active attacker. P2 attacks P1
/// naturally; a second opponent P3 also has a creature attacking P1 (placed in
/// combat directly, since only the active player declares — a state reachable
/// whenever multiple players have creatures attacking the same player). Both P2
/// and P3 gain life; the non-attacking opponent P4 and the enchanted defender P1
/// do not. This proves the scope resolves to the full attacker set, not a
/// single player.
///
/// CR 506.4: a third opponent P5 was recorded as having attacked P1 (its
/// declaration is in the ledger) but its creature is removed from combat before
/// the trigger resolves, so P5 is no longer attacking that player and gains
/// nothing — the removed-attacker case for a non-token rider.
#[test]
fn curse_of_vitality_rider_fans_out_to_all_attacking_opponents() {
    let mut scenario = GameScenario::new_n_player(6, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let curse_id = {
        let mut builder = scenario.add_creature(P0, "Curse of Vitality", 0, 0);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.from_oracle_text(CURSE_OF_VITALITY_DOES_THE_SAME_ORACLE);
        builder.id()
    };

    let p2_attacker = scenario.add_creature(P2, "Grizzly Bears", 2, 2).id();
    let p3_attacker = scenario.add_creature(P3, "Hill Giant", 3, 3).id();
    let _p4_idle = scenario.add_creature(P4, "Bear Cub", 1, 1).id();
    let p5_attacker = scenario.add_creature(P5, "Bear Cub", 1, 1).id();

    for pid in 0..6u8 {
        for _ in 0..10 {
            scenario.add_card_to_library_top(PlayerId(pid), "Plains");
        }
    }

    let mut runner = scenario.build();
    attach_to_player(runner.state_mut(), curse_id, P1);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse_id);

    let baseline: Vec<i32> = (0..6u8).map(|p| runner.life(PlayerId(p))).collect();

    hand_turn_to(&mut runner, P2);
    runner
        .declare_attackers(&[(p2_attacker, AttackTarget::Player(P1))])
        .expect("P2 declares an attacker against the enchanted player P1");

    // Additional attacking controllers. Only the active player may *declare*
    // attackers, so P3's and P5's creatures are placed in combat directly. P5
    // is also written to the declaration ledger (what a declaration does), and
    // its creature is then removed from combat (CR 506.4).
    {
        let combat = runner
            .state_mut()
            .combat
            .as_mut()
            .expect("combat is active after declaring attackers");
        combat
            .attackers
            .push(AttackerInfo::attacking_player(p3_attacker, P1));
        combat
            .attackers
            .push(AttackerInfo::attacking_player(p5_attacker, P1));
        combat
            .attacked_defenders_this_combat
            .entry(P5)
            .or_default()
            .insert(P1);
    }
    remove_object_from_combat(runner.state_mut(), p5_attacker);

    assert!(
        stack_triggers_from(&runner, curse_id) >= 1,
        "curse must trigger when the enchanted player P1 is attacked"
    );

    runner.advance_until_stack_empty();

    // Controller: base "you gain 2 life".
    assert_eq!(
        runner.life(P0),
        baseline[0] + 2,
        "controller P0 must gain 2 life from the base effect"
    );
    // Both attacking opponents get the copied effect.
    assert_eq!(
        runner.life(P2),
        baseline[2] + 2,
        "attacking opponent P2 must gain 2 life from the rider"
    );
    assert_eq!(
        runner.life(P3),
        baseline[3] + 2,
        "second attacking opponent P3 must gain 2 life from the rider"
    );
    // Enchanted defender and non-attacking opponent get nothing.
    assert_eq!(
        runner.life(P1),
        baseline[1],
        "enchanted defender P1 must NOT gain life from the rider"
    );
    assert_eq!(
        runner.life(P4),
        baseline[4],
        "non-attacking opponent P4 must NOT gain life from the rider"
    );
    // Removed from combat before resolution → no longer attacking (CR 506.4).
    assert_eq!(
        runner.life(P5),
        baseline[5],
        "P5's attacker was removed from combat: P5 must NOT gain life from the rider"
    );
}
