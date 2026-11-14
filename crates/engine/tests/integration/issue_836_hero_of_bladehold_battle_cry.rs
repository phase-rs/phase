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
//! CR 508.1a + CR 508.1k: the active player chooses which creatures attack, and
//! each chosen creature still controlled by the active player becomes an
//! attacking creature — CR 508.1k points at CR 506.4 for how long it stays one.
//! CR 508.4: Hero's Soldier tokens are PUT onto the battlefield attacking, so
//! they are attacking creatures without ever having been declared as attackers
//! (the printed ruling: "although the tokens are attacking, they never were
//! declared as attacking creatures").
//!
//! There was no runtime coverage for battle cry anywhere in the engine, so a
//! registration or resolution regression would have been invisible. These tests
//! drive the real declare-attackers → trigger → resolution pipeline.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::AttackTarget;
use super::support::shared_card_db;

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

/// CR 508.1k + CR 508.4 + CR 506.4: attacking membership is read off
/// `combat.attackers`, whose entries carry the attacker's `object_id` (see
/// `cr733_resolved_combat_membership`). Declared attackers join it at CR 508.1k;
/// creatures put onto the battlefield attacking join it at CR 508.4; CR 506.4
/// governs when a member is removed.
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
/// The committed export fixture supplies the keyword and token trigger; database
/// synthesis must install the battle-cry trigger without injected keyword hints.
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

    let db = shared_card_db().expect("the curated fixture must contain Hero of Bladehold");
    let hero = scenario.add_real_card(P0, "Hero of Bladehold", Zone::Battlefield, db);
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

    let db = shared_card_db().expect("the curated fixture must contain Hero of Bladehold");
    let hero = scenario.add_real_card(P0, "Hero of Bladehold", Zone::Battlefield, db);

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

/// Battle cry's description, used to tell Hero's two `Attacks` triggers apart in
/// the CR 603.3b ordering prompt (both share Hero as their source).
const BATTLE_CRY_TRIGGER_PREFIX: &str = "CR 702.91a: Battle cry";

/// Declare Hero + a vanilla ally as attackers and answer the CR 603.3b ordering
/// prompt so that `token_trigger_first` decides which of Hero's two triggers
/// resolves first. Returns the Soldier tokens Hero created.
///
/// CR 603.3b: the controller chooses the relative order of their triggers.
/// The engine accepts a bottom-first order: the last index goes on top
/// (CR 405.2), and the topmost object resolves first (CR 608.1).
fn declare_and_order(token_trigger_first: bool) -> (GameRunner, ObjectId, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let db = shared_card_db().expect("the curated fixture must contain Hero of Bladehold");
    let hero = scenario.add_real_card(P0, "Hero of Bladehold", Zone::Battlefield, db);
    let ally = scenario.add_creature(P0, "Vanilla Ally", 2, 2).id();

    let mut runner = scenario.build();
    assert_battle_cry_installed(&runner, hero);
    let before: Vec<ObjectId> = runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Battlefield && o.controller == P0)
        .map(|o| o.id)
        .collect();

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

    let WaitingFor::OrderTriggers { player, triggers } = runner.state().waiting_for.clone() else {
        panic!(
            "reach-guard: Hero's two Attacks triggers must require CR 603.3b ordering; got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "P0 controls both of Hero's attack triggers");
    assert_eq!(triggers.len(), 2, "exactly two attack triggers must fire");

    let battle_cry = triggers
        .iter()
        .position(|t| t.description.starts_with(BATTLE_CRY_TRIGGER_PREFIX))
        .expect("reach-guard: the ordering prompt must identify the battle cry trigger");
    let tokens = 1 - battle_cry;

    // Last index = top of stack = resolves first.
    let order = if token_trigger_first {
        vec![battle_cry, tokens]
    } else {
        vec![tokens, battle_cry]
    };
    runner
        .act(GameAction::OrderTriggers { order })
        .expect("submitting a trigger order must succeed");
    runner.advance_until_stack_empty();

    let created: Vec<ObjectId> = runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Battlefield && o.controller == P0 && !before.contains(&o.id))
        .map(|o| o.id)
        .collect();
    assert_eq!(
        created.len(),
        2,
        "reach-guard: Hero's other trigger must have created two Soldier tokens"
    );
    (runner, hero, ally, created)
}

/// CR 508.4 + CR 702.91a + the printed ruling: when the token trigger resolves
/// FIRST, the Soldiers are already attacking creatures when battle cry resolves,
/// so each gets +1/+0 — a 1/1 token becomes 2/1.
#[test]
fn tokens_created_before_battle_cry_resolves_are_pumped() {
    let (runner, hero, ally, tokens) = declare_and_order(true);

    for token in &tokens {
        assert!(
            is_attacking(&runner, *token),
            "CR 508.4: a token put onto the battlefield attacking is an attacking creature"
        );
        assert_eq!(
            (
                runner.state().objects[token].power,
                runner.state().objects[token].toughness
            ),
            (Some(2), Some(1)),
            "battle cry resolving after the tokens exist must pump each to 2/1"
        );
    }
    assert_eq!(
        runner.state().objects[&ally].power,
        Some(3),
        "the declared co-attacker is pumped in either order"
    );
    assert_eq!(
        runner.state().objects[&hero].power,
        Some(3),
        "battle cry never pumps its own source"
    );
}

/// The other permutation: battle cry resolves FIRST, before the tokens exist.
/// Nothing pumps them, so they stay 1/1 — the rules-correct branch the reporter
/// most likely saw.
#[test]
fn tokens_created_after_battle_cry_resolves_are_not_pumped() {
    let (runner, hero, ally, tokens) = declare_and_order(false);

    for token in &tokens {
        assert!(
            is_attacking(&runner, *token),
            "CR 508.4: the tokens are attacking even though battle cry missed them"
        );
        assert_eq!(
            (
                runner.state().objects[token].power,
                runner.state().objects[token].toughness
            ),
            (Some(1), Some(1)),
            "battle cry resolving before the tokens exist must leave them 1/1"
        );
    }
    assert_eq!(
        runner.state().objects[&ally].power,
        Some(3),
        "the declared co-attacker is pumped in either order"
    );
    assert_eq!(
        runner.state().objects[&hero].power,
        Some(3),
        "battle cry never pumps its own source"
    );
}
