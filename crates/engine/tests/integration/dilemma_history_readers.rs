//! Readers of the per-turn dilemma history (TRK "face a dilemma"; you face a
//! dilemma as you choose one or more modes for a spell or ability — needs
//! manual verification, no CR rule names it; the mode choices are CR 700.2a /
//! CR 700.2b).
//!
//! - Captain Benjamin Sisko: "At the beginning of your end step, create a 1/1
//!   red Officer creature token for each dilemma you've faced this turn."
//! - The Caretaker: "At the beginning of each end step, if you faced a dilemma
//!   this turn, target opponent exiles cards from the top of their library
//!   until they exile a nonland card. You may cast that card without paying its
//!   mana cost." The "if" is an intervening-if (CR 603.4); the free cast is
//!   CR 608.2g.
//!
//! Both read the controller's own history, which starts empty each turn.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastOfferKind, CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const SISKO: &str = "At the beginning of your end step, create a 1/1 red Officer creature token for each dilemma you've faced this turn.\n{2}{W}{U}{B}{R}{G}, {T}: Exile the top five cards of your library. You may cast any number of spells with mana value less than or equal to the number of Officers you control from among those cards without paying their mana costs.";
const CARETAKER: &str = "Vigilance, ward {2}\nAt the beginning of each end step, if you faced a dilemma this turn, target opponent exiles cards from the top of their library until they exile a nonland card. You may cast that card without paying its mana cost. (You face a dilemma as you choose one or more modes for a spell or ability.)";
const ABZAN_CHARM: &str = "Choose one —\n• Exile target creature with power 3 or greater.\n• You draw two cards and you lose 2 life.\n• Distribute two +1/+1 counters among one or two target creatures.";
const LIFE_SPELL: &str = "You gain 3 life.";

#[derive(Default)]
struct Plan {
    modes: Vec<Vec<usize>>,
    targets: Vec<TargetRef>,
    optional: Vec<bool>,
}

#[derive(Debug, Default)]
struct Record {
    target_prompts: usize,
    optional_actors: Vec<PlayerId>,
}

/// Answer every prompt from `plan` until the stack is empty at priority.
fn drive(runner: &mut GameRunner, plan: Plan) -> Record {
    let mut modes = plan.modes.into_iter();
    let mut targets = plan.targets.into_iter();
    let mut optional = plan.optional.into_iter();
    let mut record = Record::default();
    for _ in 0..256 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ModeChoice { .. } | WaitingFor::AbilityModeChoice { .. } => {
                let indices = modes.next().expect("unplanned mode prompt");
                runner
                    .act(GameAction::SelectModes { indices })
                    .expect("choose the planned modes");
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                record.target_prompts += 1;
                let target = targets.next().expect("unplanned target prompt");
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(target),
                    })
                    .expect("choose the planned target");
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                record.optional_actors.push(player);
                let accept = optional.next().expect("unplanned optional prompt");
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("answer the optional prompt");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            // The turn walk in S3 passes through combat; nobody attacks or
            // blocks.
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declare no blockers");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return record;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while the stack resolves");
            }
            other => panic!("unexpected prompt {other:?}; record {record:?}"),
        }
    }
    panic!("the stack did not resolve: {record:?}");
}

fn cast(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast the spell");
}

/// Cast an Abzan Charm (its draw mode) from P0's hand: one faced dilemma.
fn face_one_dilemma(runner: &mut GameRunner, charm: ObjectId) {
    cast(runner, charm);
    drive(
        runner,
        Plan {
            modes: vec![vec![1]],
            ..Plan::default()
        },
    );
}

fn officers(runner: &GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|object| object.zone == Zone::Battlefield && object.name == "Officer")
        .count()
}

fn sisko_board(charms: usize) -> (GameRunner, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        for index in 0..8 {
            scenario.add_spell_to_library_top(
                player,
                &format!("Library {}-{index}", player.0),
                false,
            );
        }
    }
    // Summoning sick, so the turn walks to the end step without an attack
    // declaration.
    scenario
        .add_creature_from_oracle(P0, "Captain Benjamin Sisko", 2, 2, SISKO)
        .with_summoning_sickness();
    let charms = (0..charms)
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(P0, "Abzan Charm", true, ABZAN_CHARM)
                .with_mana_cost(ManaCost::zero())
                .id()
        })
        .collect();
    (scenario.build(), charms)
}

/// S1 — one Officer per dilemma faced this turn: two dilemmas, two Officers.
#[test]
fn sisko_creates_an_officer_per_dilemma() {
    let (mut runner, charms) = sisko_board(2);
    for charm in charms {
        face_one_dilemma(&mut runner, charm);
    }
    assert_eq!(officers(&runner), 0, "REACH: none before the end step");
    runner.advance_to_end_step();
    drive(&mut runner, Plan::default());
    assert_eq!(officers(&runner), 2);
}

/// S2 — no dilemma this turn, no Officer.
#[test]
fn sisko_creates_nothing_without_a_dilemma() {
    let (mut runner, _) = sisko_board(0);
    runner.advance_to_end_step();
    drive(&mut runner, Plan::default());
    assert_eq!(officers(&runner), 0);
}

/// S3 — "this turn": a dilemma faced on an earlier turn of P0's is not counted
/// at a later end step of P0's.
#[test]
fn sisko_counts_only_this_turns_dilemmas() {
    let (mut runner, charms) = sisko_board(1);
    face_one_dilemma(&mut runner, charms[0]);
    runner.advance_to_end_step();
    drive(&mut runner, Plan::default());
    assert_eq!(
        officers(&runner),
        1,
        "REACH: the first end step counted one"
    );
    // Through P1's turn to P0's next end step.
    for _ in 0..64 {
        runner.pass_both_players();
        drive(&mut runner, Plan::default());
        if runner.state().active_player == P0 && runner.state().phase == Phase::End {
            break;
        }
    }
    assert_eq!(runner.state().active_player, P0, "REACH: P0's next turn");
    assert_eq!(runner.state().phase, Phase::End, "REACH: its end step");
    drive(&mut runner, Plan::default());
    assert_eq!(officers(&runner), 1, "no Officer for last turn's dilemma");
}

fn caretaker_board() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for index in 0..5 {
        scenario.add_spell_to_library_top(P0, &format!("P0 Library {index}"), false);
    }
    // P1's library top reads: land, land, a sorcery that gains 3 life.
    scenario
        .add_spell_to_library_top(P1, "P1 Life Spell", false)
        .from_oracle_text(LIFE_SPELL);
    scenario.add_land_to_library_top(P1, "P1 Land A");
    scenario.add_land_to_library_top(P1, "P1 Land B");
    scenario
        .add_creature_from_oracle(P0, "The Caretaker", 5, 6, CARETAKER)
        .with_summoning_sickness();
    let charm = scenario
        .add_spell_to_hand_from_oracle(P0, "Abzan Charm", true, ABZAN_CHARM)
        .with_mana_cost(ManaCost::zero())
        .id();
    (scenario.build(), charm)
}

fn exiled(runner: &GameRunner, name: &str) -> bool {
    runner
        .state()
        .objects
        .values()
        .any(|object| object.name == name && object.zone == Zone::Exile)
}

/// C1 — CR 603.4 + CR 608.2g: after a dilemma, at the end step P1 exiles
/// until a nonland card, and P0 may cast that card free (P0 gains 3).
#[test]
fn caretaker_exiles_until_nonland_and_offers_the_free_cast() {
    let (mut runner, charm) = caretaker_board();
    face_one_dilemma(&mut runner, charm);
    let life = runner.life(P0);
    runner.advance_to_end_step();
    let record = drive(
        &mut runner,
        Plan {
            targets: vec![TargetRef::Player(P1)],
            optional: vec![true],
            ..Plan::default()
        },
    );
    assert_eq!(record.optional_actors, vec![P0]);
    assert!(exiled(&runner, "P1 Land A") && exiled(&runner, "P1 Land B"));
    assert_eq!(runner.life(P0), life + 3, "the free cast resolved");
}

/// C2 — CR 603.4: with no dilemma this turn the ability does not trigger.
#[test]
fn caretaker_does_not_trigger_without_a_dilemma() {
    let (mut runner, _) = caretaker_board();
    let life = runner.life(P0);
    runner.advance_to_end_step();
    let record = drive(&mut runner, Plan::default());
    assert_eq!(record.target_prompts, 0);
    assert!(record.optional_actors.is_empty());
    assert!(!exiled(&runner, "P1 Land A"));
    assert_eq!(runner.life(P0), life);
}

/// S4 — Sisko's activated ability: exile the top five; P0 may cast spells
/// among them with mana value ≤ the number of Officers P0 controls (two here)
/// without paying their mana costs (CR 608.2g). The mana value 2 spell is
/// castable; the mana value 3 spell is not.
#[test]
fn sisko_casts_only_spells_within_the_officer_count() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for index in 0..3 {
        scenario.add_spell_to_library_top(P0, &format!("P0 Filler {index}"), false);
    }
    scenario
        .add_spell_to_library_top(P0, "Three Drop", false)
        .with_mana_cost(ManaCost::generic(3))
        .from_oracle_text("You gain 5 life.");
    scenario
        .add_spell_to_library_top(P0, "Two Drop", false)
        .with_mana_cost(ManaCost::generic(2))
        .from_oracle_text("You gain 2 life.");
    let sisko = scenario
        .add_creature_from_oracle(P0, "Captain Benjamin Sisko", 2, 2, SISKO)
        .id();
    for _ in 0..2 {
        scenario
            .add_creature(P0, "Officer", 1, 1)
            .with_subtypes(vec!["Officer"]);
    }
    let mana = [
        ManaType::Colorless,
        ManaType::Colorless,
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
    ]
    .into_iter()
    .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
    .collect();
    scenario.with_mana_pool(P0, mana);
    let mut runner = scenario.build();
    let life = runner.life(P0);
    runner
        .act(GameAction::ActivateAbility {
            source_id: sisko,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Sisko");
    let record = drive_casting_from_among(&mut runner);
    assert!(
        record.offered.iter().any(|name| name == "Two Drop"),
        "REACH: the mana value 2 spell is offered: {record:?}"
    );
    assert!(
        !record.offered.iter().any(|name| name == "Three Drop"),
        "the mana value 3 spell exceeds the Officer count: {record:?}"
    );
    assert_eq!(runner.life(P0), life + 2);
}

/// The names each free-cast window offered, in order.
#[derive(Debug, Default)]
struct FromAmong {
    offered: Vec<String>,
}

/// Resolve Sisko's ability: at each free-cast window record every offered
/// card, cast "Two Drop" once if offered, and otherwise stop casting.
fn drive_casting_from_among(runner: &mut GameRunner) -> FromAmong {
    let mut record = FromAmong::default();
    let mut cast_two_drop = false;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::CastOffer {
                kind: CastOfferKind::FreeCastWindow { candidates, .. },
                ..
            } => {
                let names: Vec<String> = candidates
                    .iter()
                    .map(|id| runner.state().objects[id].name.clone())
                    .collect();
                record.offered.extend(names.iter().cloned());
                let selection = (!cast_two_drop)
                    .then(|| {
                        candidates
                            .iter()
                            .zip(&names)
                            .find(|(_, name)| *name == "Two Drop")
                            .map(|(id, _)| *id)
                    })
                    .flatten();
                cast_two_drop |= selection.is_some();
                runner
                    .act(GameAction::FreeCastWindowChoice { selection })
                    .expect("answer the free-cast window");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return record;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while the stack resolves");
            }
            other => panic!("unexpected prompt {other:?}; record {record:?}"),
        }
    }
    panic!("Sisko's ability did not resolve: {record:?}");
}

/// C3 — "each end step": a dilemma P0 faces on P1's turn (an instant-speed
/// modal spell) makes The Caretaker trigger at P1's end step.
#[test]
fn caretaker_triggers_at_an_opponents_end_step() {
    let (mut runner, charm) = caretaker_board();
    for _ in 0..64 {
        if runner.state().active_player == P1 && runner.state().phase == Phase::PreCombatMain {
            break;
        }
        runner.pass_both_players();
        drive(&mut runner, Plan::default());
    }
    assert_eq!(runner.state().active_player, P1, "REACH: P1's turn");
    // P1 passes priority in its main phase, so P0 may cast the instant.
    if matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1) {
        runner.act(GameAction::PassPriority).expect("P1 passes");
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "REACH: P0 holds priority on P1's turn: {:?}",
        runner.state().waiting_for
    );
    face_one_dilemma(&mut runner, charm);
    let life = runner.life(P0);
    runner.advance_to_end_step();
    assert_eq!(runner.state().active_player, P1, "REACH: P1's end step");
    let record = drive(
        &mut runner,
        Plan {
            optional: vec![true],
            ..Plan::default()
        },
    );
    assert_eq!(record.optional_actors, vec![P0]);
    assert_eq!(runner.life(P0), life + 3, "the free cast resolved");
}
