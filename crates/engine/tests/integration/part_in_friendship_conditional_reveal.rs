//! Runtime regression for Part in Friendship (HOB), driven end-to-end through
//! the real `apply` pipeline (burn spell → SBA lethal damage → dies trigger →
//! RevealUntil with a card-property-driven conditional destination).
//!
//! Part in Friendship (Scryfall/MTGJSON-verified Oracle): "Whenever a nontoken
//! creature you control dies, reveal cards from the top of your library until
//! you reveal a creature card. If its mana value is less than or equal to the
//! number of lands you control, put it onto the battlefield. Otherwise, put it
//! into your hand. Put the rest on the bottom of your library in a random
//! order. This ability triggers only once each turn."
//!
//! CR citations:
//!   * CR 700.4 + CR 111.1 — "dies" = a nontoken creature moves battlefield →
//!     graveyard; the `nontoken` filter (CR 111.1) excludes token deaths.
//!   * CR 701.20a — the reveal-until-a-matching-card dig, with the
//!     non-matching "rest" pile bottomed in a random order.
//!   * CR 202.3 + CR 608.2c — the card-property (mana value) comparison
//!     against a dynamic quantity (the number of lands the controller
//!     controls) that branches the found card's destination.
//!   * CR 608.2c (`TriggerConstraint::OncePerTurn`) — "this ability triggers
//!     only once each turn" is a per-turn triggered-ability frequency
//!     limiter, applied as the ability's own written instruction.
//!
//! Four scenarios, each a distinct discriminating fixture:
//!   T1 cheap creature (MV <= lands controlled) → battlefield, miss → library.
//!   T2 expensive creature (MV > lands controlled) → hand instead.
//!   T3 a second nontoken creature dying THE SAME TURN does not retrigger —
//!      the once-per-turn limiter (the library's second creature card is
//!      untouched after the second death).
//!   T4 a TOKEN creature dying does not trigger the ability at all — paired
//!      with a same-turn nontoken death afterward as the reach-guard (proving
//!      the token death did not consume the once-per-turn budget, so the
//!      absence of a reveal on the token kill is a real filter miss, not an
//!      already-spent trigger).

use super::rules::{GameScenario, Phase, P0};
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::zones::Zone;

const PART_IN_FRIENDSHIP: &str = "Whenever a nontoken creature you control dies, reveal cards from the top of your library until you reveal a creature card. If its mana value is less than or equal to the number of lands you control, put it onto the battlefield. Otherwise, put it into your hand. Put the rest on the bottom of your library in a random order. This ability triggers only once each turn.";

/// "Zap deals 3 damage to target creature." — a zero-cost burn spell used to
/// kill a creature via CR 704.5g (lethal damage) SBA, exercising the real
/// dies-event pipeline (not a raw internal zone move).
const BURN: &str = "Zap deals 3 damage to target creature.";

/// Push a creature type into both `card_types` and `base_card_types` (survives
/// a layer recompute, which reverts `card_types` from `base_card_types` —
/// mirrors `descendants_fury_sacrificed_referent_4795::make_creature`) and set
/// a printed mana value via a generic-only `ManaCost`.
fn make_library_creature(state: &mut GameState, id: ObjectId, mv: u32) {
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types.core_types.push(CoreType::Creature);
    let cost = ManaCost::generic(mv);
    obj.mana_cost = cost.clone();
    obj.base_mana_cost = cost;
}

fn make_library_noncreature(state: &mut GameState, id: ObjectId) {
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Instant);
    obj.base_card_types.core_types.push(CoreType::Instant);
}

/// Cast `burn` at `target`, killing it via CR 704.5g lethal-damage SBA, and
/// drive the resulting dies trigger through to a settled stack.
/// `advance_until_stack_empty` auto-drains `OrderTriggers` and passes
/// priority — Part in Friendship's ability is fully mandatory (no `you may`
/// decision point), so no other prompt can surface here.
fn kill_via_burn(runner: &mut super::rules::GameRunner, burn: ObjectId, target: ObjectId) {
    runner.cast(burn).target_object(target).resolve();
    runner.advance_until_stack_empty();
}

/// T1 — a cheap creature (mana value <= lands controlled) is found and enters
/// the battlefield; the non-creature card dug past goes to the bottom of the
/// library.
#[test]
fn part_in_friendship_cheap_creature_enters_battlefield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Part in Friendship", PART_IN_FRIENDSHIP);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Green);
    let fodder = scenario.add_creature(P0, "Fodder Bear", 2, 2).id();
    let burn = scenario
        .add_spell_to_hand_from_oracle(P0, "Zap", true, BURN)
        .with_mana_cost(ManaCost::zero())
        .id();

    // Library, top to bottom: a non-creature miss, then a 2-mana-value
    // creature (<= the 2 lands controlled).
    let miss = scenario.add_card_to_library_top(P0, "Library Instant");
    let cheap_creature = scenario.add_card_to_library_top(P0, "Library Cheap Bear");

    let mut runner = scenario.build();
    {
        let s = runner.state_mut();
        let lib = &mut s.players[0].library;
        lib.retain(|&id| id != miss && id != cheap_creature);
        lib.insert(0, cheap_creature);
        lib.insert(0, miss);
    }
    make_library_noncreature(runner.state_mut(), miss);
    make_library_creature(runner.state_mut(), cheap_creature, 2);

    kill_via_burn(&mut runner, burn, fodder);

    let s = runner.state();
    // Reach guard: the fodder creature actually died.
    assert_eq!(
        s.objects[&fodder].zone,
        Zone::Graveyard,
        "Fodder Bear must die from the burn spell's lethal damage"
    );
    // Primary assertion: mana value 2 <= 2 lands controlled → battlefield.
    assert_eq!(
        s.objects[&cheap_creature].zone,
        Zone::Battlefield,
        "a creature card with mana value <= lands controlled must enter the battlefield"
    );
    // The dug-past non-creature miss goes to the bottom of the library, not
    // the graveyard or hand.
    assert_eq!(
        s.objects[&miss].zone,
        Zone::Library,
        "the non-creature card revealed along the way returns to the library"
    );
}

/// T2 — an expensive creature (mana value > lands controlled) is found and
/// goes to hand instead of the battlefield.
#[test]
fn part_in_friendship_expensive_creature_goes_to_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Part in Friendship", PART_IN_FRIENDSHIP);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Green);
    let fodder = scenario.add_creature(P0, "Fodder Bear", 2, 2).id();
    let burn = scenario
        .add_spell_to_hand_from_oracle(P0, "Zap", true, BURN)
        .with_mana_cost(ManaCost::zero())
        .id();

    let expensive_creature = scenario.add_card_to_library_top(P0, "Library Expensive Wurm");

    let mut runner = scenario.build();
    make_library_creature(runner.state_mut(), expensive_creature, 4);

    kill_via_burn(&mut runner, burn, fodder);

    let s = runner.state();
    assert_eq!(
        s.objects[&fodder].zone,
        Zone::Graveyard,
        "Fodder Bear must die from the burn spell's lethal damage"
    );
    // Primary assertion: mana value 4 > 2 lands controlled → hand, NOT
    // battlefield.
    assert_eq!(
        s.objects[&expensive_creature].zone,
        Zone::Hand,
        "a creature card with mana value > lands controlled must go to hand instead of the battlefield"
    );
}

/// T3 — the single most important hostile fixture: a SECOND nontoken creature
/// dying in the same turn must NOT retrigger the ability (CR 608.2c: "This
/// ability triggers only once each turn" is the ability's own written
/// instruction). The library's second creature card is left completely
/// untouched by the second death.
#[test]
fn part_in_friendship_second_death_same_turn_does_not_retrigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Part in Friendship", PART_IN_FRIENDSHIP);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Green);
    let fodder1 = scenario.add_creature(P0, "Fodder Bear One", 2, 2).id();
    let fodder2 = scenario.add_creature(P0, "Fodder Bear Two", 2, 2).id();
    let burn1 = scenario
        .add_spell_to_hand_from_oracle(P0, "Zap", true, BURN)
        .with_mana_cost(ManaCost::zero())
        .id();
    let burn2 = scenario
        .add_spell_to_hand_from_oracle(P0, "Zap", true, BURN)
        .with_mana_cost(ManaCost::zero())
        .id();

    // Library, top to bottom: two cheap creature cards. Killing fodder1
    // consumes the first (battlefield); killing fodder2 must leave the
    // second one completely alone (still in the library).
    let creature1 = scenario.add_card_to_library_top(P0, "Library Bear One");
    let creature2 = scenario.add_card_to_library_top(P0, "Library Bear Two");

    let mut runner = scenario.build();
    {
        let s = runner.state_mut();
        let lib = &mut s.players[0].library;
        lib.retain(|&id| id != creature1 && id != creature2);
        lib.insert(0, creature2);
        lib.insert(0, creature1);
    }
    make_library_creature(runner.state_mut(), creature1, 2);
    make_library_creature(runner.state_mut(), creature2, 2);

    // First death: triggers the ability, digs to creature1, battlefields it
    // (mana value 2 <= 2 lands controlled).
    kill_via_burn(&mut runner, burn1, fodder1);
    {
        let s = runner.state();
        assert_eq!(
            s.objects[&fodder1].zone,
            Zone::Graveyard,
            "Fodder Bear One must die from the first burn"
        );
        assert_eq!(
            s.objects[&creature1].zone,
            Zone::Battlefield,
            "reach guard: the first death must trigger the reveal and battlefield the cheap creature"
        );
    }

    // Second death, same turn: the once-per-turn limiter must block a second
    // trigger. creature2 must remain in the library, untouched.
    kill_via_burn(&mut runner, burn2, fodder2);

    let s = runner.state();
    assert_eq!(
        s.objects[&fodder2].zone,
        Zone::Graveyard,
        "Fodder Bear Two must still die from the second burn"
    );
    assert_eq!(
        s.objects[&creature2].zone,
        Zone::Library,
        "a second nontoken creature death in the same turn must NOT retrigger the reveal (CR 608.2c once-per-turn)"
    );
}

/// T4 — a TOKEN creature dying does not trigger the ability at all (the
/// `nontoken` filter). Paired with a same-turn nontoken death afterward as the
/// reach-guard: it proves the token death did not consume the once-per-turn
/// budget, so the absent reveal on the token kill is a genuine filter miss,
/// not an already-spent trigger.
#[test]
fn part_in_friendship_token_death_does_not_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Part in Friendship", PART_IN_FRIENDSHIP);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Green);
    let token = scenario.add_creature(P0, "Token Bear", 2, 2).id();
    let fodder = scenario.add_creature(P0, "Fodder Bear", 2, 2).id();
    let burn_token = scenario
        .add_spell_to_hand_from_oracle(P0, "Zap", true, BURN)
        .with_mana_cost(ManaCost::zero())
        .id();
    let burn_fodder = scenario
        .add_spell_to_hand_from_oracle(P0, "Zap", true, BURN)
        .with_mana_cost(ManaCost::zero())
        .id();

    let cheap_creature = scenario.add_card_to_library_top(P0, "Library Cheap Bear");

    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&token).unwrap().is_token = true;
    make_library_creature(runner.state_mut(), cheap_creature, 2);

    // Kill the token first: CR 111.1's `nontoken` filter must exclude it —
    // no reveal, the library creature stays put.
    kill_via_burn(&mut runner, burn_token, token);
    {
        let s = runner.state();
        // Reach guard: the token actually died. CR 111.7 + CR 704.5d: a token
        // that leaves the battlefield ceases to exist as a state-based
        // action, so it is removed from `objects` entirely rather than
        // sitting in the graveyard with `zone == Graveyard`.
        assert!(
            s.objects.get(&token).is_none(),
            "reach guard: the token must die and cease to exist (CR 111.7) from the burn"
        );
        assert_eq!(
            s.objects[&cheap_creature].zone,
            Zone::Library,
            "a token creature's death must NOT trigger the reveal (CR 111.1 nontoken filter)"
        );
    }

    // Same turn, kill the nontoken fodder creature: the ability must still
    // fire normally — proving the token death above did not silently consume
    // the once-per-turn budget.
    kill_via_burn(&mut runner, burn_fodder, fodder);

    let s = runner.state();
    assert_eq!(
        s.objects[&fodder].zone,
        Zone::Graveyard,
        "reach guard: the fodder creature must die from the second burn"
    );
    assert_eq!(
        s.objects[&cheap_creature].zone,
        Zone::Battlefield,
        "a same-turn NONTOKEN death must still trigger the reveal — proves the token death \
         above was a real filter miss, not an already-spent once-per-turn trigger"
    );
}

/// T5/T6 (PR #8008 review — matthewevans): a chained production consumer
/// targeting the reveal's `TrackedSet` — the same runtime construct a printed
/// "…put a +1/+1 counter on each of those creatures" continuation would
/// compile to — must resolve against whichever hit actually landed: the
/// conditional (Battlefield) branch AND the otherwise (Hand) branch alike.
/// Before the `affected_objects_from_events` fix, the tracked set published
/// after `RevealUntil` was scoped ONLY to `kept_destination` (Hand here), so a
/// hit that resolved through the CONDITIONAL branch (Battlefield) was
/// silently omitted — a chained tracked-set consumer would see an
/// empty/incomplete set and grant nothing to the battlefield-entering
/// creature.
///
/// Rather than depend on the parser's (separate, unrelated) anaphora
/// resolution for a brand-new "put a counter on it/that creature" phrase —
/// which is its own architectural question outside this fix's scope — this
/// rewires the REAL parsed ability's chain to insert a
/// `TargetFilter::TrackedSet(0)`-targeting sub-ability directly between the
/// `RevealUntil` root and its existing "put the rest on the bottom" tail, so
/// it consumes EXACTLY the set `RevealUntil`'s own resolution publishes
/// (before the tail's unrelated library-position step runs and republishes
/// its own, different, affected set for anything chained after it).
/// Exercises the real runtime mechanism (`affected_objects_from_events` →
/// `publish_tracked_set_with_causes` → `TargetFilter::TrackedSet` resolution)
/// end-to-end through the real `apply` pipeline.
fn insert_tracked_set_counter_after_reveal_until(
    runner: &mut super::rules::GameRunner,
    source: ObjectId,
) {
    use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, TargetFilter};
    use engine::types::identifiers::TrackedSetId;

    fn splice(root: &mut AbilityDefinition) {
        assert!(
            matches!(&*root.effect, Effect::RevealUntil { .. }),
            "the trigger's root effect must be the RevealUntil this fix concerns"
        );
        // Detach whatever the parser chained after RevealUntil (the "put the
        // rest on the bottom" tail) and splice our TrackedSet-consuming node
        // in between, so OUR node sees RevealUntil's own published set first.
        let existing_tail = root.sub_ability.take();
        let mut counter_node = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: engine::types::counter::CounterType::Plus1Plus1,
                count: engine::types::ability::QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
            },
        );
        counter_node.sub_ability = existing_tail;
        root.sub_ability = Some(Box::new(counter_node));
    }

    let obj = runner.state_mut().objects.get_mut(&source).unwrap();
    assert_eq!(
        obj.trigger_definitions.len(),
        1,
        "Part in Friendship must parse exactly one triggered ability"
    );
    // Every layer recompute resets the LIVE `trigger_definitions` field from
    // `base_trigger_definitions` (see `expand_granted_triggered_abilities`'s
    // doc comment: "each provider's LIVE `trigger_definitions` is read — reset
    // from `base_trigger_definitions` every layer pass"). Editing only the
    // live copy is inert past the next layer evaluation (which the burn
    // spell's resolution triggers) — both copies must carry the same splice.
    {
        let trigger_entry = &mut obj.trigger_definitions[0];
        let root = trigger_entry
            .definition
            .execute
            .as_deref_mut()
            .expect("the dies trigger must have an executable ability chain");
        splice(root);
    }
    {
        let base = std::sync::Arc::make_mut(&mut obj.base_trigger_definitions);
        let base_trigger = base
            .first_mut()
            .expect("Part in Friendship's printed trigger must be in base_trigger_definitions");
        let root = base_trigger
            .execute
            .as_deref_mut()
            .expect("the dies trigger must have an executable ability chain");
        splice(root);
    }
    // `obj.trigger_definitions` is the object's own payload, but the actual
    // firing path consults `state.trigger_index` — a separate lookup
    // structure snapshotted (cloned) from `trigger_definitions` at
    // battlefield-entry time. Editing the object's field alone is inert;
    // re-run the same reindex the entry pipeline uses so the mutated chain is
    // what actually fires.
    engine::game::trigger_index::reindex_object_triggers(runner.state_mut(), source);
}

/// T5 — the conditional (Battlefield) branch: the hit creature enters the
/// battlefield AND receives the tracked-set-sourced +1/+1 counter, proving
/// the tracked set published after the conditional branch is non-empty and
/// correctly scoped to the actual landed object.
#[test]
fn part_in_friendship_battlefield_hit_receives_chained_tracked_set_counter() {
    use engine::types::counter::CounterType;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Part in Friendship", PART_IN_FRIENDSHIP)
        .id();
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Green);
    let fodder = scenario.add_creature(P0, "Fodder Bear", 2, 2).id();
    let burn = scenario
        .add_spell_to_hand_from_oracle(P0, "Zap", true, BURN)
        .with_mana_cost(ManaCost::zero())
        .id();

    let cheap_creature = scenario.add_card_to_library_top(P0, "Library Cheap Bear");

    let mut runner = scenario.build();
    make_library_creature(runner.state_mut(), cheap_creature, 2);
    insert_tracked_set_counter_after_reveal_until(&mut runner, source);

    kill_via_burn(&mut runner, burn, fodder);

    let s = runner.state();
    assert_eq!(
        s.objects[&cheap_creature].zone,
        Zone::Battlefield,
        "reach guard: mana value 2 <= 2 lands controlled must enter the battlefield"
    );
    assert_eq!(
        s.objects[&cheap_creature]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "the chained TrackedSet-targeting consumer must resolve against the conditional \
         (battlefield) branch's hit — the tracked set must not have dropped it"
    );
}

/// T6 — the otherwise (Hand) branch: the hit creature goes to hand AND still
/// receives the tracked-set-sourced +1/+1 counter, proving the otherwise-
/// branch hit remains tracked exactly as it did before this fix (regression
/// guard against the fix accidentally narrowing coverage instead of
/// widening it).
#[test]
fn part_in_friendship_hand_hit_receives_chained_tracked_set_counter() {
    use engine::types::counter::CounterType;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Part in Friendship", PART_IN_FRIENDSHIP)
        .id();
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Green);
    let fodder = scenario.add_creature(P0, "Fodder Bear", 2, 2).id();
    let burn = scenario
        .add_spell_to_hand_from_oracle(P0, "Zap", true, BURN)
        .with_mana_cost(ManaCost::zero())
        .id();

    let expensive_creature = scenario.add_card_to_library_top(P0, "Library Expensive Wurm");

    let mut runner = scenario.build();
    make_library_creature(runner.state_mut(), expensive_creature, 4);
    insert_tracked_set_counter_after_reveal_until(&mut runner, source);

    kill_via_burn(&mut runner, burn, fodder);

    let s = runner.state();
    assert_eq!(
        s.objects[&expensive_creature].zone,
        Zone::Hand,
        "reach guard: mana value 4 > 2 lands controlled must go to hand instead of the battlefield"
    );
    assert_eq!(
        s.objects[&expensive_creature]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "the chained TrackedSet-targeting consumer must resolve against the otherwise \
         (hand) branch's hit"
    );
}
