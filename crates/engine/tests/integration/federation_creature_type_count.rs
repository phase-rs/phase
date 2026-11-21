//! Federation creature-type counts (TRK) — runtime pipeline coverage.
//!
//! * Sarek, Federation Ambassador: "Federation — At the beginning of combat on
//!   your turn, draw a card for every five creature types among non-Borg
//!   creatures you control. If you draw three or more cards this way, put two
//!   +1/+1 counters on each creature you control."
//! * Infinite Diversity: "Federation — Exile target instant or sorcery card
//!   from your graveyard. Copy that card X times, where X is the number of
//!   creature types among non-Borg creatures you control. You may cast the
//!   copies without paying their mana costs. Exile Infinite Diversity."
//! * Mnemonic Deluge (same copy-N-times class, fixed count): "Exile target
//!   instant or sorcery card from a graveyard. Copy that card three times. You
//!   may cast the copies without paying their mana costs. Exile Mnemonic
//!   Deluge."
//!
//! CR 205.3m: creature types. CR 702.73a: a changeling is every creature type,
//! so it is a Borg and "non-Borg creatures" excludes it. CR 107.1a: "for every
//! five" counts complete groups (rounded down). CR 707.12 + CR 707.12a: each
//! card copy may be cast individually.
//!
//! Static discrimination expectations (not executed revert probes):
//!  * Dropping the "for every" count binder (`oracle_util::parse_count_binder`)
//!    or the creature-type arm of the for-each reader leaves Sarek's draw
//!    `Unimplemented` — the shared Draw reach guard fails, including the
//!    zero-draw case; the positive draw outcomes also drop to 0.
//!  * Restricting `parse_you_draw_this_way_condition` to "one or more" again
//!    drops the "three or more" gate silently, so the 5-type case gains counters.
//!  * Dropping the Case 1 multiplicity carry in `fold_cast_copy_of_card_defs`
//!    makes Infinite Diversity / Mnemonic Deluge offer a single copy.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, CopyChoiceMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::parse_mtgjson_mana_cost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SAREK: &str = "Federation \u{2014} At the beginning of combat on your turn, draw a card for every five creature types among non-Borg creatures you control. If you draw three or more cards this way, put two +1/+1 counters on each creature you control.";

const INFINITE_DIVERSITY: &str = "Federation \u{2014} Exile target instant or sorcery card from your graveyard. Copy that card X times, where X is the number of creature types among non-Borg creatures you control. You may cast the copies without paying their mana costs. Exile Infinite Diversity.";

const MNEMONIC_DELUGE: &str = "Exile target instant or sorcery card from a graveyard. Copy that card three times. You may cast the copies without paying their mana costs. Exile Mnemonic Deluge.";

/// Every creature type any fixture below stamps, plus Borg (the production
/// vocabulary always carries it), so a changeling expands to all of them.
const TYPE_DB: &[&str] = &[
    "Advisor", "Android", "Bear", "Bird", "Borg", "Cat", "Cleric", "Dog", "Elf", "Goblin", "Human",
    "Merfolk", "Rogue", "Soldier", "Vulcan", "Warrior", "Wizard",
];

fn install_type_db(runner: &mut GameRunner) {
    runner.state_mut().all_creature_types = TYPE_DB.iter().map(|t| t.to_string()).collect();
}

/// Stage a non-Borg creature of `subtypes`, a Borg of a type nobody else has
/// (Wizard), and a changeling — the latter two must contribute nothing.
fn add_population(scenario: &mut GameScenario, creatures: &[&[&str]]) -> Vec<ObjectId> {
    let mut ids = Vec::new();
    for (index, subtypes) in creatures.iter().enumerate() {
        ids.push(
            scenario
                .add_creature(P0, &format!("Crew Member {index}"), 1, 1)
                .with_subtypes(subtypes.to_vec())
                .id(),
        );
    }
    ids.push(
        scenario
            .add_creature(P0, "Borg Drone", 2, 2)
            .with_subtypes(vec!["Borg", "Wizard"])
            .id(),
    );
    ids.push(
        scenario
            .add_creature(P0, "Masked Vandal", 1, 3)
            .with_subtypes(vec!["Shapeshifter"])
            .with_keyword(Keyword::Changeling)
            .id(),
    );
    ids
}

struct SarekRun {
    drawn: usize,
    counters: Vec<u32>,
}

/// Sarek (Vulcan Advisor) plus `creatures`, then advance into beginning of
/// combat and let the trigger resolve.
fn run_sarek(creatures: &[&[&str]]) -> SarekRun {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sarek = scenario
        .add_creature_from_oracle(P0, "Sarek, Federation Ambassador", 0, 5, SAREK)
        .with_subtypes(vec!["Vulcan", "Advisor"])
        .id();
    let mut ids = add_population(&mut scenario, creatures);
    ids.push(sarek);
    scenario.with_library_top(
        P0,
        &[
            "Library A",
            "Library B",
            "Library C",
            "Library D",
            "Library E",
        ],
    );
    let mut runner = scenario.build();
    install_type_db(&mut runner);
    let hand_before = runner.state().players[0].hand.len();

    runner.advance_to_phase(Phase::BeginCombat);
    // CR 603.2 + CR 603.3: entering combat must put Sarek's actual parsed
    // Draw trigger on the stack, even when its count will resolve to zero.
    let trigger = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == sarek)
        .expect("Sarek's beginning-of-combat trigger reached the stack");
    let StackEntryKind::TriggeredAbility { ability, .. } = &trigger.kind else {
        panic!("expected Sarek's triggered ability, got {:?}", trigger.kind);
    };
    assert!(
        matches!(&ability.effect, Effect::Draw { .. }),
        "Sarek's trigger must contain the parsed Draw, got {:?}",
        ability.effect
    );
    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the trigger resolves to a priority window, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());

    let state = runner.state();
    SarekRun {
        drawn: state.players[0].hand.len() - hand_before,
        counters: ids
            .iter()
            .map(|id| {
                state.objects[id]
                    .counters
                    .get(&CounterType::Plus1Plus1)
                    .copied()
                    .unwrap_or(0)
            })
            .collect(),
    }
}

/// 4 distinct non-Borg types (Vulcan, Advisor, Human, Android — Advisor shared
/// by two creatures) → no complete group of five → no draw, no counters. The
/// Borg Drone's Wizard and the changeling's every-type add nothing.
#[test]
fn sarek_with_four_creature_types_draws_nothing() {
    let run = run_sarek(&[&["Human", "Advisor"], &["Android"]]);
    assert_eq!(run.drawn, 0, "floor(4 / 5) = 0 cards");
    assert!(run.counters.iter().all(|&c| c == 0), "{:?}", run.counters);
}

/// 5 distinct non-Borg types → one card; one draw is below "three or more", so
/// no +1/+1 counters.
#[test]
fn sarek_with_five_creature_types_draws_one_without_counters() {
    let run = run_sarek(&[&["Human", "Advisor"], &["Android", "Human"], &["Soldier"]]);
    assert_eq!(run.drawn, 1, "floor(5 / 5) = 1 card");
    assert!(
        run.counters.iter().all(|&c| c == 0),
        "one draw is fewer than three: {:?}",
        run.counters
    );
}

/// 15 distinct non-Borg types → three cards, which meets "three or more", so
/// every creature P0 controls (Borg and changeling included — the rider is
/// "each creature you control") gets two +1/+1 counters.
#[test]
fn sarek_with_fifteen_creature_types_draws_three_and_adds_counters() {
    let run = run_sarek(&[
        &["Human", "Soldier", "Elf"],
        &["Android", "Goblin", "Merfolk"],
        &["Bear", "Bird", "Cat"],
        &["Dog", "Cleric", "Rogue"],
        &["Warrior", "Human"],
    ]);
    assert_eq!(run.drawn, 3, "floor(15 / 5) = 3 cards");
    assert!(
        run.counters.iter().all(|&c| c == 2),
        "three draws → two +1/+1 counters on each creature: {:?}",
        run.counters
    );
}

/// A graveyard sorcery whose copies have a target-free, observable effect.
fn add_gain_life_sorcery(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_graveyard(P0, "Sacred Nectar", false)
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{W}"))
        .from_oracle_text("You gain 4 life.")
        .id()
}

fn p0_life(runner: &GameRunner) -> i32 {
    runner.state().players[0].life
}

/// Pass priority until a non-priority prompt opens or the stack is empty.
fn settle(runner: &mut GameRunner) {
    for _ in 0..60 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            return;
        }
        if runner.state().stack.is_empty() {
            return;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            return;
        }
    }
}

/// Cast `spell` targeting `target` and drive to its first resolution prompt.
fn cast_targeting(runner: &mut GameRunner, spell: ObjectId, target: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast the copy spell");
    // The graveyard card is the only legal target, so the engine may lock it
    // in without a prompt; answer the prompt when it does open.
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .expect("declare the graveyard target");
    }
    assert_eq!(runner.state().stack.len(), 1, "the spell is on the stack");
    settle(runner);
}

/// Answer one CR 707.12a per-copy cast choice: `accept` casts this copy.
fn answer_copy_choice(runner: &mut GameRunner, source: ObjectId, accept: bool) {
    match runner.state().waiting_for.clone() {
        WaitingFor::ChooseFromZoneChoice {
            player,
            cards,
            count,
            up_to,
            ..
        } => {
            assert_eq!(player, P0);
            assert!(up_to, "CR 707.12a: casting each copy is optional");
            assert_eq!(count, 1, "one copy is offered per iteration");
            assert_eq!(cards, vec![source], "the exiled card is the copy source");
        }
        other => panic!("expected a per-copy ChooseFromZoneChoice, got {other:?}"),
    }
    let cards = if accept { vec![source] } else { vec![] };
    runner
        .act(GameAction::SelectCards { cards })
        .expect("answer the copy cast choice");
    settle(runner);
}

/// X = 3 distinct non-Borg types (Human, Soldier, Android; Human shared; the
/// Borg Drone's Wizard and the changeling excluded) → three copies, each
/// offered individually. Casting two Sacred Nectars and declining one gains
/// 8 life; the exiled card stays in exile and Infinite Diversity exiles itself.
#[test]
fn infinite_diversity_offers_one_copy_per_non_borg_creature_type() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Infinite Diversity", false, INFINITE_DIVERSITY)
        .id();
    let nectar = add_gain_life_sorcery(&mut scenario);
    add_population(
        &mut scenario,
        &[&["Human", "Soldier"], &["Android", "Human"]],
    );
    let mut runner = scenario.build();
    install_type_db(&mut runner);
    let life_before = p0_life(&runner);

    cast_targeting(&mut runner, spell, nectar);
    answer_copy_choice(&mut runner, nectar, true);
    answer_copy_choice(&mut runner, nectar, false);
    answer_copy_choice(&mut runner, nectar, true);

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "exactly three copy choices, then priority; got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        p0_life(&runner),
        life_before + 8,
        "two of three copies cast"
    );
    assert_eq!(runner.state().objects[&nectar].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Exile);
}

/// X = 0 (only a Borg and a changeling) → no copy is offered; the card is still
/// exiled and so is Infinite Diversity.
#[test]
fn infinite_diversity_with_no_non_borg_types_makes_no_copies() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Infinite Diversity", false, INFINITE_DIVERSITY)
        .id();
    let nectar = add_gain_life_sorcery(&mut scenario);
    add_population(&mut scenario, &[]);
    let mut runner = scenario.build();
    install_type_db(&mut runner);
    let life_before = p0_life(&runner);

    cast_targeting(&mut runner, spell, nectar);

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "no copy choice with X = 0, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
    assert_eq!(p0_life(&runner), life_before);
    assert_eq!(runner.state().objects[&nectar].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Exile);
}

/// Fixed-count sibling: Mnemonic Deluge offers three copies; casting all three
/// gains 12 life from Sacred Nectar.
#[test]
fn mnemonic_deluge_offers_three_copies() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Mnemonic Deluge", false, MNEMONIC_DELUGE)
        .id();
    let nectar = add_gain_life_sorcery(&mut scenario);
    let mut runner = scenario.build();
    let life_before = p0_life(&runner);

    cast_targeting(&mut runner, spell, nectar);
    for _ in 0..3 {
        answer_copy_choice(&mut runner, nectar, true);
    }

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "exactly three copy choices, then priority; got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
    assert_eq!(p0_life(&runner), life_before + 12, "three copies cast");
    assert_eq!(runner.state().objects[&nectar].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Exile);
}

/// CR 707.12 + CR 707.12a + CR 601.2c: each accepted Lightning Bolt copy
/// announces its own target. The target-announcement pause must resume only
/// that iteration, with one independent opportunity to decline the middle copy.
#[test]
fn mnemonic_deluge_targeted_copies_resume_with_independent_choices_and_targets() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Mnemonic Deluge", false, MNEMONIC_DELUGE)
        .id();
    let bolt = scenario
        .add_spell_to_graveyard(P0, "Lightning Bolt", true)
        .with_mana_cost(parse_mtgjson_mana_cost("{R}"))
        .from_oracle_text("Lightning Bolt deals 3 damage to any target.")
        .id();
    let mut runner = scenario.build();
    let p0_before = runner.life(P0);
    let p1_before = runner.life(P1);

    cast_targeting(&mut runner, spell, bolt);
    for target in [Some(P0), None, Some(P1)] {
        answer_copy_choice(&mut runner, bolt, target.is_some());
        if let Some(target) = target {
            let WaitingFor::CopyRetarget {
                player,
                mode,
                current_slot,
                target_slots,
                ..
            } = &runner.state().waiting_for
            else {
                panic!(
                    "accepted Bolt copy must announce a target, got {:?}",
                    runner.state().waiting_for
                );
            };
            assert_eq!(*player, P0);
            assert_eq!(*mode, Some(CopyChoiceMode::Announce));
            assert_eq!(*current_slot, 0);
            assert_eq!(target_slots.len(), 1);
            assert!(target_slots[0]
                .legal_alternatives
                .contains(&TargetRef::Player(target)));
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(target)),
                })
                .expect("announce this copy's distinct player target");
            settle(&mut runner);
        }
    }

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "exactly three copy choices and two announcements, then priority; got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.life(P0), p0_before - 3, "first accepted Bolt");
    assert_eq!(runner.life(P1), p1_before - 3, "third accepted Bolt");
    assert_eq!(runner.state().objects[&bolt].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Exile);
}
