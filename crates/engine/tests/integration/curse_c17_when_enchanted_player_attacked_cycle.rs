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

use engine::game::combat::AttackerInfo;
use engine::game::effects::attach::attach_to_player;
use engine::game::effects::remove_from_combat::remove_object_from_combat;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::game::zones::move_to_zone;
use engine::types::ability::{AbilityCost, AbilityKind, Effect, ManaProduction};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
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

    let mut runner = scenario.build();
    attach_to_player(runner.state_mut(), curse_id, enchanted);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse_id);

    (runner, curse_id, creature_ids)
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

    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), curse_id, Zone::Graveyard, &mut events);
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
