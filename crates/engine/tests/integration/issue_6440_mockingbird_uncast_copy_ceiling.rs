//! Issue #6440: Mockingbird put onto the battlefield via Chord of Calling
//! (never cast) must not treat "mana spent to cast this creature" as open /
//! Chord's payment. Ceiling is the entering object's cast-payment stamp
//! (default 0 when never cast) → `CopyTargetChoice.max_mana_value == Some(0)`.
//!
//! Verbatim Oracle (Scryfall):
//!   Mockingbird — Flying. You may have this creature enter as a copy of any
//!   creature on the battlefield with mana value less than or equal to the
//!   amount of mana spent to cast this creature, except it's a Bird in
//!   addition to its other types and it has flying.
//!   Chord of Calling — Convoke. Search your library for a creature card with
//!   mana value X or less, put it onto the battlefield, then shuffle.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const CHORD_ORACLE: &str = "Convoke (Your creatures can help cast this spell. Each \
creature you tap while casting this spell pays for {1} or one mana of that creature's \
color.)\nSearch your library for a creature card with mana value X or less, put it \
onto the battlefield, then shuffle.";

const MOCKINGBIRD_ORACLE: &str = "Flying\nYou may have this creature enter as a copy of any \
creature on the battlefield with mana value less than or equal to the amount of mana \
spent to cast this creature, except it's a Bird in addition to its other types and it \
has flying.";

const CLONE_ORACLE: &str =
    "You may have this creature enter as a copy of any creature on the battlefield.";

fn chord_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![
            ManaCostShard::X,
            ManaCostShard::Green,
            ManaCostShard::Green,
            ManaCostShard::Green,
        ],
        generic: 0,
    }
}

fn mockingbird_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::X, ManaCostShard::Blue],
        generic: 0,
    }
}

fn add_mana(runner: &mut GameRunner, ty: ManaType, count: usize) {
    for _ in 0..count {
        let unit = ManaUnit::new(ty, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

fn add_bf_creature_with_mv(scenario: &mut GameScenario, name: &str, mv: u32) -> ObjectId {
    scenario
        .add_creature(P0, name, 1, 1)
        .with_mana_cost(ManaCost::generic(mv))
        .id()
}

/// Drive through optional/replacement prompts until CopyTargetChoice, then halt.
fn drive_to_copy_target_choice(runner: &mut GameRunner) -> WaitingFor {
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::CopyTargetChoice { .. } => return runner.state().waiting_for.clone(),
            WaitingFor::ReplacementChoice { .. } => {
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("accept enter-as-copy replacement");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept optional enter-as-copy");
            }
            WaitingFor::SearchChoice { cards, .. } => {
                // Prefer Mockingbird / Clone if present; else first card.
                let pick = cards
                    .iter()
                    .copied()
                    .find(|id| {
                        let name = &runner.state().objects[id].name;
                        name == "Mockingbird" || name == "Clone"
                    })
                    .unwrap_or(cards[0]);
                runner
                    .act(GameAction::SelectCards { cards: vec![pick] })
                    .expect("select tutored creature");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay mana");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                panic!("stack emptied before CopyTargetChoice");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected waiting_for while driving to copy choice: {other:?}"),
        }
    }
    panic!("exhausted drive loop without CopyTargetChoice");
}

/// Chord puts Mockingbird onto BF (never cast) → ceiling Some(0); MV2 excluded.
#[test]
fn chord_uncast_mockingbird_ceiling_is_zero() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mv0 = add_bf_creature_with_mv(&mut scenario, "Mv0 Birdfood", 0);
    let mv2 = add_bf_creature_with_mv(&mut scenario, "Mv2 Threat", 2);

    scenario
        .add_spell_to_library_top(P0, "Mockingbird", false)
        .as_creature()
        .with_mana_cost(mockingbird_cost())
        .from_oracle_text(MOCKINGBIRD_ORACLE);

    let mut chord =
        scenario.add_spell_to_hand_from_oracle(P0, "Chord of Calling", true, CHORD_ORACLE);
    chord.with_mana_cost(chord_cost());
    chord.from_oracle_text_with_keywords(&["Convoke"], CHORD_ORACLE);
    let chord_id = chord.id();

    let mut runner = scenario.build();
    // X=2 covers Mockingbird's printed MV (X+U with X=0 is MV1, but library card
    // uses ManaCost with X shard — effective MV for search uses concretized
    // value; set library card generic MV via cost: use fixed MV 2 for search).
    // Ensure Mockingbird's mana_cost reports MV ≤ X. X-cost cards report MV 0
    // for the variable part when not on stack — use generic(2) for search match.
    let mockingbird_id = *runner.state().players[0]
        .library
        .iter()
        .find(|id| runner.state().objects[id].name == "Mockingbird")
        .expect("Mockingbird in library");
    runner
        .state_mut()
        .objects
        .get_mut(&mockingbird_id)
        .unwrap()
        .mana_cost = ManaCost::generic(2);

    add_mana(&mut runner, ManaType::Green, 5); // X=2 + GGG
    let _ = runner.cast(chord_id).x(2).resolve();
    let waiting = drive_to_copy_target_choice(&mut runner);

    match waiting {
        WaitingFor::CopyTargetChoice {
            max_mana_value,
            valid_targets,
            source_id,
            ..
        } => {
            assert_eq!(
                max_mana_value,
                Some(0),
                "uncast Mockingbird ceiling must be Some(0)"
            );
            assert_eq!(
                runner.state().objects[&source_id].mana_spent_to_cast_amount,
                0,
                "Chord-tutored Mockingbird must not carry a cast stamp"
            );
            assert!(
                valid_targets.contains(&mv0),
                "MV 0 must be legal; got {valid_targets:?}"
            );
            assert!(
                !valid_targets.contains(&mv2),
                "MV 2 must be excluded; got {valid_targets:?}"
            );
        }
        other => panic!("expected CopyTargetChoice, got {other:?}"),
    }
}

/// Cast Mockingbird paying 2 mana → ceiling Some(2).
#[test]
fn cast_mockingbird_ceiling_matches_stamp() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mv2 = add_bf_creature_with_mv(&mut scenario, "Mv2", 2);
    let mv3 = add_bf_creature_with_mv(&mut scenario, "Mv3", 3);

    let mut bird =
        scenario.add_spell_to_hand_from_oracle(P0, "Mockingbird", false, MOCKINGBIRD_ORACLE);
    bird.as_creature().with_mana_cost(mockingbird_cost());
    let bird_id = bird.id();

    let mut runner = scenario.build();
    // Pay X=1 + {U} = 2 mana total.
    add_mana(&mut runner, ManaType::Blue, 1);
    add_mana(&mut runner, ManaType::Colorless, 1);

    let _ = runner.cast(bird_id).x(1).resolve();
    let waiting = drive_to_copy_target_choice(&mut runner);

    match waiting {
        WaitingFor::CopyTargetChoice {
            max_mana_value,
            valid_targets,
            source_id,
            ..
        } => {
            assert_eq!(
                runner.state().objects[&source_id].mana_spent_to_cast_amount,
                2,
                "cast finalization must stamp spent mana"
            );
            assert_eq!(max_mana_value, Some(2));
            assert!(valid_targets.contains(&mv2));
            assert!(!valid_targets.contains(&mv3));
        }
        other => panic!("expected CopyTargetChoice, got {other:?}"),
    }
}

/// Clone put onto BF without cast: no mana_value_limit → MV2 still legal.
#[test]
fn clone_uncast_has_no_ceiling() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mv2 = add_bf_creature_with_mv(&mut scenario, "Mv2", 2);

    scenario
        .add_spell_to_library_top(P0, "Clone", false)
        .as_creature()
        .with_mana_cost(ManaCost::generic(3))
        .from_oracle_text(CLONE_ORACLE);

    let mut chord =
        scenario.add_spell_to_hand_from_oracle(P0, "Chord of Calling", true, CHORD_ORACLE);
    chord.with_mana_cost(chord_cost());
    chord.from_oracle_text_with_keywords(&["Convoke"], CHORD_ORACLE);
    let chord_id = chord.id();

    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Green, 6); // X=3 + GGG
    let _ = runner.cast(chord_id).x(3).resolve();
    let waiting = drive_to_copy_target_choice(&mut runner);

    match waiting {
        WaitingFor::CopyTargetChoice {
            max_mana_value,
            valid_targets,
            ..
        } => {
            assert_eq!(max_mana_value, None, "Clone has no spent-mana ceiling");
            assert!(
                valid_targets.contains(&mv2),
                "Clone without limit must allow MV 2"
            );
        }
        other => panic!("expected CopyTargetChoice, got {other:?}"),
    }
}

/// Reach-guard: Mockingbird still parses AmountSpentToCastSource (parser unchanged).
#[test]
fn mockingbird_oracle_parses_amount_spent_limit() {
    use engine::parser::oracle::parse_oracle_text;
    use engine::types::ability::{CopyManaValueLimit, Effect};

    let parsed = parse_oracle_text(MOCKINGBIRD_ORACLE, "Mockingbird", &[], &[], &[]);
    let has_limit = parsed.replacements.iter().any(|rd| {
        let mut cursor = rd.execute.as_deref();
        while let Some(def) = cursor {
            if let Effect::BecomeCopy {
                mana_value_limit: Some(CopyManaValueLimit::AmountSpentToCastSource),
                ..
            } = &*def.effect
            {
                return true;
            }
            cursor = def.sub_ability.as_deref();
        }
        false
    });
    assert!(
        has_limit,
        "Mockingbird must parse AmountSpentToCastSource; got {:#?}",
        parsed.replacements
    );
}
