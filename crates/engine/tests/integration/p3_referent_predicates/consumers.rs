//! S1b consumer controls: the four printed cards whose "if its power|toughness is N" clause
//! (`strip_property_conditional`) changes form at S1b — from a quantity comparison to the owner's
//! referent predicate. Each asserts the PRINTED behaviour at values on both sides of the
//! threshold, so the same test is green before and after the form change (the AST differs, the
//! game does not). Oracle text is verbatim Scryfall (`cards/named?exact=`, fetched 2026-11-16).
//!
//! Ent's Fury and Welcome to the Fold are unsupported cards (one gap each at base): their
//! condition clause is exercised on its own through the production cast path; the unsupported
//! clauses of the same card are not asserted.

use super::fixture::*;
use engine::game::scenario::{GameRunner, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::zones::Zone;

const ENTS_FURY: &str = "Put a +1/+1 counter on target creature you control if its power is 4 or greater. Then that creature gets +1/+1 until end of turn and fights target creature you don't control.";
const ENTS_FURY_COST: &str = "{1}{G}";
const SAVAGE_SWIPE: &str = "Target creature you control gets +2/+2 until end of turn if its power is 2. Then it fights target creature you don't control. (Each deals damage equal to its power to the other.)";
const SAVAGE_SWIPE_COST: &str = "{G}";
const TRIBUTE_TO_THE_WORLD_TREE: &str = "Whenever a creature you control enters, draw a card if its power is 3 or greater. Otherwise, put two +1/+1 counters on it.";
const WELCOME_TO_THE_FOLD: &str = "Madness {X}{U}{U} (If you discard this card, discard it into exile. When you do, cast it for its madness cost or put it into your graveyard.)\nGain control of target creature if its toughness is 2 or less. If this spell's madness cost was paid, instead gain control of that creature if its toughness is X or less.";
const WELCOME_TO_THE_FOLD_COST: &str = "{2}{U}{U}";

/// P0 casts `name` at their own creature of power `power` (toughness 6 so the fight never kills it)
/// and at P1's filler; returns the runner and P0's creature.
fn fight_spell(name: &str, oracle: &str, printed_cost: &str, power: i32) -> (GameRunner, ObjectId) {
    let mut sc = scenario();
    sc.with_mana_pool(P0, mana_for(&[printed_cost], 0));
    let mine = sc.add_creature(P0, "Filler Mine", power, 6).id();
    let theirs = sc.add_creature(P1, "Filler Theirs", 1, 9).id();
    let spell = sc
        .add_spell_to_hand_from_oracle(P0, name, false, oracle)
        .with_mana_cost(cost(printed_cost))
        .id();
    let mut runner = sc.build();
    let out = runner
        .cast(spell)
        .target_object(mine)
        .target_object(theirs)
        .resolve();
    assert!(
        out.state().stack.is_empty(),
        "reach guard: the spell resolved, got {:?}",
        out.final_waiting_for()
    );
    assert_eq!(
        zone(out.state(), spell),
        Zone::Graveyard,
        "reach guard: {name} left the stack"
    );
    (runner, mine)
}

/// CR 608.2c + CR 208.1: Ent's Fury puts the counter only if the target's power is 4 or greater.
#[test]
fn ents_fury_counter_only_at_power_four_or_more() {
    for (power, counters) in [(3, 0), (4, 1), (5, 1)] {
        let (runner, mine) = fight_spell("Ent's Fury", ENTS_FURY, ENTS_FURY_COST, power);
        assert_eq!(
            plus_counters(runner.state(), mine),
            counters,
            "Ent's Fury at power {power}"
        );
    }
}

/// CR 608.2c + CR 208.1: Savage Swipe gives +2/+2 only if the target's power is exactly 2.
#[test]
fn savage_swipe_pumps_only_at_power_two() {
    for (power, expected) in [(1, 1), (2, 4), (3, 3)] {
        let (runner, mine) = fight_spell("Savage Swipe", SAVAGE_SWIPE, SAVAGE_SWIPE_COST, power);
        assert_eq!(
            runner.state().objects[&mine].power,
            Some(expected),
            "Savage Swipe at power {power}"
        );
    }
}

/// CR 603.2 + CR 608.2c: Tribute to the World Tree reads the ENTERING creature's power: draws at
/// 3 or more, otherwise two +1/+1 counters on it.
#[test]
fn tribute_to_the_world_tree_reads_the_entering_creature() {
    for power in [2, 3, 4] {
        let mut sc = scenario();
        sc.with_mana_pool(P0, mana_for(&[], 2));
        sc.add_enchantment_from_oracle(P0, "Tribute to the World Tree", TRIBUTE_TO_THE_WORLD_TREE);
        sc.with_library_top(P0, &["Forest", "Forest", "Forest"]);
        let creature = {
            let mut b = sc.add_creature_to_hand(P0, "Filler Entrant", power, power);
            b.with_mana_cost(engine::types::mana::ManaCost::generic(2));
            b.id()
        };
        let mut runner = sc.build();
        let library_before = runner.state().players[0].library.len();
        cast_spell(&mut runner, creature);
        let w = drain(&mut runner);
        assert!(
            matches!(w, engine::types::game_state::WaitingFor::Priority { .. }),
            "resolution completed, got {w:?}"
        );
        assert_eq!(
            zone(runner.state(), creature),
            Zone::Battlefield,
            "reach guard: the creature entered"
        );
        let drew = library_before - runner.state().players[0].library.len();
        let counters = plus_counters(runner.state(), creature);
        if power >= 3 {
            assert_eq!((drew, counters), (1, 0), "power {power}: draw, no counters");
        } else {
            assert_eq!(
                (drew, counters),
                (0, 2),
                "power {power}: two counters, no draw"
            );
        }
    }
}

/// CR 608.2c + CR 208.1: Welcome to the Fold's first clause gains control only of a creature with
/// toughness 2 or less (madness not paid).
#[test]
fn welcome_to_the_fold_first_clause_reads_toughness() {
    for (toughness, stolen) in [(2, true), (3, false)] {
        let mut sc = scenario();
        sc.with_mana_pool(P0, mana_for(&[WELCOME_TO_THE_FOLD_COST], 0));
        let victim = sc.add_creature(P1, "Filler Victim", 1, toughness).id();
        let spell = sc
            .add_spell_to_hand_from_oracle(P0, "Welcome to the Fold", false, WELCOME_TO_THE_FOLD)
            .with_mana_cost(cost(WELCOME_TO_THE_FOLD_COST))
            .id();
        let mut runner = sc.build();
        let out = runner.cast(spell).target_object(victim).resolve();
        assert_eq!(
            zone(out.state(), spell),
            Zone::Graveyard,
            "reach guard: the spell resolved"
        );
        let controller = out.state().objects[&victim].controller;
        assert_eq!(
            controller == P0,
            stolen,
            "Welcome to the Fold at toughness {toughness}: controller {controller:?}"
        );
    }
}
