//! Runtime regression for the CR 611.2a CONTINUOUS chosen-permanent damage
//! redirection created by a resolving spell (Heroic Sacrifice, Gideon's
//! Sacrifice, and the Saving Grace attachment-host sibling).
//!
//! Heroic Sacrifice was, in sequence: (a) silently misparsed into a card-level
//! `ShieldKind::Prevention { All }` that DELETED the damage, dropped the
//! "creatures you control" victim leg and swallowed its CR 603.7a delayed
//! trigger; then (b) an honest `Effect::Unimplemented` gap. Both left the card a
//! complete no-op — casting it protected nothing.
//!
//! These tests drive the real cast pipeline (`GameRunner::cast(..).resolve()`)
//! followed by the real damage pipeline, so they exercise parser + targeting +
//! resolver + `damage_done_applier` end to end. Every Oracle text here is
//! verbatim Scryfall text.
//!
//! SCOPE / KNOWN GAP (pre-existing, NOT introduced or asserted here): Heroic
//! Sacrifice's third sentence — "When that creature dies this turn, put its
//! counters on up to one target creature you control and draw a card." — is
//! chunked with the "and draw a card" conjunct as a SIBLING of the
//! `CreateDelayedTrigger` rather than inside its payload, so the draw happens on
//! resolution instead of on death. That is identical before and after this
//! change (it is present in the committed `data/card-data.json` baseline) and
//! lives in the clause-assembly/delayed-payload subsystem, not the damage
//! redirection this file covers. These fixtures therefore stock a library so the
//! stray draw is inert, and deliberately assert NOTHING about the draw — pinning
//! the wrong placement would bless it.

use super::rules::damage_ability;
use engine::game::effects::deal_damage;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::phase::Phase;

/// Verbatim Heroic Sacrifice (Scryfall-verified).
const HEROIC_SACRIFICE_TEXT: &str = "Choose target creature you control. Until end of turn, all damage that would be dealt to you and creatures you control is dealt to the chosen creature instead (if it's still on the battlefield). When that creature dies this turn, put its counters on up to one target creature you control and draw a card.";

/// Verbatim Gideon's Sacrifice (Scryfall-verified) — the inline "this turn"
/// duration spelling with an untyped permanent leg.
const GIDEONS_SACRIFICE_TEXT: &str = "Choose a creature or planeswalker you control. All damage that would be dealt this turn to you and permanents you control is dealt to the chosen permanent instead (if it's still on the battlefield).";

/// CR 611.2a + CR 614.9 + CR 614.1a: the whole card, through the production
/// cast pipeline.
///
/// REVERT GUARDS — each assertion names the axis it pins:
/// * `protector.damage_marked == 3` after event 1 → the parser production and
///   the `ChosenObjectTarget` recipient reading the parent's bound target. With
///   the clause back to `Effect::Unimplemented`, no shield exists at all and P0
///   simply loses 3 life.
/// * `bystander.damage_marked == 0` / `protector == 7` after event 2 → BOTH the
///   `PlayerOrPermanentsControlledBy` victim conjunct (without it only "you" is
///   protected) AND `RedirectionLifetime::Continuous` (without it the shield was
///   consumed by event 1).
#[test]
fn heroic_sacrifice_redirects_every_event_to_the_chosen_creature_until_end_of_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Filler A", "Filler B"]);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Heroic Sacrifice", true, HEROIC_SACRIFICE_TEXT)
        .id();
    let protector = scenario.add_creature(P0, "Chosen Creature", 2, 20).id();
    let bystander = scenario.add_creature(P0, "Bystander", 2, 20).id();
    let enemy = scenario.add_creature(P1, "Enemy Creature", 2, 20).id();
    let source = scenario.add_creature(P1, "Damage Source", 3, 3).id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_objects(&[protector]).resolve();
    assert_eq!(
        outcome.life_delta(P0),
        0,
        "resolving the spell itself changes no life total"
    );

    let life_before = runner.life(P0);

    // Event 1 — the "to you" victim leg.
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Player(P0), 3),
        &mut events,
    )
    .expect("damage to the protected controller resolves");
    assert_eq!(
        runner.life(P0),
        life_before,
        "damage aimed at you must be redirected, not dealt"
    );
    assert_eq!(
        runner.state().objects[&protector].damage_marked,
        3,
        "the chosen creature takes it instead"
    );

    // Event 2 — the "creatures you control" victim leg, on a shield a
    // one-opportunity lifetime would already have spent.
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Object(bystander), 4),
        &mut events,
    )
    .expect("damage to another creature you control resolves");
    assert_eq!(
        runner.state().objects[&bystander].damage_marked,
        0,
        "the \"and creatures you control\" leg must be protected too"
    );
    assert_eq!(
        runner.state().objects[&protector].damage_marked,
        7,
        "a continuous redirection re-fires for every damage event this turn"
    );

    // Negative: a creature you do NOT control is outside the victim scope.
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Object(enemy), 5),
        &mut events,
    )
    .expect("damage to an opponent's creature resolves");
    assert_eq!(runner.state().objects[&enemy].damage_marked, 5);
    assert_eq!(
        runner.state().objects[&protector].damage_marked,
        7,
        "an opponent's creature must not be redirected onto the chosen creature"
    );
}

/// CR 514.2 + CR 611.2a: "Until end of turn" — the shield must be gone next
/// turn. A continuous redirection that outlived its window would silently
/// protect its controller forever.
///
/// Revert guard: `turns::execute_cleanup` prunes on the typed `expiry` field
/// alone — `shield_kind` classifies what a replacement does and carries no
/// lifetime meaning (CR 604.2 makes a printed static's shield durable while
/// holding the same `ShieldKind` value). This shield's `expiry ==
/// Some(RestrictionExpiry::EndOfTurn)` is stamped by
/// `ReplacementDefinition::redirection_shield`; drop that stamp and the
/// next-turn event would still redirect and the life total would be untouched.
#[test]
fn heroic_sacrifice_redirect_expires_at_end_of_turn() {
    let mut scenario = GameScenario::new();
    // Cast in the END step (CR 513) so advancing to the next turn's upkeep never
    // passes through the declare-attackers turn-based action, which would halt
    // the scenario driver's priority-passing loop before cleanup.
    scenario.at_phase(Phase::End);
    scenario.with_library_top(P0, &["Filler A", "Filler B"]);
    scenario.with_library_top(P1, &["Filler C", "Filler D"]);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Heroic Sacrifice", true, HEROIC_SACRIFICE_TEXT)
        .id();
    let protector = scenario.add_creature(P0, "Chosen Creature", 2, 20).id();
    let source = scenario.add_creature(P1, "Damage Source", 3, 3).id();
    let mut runner = scenario.build();

    runner.cast(spell).target_objects(&[protector]).resolve();

    // Reach guard: it really does redirect while the window is open.
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Player(P0), 3),
        &mut events,
    )
    .expect("this-turn damage resolves");
    assert_eq!(runner.state().objects[&protector].damage_marked, 3);

    // CR 514.2: cleanup prunes the shield; the next turn is unprotected.
    runner.advance_to_phase(Phase::Upkeep);
    let life_before = runner.life(P0);
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Player(P0), 3),
        &mut events,
    )
    .expect("next-turn damage resolves");
    assert_eq!(
        runner.life(P0),
        life_before - 3,
        "the \"until end of turn\" redirection must not survive cleanup"
    );
    // CR 514.2 also removes all damage from permanents during that cleanup step,
    // so the chosen creature is back to 0 — and must stay there, proving the
    // next-turn event was NOT redirected onto it.
    assert_eq!(
        runner.state().objects[&protector].damage_marked,
        0,
        "the chosen creature takes nothing more after the window closes"
    );
}

/// CR 611.2a: the sibling spelling. Gideon's Sacrifice carries its duration
/// INLINE ("dealt this turn to …") and its chosen permanent may be a
/// planeswalker, so its victim leg is untyped ("permanents you control"). Same
/// class, same runtime behavior — this is the "build for the class, not the
/// card" guard.
#[test]
fn gideons_sacrifice_untyped_permanent_leg_redirects_onto_the_chosen_permanent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Gideon's Sacrifice", true, GIDEONS_SACRIFICE_TEXT)
        .id();
    let protector = scenario.add_creature(P0, "Chosen Creature", 2, 20).id();
    let artifact = scenario
        .add_creature(P0, "Bystanding Artifact", 0, 0)
        .as_artifact()
        .id();
    let source = scenario.add_creature(P1, "Damage Source", 3, 3).id();
    let mut runner = scenario.build();

    runner.cast(spell).target_objects(&[protector]).resolve();

    // The UNTYPED permanent leg: a noncreature permanent you control is covered
    // here, where Heroic Sacrifice's "creatures you control" would not cover it.
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Object(artifact), 3),
        &mut events,
    )
    .expect("damage to a noncreature permanent you control resolves");
    assert_eq!(
        runner.state().objects[&artifact].damage_marked,
        0,
        "an untyped permanent leg must cover noncreature permanents"
    );
    assert_eq!(runner.state().objects[&protector].damage_marked, 3);
}
