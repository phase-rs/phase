//! Glen Elendra's Answer — "Counter all spells your opponents control and all
//! abilities your opponents control. Create a 1/1 blue and black Faerie creature
//! token with flying for each spell and ability countered this way."
//!
//! Two production seams under test, both driven through the real cast pipeline
//! (`GameRunner::cast(..).resolve()`):
//!
//!   * PARSER (parser/oracle_effect/mod.rs, `try_parse_counter_all_conjunction`)
//!     — the "counter all A and all B" compound must lower to ONE
//!     `CounterAll { Or[spell-leg, ability-leg] }`, so the ability conjunct is
//!     actually countered at runtime instead of being dropped to
//!     `Effect::Unimplemented`.
//!   * ENGINE (game/effects/mod.rs, `affected_objects_from_events`) — the
//!     "for each spell AND ability countered this way" tracked set must read
//!     `SpellCountered` (CR 701.6a/113.9/608.2c), not `ZoneChanged`: a countered
//!     ability emits `SpellCountered` but no `ZoneChanged` (CR 405.1), so the
//!     old ZoneChanged-only path undercounted abilities.
//!
//! CR 113.9: activated/triggered abilities on the stack can be countered by
//! effects that specifically counter abilities. CR 608.2c: "for each spell and
//! ability countered this way" counts exactly the objects this instruction
//! countered.
//!
//! Test of Talents is verified UNAFFECTED: its
//! `FilteredTrackedSetSize { caused_by: Exiled }` excludes Counter members
//! because `this_way_cause_for_effect(Counter) => None`, so a Counter never
//! stamps an `Exiled` cause.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{Effect, ResolvedAbility};
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::PlayerId;

const GLEN: &str = "This spell can't be countered.\nCounter all spells your opponents control and all abilities your opponents control. Create a 1/1 blue and black Faerie creature token with flying for each spell and ability countered this way.";
const SWIFT_SILENCE: &str =
    "Counter all other spells. Draw a card for each spell countered this way.";

/// Push a noncreature (instant) spell onto the stack under `controller`.
fn push_spell(runner: &mut GameRunner, controller: PlayerId, card: u64) -> ObjectId {
    let id = create_object(
        runner.state_mut(),
        CardId(card),
        controller,
        format!("Stack Spell {card}"),
        Zone::Stack,
    );
    if let Some(obj) = runner.state_mut().objects.get_mut(&id) {
        obj.card_types.core_types = vec![CoreType::Instant];
    }
    runner.state_mut().stack.push_back(StackEntry {
        id,
        source_id: id,
        controller,
        kind: StackEntryKind::Spell {
            card_id: CardId(card),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    id
}

/// Push a spell COPY (is_copy = true, is_token = false — the shape
/// `Effect::CastCopyOfCard` produces) onto the stack under `controller`.
fn push_copy_spell(runner: &mut GameRunner, controller: PlayerId, card: u64) -> ObjectId {
    let id = push_spell(runner, controller, card);
    if let Some(obj) = runner.state_mut().objects.get_mut(&id) {
        obj.is_copy = true;
    }
    id
}

/// Push an activated ability onto the stack under `controller`, backed by a
/// battlefield source permanent. Returns the ability's stack-entry id.
fn push_activated_ability(runner: &mut GameRunner, controller: PlayerId, card: u64) -> ObjectId {
    let source = create_object(
        runner.state_mut(),
        CardId(card),
        controller,
        format!("Ability Source {card}"),
        Zone::Battlefield,
    );
    let ability_id = ObjectId(90_000 + card);
    runner.state_mut().stack.push_back(StackEntry {
        id: ability_id,
        source_id: source,
        controller,
        kind: StackEntryKind::ActivatedAbility {
            source_id: source,
            ability: ResolvedAbility::new(
                Effect::Unimplemented {
                    name: "test_ability".to_string(),
                    description: None,
                },
                vec![],
                source,
                controller,
            ),
        },
    });
    ability_id
}

fn spell_countered_ids(events: &[GameEvent]) -> Vec<ObjectId> {
    events
        .iter()
        .filter_map(|e| match e {
            GameEvent::SpellCountered { object_id, .. } => Some(*object_id),
            _ => None,
        })
        .collect()
}

fn count_faeries(state: &engine::types::game_state::GameState, controller: PlayerId) -> usize {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| obj.is_token && obj.controller == controller && obj.name == "Faerie")
        .count()
}

fn cast_glen(runner: &mut GameRunner) -> engine::game::scenario::CastOutcome {
    let glen = runner
        .state()
        .objects
        .values()
        .find(|o| o.name == "Glen Elendra's Answer")
        .expect("Glen in hand")
        .id;
    runner.cast(glen).resolve()
}

fn scenario_with_glen() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_spell_to_hand_from_oracle(P0, "Glen Elendra's Answer", true, GLEN);
    scenario
}

/// STEP 1 discriminator: the ability conjunct must actually be countered at
/// runtime. Revert `try_parse_counter_all_conjunction` and the second conjunct
/// lowers to `Effect::Unimplemented`, so the opponent ability is never countered
/// and survives — this assertion flips. Reach-guard: the opponent SPELL is
/// countered in the same test (proving the input reached a real counter).
#[test]
fn glen_counters_opponent_spell_and_ability() {
    let mut runner = scenario_with_glen().build();
    let opp_spell = push_spell(&mut runner, P1, 501);
    let opp_ability = push_activated_ability(&mut runner, P1, 601);

    let outcome = cast_glen(&mut runner);
    let countered = spell_countered_ids(outcome.events());

    // Reach-guard: the spell leg fired (input reached a real counter).
    assert!(
        countered.contains(&opp_spell),
        "opponent spell must be countered (reach-guard): {countered:?}"
    );
    // The load-bearing assertion (STEP 1): the ability conjunct was countered.
    assert!(
        countered.contains(&opp_ability),
        "opponent ability must be countered — reverting the parser handler drops \
         it to Unimplemented and this fails: {countered:?}"
    );
    assert!(
        outcome.state().stack.is_empty(),
        "both opponent objects must leave the stack: {:?}",
        outcome.state().stack
    );
}

/// STEP 2 discriminator: the Faerie count reads `SpellCountered`, so it counts
/// the countered ability. Revert the `Counter/CounterAll` arm in
/// `affected_objects_from_events` and the tracked set falls back to the
/// `ZoneChanged`-only path, which omits the ability (no `ZoneChanged`) — the
/// count drops from 2 to 1 and this fails.
#[test]
fn glen_faerie_count_equals_spell_plus_ability() {
    let mut runner = scenario_with_glen().build();
    push_spell(&mut runner, P1, 502);
    push_activated_ability(&mut runner, P1, 602);

    let outcome = cast_glen(&mut runner);
    assert_eq!(
        count_faeries(outcome.state(), P0),
        2,
        "1 spell + 1 ability countered ⇒ 2 Faerie tokens (CR 608.2c)"
    );
}

/// CONSTRAINT 2 dedup lock: a countered spell emits BOTH `SpellCountered` and a
/// stack->graveyard `ZoneChanged`, but the arm reads only `SpellCountered`, so
/// two countered spells yield exactly 2 (not 4). This is the direct proof the
/// arm is exclusive and does not double-count into the raw `tracked_object_sets`
/// Vec.
#[test]
fn glen_two_spells_count_two_not_four() {
    let mut runner = scenario_with_glen().build();
    let s1 = push_spell(&mut runner, P1, 503);
    let s2 = push_spell(&mut runner, P1, 504);

    let outcome = cast_glen(&mut runner);
    let countered = spell_countered_ids(outcome.events());

    // Each countered spell contributes exactly one SpellCountered id.
    assert!(countered.contains(&s1) && countered.contains(&s2));
    assert_eq!(
        count_faeries(outcome.state(), P0),
        2,
        "two countered spells ⇒ exactly 2 Faeries, NOT 4 (SpellCountered-only, \
         no double-count with the co-emitted ZoneChanged)"
    );
}

/// Multi-authority hostile: opponent spell + opponent ability + YOUR ability on
/// the stack. Exactly the two opponent objects are countered (controller scope),
/// your ability survives uncounted, and the Faerie count is 2 (count source is
/// the countered set, not the whole stack). Each negative is paired with a
/// positive reach-guard.
#[test]
fn glen_multi_authority_only_counters_opponents() {
    let mut runner = scenario_with_glen().build();
    let opp_spell = push_spell(&mut runner, P1, 505);
    let opp_ability = push_activated_ability(&mut runner, P1, 605);
    let your_ability = push_activated_ability(&mut runner, P0, 705);

    let outcome = cast_glen(&mut runner);
    let countered = spell_countered_ids(outcome.events());

    // Positive reach-guards: both opponent objects are countered.
    assert!(
        countered.contains(&opp_spell) && countered.contains(&opp_ability),
        "both opponent objects must be countered: {countered:?}"
    );
    // Negatives, each paired with the reach-guard above: YOUR ability is NOT
    // countered, and exactly 2 objects were countered.
    assert!(
        !countered.contains(&your_ability),
        "your own ability must NOT be countered (controller scope): {countered:?}"
    );
    assert_eq!(countered.len(), 2, "exactly 2 countered: {countered:?}");
    assert_eq!(
        count_faeries(outcome.state(), P0),
        2,
        "Faerie count is the countered set (2), not the whole stack (3)"
    );
}

fn scenario_with_swift_silence() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["L1", "L2", "L3", "L4", "L5"]);
    scenario.add_spell_to_hand_from_oracle(P0, "Swift Silence", true, SWIFT_SILENCE);
    scenario
}

fn cast_swift_silence(runner: &mut GameRunner) -> engine::game::scenario::CastOutcome {
    let ss = runner
        .state()
        .objects
        .values()
        .find(|o| o.name == "Swift Silence")
        .expect("Swift Silence in hand")
        .id;
    runner.cast(ss).resolve()
}

/// CONSTRAINT 1(a) — no-regression: Swift Silence ("Counter all other spells.
/// Draw a card for each spell countered this way.") counters N opponent
/// NON-copy spells and draws EXACTLY N. Non-copy spells go to the graveyard
/// (they emit `ZoneChanged`), so this is byte-identical to the pre-Step-2
/// behavior — the class-fix does not regress the existing card.
#[test]
fn swift_silence_counters_noncopy_spells_draws_exactly_n() {
    let mut runner = scenario_with_swift_silence().build();
    for card in [511, 512, 513] {
        push_spell(&mut runner, P1, card);
    }

    let outcome = cast_swift_silence(&mut runner);
    outcome.assert_hand_drawn(P0, 3);
}

/// CONSTRAINT 1(b) — CR 608.2c copy inclusion: a countered spell COPY is counted
/// too ("for each spell countered this way" counts every countered spell). The
/// copy is counted via `SpellCountered`. This test also LOCKS the measured
/// engine reality that a countered copy still emits its own stack->graveyard
/// `ZoneChanged` (before the CR 704.5e cease-to-exist SBA) — so copies were
/// already included by both the old ZoneChanged-only path and the new
/// SpellCountered path. Copies are therefore NOT the Step-2 delta; abilities are
/// (see the ability tests above, which DO flip on revert).
#[test]
fn swift_silence_countered_copy_is_counted_cr_608_2c() {
    let mut runner = scenario_with_swift_silence().build();
    let real_spell = push_spell(&mut runner, P1, 521);
    let copy = push_copy_spell(&mut runner, P1, 522);

    let outcome = cast_swift_silence(&mut runner);
    let countered = spell_countered_ids(outcome.events());

    assert!(
        countered.contains(&real_spell) && countered.contains(&copy),
        "both the real spell and the copy must be countered: {countered:?}"
    );
    // Correct behavior (CR 608.2c): both countered spells are counted ⇒ draw 2.
    outcome.assert_hand_drawn(P0, 2);

    // Measured-reality lock: the countered copy DID emit a graveyard ZoneChanged,
    // so it is not the Step-2 delta (both old and new count it). Abilities are.
    let copy_zonechanged = outcome.events().iter().any(|e| {
        matches!(
            e,
            GameEvent::ZoneChanged { object_id, to: Zone::Graveyard, .. } if *object_id == copy
        )
    });
    assert!(
        copy_zonechanged,
        "a countered copy briefly enters the graveyard (CR 704.5e), emitting a \
         ZoneChanged — copies were already counted by the old path"
    );
}
