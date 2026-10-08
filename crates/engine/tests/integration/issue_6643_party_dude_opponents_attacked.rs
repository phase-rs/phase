//! Regression for issue #6643: Party Dude's level-3 ability ("Whenever one or
//! more of your opponents are attacked, ...") never triggered because the
//! Oracle parser fell through to `TriggerMode::Unknown` for that condition —
//! confirmed a class-level parser gap, not a card-specific bug.
//!
//! https://github.com/phase-rs/phase/issues/6643
//!
//! CR references:
//!   - CR 508.3b: "Whenever [a player, planeswalker, or battle] is attacked"
//!     triggers if one or more creatures are declared as attackers attacking
//!     that player or permanent — and only that named object, not the other
//!     two slots in the bracket.
//!   - CR 102.3: every other player is your opponent absent a team format.
//!   - CR 603.2c: an ability triggers only once each time its trigger event
//!     occurs, even if that event contains multiple occurrences — the
//!     aggregate "one or more ... are attacked" phrasing must fire once per
//!     declaration, not once per attacked opponent.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// Convenience constant for the third player (no `P2` const in the scenario
/// module) — mirrors `militant_angel_attacked_opponents.rs`.
const P2: PlayerId = PlayerId(2);

const PARTY_DUDE_ORACLE: &str = "(Gain the next level as a sorcery to add its ability.)\n\
When this Class enters, each player creates a Food token.\n\
{1}{G}: Level 2\n\
Whenever an artifact an opponent controls is put into a graveyard from the battlefield, draw a card.\n\
{4}{G}: Level 3\n\
Whenever one or more of your opponents are attacked, up to one target attacking creature gets +X/+X until end of turn, where X is the number of cards in your hand.";

/// Count triggered abilities on the stack sourced from `source` — mirrors the
/// helper in `curse_attack_triggers.rs` for the sibling "is attacked" pattern.
fn stack_triggers_from(runner: &GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|e| e.source_id == source)
        .count()
}

/// P0 controls Party Dude at level 3 and a lone attacker; P1 is P0's only
/// opponent. Library padding avoids empty-library draw-loss during setup.
fn setup_party_dude_level3() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let party_dude = scenario
        .add_creature(P0, "Party Dude", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Class"])
        .from_oracle_text(PARTY_DUDE_ORACLE)
        .id();
    let attacker = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    for _ in 0..20 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&party_dude)
        .unwrap()
        .class_level = Some(3);

    (runner, party_dude, attacker)
}

#[test]
fn party_dude_level3_triggers_when_an_opponent_is_attacked() {
    let (mut runner, party_dude, attacker) = setup_party_dude_level3();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("DeclareAttackers must succeed");

    assert!(
        stack_triggers_from(&runner, party_dude) >= 1,
        "Party Dude's level-3 ability must trigger when an opponent (P1) is attacked"
    );
}

/// CR 508.3b: the ability names "opponents" (a player), not the CR's broader
/// "player, planeswalker, or battle" bracket, so attacking a planeswalker an
/// opponent controls must NOT satisfy it even though that planeswalker's
/// controller is an opponent of Party Dude's controller.
#[test]
fn party_dude_level3_does_not_trigger_when_only_a_planeswalker_is_attacked() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let party_dude = scenario
        .add_creature(P0, "Party Dude", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Class"])
        .from_oracle_text(PARTY_DUDE_ORACLE)
        .id();
    let attacker = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let pw = scenario
        .add_creature(P1, "Test Planeswalker", 0, 0)
        .as_planeswalker_with_loyalty("Test", 5)
        .id();

    for _ in 0..20 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&party_dude)
        .unwrap()
        .class_level = Some(3);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Planeswalker(pw))])
        .expect("DeclareAttackers must succeed");

    assert_eq!(
        stack_triggers_from(&runner, party_dude),
        0,
        "Party Dude's level-3 ability must not trigger when a planeswalker (not the opponent player) is attacked"
    );
}

/// Level 1/2 must be unaffected: with no level set (Class enters at level 1),
/// the level-3 ability's `ClassLevelGE { level: 3 }` condition gates it off
/// even though the trigger definition itself now parses and matches.
#[test]
fn party_dude_level1_does_not_trigger_the_level3_ability() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let party_dude = scenario
        .add_creature(P0, "Party Dude", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Class"])
        .from_oracle_text(PARTY_DUDE_ORACLE)
        .id();
    let attacker = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    for _ in 0..20 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&party_dude)
        .unwrap()
        .class_level = Some(1);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("DeclareAttackers must succeed");

    assert_eq!(
        stack_triggers_from(&runner, party_dude),
        0,
        "Party Dude at level 1 must not fire the level-3 attacked-opponent ability"
    );
}

/// CR 603.2c: "one or more of your opponents are attacked" is a single
/// aggregate trigger event, not one occurrence per attacked opponent. In a
/// three-player game where P0 declares attackers against BOTH P1 and P2 in
/// the same declaration, Party Dude must trigger exactly once, not twice —
/// this is the shape `AttachedTo`-scoped siblings ("enchanted player is
/// attacked") can never exercise, since an Aura can only ever enchant one
/// specific player.
#[test]
fn party_dude_level3_triggers_exactly_once_when_two_opponents_are_attacked_simultaneously() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let party_dude = scenario
        .add_creature(P0, "Party Dude", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Class"])
        .from_oracle_text(PARTY_DUDE_ORACLE)
        .id();
    let attacker_vs_p1 = scenario.add_creature(P0, "Soldier", 2, 2).id();
    let attacker_vs_p2 = scenario.add_creature(P0, "Soldier", 2, 2).id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&party_dude)
        .unwrap()
        .class_level = Some(3);

    for _ in 0..12 {
        if runner.waiting_for_kind() == "DeclareAttackers" {
            break;
        }
        let _ = runner.act(GameAction::PassPriority);
    }
    assert_eq!(
        runner.waiting_for_kind(),
        "DeclareAttackers",
        "ramp-up loop must reach DeclareAttackers before declaring attacks"
    );
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![
                (attacker_vs_p1, AttackTarget::Player(P1)),
                (attacker_vs_p2, AttackTarget::Player(P2)),
            ],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    assert_eq!(
        stack_triggers_from(&runner, party_dude),
        1,
        "Party Dude must trigger exactly once when two opponents are attacked in the same declaration (CR 603.2c), not once per opponent"
    );
}
