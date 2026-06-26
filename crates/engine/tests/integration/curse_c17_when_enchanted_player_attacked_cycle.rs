//! Commander 2017 "whenever enchanted player is attacked" curse cycle.
//!
//! Five curses share the same trigger structure:
//!   - **Curse of Bounty** (1G) — untap all nonland permanents you control
//!   - **Curse of Disturbance** (2B) — create a 2/2 black Zombie creature token
//!   - **Curse of Opulence** (R) — create a Gold token
//!   - **Curse of Verbosity** (2U) — draw a card
//!   - **Curse of Vitality** (2W) — you gain 2 life
//!
//! Each also has "Each opponent attacking that player does the same." which is
//! the `EachPlayerAction` clause. In a 2-player game the curse controller IS
//! the only opponent, so the reward fires for "you" (curse controller) and then
//! the `EachPlayerAction` clause also targets the attacker (who is the same
//! player in 2-player). This test focuses on the core trigger firing.
//!
//! CR references:
//!   - CR 508.3b: "Whenever [a player] is attacked" triggers when one or more
//!     creatures are declared as attackers attacking that player.
//!   - CR 303.4b: An Aura that enchants a player is attached to that player.
//!   - CR 508.1a: The active player chooses which creatures will attack.

use engine::game::effects::attach::attach_to_player;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

// ─── Oracle text constants ───────────────────────────────────────────────────

const CURSE_OF_BOUNTY_ORACLE: &str = "Enchant player\n\
     Whenever enchanted player is attacked, untap all nonland permanents you control. \
     Each opponent attacking that player untaps all nonland permanents they control.";

const CURSE_OF_DISTURBANCE_ORACLE: &str = "Enchant player\n\
     Whenever enchanted player is attacked, create a 2/2 black Zombie creature token. \
     Each opponent attacking that player does the same.";

const CURSE_OF_OPULENCE_ORACLE: &str = "Enchant player\n\
     Whenever enchanted player is attacked, create a Gold token. \
     Each opponent attacking that player does the same.";

const CURSE_OF_VERBOSITY_ORACLE: &str = "Enchant player\n\
     Whenever enchanted player is attacked, draw a card. \
     Each opponent attacking that player draws a card.";

const CURSE_OF_VITALITY_ORACLE: &str = "Enchant player\n\
     Whenever enchanted player is attacked, you gain 2 life. \
     Each opponent attacking that player gains 2 life.";

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

/// CR 508.3b: Trigger fires when enchanted player is attacked; curse controller
/// creates a Gold token.
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

    // P0 should have at least one Gold token (artifact with "Gold" subtype).
    let gold_count = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner.state().objects.get(id).is_some_and(|obj| {
                obj.zone == Zone::Battlefield
                    && obj.controller == P0
                    && obj.is_token
                    && obj.card_types.subtypes.iter().any(|s| s == "Gold")
            })
        })
        .count();

    assert!(
        gold_count >= 1,
        "Curse of Opulence: P0 must have at least one Gold token, got {gold_count}"
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
