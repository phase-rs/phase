//! Firkraag, Cunning Instigator — "Whenever a creature deals combat damage to
//! one of your opponents, if that creature had to attack this combat, you put
//! a +1/+1 counter on Firkraag and you draw a card."
//!
//! Regression for issue #4732: the intervening-if "if that creature had to
//! attack this combat" (CR 603.4) was silently dropped during trigger
//! parsing (flagged by the card data's own `SwallowedClause` diagnostic), so
//! the ability put a counter on Firkraag and drew a card on ANY creature
//! dealing combat damage to an opponent — including creatures that attacked
//! purely by their controller's free choice, not under any must-attack
//! requirement (e.g. goad). The fix wires the clause to
//! `TriggerCondition::EventDamageSourceMatchesFilter` (the same "match the
//! event's damage source" mechanism already used by Mindblade Render's "if
//! any of that damage was dealt by a Warrior") over a new
//! `FilterProp::RequiredToAttack`, backed by a declaration-time snapshot
//! (`AttackerInfo::required_to_attack`) taken in
//! `declare_attackers_with_bands` BEFORE attackers are tapped — the
//! must-attack predicate exempts tapped creatures (CR 508.1a), so a live
//! recheck at combat-damage time would spuriously read `false` for every
//! non-vigilance attacker.
//!
//! Discriminating end-to-end: in one game a goaded attacker (CR 701.15b: must
//! attack if able) deals the triggering combat damage — the intervening-if
//! holds, Firkraag gets a counter and its controller draws. In the other, an
//! unencumbered attacker under no requirement deals the same combat damage —
//! the intervening-if fails, matching the pre-fix bug this test guards
//! against (before the fix, this case incorrectly fired too).

use super::rules::run_combat;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const FIRKRAAG_TRIGGER_ORACLE: &str = "Whenever a creature deals combat damage to one of your \
    opponents, if that creature had to attack this combat, you put a +1/+1 counter on Firkraag \
    and you draw a card.";

/// +1/+1 counters on `id`.
fn plus_one_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.hand.len())
        .expect("player must exist")
}

/// Mark `creature` as goaded by `goader` — CR 701.15b: must attack each
/// combat if able. Mirrors the proven pattern in
/// `goaded_creature_under_pacifism_visible.rs`.
fn goad(runner: &mut GameRunner, creature: ObjectId, goader: PlayerId) {
    runner
        .state_mut()
        .objects
        .get_mut(&creature)
        .unwrap()
        .goaded_by
        .insert(goader);
}

/// CR 603.4 + CR 701.15b + CR 508.1a: a goaded creature had to attack this
/// combat, and it dealt the triggering combat damage — the intervening-if
/// holds. Firkraag's controller gets a +1/+1 counter on Firkraag and draws a
/// card.
#[test]
fn firkraag_counter_and_draw_when_attacker_had_to_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for name in ["Lib A", "Lib B", "Lib C"] {
        scenario.add_card_to_library_top(P0, name);
    }

    let firkraag = scenario
        .add_creature_from_oracle(
            P0,
            "Firkraag, Cunning Instigator",
            4,
            4,
            FIRKRAAG_TRIGGER_ORACLE,
        )
        .id();
    // Goaded by P1 (the only opponent in a 2-player game, so attacking P1
    // stays legal per CR 701.15b even though P1 is also the goading player —
    // there is no other attackable player to redirect to).
    let goaded = scenario.add_creature(P0, "Goaded Dragon", 3, 3).id();

    let mut runner = scenario.build();
    goad(&mut runner, goaded, P1);

    let p0_hand_before = hand_len(&runner, P0);
    let p1_life_before = runner.life(P1);
    let firkraag_counters_before = plus_one_counters(&runner, firkraag);

    run_combat(&mut runner, vec![goaded], vec![]);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        p1_life_before - 3,
        "precondition: the goaded creature dealt 3 combat damage to P1"
    );
    assert_eq!(
        plus_one_counters(&runner, firkraag),
        firkraag_counters_before + 1,
        "the damage source had to attack this combat (goaded) — Firkraag gets a +1/+1 counter"
    );
    assert_eq!(
        hand_len(&runner, P0),
        p0_hand_before + 1,
        "the damage source had to attack this combat (goaded) — Firkraag's controller draws a card"
    );
}

/// CR 603.4 — negative control and the exact case the dropped intervening-if
/// got wrong: an unencumbered creature attacks purely by its controller's
/// free choice (no goad, no must-attack static) and deals the triggering
/// combat damage. The intervening-if fails: no counter, no draw. Before the
/// fix, the swallowed clause meant this case incorrectly fired anyway.
#[test]
fn firkraag_silent_when_attacker_was_not_required_to_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for name in ["Lib A", "Lib B", "Lib C"] {
        scenario.add_card_to_library_top(P0, name);
    }

    let firkraag = scenario
        .add_creature_from_oracle(
            P0,
            "Firkraag, Cunning Instigator",
            4,
            4,
            FIRKRAAG_TRIGGER_ORACLE,
        )
        .id();
    // Not goaded, no must-attack static — attacks purely by choice.
    let volunteer = scenario.add_creature(P0, "Unencumbered Dragon", 3, 3).id();

    let mut runner = scenario.build();

    let p0_hand_before = hand_len(&runner, P0);
    let p1_life_before = runner.life(P1);
    let firkraag_counters_before = plus_one_counters(&runner, firkraag);

    run_combat(&mut runner, vec![volunteer], vec![]);
    // CR 603.3: with the intervening-if respected, the trigger must not even
    // go on the stack; draining confirms no counter/draw sneaks through.
    // Before the fix the dropped condition queued the effect here.
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        p1_life_before - 3,
        "precondition: the unencumbered creature dealt 3 combat damage to P1"
    );
    assert_eq!(
        plus_one_counters(&runner, firkraag),
        firkraag_counters_before,
        "the damage source was not required to attack — intervening-if fails, no counter placed"
    );
    assert_eq!(
        hand_len(&runner, P0),
        p0_hand_before,
        "the damage source was not required to attack — intervening-if fails, no card drawn"
    );
}
