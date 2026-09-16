//! Issue #836 — Hero of Bladehold: "tokens are spawned but battle cry is not
//! happening."
//!
//! Verbatim Oracle text (Scryfall, 2026-09-16):
//!
//! > Battle cry (Whenever this creature attacks, each other attacking creature
//! > gets +1/+0 until end of turn.)
//! > Whenever this creature attacks, create two 1/1 white Soldier creature
//! > tokens that are tapped and attacking.
//!
//! (The issue body quotes "battle cry" as *Battle Cry*, the 1994 instant —
//! "Untap all white creatures you control." — which is a different card. The
//! keyword is what Hero of Bladehold carries.)
//!
//! CR 702.91a: battle cry is "Whenever this creature attacks, each other
//! attacking creature gets +1/+0 until end of turn."
//! CR 508.1a: the active player chooses which creatures become attacking
//! creatures; those are the declared attackers.
//! CR 506.4: a creature is "attacking" from declaration until it leaves combat.
//!
//! There was no runtime coverage for battle cry anywhere in the engine, so a
//! registration or resolution regression would have been invisible. These tests
//! drive the real declare-attackers → trigger → resolution pipeline.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

// Verbatim Oracle text (Scryfall, 2026-09-16).
const HERO_OF_BLADEHOLD: &str = "Battle cry (Whenever this creature attacks, each other attacking creature gets +1/+0 until end of turn.)\nWhenever this creature attacks, create two 1/1 white Soldier creature tokens that are tapped and attacking.";

/// Derived power of an object (counters and layers applied), read off live state.
fn power_of(runner: &GameRunner, id: ObjectId) -> i32 {
    runner.state().objects[&id].power.unwrap_or(0)
}

fn toughness_of(runner: &GameRunner, id: ObjectId) -> i32 {
    runner.state().objects[&id].toughness.unwrap_or(0)
}

/// Creatures P0 controls on the battlefield — used to count the created tokens.
fn p0_battlefield_creatures(runner: &GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Battlefield && o.controller == P0)
        .count()
}

/// CR 506.4: attacking membership is read off `combat.attackers`, whose entries
/// carry the attacker's `object_id` (see `cr733_resolved_combat_membership`).
fn is_attacking(runner: &GameRunner, id: ObjectId) -> bool {
    runner
        .state()
        .combat
        .as_ref()
        .is_some_and(|combat| combat.attackers.iter().any(|a| a.object_id == id))
}

/// Reach-guard: Hero must carry the battle cry KEYWORD and both `Attacks`
/// triggers before any P/T assertion below means anything.
///
/// Without explicit keyword hints the scenario builder extracts no keyword from
/// a parenthetical "Battle cry (...)" line, `synthesize_all` then installs no
/// battle-cry trigger, and every "was it pumped?" assertion fails for a reason
/// that has nothing to do with the engine.
fn assert_battle_cry_installed(runner: &GameRunner, hero: ObjectId) {
    let obj = &runner.state().objects[&hero];
    assert!(
        obj.keywords
            .iter()
            .any(|k| format!("{k:?}").contains("Battlecry")),
        "reach-guard: Hero must carry the battle cry keyword; got {:?}",
        obj.keywords
    );
    assert!(
        obj.trigger_definitions.len() >= 2,
        "reach-guard: Hero must carry both Attacks triggers (battle cry + tokens); got {}",
        obj.trigger_definitions.len()
    );
}

/// CR 702.91a + CR 508.1a: with Hero and a vanilla creature both declared as
/// attackers, battle cry gives the OTHER attacker +1/+0 — and gives nothing to
/// Hero itself (the ability says "each other attacking creature") nor to a
/// creature that stayed home.
#[test]
fn battle_cry_pumps_the_other_declared_attacker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let hero = {
        let mut b = scenario.add_creature(P0, "Hero of Bladehold", 3, 4);
        b.from_oracle_text_with_keywords(&["Battle cry"], HERO_OF_BLADEHOLD);
        b.id()
    };
    let ally = scenario.add_creature(P0, "Vanilla Ally", 2, 2).id();
    let home = scenario.add_creature(P0, "Stayed Home", 2, 2).id();

    let mut runner = scenario.build();
    assert_battle_cry_installed(&runner, hero);
    let before = p0_battlefield_creatures(&runner);

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![
                (hero, AttackTarget::Player(P1)),
                (ally, AttackTarget::Player(P1)),
            ],
            bands: vec![],
        })
        .expect("declaring Hero and the ally as attackers must succeed");
    runner.advance_until_stack_empty();

    // Reach-guards: both creatures really are attacking, the one that stayed
    // home is not, and Hero's other trigger actually resolved (two tokens), so
    // a zero delta below cannot mean "no trigger ran at all".
    assert!(
        is_attacking(&runner, hero) && is_attacking(&runner, ally),
        "reach-guard: both declared creatures must be attacking (CR 506.4)"
    );
    assert!(
        !is_attacking(&runner, home),
        "reach-guard: the creature that stayed home must not be attacking"
    );
    assert_eq!(
        p0_battlefield_creatures(&runner),
        before + 2,
        "reach-guard: Hero's other attack trigger must have created two Soldier tokens"
    );

    // CR 702.91a: the other declared attacker gets +1/+0.
    assert_eq!(
        (power_of(&runner, ally), toughness_of(&runner, ally)),
        (3, 2),
        "battle cry must give the other attacking creature +1/+0"
    );
    // "each OTHER attacking creature" — never the source itself.
    assert_eq!(
        (power_of(&runner, hero), toughness_of(&runner, hero)),
        (3, 4),
        "battle cry must not pump its own source"
    );
    // A creature that never attacked is untouched.
    assert_eq!(
        (power_of(&runner, home), toughness_of(&runner, home)),
        (2, 2),
        "battle cry must not pump a creature that stayed home"
    );
}

/// The reporter's scenario, documented as rules-correct: Hero attacking with no
/// other DECLARED attacker still makes its tokens, and nothing gains power from
/// battle cry's own source. Per the printed ruling, "although the tokens are
/// attacking, they never were declared as attacking creatures" — whether they
/// are pumped depends on which of Hero's two triggers resolves first, so this
/// test deliberately asserts only what is order-independent.
#[test]
fn hero_attacking_alone_still_creates_its_tokens_and_never_pumps_itself() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let hero = {
        let mut b = scenario.add_creature(P0, "Hero of Bladehold", 3, 4);
        b.from_oracle_text_with_keywords(&["Battle cry"], HERO_OF_BLADEHOLD);
        b.id()
    };

    let mut runner = scenario.build();
    assert_battle_cry_installed(&runner, hero);
    let before = p0_battlefield_creatures(&runner);

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(hero, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declaring Hero as the lone attacker must succeed");
    runner.advance_until_stack_empty();

    assert!(
        is_attacking(&runner, hero),
        "reach-guard: Hero must be attacking (CR 506.4)"
    );
    assert_eq!(
        p0_battlefield_creatures(&runner),
        before + 2,
        "Hero's attack trigger creates two 1/1 Soldier tokens"
    );
    assert_eq!(
        (power_of(&runner, hero), toughness_of(&runner, hero)),
        (3, 4),
        "battle cry must not pump its own source, even attacking alone"
    );
}
