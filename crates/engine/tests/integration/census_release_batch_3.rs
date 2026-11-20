//! Runtime witnesses for the `confirmed-misparses.toml` rows released in batch 3: the class of cards
//! that state a blocking requirement (CR 509.1c): "must be blocked ... if able" and "all creatures
//! able to block <it> do so". Every released card is the printed card (printed cost, type line, P/T
//! and verbatim Oracle text, loaded from the card database through `add_real_card`) and is driven
//! through the public cast / activate / attack / block actions of `GameRunner`. Only textless filler
//! creatures and plain lands are synthetic.
//!
//! Each shape runs on a board with two attackers and three blockers, so a requirement that is
//! satisfied by blocking the wrong attacker, by blocking with one creature when all must block, or
//! that outlives its turn, fails. Every positive has a control: the same board with the card present
//! but its effect not applied accepts an empty block declaration.
use engine::database::card_db::CardDatabase;
use engine::game::combat::{validate_blockers_for_player, AttackTarget};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

/// P0's two attackers (textless fillers), P1's three blockers (textless 0/4 fillers) and the
/// runner. The blockers deal no damage, so every attacker survives to the next turn.
struct Board {
    runner: GameRunner,
    attacker: ObjectId,
    other: ObjectId,
    blockers: Vec<ObjectId>,
}

/// Mana for every printed cost used below, as pool mana (CR 106.4) so that the cast and activation
/// pipelines pay exactly the printed cost and no land the test needs untapped is tapped for it.
fn pool() -> Vec<ManaUnit> {
    let mut kinds = vec![ManaType::Green; 6];
    kinds.extend([ManaType::Black, ManaType::Red, ManaType::Red]);
    kinds.extend([ManaType::White, ManaType::White]);
    kinds.extend([ManaType::Colorless; 4]);
    kinds
        .into_iter()
        .enumerate()
        .map(|(i, kind)| ManaUnit::new(kind, ObjectId(9900 + i as u64), false, vec![]))
        .collect()
}

/// Build the shared board; `setup` adds the released card (and anything else it needs) and returns
/// the ids it created.
fn board<T>(setup: impl FnOnce(&mut GameScenario) -> T) -> (Board, T) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["P0 Draw 1", "P0 Draw 2", "P0 Draw 3"]);
    scenario.with_library_top(P1, &["P1 Draw 1", "P1 Draw 2", "P1 Draw 3"]);
    scenario.with_mana_pool(P0, pool());
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let other = scenario.add_creature(P0, "Other Attacker", 2, 2).id();
    let blockers = (0..3)
        .map(|i| {
            scenario
                .add_creature(P1, &format!("Blocker {i}"), 0, 4)
                .id()
        })
        .collect();
    let made = setup(&mut scenario);
    (
        Board {
            runner: scenario.build(),
            attacker,
            other,
            blockers,
        },
        made,
    )
}

/// Answer every prompt with the plain default (pass priority, no attacks, no blocks, all combat
/// damage on the first blocker) until `done` holds.
fn drive_until(runner: &mut GameRunner, label: &str, done: impl Fn(&GameRunner) -> bool) {
    for _ in 0..400 {
        if done(runner) {
            return;
        }
        let action = match runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::AssignCombatDamage {
                total_damage,
                ref blockers,
                ..
            } => GameAction::AssignCombatDamage {
                mode: Default::default(),
                assignments: vec![(blockers[0].blocker_id, total_damage)],
                trample_damage: 0,
                controller_damage: 0,
            },
            ref other => panic!("{label}: unexpected prompt {other:?}"),
        };
        runner.act(action).expect(label);
    }
    panic!("{label}: never reached");
}

/// Attack P1 with `attackers` and stop at P1's declare-blockers prompt.
fn to_blockers(runner: &mut GameRunner, attackers: &[ObjectId]) {
    drive_until(runner, "declare attackers", |r| {
        matches!(
            r.state().waiting_for,
            WaitingFor::DeclareAttackers { player: P0, .. }
        )
    });
    let attacks: Vec<_> = attackers
        .iter()
        .map(|a| (*a, AttackTarget::Player(P1)))
        .collect();
    runner
        .declare_attackers(&attacks)
        .expect("declare attackers");
    drive_until(runner, "declare blockers", |r| {
        matches!(
            r.state().waiting_for,
            WaitingFor::DeclareBlockers { player: P1, .. }
        )
    });
}

fn legal(runner: &GameRunner, blocks: &[(ObjectId, ObjectId)]) -> bool {
    validate_blockers_for_player(runner.state(), P1, blocks).is_ok()
}

/// Control: with no requirement in force an empty declaration is legal, so the `Err` the positive
/// checks see is the card's requirement and not a permanently illegal board.
fn assert_control_free(b: &mut Board, card: &str) {
    to_blockers(&mut b.runner, &[b.attacker, b.other]);
    assert!(
        legal(&b.runner, &[]),
        "{card} control: without the effect an empty declaration is legal"
    );
}

/// "<X> must be blocked if able": at least one creature must block X (CR 509.1c). One blocker is
/// enough; blocking only the other attacker does not obey it; the remaining blockers stay free.
fn assert_must_be_blocked(b: &mut Board, card: &str, required: ObjectId, unrequired: ObjectId) {
    let [b0, b1, ..] = b.blockers[..] else {
        panic!("three blockers")
    };
    assert!(
        b.runner.declare_blockers(&[]).is_err(),
        "{card}: the DeclareBlockers action rejects an empty declaration"
    );
    assert!(
        !legal(&b.runner, &[]),
        "{card}: an empty declaration disobeys the requirement"
    );
    assert!(
        legal(&b.runner, &[(b0, required)]),
        "{card}: one blocker is enough to obey it"
    );
    assert!(
        !legal(&b.runner, &[(b0, unrequired)]),
        "{card}: blocking only the other attacker does not obey it"
    );
    assert!(
        legal(&b.runner, &[(b0, required), (b1, unrequired)]),
        "{card}: it forces one blocker only; another blocker may block the other attacker"
    );
}

/// "All creatures able to block <X> do so": every able blocker must block X (CR 509.1c).
fn assert_all_must_block(b: &mut Board, card: &str, required: ObjectId, unrequired: ObjectId) {
    let [b0, b1, b2] = b.blockers[..] else {
        panic!("three blockers")
    };
    assert!(
        b.runner.declare_blockers(&[]).is_err(),
        "{card}: the DeclareBlockers action rejects an empty declaration"
    );
    assert!(
        !legal(&b.runner, &[]),
        "{card}: an empty declaration disobeys the requirement"
    );
    assert!(
        !legal(&b.runner, &[(b0, required)]),
        "{card}: one blocker alone leaves the other able creatures unobeyed"
    );
    assert!(
        !legal(&b.runner, &[(b0, required), (b1, required)]),
        "{card}: two of three able blockers leave the third unobeyed"
    );
    assert!(
        !legal(
            &b.runner,
            &[(b0, required), (b1, required), (b2, unrequired)]
        ),
        "{card}: every able blocker must block this attacker, not another one"
    );
    assert!(
        legal(&b.runner, &[(b0, required), (b1, required), (b2, required)]),
        "{card}: every able blocker blocking it obeys the requirement"
    );
}

/// "This turn": declare the legal blocks, play the turn and the opponent's turn out, attack again
/// with `attackers` and require that an empty declaration is legal again (CR 611.2a, 514.2).
fn assert_expires_with_the_turn(
    b: &mut Board,
    card: &str,
    blocks: &[(ObjectId, ObjectId)],
    attackers: &[ObjectId],
) {
    let turn = b.runner.state().turn_number;
    b.runner.declare_blockers(blocks).expect("legal blocks");
    drive_until(&mut b.runner, "next own turn", |r| {
        r.state().turn_number > turn + 1
            && matches!(
                r.state().waiting_for,
                WaitingFor::DeclareAttackers { player: P0, .. }
            )
    });
    to_blockers(&mut b.runner, attackers);
    assert!(
        legal(&b.runner, &[]),
        "{card}: the \"this turn\" requirement is gone on the controller's next turn"
    );
}

fn power_toughness(runner: &GameRunner, id: ObjectId) -> (i32, i32) {
    let o = &runner.state().objects[&id];
    (o.power.unwrap_or(0), o.toughness.unwrap_or(0))
}

fn db() -> Option<&'static CardDatabase> {
    shared_card_db()
}

// ---- Shape 1: a spell pumps or animates its target and says "must be blocked this turn if able"
// (Enlarge, Compelled Duel, King Harald's Revenge, Deadly Allure, Elemental Uprising). ----
#[test]
fn spell_target_creature_must_be_blocked_this_turn() {
    let Some(db) = db() else { return };
    for card in [
        "Enlarge",
        "Compelled Duel",
        "King Harald's Revenge",
        "Deadly Allure",
    ] {
        let (mut control, _) = board(|s| s.add_real_card(P0, card, Zone::Hand, db));
        assert_control_free(&mut control, card);

        let (mut b, spell) = board(|s| s.add_real_card(P0, card, Zone::Hand, db));
        b.runner.cast(spell).target_object(b.attacker).resolve();
        let (attacker, other, b0) = (b.attacker, b.other, b.blockers[0]);
        to_blockers(&mut b.runner, &[attacker, other]);
        assert_must_be_blocked(&mut b, card, attacker, other);
        assert_expires_with_the_turn(&mut b, card, &[(b0, attacker)], &[attacker, other]);
    }
}

/// The pump and keyword clauses the Enlarge, Compelled Duel, King Harald's Revenge and Deadly
/// Allure rows sit beside: they apply to the target and end with the turn (CR 611.2a).
#[test]
fn spell_pump_clauses_apply_and_end_with_the_turn() {
    let Some(db) = db() else { return };
    for (card, expected, keyword) in [
        ("Enlarge", (9, 9), Some(Keyword::Trample)),
        ("Compelled Duel", (5, 5), None),
        // +1/+1 for each creature P0 controls: Attacker, Other Attacker.
        ("King Harald's Revenge", (4, 4), Some(Keyword::Trample)),
        ("Deadly Allure", (2, 2), Some(Keyword::Deathtouch)),
    ] {
        let (mut b, spell) = board(|s| s.add_real_card(P0, card, Zone::Hand, db));
        b.runner.cast(spell).target_object(b.attacker).resolve();
        assert_eq!(power_toughness(&b.runner, b.attacker), expected, "{card}");
        if let Some(keyword) = &keyword {
            assert!(
                b.runner.state().objects[&b.attacker].has_keyword(keyword),
                "{card}: gains {keyword:?}"
            );
            assert!(
                !b.runner.state().objects[&b.other].has_keyword(keyword),
                "{card}: the other creature gains nothing"
            );
        }
        let (attacker, turn) = (b.attacker, b.runner.state().turn_number);
        drive_until(&mut b.runner, "next turn", |r| r.state().turn_number > turn);
        assert_eq!(power_toughness(&b.runner, attacker), (2, 2), "{card} ends");
        if let Some(keyword) = &keyword {
            assert!(
                !b.runner.state().objects[&attacker].has_keyword(keyword),
                "{card}: the keyword ends with the turn"
            );
        }
    }
}

/// King Harald's Revenge counts every creature P0 controls, so a third creature raises the pump.
#[test]
fn king_harald_pump_counts_creatures_you_control() {
    let Some(db) = db() else { return };
    let (mut b, (spell, _)) = board(|s| {
        (
            s.add_real_card(P0, "King Harald's Revenge", Zone::Hand, db),
            s.add_creature(P0, "Third Creature", 1, 1).id(),
        )
    });
    b.runner.cast(spell).target_object(b.attacker).resolve();
    assert_eq!(power_toughness(&b.runner, b.attacker), (5, 5));
}

#[test]
fn elemental_uprising_animated_land_must_be_blocked_this_turn() {
    let Some(db) = db() else { return };
    let card = "Elemental Uprising";
    let setup = |s: &mut GameScenario| {
        (
            s.add_real_card(P0, card, Zone::Hand, db),
            s.add_basic_land(P0, ManaColor::Green),
        )
    };
    let (mut control, _) = board(setup);
    assert_control_free(&mut control, card);

    let (mut b, (spell, land)) = board(setup);
    b.runner.cast(spell).target_object(land).resolve();
    let state = b.runner.state();
    assert!(
        state.objects[&land]
            .card_types
            .core_types
            .contains(&CoreType::Creature)
            && state.objects[&land]
                .card_types
                .core_types
                .contains(&CoreType::Land),
        "reach: still a land, now a creature"
    );
    assert_eq!(power_toughness(&b.runner, land), (4, 4));
    let (other, b0) = (b.other, b.blockers[0]);
    to_blockers(&mut b.runner, &[land, other]);
    assert_must_be_blocked(&mut b, card, land, other);
    let turn = b.runner.state().turn_number;
    b.runner
        .declare_blockers(&[(b0, land)])
        .expect("legal block");
    drive_until(&mut b.runner, "next own turn", |r| {
        r.state().turn_number > turn + 1
            && matches!(
                r.state().waiting_for,
                WaitingFor::DeclareAttackers { player: P0, .. }
            )
    });
    assert!(
        !b.runner.state().objects[&land]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "the animation ends with the turn"
    );
    assert!(
        b.runner.state().transient_continuous_effects.is_empty(),
        "no \"this turn\" effect survives into the next turn"
    );
}

// ---- Shape 2: an activated or triggered ability says "target creature must be blocked this turn if
// able" (Satyr Piper, Goldenhide Ox). ----
#[test]
fn activated_ability_target_creature_must_be_blocked_this_turn() {
    let Some(db) = db() else { return };
    let card = "Satyr Piper";
    let (mut control, _) = board(|s| s.add_real_card(P0, card, Zone::Battlefield, db));
    assert_control_free(&mut control, card);

    let (mut b, piper) = board(|s| s.add_real_card(P0, card, Zone::Battlefield, db));
    b.runner
        .activate(piper, 0)
        .target_object(b.attacker)
        .resolve();
    let (attacker, other, b0) = (b.attacker, b.other, b.blockers[0]);
    to_blockers(&mut b.runner, &[attacker, other]);
    assert_must_be_blocked(&mut b, card, attacker, other);
    assert_expires_with_the_turn(&mut b, card, &[(b0, attacker)], &[attacker, other]);
}

#[test]
fn constellation_trigger_target_creature_must_be_blocked_this_turn() {
    let Some(db) = db() else { return };
    let card = "Goldenhide Ox";
    // The Ox entering is the first leg of "this creature or another enchantment you control enters".
    let (mut control, _) = board(|s| s.add_real_card(P0, card, Zone::Hand, db));
    assert_control_free(&mut control, card);

    let (mut b, ox) = board(|s| s.add_real_card(P0, card, Zone::Hand, db));
    b.runner.cast(ox).target_object(b.attacker).resolve();
    let (attacker, other, b0) = (b.attacker, b.other, b.blockers[0]);
    to_blockers(&mut b.runner, &[attacker, other]);
    assert_must_be_blocked(&mut b, card, attacker, other);
    assert_expires_with_the_turn(&mut b, card, &[(b0, attacker)], &[attacker, other]);
}

/// The second leg: another enchantment P0 controls entering. A nonenchantment entering and an
/// enchantment entering under the opponent's control do not trigger it.
#[test]
fn constellation_trigger_fires_for_another_enchantment_you_control_only() {
    let Some(db) = db() else { return };
    let card = "Goldenhide Ox";
    let setup = |s: &mut GameScenario| {
        (
            s.add_real_card(P0, card, Zone::Battlefield, db),
            s.add_real_card(P0, "Glorious Anthem", Zone::Hand, db),
            s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db),
        )
    };
    let (mut b, (_, anthem, _)) = board(setup);
    b.runner.cast(anthem).target_object(b.attacker).resolve();
    let (attacker, other) = (b.attacker, b.other);
    to_blockers(&mut b.runner, &[attacker, other]);
    assert_must_be_blocked(
        &mut b,
        "Goldenhide Ox (Glorious Anthem enters)",
        attacker,
        other,
    );

    let (mut b, (_, _, bears)) = board(setup);
    b.runner.cast(bears).resolve();
    assert_control_free(&mut b, "Goldenhide Ox (Grizzly Bears enters)");

    // The opponent's Glorious Anthem enters on the opponent's own turn.
    let (mut b, anthem) = board(|s| {
        s.add_real_card(P0, card, Zone::Battlefield, db);
        for _ in 0..3 {
            s.add_basic_land(P1, ManaColor::White);
        }
        s.add_real_card(P1, "Glorious Anthem", Zone::Hand, db)
    });
    drive_until(&mut b.runner, "opponent's main phase", |r| {
        r.state().active_player == P1
            && r.state().phase == Phase::PreCombatMain
            && matches!(r.state().waiting_for, WaitingFor::Priority { player: P1 })
    });
    b.runner.cast(anthem).resolve();
    assert_eq!(
        b.runner.state().objects[&anthem].zone,
        Zone::Battlefield,
        "reach: the opponent's enchantment entered"
    );
    assert!(
        b.runner.state().stack.is_empty()
            && b.runner.state().transient_continuous_effects.is_empty(),
        "the Ox does not trigger for an enchantment the opponent controls"
    );
}

// ---- Shape 3: an activated ability makes the creature itself "must be blocked this turn if able"
// (Loathsome Catoblepas). ----
#[test]
fn self_activated_must_be_blocked_this_turn() {
    let Some(db) = db() else { return };
    let card = "Loathsome Catoblepas";
    let (mut control, idle) = board(|s| s.add_real_card(P0, card, Zone::Battlefield, db));
    let other = control.other;
    to_blockers(&mut control.runner, &[idle, other]);
    assert!(legal(&control.runner, &[]), "control: not yet activated");

    let (mut b, catoblepas) = board(|s| s.add_real_card(P0, card, Zone::Battlefield, db));
    b.runner.activate(catoblepas, 0).resolve();
    let (other, b0) = (b.other, b.blockers[0]);
    to_blockers(&mut b.runner, &[catoblepas, other]);
    assert_must_be_blocked(&mut b, card, catoblepas, other);
    assert_expires_with_the_turn(&mut b, card, &[(b0, catoblepas)], &[catoblepas, other]);
}

// ---- Shape 4: a one-shot "All creatures able to block target creature this turn do so" (Taunting
// Challenge, Declare Dominance, Roar of Challenge, Shinen of Life's Roar's channel). ----
#[test]
fn one_shot_all_creatures_able_to_block_target_creature_do_so() {
    let Some(db) = db() else { return };
    for card in [
        "Taunting Challenge",
        "Declare Dominance",
        "Roar of Challenge",
    ] {
        let (mut control, _) = board(|s| s.add_real_card(P0, card, Zone::Hand, db));
        assert_control_free(&mut control, card);

        let (mut b, spell) = board(|s| s.add_real_card(P0, card, Zone::Hand, db));
        b.runner.cast(spell).target_object(b.attacker).resolve();
        let (attacker, other) = (b.attacker, b.other);
        let blocks: Vec<_> = b.blockers.iter().map(|x| (*x, attacker)).collect();
        to_blockers(&mut b.runner, &[attacker, other]);
        assert_all_must_block(&mut b, card, attacker, other);
        assert_expires_with_the_turn(&mut b, card, &blocks, &[attacker, other]);
    }
}

#[test]
fn channel_all_creatures_able_to_block_target_creature_do_so() {
    let Some(db) = db() else { return };
    let card = "Shinen of Life's Roar";
    let (mut control, _) = board(|s| s.add_real_card(P0, card, Zone::Hand, db));
    assert_control_free(&mut control, card);

    let (mut b, shinen) = board(|s| s.add_real_card(P0, card, Zone::Hand, db));
    b.runner
        .activate(shinen, 0)
        .target_object(b.attacker)
        .resolve();
    assert_eq!(
        b.runner.state().objects[&shinen].zone,
        Zone::Graveyard,
        "Channel discards the card as its cost"
    );
    let (attacker, other) = (b.attacker, b.other);
    let blocks: Vec<_> = b.blockers.iter().map(|x| (*x, attacker)).collect();
    to_blockers(&mut b.runner, &[attacker, other]);
    assert_all_must_block(&mut b, card, attacker, other);
    assert_expires_with_the_turn(&mut b, card, &blocks, &[attacker, other]);
}

/// Declare Dominance's "+3/+3" and Roar of Challenge's Ferocious clause ("That creature gains
/// indestructible until end of turn if you control a creature with power 4 or greater").
#[test]
fn all_able_to_block_spells_other_clauses() {
    let Some(db) = db() else { return };
    let (mut b, spell) = board(|s| s.add_real_card(P0, "Declare Dominance", Zone::Hand, db));
    b.runner.cast(spell).target_object(b.attacker).resolve();
    assert_eq!(
        power_toughness(&b.runner, b.attacker),
        (5, 5),
        "Declare Dominance +3/+3"
    );

    // (label, controller of the big creature, power, toughness, expect indestructible)
    for (label, owner, power, toughness, expect) in [
        ("no creature with power 4", P0, 3, 3, false),
        ("opponent's power-4 creature", P1, 4, 4, false),
        (
            "your creature with power 3 and toughness 6",
            P0,
            3,
            6,
            false,
        ),
        ("your creature with power exactly 4", P0, 4, 1, true),
    ] {
        let (mut b, spell) = board(|s| {
            s.add_creature(owner, "Big", power, toughness);
            s.add_real_card(P0, "Roar of Challenge", Zone::Hand, db)
        });
        b.runner.cast(spell).target_object(b.attacker).resolve();
        let attacker = b.attacker;
        let has =
            |r: &GameRunner| r.state().objects[&attacker].has_keyword(&Keyword::Indestructible);
        assert_eq!(has(&b.runner), expect, "Ferocious with {label}");
        if expect {
            let turn = b.runner.state().turn_number;
            drive_until(&mut b.runner, "next turn", |r| r.state().turn_number > turn);
            assert!(!has(&b.runner), "indestructible ends with the turn");
        }
    }
}

// ---- Shape 5: a static ability on the creature itself ("This creature must be blocked if able",
// "All creatures able to block this creature do so": Treeshaker Chimera, Shinen of Life's Roar,
// Raphael, The Foretold Soldier). ----
#[test]
fn static_self_all_creatures_able_to_block_this_creature_do_so() {
    let Some(db) = db() else { return };
    for card in ["Treeshaker Chimera", "Shinen of Life's Roar"] {
        let (mut b, creature) = board(|s| s.add_real_card(P0, card, Zone::Battlefield, db));
        let other = b.other;
        to_blockers(&mut b.runner, &[creature, other]);
        assert_all_must_block(&mut b, card, creature, other);

        // Control: the same card is not attacking, and the filler that attacks is not required.
        let (mut control, _) = board(|s| s.add_real_card(P0, card, Zone::Battlefield, db));
        assert_control_free(&mut control, card);
    }
}

#[test]
fn static_self_must_be_blocked_if_able() {
    let Some(db) = db() else { return };
    for card in ["Raphael, Ninja Destroyer", "The Foretold Soldier"] {
        let (mut b, creature) = board(|s| s.add_real_card(P0, card, Zone::Battlefield, db));
        let other = b.other;
        to_blockers(&mut b.runner, &[creature, other]);
        assert_must_be_blocked(&mut b, card, creature, other);

        let (mut control, _) = board(|s| s.add_real_card(P0, card, Zone::Battlefield, db));
        assert_control_free(&mut control, card);
    }
}

/// The Foretold Soldier's other row clauses: "can't be blocked by more than one creature" and
/// "Whenever this creature deals damage, exile it face down. It becomes foretold."
#[test]
fn foretold_soldier_other_clauses() {
    let Some(db) = db() else { return };
    let card = "The Foretold Soldier";
    let (mut b, soldier) = board(|s| s.add_real_card(P0, card, Zone::Battlefield, db));
    let (other, b0, b1) = (b.other, b.blockers[0], b.blockers[1]);
    to_blockers(&mut b.runner, &[soldier, other]);
    assert!(
        !legal(&b.runner, &[(b0, soldier), (b1, soldier)]),
        "can't be blocked by more than one creature"
    );
    assert!(legal(&b.runner, &[(b0, soldier), (b1, other)]));
    b.runner
        .declare_blockers(&[(b0, soldier), (b1, other)])
        .expect("legal blocks");
    drive_until(&mut b.runner, "damage trigger", |r| {
        r.state().objects[&soldier].zone == Zone::Exile
    });
    let o = &b.runner.state().objects[&soldier];
    assert!(o.face_down, "exiled face down by its own damage trigger");
    assert!(o.foretold, "it becomes foretold");
}

/// Raphael's other row clause: "Enrage — Whenever Raphael is dealt damage, add that much {R}."
#[test]
fn raphael_enrage_adds_that_much_red_mana() {
    let Some(db) = db() else { return };
    let card = "Raphael, Ninja Destroyer";
    let (mut b, (raphael, biter)) = board(|s| {
        (
            s.add_real_card(P0, card, Zone::Battlefield, db),
            s.add_creature(P1, "Biter", 3, 5).id(),
        )
    });
    // The pool's own mana emptied at the end of the first main phase, so the Red left in the second main
    // phase is Enrage's: it survived the end of the combat damage step, end of combat and the phase change.
    to_blockers(&mut b.runner, &[raphael]);
    b.runner
        .declare_blockers(&[(biter, raphael)])
        .expect("legal block");
    drive_until(&mut b.runner, "second main phase", |r| {
        r.state().phase == Phase::PostCombatMain
    });
    assert_eq!(b.runner.state().objects[&raphael].damage_marked, 3);
    assert_eq!(
        b.runner.state().players[0]
            .mana_pool
            .count_color(ManaType::Red),
        3,
        "dealt 3 damage: adds {{R}}{{R}}{{R}} and, as steps end, does not lose it"
    );
}

// ---- Shape 6: an Aura or Equipment gives the creature it is attached to the requirement (Lure,
// Predatory Impetus, Nemesis Mask). ----
#[test]
fn attached_aura_requirement() {
    let Some(db) = db() else { return };
    for (card, all_able) in [("Lure", true), ("Predatory Impetus", false)] {
        let (mut control, _) = board(|s| s.add_real_card(P0, card, Zone::Hand, db));
        assert_control_free(&mut control, card);

        let (mut b, aura) = board(|s| s.add_real_card(P0, card, Zone::Hand, db));
        b.runner.cast(aura).target_object(b.attacker).resolve();
        assert_eq!(
            b.runner.state().objects[&aura]
                .attached_to
                .and_then(|t| t.as_object()),
            Some(b.attacker),
            "{card}: reach: the Aura resolved onto the attacker"
        );
        let (attacker, other) = (b.attacker, b.other);
        to_blockers(&mut b.runner, &[attacker, other]);
        if all_able {
            assert_all_must_block(&mut b, card, attacker, other);
        } else {
            assert_must_be_blocked(&mut b, card, attacker, other);
        }
    }
}

#[test]
fn equipped_creature_requirement() {
    let Some(db) = db() else { return };
    let card = "Nemesis Mask";
    // Control: the Equipment is on the battlefield and never equipped.
    let (mut control, _) = board(|s| s.add_real_card(P0, card, Zone::Battlefield, db));
    assert_control_free(&mut control, card);

    let (mut b, mask) = board(|s| s.add_real_card(P0, card, Zone::Battlefield, db));
    b.runner
        .activate(mask, 0)
        .target_object(b.attacker)
        .resolve();
    assert_eq!(
        b.runner.state().objects[&mask]
            .attached_to
            .and_then(|t| t.as_object()),
        Some(b.attacker),
        "reach: equipped to the attacker"
    );
    let (attacker, other) = (b.attacker, b.other);
    to_blockers(&mut b.runner, &[attacker, other]);
    assert_all_must_block(&mut b, card, attacker, other);
}
