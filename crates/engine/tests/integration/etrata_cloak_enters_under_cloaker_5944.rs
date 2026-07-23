//! Runtime regression for issue #5944 (Etrata cloak enters under the wrong
//! player).
//!
//! Etrata, Deadly Fugitive: "Deathtouch\nFace-down creatures you control have
//! \"{2}{U}{B}: Turn this creature face up. If you can't, exile it, then you
//! may cast the exiled card without paying its mana cost.\"\nWhenever an
//! Assassin you control deals combat damage to an opponent, cloak the top card
//! of that player's library."
//!
//! This drives the real pipeline end to end: `from_oracle_text_with_keywords`
//! parses the combat-damage trigger into `Effect::Cloak { target:
//! TriggeringPlayer, count: 1, enters_under: You }` → Etrata attacks and deals
//! combat damage → the trigger fires and resolves → the top card of the
//! damaged opponent's library enters the battlefield face down.
//!
//! The discriminator is the CR 110.2a controller redirect: the cloaked card is
//! the damaged opponent's (their library is the source), but the player
//! instructed to cloak — Etrata's controller — puts it onto the battlefield,
//! so it enters under P0's control while ownership stays with the library
//! owner. A resolver that collapsed `enters_under: You` to the library owner
//! (the pre-fix `morph::cloak` path) would leave it under the opponent's
//! control and fail `controller == P0`.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 110.2a: an effect that instructs a player to put an object onto the
//!     battlefield puts it under that player's control unless the effect
//!     states otherwise.
//!   - CR 701.58a: cloak — face-down 2/2 with ward {2}, put onto the
//!     battlefield face down.
//!   - CR 701.58e: multiple cloaks from a single library happen one at a time
//!     (the empty-library fizzle stops the loop cleanly).

use super::rules::{
    AttackTarget, GameAction, GameEvent, GameRunner, GameScenario, Keyword, ObjectId, Phase,
    PlayerId, WaitingFor, Zone, P0, P1,
};
use engine::types::ability::EffectKind;
use engine::types::keywords::WardCost;
use engine::types::mana::ManaCost;

/// Verbatim Oracle text (data/card-data.json / Scryfall) — never a paraphrase,
/// so the test exercises the same parser branches as the real card.
const ETRATA_ORACLE: &str = "Deathtouch\nFace-down creatures you control have \
    \"{2}{U}{B}: Turn this creature face up. If you can't, exile it, then you \
    may cast the exiled card without paying its mana cost.\"\nWhenever an \
    Assassin you control deals combat damage to an opponent, cloak the top \
    card of that player's library.";

/// Count of cards in `player`'s library.
fn library_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .library
        .len()
}

/// Add Etrata (verbatim Oracle text, real subtypes) to P0's battlefield.
fn add_etrata(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature(P0, "Etrata, Deadly Fugitive", 1, 4)
        .with_subtypes(vec!["Vampire", "Assassin"])
        .from_oracle_text_with_keywords(&["Deathtouch"], ETRATA_ORACLE)
        .id()
}

/// Drive one unblocked attack by `attacker` at `defender` through combat
/// damage and resolve the damage trigger. Returns every engine event the
/// harness collected along the way.
fn attack_and_resolve(
    runner: &mut GameRunner,
    attacker: ObjectId,
    defender: PlayerId,
) -> Vec<GameEvent> {
    let mut events = Vec::new();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(defender))])
        .expect("the attacker must be legal");
    // CR 510: pass priority through the combat-damage step; the cloak trigger
    // resolves during these passes. CR 509.1: with no eligible blockers the
    // engine auto-submits the empty declaration; when it prompts, decline.
    events.extend(runner.combat_damage().events().to_vec());
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        let result = runner
            .act(GameAction::DeclareBlockers {
                assignments: vec![],
            })
            .expect("empty blocker declaration");
        events.extend(result.events);
        events.extend(runner.combat_damage().events().to_vec());
    }
    runner.advance_until_stack_empty();
    events
}

/// CR 110.2a + CR 701.58a: Etrata's combat damage cloaks the top card of the
/// damaged opponent's library UNDER ETRATA'S CONTROLLER — P1 owns the
/// face-down 2/2 ward {2}, P0 controls it.
#[test]
fn etrata_cloak_enters_under_cloaking_player() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let etrata = add_etrata(&mut scenario);
    scenario.with_library_top(P1, &["Top Card A", "Deeper Card B"]);
    let mut runner = scenario.build();
    let p1_lib_before = library_len(&runner, P1);
    let p0_lib_before = library_len(&runner, P0);

    attack_and_resolve(&mut runner, etrata, P1);

    // Reach guard: exactly one face-down battlefield object owned by P1.
    let cloaked: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Battlefield && o.face_down && o.owner == P1)
        .collect();
    assert_eq!(
        cloaked.len(),
        1,
        "exactly one face-down permanent owned by P1 must be on the battlefield"
    );
    let obj = cloaked[0];

    // CR 701.58a: face-down 2/2 with ward {2}.
    assert_eq!(obj.power, Some(2));
    assert_eq!(obj.toughness, Some(2));
    assert!(
        obj.keywords.iter().any(|k| matches!(
            k,
            Keyword::Ward(WardCost::Mana(c)) if *c == ManaCost::generic(2)
        )),
        "a cloaked permanent enters with ward {{2}}"
    );

    // CR 110.2a: the revert-sensitive discriminator — the player instructed to
    // cloak (P0, Etrata's controller) puts the card onto the battlefield, so it
    // enters under P0's control even though P1 owns it.
    assert_eq!(
        obj.controller, P0,
        "CR 110.2a: the cloaked card must enter under the cloaking player's \
         control, not the library owner's"
    );

    // Negative sibling: NOTHING face down entered under P1's control.
    assert!(
        !runner
            .state()
            .objects
            .values()
            .any(|o| o.zone == Zone::Battlefield && o.face_down && o.controller == P1),
        "no face-down battlefield permanent may be under P1's control"
    );

    // Exactly one card left P1's library; P0's library is untouched.
    assert_eq!(library_len(&runner, P1), p1_lib_before - 1);
    assert_eq!(library_len(&runner, P0), p0_lib_before);
}

/// CR 110.2a in a 3-player game: the trigger binds the DAMAGED opponent (P2) —
/// their library is cloaked from, they own the card, P0 controls it, and the
/// bystander opponent P1 is untouched.
#[test]
fn etrata_cloak_three_players_binds_damaged_opponent() {
    const P2: PlayerId = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let etrata = add_etrata(&mut scenario);
    scenario.with_library_top(P1, &["P1 Guard Card"]);
    scenario.with_library_top(P2, &["P2 Top Card", "P2 Deeper Card"]);
    let mut runner = scenario.build();
    let p1_lib_before = library_len(&runner, P1);

    attack_and_resolve(&mut runner, etrata, P2);

    let cloaked: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Battlefield && o.face_down)
        .collect();
    assert_eq!(cloaked.len(), 1, "exactly one cloaked permanent");
    assert_eq!(
        cloaked[0].owner, P2,
        "the damaged opponent's library is the cloak source"
    );
    assert_eq!(
        cloaked[0].controller, P0,
        "CR 110.2a: the cloaking player controls the entry"
    );

    // The bystander opponent is untouched: library intact, owns nothing face down.
    assert_eq!(library_len(&runner, P1), p1_lib_before);
    assert!(
        !runner
            .state()
            .objects
            .values()
            .any(|o| o.zone == Zone::Battlefield && o.face_down && o.owner == P1),
        "no P1-owned face-down permanent may exist"
    );
}

/// Sibling: "cloak the top card of your library" — `enters_under: Some(You)`
/// is a semantic no-op when the cloaker IS the library owner (controller ==
/// owner == P0).
#[test]
fn self_library_cloak_controller_equals_owner() {
    let mut scenario = GameScenario::new_n_player(2, 11);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Test Self Cloak",
            true,
            "Cloak the top card of your library.",
        )
        .id();
    scenario.with_library_top(P0, &["Own Top Card"]);
    let mut runner = scenario.build();

    runner.cast(spell).resolve();

    let cloaked: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Battlefield && o.face_down)
        .collect();
    assert_eq!(cloaked.len(), 1, "the top card of P0's library was cloaked");
    let obj = cloaked[0];
    assert_eq!(obj.power, Some(2));
    assert_eq!(obj.toughness, Some(2));
    assert!(
        obj.keywords.iter().any(|k| matches!(
            k,
            Keyword::Ward(WardCost::Mana(c)) if *c == ManaCost::generic(2)
        )),
        "a cloaked permanent enters with ward {{2}}"
    );
    // CR 110.2a: You == owner here, so the override is a no-op.
    assert_eq!(obj.controller, P0);
    assert_eq!(obj.owner, P0);
}

/// CR 701.58e boundary: the damaged opponent's library is EMPTY — the cloak
/// resolves as a clean no-op (no face-down permanent, no panic, trigger
/// resolved, post-combat state reached).
#[test]
fn etrata_cloak_empty_library_fizzles() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let etrata = add_etrata(&mut scenario);
    // P1's library is deliberately left empty.
    let mut runner = scenario.build();
    assert_eq!(library_len(&runner, P1), 0, "P1's library must start empty");

    let events = attack_and_resolve(&mut runner, etrata, P1);

    // Reach guard: the cloak effect itself resolved (it just had nothing to
    // cloak) — the fizzle is the effect's, not an upstream short-circuit's.
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Cloak,
                ..
            }
        )),
        "the cloak trigger must have resolved (reach guard)"
    );
    assert!(
        !runner
            .state()
            .objects
            .values()
            .any(|o| o.zone == Zone::Battlefield && o.face_down),
        "an empty library cloaks nothing"
    );
    // Clean post-combat state: nothing stuck on the stack or in a prompt.
    assert!(runner.state().stack.is_empty(), "the stack must be empty");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the game must reach a clean priority window, got {:?}",
        runner.state().waiting_for
    );
}
