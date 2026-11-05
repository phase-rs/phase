//! Later-instruction "unless" riders on draw-then-discard triggers.
//!
//! Each card below prints "draw a card. Then discard a card unless <predicate>".
//! The "unless" belongs to the discard instruction only: the draw is
//! unconditional, and the predicate is read when the discard instruction is
//! reached during resolution — after every earlier instruction of the same
//! ability has already happened.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 603.5: the "unless" part of a triggered ability is dealt with when
//!     the ability resolves, not when it triggers.
//!   - CR 608.2c: instructions are followed in the order written.
//!   - CR 608.2h: information the effect needs is determined when the effect
//!     is applied.
//!
//! Each card gets the positive and the negative side of its own predicate, and
//! each pair discriminates the specific binding the predicate needs:
//!   - Muse Seeker: mana spent to cast the TRIGGERING spell (not the source).
//!   - Shoreline Looter: graveyard size, reached through real combat damage and
//!     the printed Threshold ability word (which must not become a positive
//!     trigger gate).
//!   - Anthropological Mission: the quest counter placed earlier in the same
//!     resolution counts (second-to-third counter transition).
//!   - Splitskin Doll: "another" excludes the Doll itself (power 2).
//!   - Chakra Meditation: Lesson cards in the graveyard, not merely cards.
//!
//! Every discard prompt is asserted unconditionally, answered through the
//! public action path, and followed by an empty stack at `Priority`.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// A card already in hand, so every discard prompt offers a real choice: with
/// only the freshly drawn card in hand the engine performs a forced discard
/// without prompting.
const KEEPER: &str = "Consider";

const LIBRARY: [&str; 8] = [
    "Island", "Forest", "Swamp", "Mountain", "Plains", "Wastes", "Desert", "Island",
];

fn hand_len(runner: &GameRunner) -> usize {
    runner.state().players[P0.0 as usize].hand.len()
}

fn graveyard_len(runner: &GameRunner) -> usize {
    runner.state().players[P0.0 as usize].graveyard.len()
}

fn pool(blue: usize, colorless: usize) -> Vec<ManaUnit> {
    let mut mana = vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]); blue];
    mana.extend(vec![
        ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![]
        );
        colorless
    ]);
    mana
}

fn blue_cost(generic: u32, blue: usize) -> ManaCost {
    ManaCost::Cost {
        generic,
        shards: vec![ManaCostShard::Blue; blue],
    }
}

/// Divination {2}{U} — verbatim Oracle text. The probe spell for the cast
/// triggers below; its own draw happens only after the trigger resolves.
const DIVINATION: &str = "Draw two cards.";

/// Tidings {3}{U}{U} — verbatim Oracle text.
const TIDINGS: &str = "Draw four cards.";

/// Assert the discard prompt is live for P0 and answer it with the first
/// offered card. Returns the discarded card.
fn answer_required_discard(runner: &mut GameRunner, context: &str) -> ObjectId {
    let WaitingFor::DiscardChoice {
        player,
        cards,
        count,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "{context}: the discard must be live, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "{context}: the controller discards");
    assert_eq!(count, 1, "{context}: discard exactly one card");
    assert!(
        cards.len() >= 2,
        "{context}: the prompt offers a real choice, got {cards:?}"
    );
    let discarded = cards[0];
    runner
        .act(GameAction::SelectCards {
            cards: vec![discarded],
        })
        .expect("discard the prompted card");
    discarded
}

fn assert_settled(runner: &GameRunner, context: &str) {
    assert!(
        runner.state().stack.is_empty(),
        "{context}: stack must be empty"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{context}: expected Priority, got {:?}",
        runner.state().waiting_for
    );
}

// ---------------------------------------------------------------------------
// Cast-trigger harness (Muse Seeker, Anthropological Mission, Chakra Meditation)
// ---------------------------------------------------------------------------

/// Cast `spell` and let its cast trigger resolve. Returns the hand size at
/// stack commit (trigger above the spell) for draw accounting.
///
/// The cast driver resolves the trigger first (it is above the spell) and halts
/// at the discard prompt when the "unless" predicate is false; when the
/// predicate holds it carries on and resolves the spell as well.
fn cast_and_resolve_trigger(runner: &mut GameRunner, spell: ObjectId) -> (usize, WaitingFor) {
    let outcome = runner.cast(spell).resolve();
    let baseline = hand_len(runner) as i64 - outcome.hand_drawn(P0);
    (baseline as usize, outcome.final_waiting_for().clone())
}

// ---------------------------------------------------------------------------
// Muse Seeker
// ---------------------------------------------------------------------------

/// Muse Seeker {1}{U} 1/2 — verbatim Oracle text (Scryfall).
const MUSE_SEEKER: &str = "Opus — Whenever you cast an instant or sorcery spell, draw a card. \
Then discard a card unless five or more mana was spent to cast that spell.";

fn muse_seeker_runner(
    spell_name: &str,
    oracle: &str,
    cost: ManaCost,
    mana: Vec<ManaUnit>,
) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Muse Seeker", 1, 2, MUSE_SEEKER);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, spell_name, false, oracle)
        .with_mana_cost(cost)
        .id();
    scenario.with_mana_pool(P0, mana);
    scenario.with_library_top(P0, &LIBRARY);
    scenario.with_cards_in_hand(P0, &[KEEPER]);
    (scenario.build(), spell)
}

/// Three mana spent on the triggering spell: the discard happens after the
/// unconditional draw, before the spell itself resolves.
#[test]
fn muse_seeker_three_mana_spell_draws_then_discards() {
    let (mut runner, divination) =
        muse_seeker_runner("Divination", DIVINATION, blue_cost(2, 1), pool(1, 2));
    let outcome = runner.cast(divination).resolve();
    // CR 603.5 + CR 608.2c: the trigger's draw has happened; its discard is live.
    outcome.assert_hand_drawn(P0, 1);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "only Divination remains below the resolving trigger"
    );
    let discarded = answer_required_discard(&mut runner, "Muse Seeker, 3 mana spent");
    assert_eq!(runner.state().objects[&discarded].zone, Zone::Graveyard);
    let hand_after_trigger = hand_len(&runner);
    runner.advance_until_stack_empty();
    assert_settled(&runner, "Muse Seeker, 3 mana spent");
    assert_eq!(
        hand_len(&runner),
        hand_after_trigger + 2,
        "Divination draws two"
    );
    assert_eq!(runner.state().objects[&divination].zone, Zone::Graveyard);
}

/// Five mana spent on the triggering spell: the "unless" holds, so the trigger
/// only draws. Muse Seeker itself had no mana spent on it, so a predicate bound
/// to the trigger source instead of the triggering spell would still discard.
#[test]
fn muse_seeker_five_mana_spell_skips_the_discard() {
    let (mut runner, tidings) = muse_seeker_runner("Tidings", TIDINGS, blue_cost(3, 2), pool(2, 3));
    let (baseline, waiting) = cast_and_resolve_trigger(&mut runner, tidings);
    assert!(
        matches!(waiting, WaitingFor::Priority { .. }),
        "five mana spent must skip the discard, got {waiting:?}"
    );
    assert_settled(&runner, "Muse Seeker, 5 mana spent");
    // Trigger draws one, Tidings draws four, nothing discarded.
    assert_eq!(hand_len(&runner), baseline + 5);
    assert_eq!(
        graveyard_len(&runner),
        1,
        "only the resolved Tidings is in the graveyard"
    );
    assert_eq!(runner.state().objects[&tidings].zone, Zone::Graveyard);
}

// ---------------------------------------------------------------------------
// Shoreline Looter
// ---------------------------------------------------------------------------

/// Shoreline Looter {1}{U} 1/1 — verbatim Oracle text (Scryfall).
const SHORELINE_LOOTER: &str = "This creature can't be blocked.\n\
Threshold — Whenever this creature deals combat damage to a player, draw a card. \
Then discard a card unless there are seven or more cards in your graveyard.";

/// Attack P1 with Shoreline Looter and drive through the combat damage step.
/// Returns the runner halted at the first resolution prompt or end of combat.
fn shoreline_combat(graveyard_cards: usize) -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let looter = scenario
        .add_creature_from_oracle(P0, "Shoreline Looter", 1, 1, SHORELINE_LOOTER)
        .id();
    let filler: Vec<&str> = std::iter::repeat_n("Island", graveyard_cards).collect();
    scenario.with_graveyard(P0, &filler);
    scenario.with_library_top(P0, &LIBRARY);
    scenario.with_cards_in_hand(P0, &[KEEPER]);
    let mut runner = scenario.build();
    let p1_life = runner.life(P1);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(looter, AttackTarget::Player(P1))])
        .expect("declare Shoreline Looter as an attacker");
    // CR 510.2 + CR 510.3a: pass through blockers into combat damage; the
    // damage trigger goes on the stack and resolves until it needs a choice
    // or combat ends.
    for _ in 0..32 {
        if matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ) {
            break;
        }
        match runner.state().waiting_for {
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).expect("no blocks");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority through combat");
            }
            _ => break,
        }
    }
    assert_eq!(
        runner.life(P1),
        p1_life - 1,
        "Shoreline Looter dealt combat damage to the player"
    );
    runner
}

/// Six cards in the graveyard: the trigger still fires (Threshold is only an
/// ability word, CR 207.2c — it must not become a positive trigger gate), draws,
/// and the discard is live.
#[test]
fn shoreline_looter_below_threshold_draws_then_discards() {
    let mut runner = shoreline_combat(6);
    // Keeper + the trigger's draw, before the discard prompt.
    assert_eq!(
        hand_len(&runner),
        2,
        "the combat-damage trigger drew a card"
    );
    answer_required_discard(&mut runner, "Shoreline Looter, 6 in graveyard");
    runner.advance_until_stack_empty();
    assert_settled(&runner, "Shoreline Looter, 6 in graveyard");
    assert_eq!(hand_len(&runner), 1);
    assert_eq!(graveyard_len(&runner), 7);
}

/// Seven cards in the graveyard: the "unless" holds, so the trigger only draws.
/// Hoisting the "unless" onto the whole trigger would skip the draw too.
#[test]
fn shoreline_looter_at_threshold_only_draws() {
    let mut runner = shoreline_combat(7);
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::DiscardChoice { .. }),
        "seven cards in the graveyard must skip the discard"
    );
    runner.advance_until_stack_empty();
    assert_settled(&runner, "Shoreline Looter, 7 in graveyard");
    assert_eq!(hand_len(&runner), 2, "the draw is unconditional");
    assert_eq!(graveyard_len(&runner), 7, "nothing was discarded");
}

// ---------------------------------------------------------------------------
// Anthropological Mission
// ---------------------------------------------------------------------------

/// Anthropological Mission {2}{U} — verbatim Oracle text (Scryfall).
const ANTHROPOLOGICAL_MISSION: &str = "Whenever you cast a noncreature spell, put a quest counter \
on this enchantment. Draw a card. Then discard a card unless this enchantment has three or more \
quest counters on it.";

fn quest() -> CounterType {
    CounterType::Generic("quest".to_string())
}

fn mission_runner(starting_counters: u32) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mission = scenario
        .add_enchantment_from_oracle(P0, "Anthropological Mission", ANTHROPOLOGICAL_MISSION)
        .id();
    scenario.with_counter(mission, quest(), starting_counters);
    let divination = scenario
        .add_spell_to_hand_from_oracle(P0, "Divination", false, DIVINATION)
        .with_mana_cost(blue_cost(2, 1))
        .id();
    scenario.with_mana_pool(P0, pool(1, 2));
    scenario.with_library_top(P0, &LIBRARY);
    scenario.with_cards_in_hand(P0, &[KEEPER]);
    (scenario.build(), mission, divination)
}

fn quest_counters(runner: &GameRunner, mission: ObjectId) -> u32 {
    runner.state().objects[&mission]
        .counters
        .get(&quest())
        .copied()
        .unwrap_or(0)
}

/// One counter → two after this resolution's counter: below three, so discard.
#[test]
fn anthropological_mission_second_counter_discards() {
    let (mut runner, mission, divination) = mission_runner(1);
    let outcome = runner.cast(divination).resolve();
    outcome.assert_hand_drawn(P0, 1);
    assert_eq!(quest_counters(&runner, mission), 2);
    answer_required_discard(&mut runner, "Anthropological Mission, 1 -> 2 counters");
    runner.advance_until_stack_empty();
    assert_settled(&runner, "Anthropological Mission, 1 -> 2 counters");
    assert_eq!(quest_counters(&runner, mission), 2);
}

/// Two counters when the ability triggers and resolves; the ability's own first
/// instruction adds the third. CR 608.2c + CR 608.2h: the "unless" is checked
/// when the discard is reached, after that counter, so it holds and the
/// discard is skipped. A predicate checked before the counter (on the trigger)
/// would see two and discard.
#[test]
fn anthropological_mission_third_counter_from_the_same_resolution_skips_the_discard() {
    let (mut runner, mission, divination) = mission_runner(2);
    let (baseline, waiting) = cast_and_resolve_trigger(&mut runner, divination);
    assert!(
        matches!(waiting, WaitingFor::Priority { .. }),
        "the third counter placed this resolution must skip the discard, got {waiting:?}"
    );
    assert_settled(&runner, "Anthropological Mission, 2 -> 3 counters");
    assert_eq!(quest_counters(&runner, mission), 3);
    // Trigger draws one, Divination draws two, nothing discarded.
    assert_eq!(hand_len(&runner), baseline + 3);
    assert_eq!(
        graveyard_len(&runner),
        1,
        "only Divination is in the graveyard"
    );
}

// ---------------------------------------------------------------------------
// Splitskin Doll
// ---------------------------------------------------------------------------

/// Splitskin Doll {1}{W} 2/1 Artifact Creature — verbatim Oracle text (Scryfall).
const SPLITSKIN_DOLL: &str = "When this creature enters, draw a card. Then discard a card unless \
you control another creature with power 2 or less.";

/// Cast Splitskin Doll alongside `others` (controller, power, toughness) and
/// resolve it and its ETB trigger up to the first resolution prompt.
fn cast_splitskin_doll(
    others: &[(PlayerId, i32, i32)],
) -> (GameRunner, ObjectId, WaitingFor, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let doll = scenario
        .add_creature_to_hand_from_oracle(P0, "Splitskin Doll", 2, 1, SPLITSKIN_DOLL)
        .as_artifact_creature()
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::White],
        })
        .id();
    for &(controller, power, toughness) in others {
        scenario.add_vanilla(controller, power, toughness);
    }
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    scenario.with_library_top(P0, &LIBRARY);
    scenario.with_cards_in_hand(P0, &[KEEPER]);
    let mut runner = scenario.build();
    let outcome = runner.cast(doll).resolve();
    let drawn = outcome.hand_drawn(P0) as usize;
    let waiting = outcome.final_waiting_for().clone();
    assert_eq!(runner.state().objects[&doll].zone, Zone::Battlefield);
    (runner, doll, waiting, drawn)
}

/// The Doll alone (power 2) plus an opponent's 1/1: "another creature" you
/// control excludes the Doll itself, and the opponent's creature is not yours,
/// so the discard is live.
#[test]
fn splitskin_doll_alone_discards_because_it_is_not_another_creature() {
    let (mut runner, _, _, drawn) = cast_splitskin_doll(&[(P1, 1, 1)]);
    assert_eq!(drawn, 1, "the ETB draw is unconditional");
    answer_required_discard(&mut runner, "Splitskin Doll alone");
    runner.advance_until_stack_empty();
    assert_settled(&runner, "Splitskin Doll alone");
    assert_eq!(hand_len(&runner), 1);
}

/// Another creature you control, but with power 3: the discard is live.
#[test]
fn splitskin_doll_with_a_power_three_creature_discards() {
    let (mut runner, _, _, drawn) = cast_splitskin_doll(&[(P0, 3, 3)]);
    assert_eq!(drawn, 1);
    answer_required_discard(&mut runner, "Splitskin Doll with a 3/3");
    runner.advance_until_stack_empty();
    assert_settled(&runner, "Splitskin Doll with a 3/3");
    assert_eq!(hand_len(&runner), 1);
}

/// Another creature you control with power 2: the "unless" holds, so the ETB
/// only draws.
#[test]
fn splitskin_doll_with_another_small_creature_only_draws() {
    let (runner, _, waiting, drawn) = cast_splitskin_doll(&[(P0, 2, 2)]);
    assert!(
        matches!(waiting, WaitingFor::Priority { .. }),
        "another power-2 creature must skip the discard, got {waiting:?}"
    );
    assert_settled(&runner, "Splitskin Doll with a 2/2");
    assert_eq!(drawn, 1, "the ETB draw is unconditional");
    assert_eq!(hand_len(&runner), 2);
    assert_eq!(graveyard_len(&runner), 0, "nothing was discarded");
}

// ---------------------------------------------------------------------------
// Chakra Meditation
// ---------------------------------------------------------------------------

/// Chakra Meditation {2}{U} — verbatim Oracle text (Scryfall).
const CHAKRA_MEDITATION: &str = "When this enchantment enters, return up to one target instant \
or sorcery card from your graveyard to your hand.\n\
Whenever you cast an instant or sorcery spell, draw a card. Then discard a card unless there \
are three or more Lesson cards in your graveyard.";

/// Real Lesson sorceries (CR 205.3k: Lesson is a spell type).
const LESSONS: [&str; 3] = [
    "Environmental Sciences",
    "Introduction to Annihilation",
    "Expanded Anatomy",
];

fn chakra_runner(
    graveyard_lessons: usize,
    graveyard_other: usize,
    exiled_lessons: usize,
) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Chakra Meditation", CHAKRA_MEDITATION);
    for name in LESSONS.iter().take(graveyard_lessons) {
        scenario
            .add_spell_to_graveyard(P0, name, false)
            .with_subtypes(vec!["Lesson"]);
    }
    for _ in 0..graveyard_other {
        scenario.add_spell_to_graveyard(P0, "Opt", true);
    }
    for name in LESSONS.iter().take(exiled_lessons) {
        scenario
            .add_spell_to_exile(P0, name, false)
            .with_subtypes(vec!["Lesson"]);
    }
    let divination = scenario
        .add_spell_to_hand_from_oracle(P0, "Divination", false, DIVINATION)
        .with_mana_cost(blue_cost(2, 1))
        .id();
    scenario.with_mana_pool(P0, pool(1, 2));
    scenario.with_library_top(P0, &LIBRARY);
    scenario.with_cards_in_hand(P0, &[KEEPER]);
    (scenario.build(), divination)
}

/// Two Lessons among six graveyard cards, plus a third Lesson in exile: the
/// predicate counts only Lesson cards, and only in the graveyard, so the
/// discard is live.
#[test]
fn chakra_meditation_two_graveyard_lessons_discards() {
    let (mut runner, divination) = chakra_runner(2, 4, 1);
    assert_eq!(graveyard_len(&runner), 6);
    let outcome = runner.cast(divination).resolve();
    outcome.assert_hand_drawn(P0, 1);
    answer_required_discard(&mut runner, "Chakra Meditation, 2 Lessons");
    runner.advance_until_stack_empty();
    assert_settled(&runner, "Chakra Meditation, 2 Lessons");
    // Six seeded + the discard + the resolved Divination.
    assert_eq!(graveyard_len(&runner), 8);
}

/// Three Lessons in the graveyard: the "unless" holds, so the trigger only
/// draws.
#[test]
fn chakra_meditation_three_graveyard_lessons_skips_the_discard() {
    let (mut runner, divination) = chakra_runner(3, 0, 0);
    let (baseline, waiting) = cast_and_resolve_trigger(&mut runner, divination);
    assert!(
        matches!(waiting, WaitingFor::Priority { .. }),
        "three Lessons in the graveyard must skip the discard, got {waiting:?}"
    );
    assert_settled(&runner, "Chakra Meditation, 3 Lessons");
    // Trigger draws one, Divination draws two, nothing discarded.
    assert_eq!(hand_len(&runner), baseline + 3);
    assert_eq!(graveyard_len(&runner), 4, "three Lessons + Divination");
}
