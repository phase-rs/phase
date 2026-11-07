//! Row 1.B — a die-results-table card must select EXACTLY ONE branch by its
//! rolled value.
//!
//! Farideh's Fireball is the live rules bug this phase fixes. At base its
//! printed table was lost entirely (`RollDie.results` was empty) and both
//! branches survived as UNCONDITIONAL chain siblings, so the spell dealt
//! 2 damage to each player AND 2 damage to each opponent on every resolution,
//! regardless of the roll.
//!
//! The two fixtures below fail at base for DIFFERENT reasons, which is what
//! makes the pair discriminate branch SELECTION rather than merely table
//! PRESENCE:
//!   * low band  — base gives P1 −4 (both branches hit P1); the row requires −2.
//!   * high band — base gives P0 −2 (the "each player" row hit P0); requires 0.
//!
//! A fix that restored the table but not branch selection still fails both.
//!
//! The card is built from VERBATIM Oracle text through the live parser rather
//! than loaded from the committed integration fixture: that fixture is a
//! snapshot of the BASE parse (and does not contain this card at all), so a
//! fixture-backed test could not observe this change.

use engine::game::scenario::{CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameRng, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::{Player, PlayerId};
use engine::types::zones::Zone;

/// Verbatim Oracle text, including the printed em-dash result rows.
const FARIDEHS_FIREBALL: &str = "Farideh's Fireball deals 5 damage to target creature or planeswalker. Roll a d20.\n1\u{2014}9 | Farideh's Fireball deals 2 damage to each player.\n10\u{2014}20 | Farideh's Fireball deals 2 damage to each opponent.";

/// Cast the real spell at a fat creature P1 controls, forcing the die result.
///
/// The RNG is reset AFTER `commit()` and immediately before resolution, so the
/// seed -> face mapping is a property of the documented seed rather than of
/// setup activity that may evolve independently of this card.
fn cast_fireball(seed: u64) -> (CastOutcome, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Toughness 9 so the 5 damage is survivable and readable as marked damage;
    // a creature that died would make "took 5" indistinguishable from "took 7".
    let victim = scenario.add_creature(P1, "Test Victim", 9, 9).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Farideh's Fireball", false, FARIDEHS_FIREBALL)
        .id();

    let mut runner = scenario.build();
    let mut committed = runner.cast(spell).target_object(victim).commit();
    let state = committed.state_mut();
    state.rng_seed = seed;
    state.rng_word_pos = 0;
    state.rng = GameRng::seed_from_u64(seed);
    (committed.resolve(), victim)
}

/// Read the emitted d20 face. This is a POSITIVE REACH-GUARD, not a
/// convenience: a fixture that never rolled cannot satisfy any assertion below.
fn rolled_face(outcome: &CastOutcome) -> Option<u32> {
    outcome.events().iter().find_map(|event| match event {
        GameEvent::DieRolled {
            sides: 20,
            result: Some(result),
            ..
        } => Some(*result as u32),
        _ => None,
    })
}

fn marked_damage(outcome: &CastOutcome, object: ObjectId) -> u32 {
    outcome
        .state()
        .objects
        .get(&object)
        .expect("the targeted creature must still exist")
        .damage_marked
}

#[test]
fn farideh_low_band_damages_every_player_including_its_controller() {
    let (outcome, victim) = cast_fireball(6);
    let face = rolled_face(&outcome);
    // Pinned empirically, so a drift in RNG word consumption fails loudly here
    // rather than silently flipping the fixture into the other band.
    assert_eq!(face, Some(1), "seed 6 must reach its pinned d20 face");
    let face = face.expect("reach-guard: the roll must have happened");
    assert!(
        (1..=9).contains(&face),
        "this fixture must land in the printed 1-9 band, got {face}"
    );

    // SECOND REACH-GUARD: the chain resolved past its first clause.
    assert_eq!(
        marked_damage(&outcome, victim),
        5,
        "the targeted creature must have taken the spell's 5 damage"
    );

    // THE DISCRIMINATOR. `1-9 | ... deals 2 damage to each player.`
    // At base P1 took -4, because the `10-20` opponent row also fired.
    outcome.assert_life_delta(P0, -2);
    outcome.assert_life_delta(P1, -2);
}

#[test]
fn farideh_high_band_spares_its_controller() {
    let (outcome, victim) = cast_fireball(15);
    let face = rolled_face(&outcome);
    assert_eq!(face, Some(16), "seed 15 must reach its pinned d20 face");
    let face = face.expect("reach-guard: the roll must have happened");
    assert!(
        (10..=20).contains(&face),
        "this fixture must land in the printed 10-20 band, got {face}"
    );

    assert_eq!(
        marked_damage(&outcome, victim),
        5,
        "the targeted creature must have taken the spell's 5 damage"
    );

    // THE DISCRIMINATOR. `10-20 | ... deals 2 damage to each OPPONENT.`
    // At base P0 took -2, because the `1-9` each-player row also fired.
    outcome.assert_life_delta(P0, 0);
    outcome.assert_life_delta(P1, -2);
}

// ---------------------------------------------------------------------------
// A replacement's action under the roll reads the amount it replaced.
//
// The d20 face is recorded for the rest of Fireball's resolution (CR 706.3 + CR 706.3b), but a
// replacement that applies to one of the table row's damage events performs its own
// action instead of that event (CR 614.1a, CR 614.6), and its "that many" is that event's
// amount: 2. Neither the face (1 or 16) nor anything else of the resolution around it.
// ---------------------------------------------------------------------------

const DELAYING_SHIELD: &str = "If damage would be dealt to you, put that many delay counters on this enchantment instead.\nAt the beginning of your upkeep, remove all delay counters from this enchantment. For each delay counter removed this way, you lose 1 life unless you pay {1}{W}.";
const FORCE_BUBBLE: &str = "If damage would be dealt to you, put that many depletion counters on this enchantment instead.\nWhen there are four or more depletion counters on this enchantment, sacrifice it.\nAt the beginning of each end step, remove all depletion counters from this enchantment.";
const DOUBLING_SEASON: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.\nIf an effect would put one or more counters on a permanent you control, it puts twice that many of those counters on that permanent instead.";
const PIR: &str = "Partner with Toothy, Imaginary Friend (When this creature enters, target player may put Toothy into their hand from their library, then shuffle.)\nIf one or more counters would be put on a permanent your team controls, that many plus one of each of those kinds of counters are put on that permanent instead.";
const NEFARIOUS_LICH: &str = "If damage would be dealt to you, exile that many cards from your graveyard instead. If you can't, you lose the game.\nIf you would gain life, draw that many cards instead.\nWhen this enchantment leaves the battlefield, you lose the game.";

/// The pinned seeds and the faces they must reach: one per printed row.
const LOW_ROLL: (u64, u32) = (6, 1);
const HIGH_ROLL: (u64, u32) = (15, 16);

/// Fireball cast at a 9/9 P1 controls with `board` added first, resolved through `apply()` up to
/// the first prompt or an empty stack. The RNG is reset exactly where `cast_fireball` resets it.
fn fireball_onto(
    (seed, face): (u64, u32),
    board: impl FnOnce(&mut GameScenario),
) -> (GameRunner, ObjectId, Vec<GameEvent>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P1, "Test Victim", 9, 9).id();
    board(&mut scenario);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Farideh's Fireball", false, FARIDEHS_FIREBALL)
        .id();

    let mut runner = scenario.build();
    let mut events = {
        let mut committed = runner.cast(spell).target_object(victim).commit();
        let state = committed.state_mut();
        state.rng_seed = seed;
        state.rng_word_pos = 0;
        state.rng = GameRng::seed_from_u64(seed);
        committed.events().to_vec()
    };
    settle(&mut runner, &mut events);

    // REACH-GUARDS: the roll happened and landed on the pinned face, and the spell's first
    // clause dealt its 5 to the creature, which no replacement here applies to.
    assert_eq!(
        emitted_face(&events),
        Some(face),
        "seed {seed} must reach its pinned d20 face"
    );
    assert_eq!(runner.state().objects[&victim].damage_marked, 5);
    (runner, victim, events)
}

/// Pass priority until a prompt is up, or until the stack is empty.
fn settle(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    for _ in 0..60 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("pass priority")
                        .events,
                );
            }
            _ => return,
        }
    }
    panic!("the game did not settle");
}

fn emitted_face(events: &[GameEvent]) -> Option<u32> {
    events.iter().find_map(|event| match event {
        GameEvent::DieRolled {
            sides: 20,
            result: Some(result),
            ..
        } => Some(*result as u32),
        _ => None,
    })
}

fn player(runner: &GameRunner, id: PlayerId) -> &Player {
    runner
        .state()
        .players
        .iter()
        .find(|player| player.id == id)
        .expect("the player exists")
}

fn life(runner: &GameRunner, id: PlayerId) -> i32 {
    player(runner, id).life
}

fn counters(runner: &GameRunner, id: ObjectId, kind: &str) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Generic(kind.to_string()))
        .copied()
        .unwrap_or(0)
}

#[track_caller]
fn assert_at_rest(runner: &GameRunner) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
}

/// Delaying Shield: "If damage would be dealt to you, put that many delay counters on this
/// enchantment instead." Either row deals P1 2 damage, so P1 gets 2 delay counters, never the
/// face. The low row also deals P0 its 2, which no replacement touches.
#[test]
fn delaying_shield_counts_the_rows_damage_not_the_d20_face() {
    for (roll, controller_loss) in [(LOW_ROLL, -2), (HIGH_ROLL, 0)] {
        let mut shield = None;
        let (runner, _, _) = fireball_onto(roll, |scenario| {
            shield = Some(
                scenario
                    .add_enchantment_from_oracle(P1, "Delaying Shield", DELAYING_SHIELD)
                    .id(),
            );
        });
        let shield = shield.expect("the shield is on the board");

        assert_eq!(counters(&runner, shield, "delay"), 2, "face {}", roll.1);
        assert_eq!(life(&runner, P1), 20, "the damage was replaced");
        assert_eq!(life(&runner, P0), 20 + controller_loss);
        assert_at_rest(&runner);
    }
}

/// Force Bubble: "put that many depletion counters on this enchantment instead. When there are
/// four or more depletion counters on this enchantment, sacrifice it." Two counters leave it on
/// the battlefield; the high face would have sacrificed it.
#[test]
fn force_bubble_counts_the_rows_damage_not_the_d20_face() {
    for roll in [LOW_ROLL, HIGH_ROLL] {
        let mut bubble = None;
        let (runner, _, _) = fireball_onto(roll, |scenario| {
            bubble = Some(
                scenario
                    .add_enchantment_from_oracle(P1, "Force Bubble", FORCE_BUBBLE)
                    .id(),
            );
        });
        let bubble = bubble.expect("the bubble is on the board");

        assert_eq!(
            runner.state().objects[&bubble].zone,
            Zone::Battlefield,
            "face {}",
            roll.1
        );
        assert_eq!(counters(&runner, bubble, "depletion"), 2, "face {}", roll.1);
        assert_eq!(life(&runner, P1), 20, "the damage was replaced");
        assert_at_rest(&runner);
    }
}

/// Nefarious Lich: "exile that many cards from your graveyard instead. If you can't, you lose the
/// game." The action asks which cards (CR 608.2d), so it pauses under the roll and resumes on
/// the answer: it asks for 2 of the 3, exiles those 2, and P1 has not lost.
#[test]
fn nefarious_lich_exiles_the_rows_damage_across_its_prompt_not_the_d20_face() {
    for roll in [LOW_ROLL, HIGH_ROLL] {
        let (mut runner, _, mut events) = fireball_onto(roll, |scenario| {
            for index in 0..3 {
                scenario.add_creature_to_graveyard(P1, &format!("Filler {index}"), 1, 1);
            }
            scenario.add_enchantment_from_oracle(P1, "Nefarious Lich", NEFARIOUS_LICH);
        });
        let graveyard: Vec<ObjectId> = player(&runner, P1).graveyard.iter().copied().collect();

        let WaitingFor::EffectZoneChoice {
            count,
            min_count,
            cards,
            ..
        } = runner.state().waiting_for.clone()
        else {
            panic!(
                "face {}: expected EffectZoneChoice, got {:?}",
                roll.1,
                runner.state().waiting_for
            );
        };
        assert_eq!(
            (count, min_count, cards.len()),
            (2, 2, 3),
            "face {}",
            roll.1
        );

        events.extend(
            runner
                .act(GameAction::SelectCards {
                    cards: vec![graveyard[0], graveyard[1]],
                })
                .expect("answer the exile prompt")
                .events,
        );
        settle(&mut runner, &mut events);

        assert_eq!(runner.state().objects[&graveyard[0]].zone, Zone::Exile);
        assert_eq!(runner.state().objects[&graveyard[1]].zone, Zone::Exile);
        assert_eq!(runner.state().objects[&graveyard[2]].zone, Zone::Graveyard);
        assert!(!player(&runner, P1).is_eliminated);
        assert_eq!(life(&runner, P1), 20, "the damage was replaced");
        assert_at_rest(&runner);
    }
}

/// Delaying Shield's counters under Doubling Season and Pir, Imaginative Rascal: the action's
/// counter event meets two replacements whose order matters, so P1 orders them (CR 616.1) and
/// the action pauses under the roll. Doubling Season first, then Pir: the shield's 2 become
/// 2 × 2 + 1 = 5.
#[test]
fn delaying_shield_counts_the_rows_damage_across_an_order_choice() {
    for (roll, controller_loss) in [(LOW_ROLL, -2), (HIGH_ROLL, 0)] {
        let mut ids = None;
        let (mut runner, _, mut events) = fireball_onto(roll, |scenario| {
            let shield = scenario
                .add_enchantment_from_oracle(P1, "Delaying Shield", DELAYING_SHIELD)
                .id();
            let season = scenario
                .add_enchantment_from_oracle(P1, "Doubling Season", DOUBLING_SEASON)
                .id();
            scenario.add_creature_from_oracle(P1, "Pir, Imaginative Rascal", 1, 1, PIR);
            ids = Some((shield, season));
        });
        let (shield, season) = ids.expect("the board is built");

        let mut paused_at = Vec::new();
        while let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for {
            let index = candidates
                .iter()
                .position(|candidate| candidate.source_id == season)
                .unwrap_or(0);
            paused_at.push(counters(&runner, shield, "delay"));
            events.extend(
                runner
                    .act(GameAction::ChooseReplacement { index })
                    .expect("order the counter replacements")
                    .events,
            );
            settle(&mut runner, &mut events);
        }

        // REACH-GUARD: the action paused for the order before any counter was put.
        assert_eq!(paused_at, vec![0], "face {}", roll.1);
        assert_eq!(counters(&runner, shield, "delay"), 5, "face {}", roll.1);
        assert_eq!(life(&runner, P1), 20, "the damage was replaced");
        assert_eq!(life(&runner, P0), 20 + controller_loss);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(runner.state().stack.is_empty());
        // Everything `assert_at_rest` checks except the resolution stack. Filed follow-up:
        // once the order is answered, Fireball's die-roll frame stays parked with its one die
        // already settled (`next_index` 1 of 1), and the spell, owed by that frame's
        // completion, is still in the stack zone rather than the graveyard (CR 608.2n). That is
        // the results-table resume after a CR 616.1 order choice, and it is the same with the
        // amount binding removed. When the follow-up lands, this row ends with
        // `assert_at_rest(&runner)` and the spell in the graveyard.
    }
}

/// Building block, not a printed card: a replacement's action whose second step reads "that many"
/// after its first step's prompt has paused it under the roll and the answer has resumed it.
/// No supported printed card reaches this seam today. The nearest are Szadek, Lord of Secrets
/// ("instead put that many +1/+1 counters on Szadek and that player mills that many cards"),
/// whose combat damage never rolls, and Eye for an Eye ("instead that source deals that much
/// damage to you and Eye for an Eye deals that much damage to that source's controller"), whose
/// replacement does not parse to an action yet. The first step is Nefarious Lich's printed
/// "exile that many cards from your graveyard". The draw runs in a later `apply()`, from the
/// continuation the exile prompt parked (CR 608.2c), and still draws the 2 the action owns
/// (CR 614.6, CR 615.5), not the face that resolution re-reads.
#[test]
fn a_replacement_actions_step_after_its_prompt_reads_the_rows_damage() {
    const EXILE_THEN_DRAW: &str = "If damage would be dealt to you, exile that many cards from your graveyard, then draw that many cards instead.";
    for roll in [LOW_ROLL, HIGH_ROLL] {
        let (mut runner, _, mut events) = fireball_onto(roll, |scenario| {
            for index in 0..3 {
                scenario.add_creature_to_graveyard(P1, &format!("Filler {index}"), 1, 1);
            }
            for index in 0..20 {
                scenario.add_card_to_library_top(P1, &format!("Library {index}"));
            }
            scenario.add_enchantment_from_oracle(P1, "Exile-then-draw shield", EXILE_THEN_DRAW);
        });
        let hand_before = player(&runner, P1).hand.len();
        let graveyard: Vec<ObjectId> = player(&runner, P1).graveyard.iter().copied().collect();

        // REACH-GUARD: the action paused on its first step, before it drew anything.
        let WaitingFor::EffectZoneChoice {
            count,
            min_count,
            cards,
            ..
        } = runner.state().waiting_for.clone()
        else {
            panic!(
                "face {}: expected EffectZoneChoice, got {:?}",
                roll.1,
                runner.state().waiting_for
            );
        };
        assert_eq!(
            (count, min_count, cards.len()),
            (2, 2, 3),
            "face {}",
            roll.1
        );

        events.extend(
            runner
                .act(GameAction::SelectCards {
                    cards: vec![graveyard[0], graveyard[1]],
                })
                .expect("answer the exile prompt")
                .events,
        );
        settle(&mut runner, &mut events);

        assert_eq!(runner.state().objects[&graveyard[0]].zone, Zone::Exile);
        assert_eq!(runner.state().objects[&graveyard[1]].zone, Zone::Exile);
        assert_eq!(
            player(&runner, P1).hand.len() - hand_before,
            2,
            "face {}: the resumed draw reads the action's own amount",
            roll.1
        );
        assert_eq!(life(&runner, P1), 20, "the damage was replaced");
        assert_at_rest(&runner);
    }
}
