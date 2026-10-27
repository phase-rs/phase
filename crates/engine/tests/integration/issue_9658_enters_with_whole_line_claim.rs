//! Issue #9658 — an "enters with" sentence is claimed as a replacement on the
//! object it is printed on only when its subject IS that object (or a
//! distributive subset of permanents). Lines whose subject another instruction
//! or a cast trigger supplies are not self replacements.
//!
//! Oracle text verified against MTGJSON `data/mtgjson/AtomicCards.json`:
//!   - Boreal Outrider: "Whenever you cast a creature spell, if {S} of any of
//!     that spell's colors was spent to cast it, that creature enters with an
//!     additional +1/+1 counter on it. ({S} is mana from a snow source.)"
//!   - Hosting Season (three paragraphs, below).
//!   - Dark Intimations (two paragraphs, below).
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 614.1c: "[this permanent] enters with …" is a replacement effect.
//!   - CR 614.12: a replacement that modifies how a permanent enters applies to
//!     that permanent or a general subset of permanents.
//!   - CR 608.2c: instructions are followed in the order written; a later
//!     instruction refers to what an earlier one produced.
//!   - CR 603.1: Dark Intimations' second paragraph is a cast trigger whose
//!     resolution exiles its source and installs a floating entry replacement.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{AbilityDefinition, Effect, TargetFilter};
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const BOREAL_OUTRIDER: &str = "Whenever you cast a creature spell, if {S} of any of that spell's colors was spent to cast it, that creature enters with an additional +1/+1 counter on it. ({S} is mana from a snow source.)";

const HOSTING_SEASON: &str = "While it's October 25th, 2024, whenever you cast your commander, create a token that's a copy of it except it isn't legendary.\nWhile it's October 26th or 27th, 2024, if one or more tokens would be created under your control, instead twice that many tokens are created and each of them enters with a +1/+1 counter on it.\nOn any other date, both abilities apply. (Because why not?)";

const DOUBLING_SEASON: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.\nIf an effect would put one or more counters on a permanent you control, it puts twice that many of those counters on that permanent instead.";

const DARK_INTIMATIONS: &str = "Each opponent sacrifices a creature or planeswalker of their choice, then discards a card. You return a creature or planeswalker card from your graveyard to your hand, then draw a card.\nWhen you cast a Bolas planeswalker spell, exile this card from your graveyard. That planeswalker enters with an additional loyalty counter on it.";

/// Control line: a plain self enters-with replacement.
const SELF_COUNTER: &str = "This creature enters with a +1/+1 counter on it.";

fn colorless(n: u32) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, Vec::new()))
        .collect()
}

fn generic_cost(generic: u32) -> ManaCost {
    ManaCost::Cost {
        shards: vec![],
        generic,
    }
}

/// Every effect reachable from `def` through sub and else branches.
fn chain_effects(def: &AbilityDefinition) -> Vec<&Effect> {
    let mut out = vec![def.effect.as_ref()];
    if let Some(sub) = def.sub_ability.as_deref() {
        out.extend(chain_effects(sub));
    }
    if let Some(else_branch) = def.else_ability.as_deref() {
        out.extend(chain_effects(else_branch));
    }
    out
}

fn all_effects(parsed: &engine::parser::oracle::ParsedAbilities) -> Vec<&Effect> {
    parsed
        .abilities
        .iter()
        .chain(parsed.triggers.iter().filter_map(|t| t.execute.as_deref()))
        .flat_map(chain_effects)
        .collect()
}

/// No counter-placing carrier of the rider anywhere on the card.
fn assert_no_counter_carrier(name: &str, parsed: &engine::parser::oracle::ParsedAbilities) {
    assert!(
        parsed.replacements.is_empty(),
        "{name}: no replacement may carry the rider, got {:?}",
        parsed.replacements
    );
    assert!(
        all_effects(parsed).iter().all(|effect| !matches!(
            effect,
            Effect::AddTargetReplacement { .. }
                | Effect::PutCounter { .. }
                | Effect::AddPendingETBCounters { .. }
        )),
        "{name}: no install, put or pending-counter carrier, got {parsed:?}"
    );
}

/// Cast a creature card from hand and return the counters it entered with.
fn cast_creature_and_count(name: &str, oracle: &str, cost: ManaCost, pool: Vec<ManaUnit>) -> u32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_creature_to_hand_from_oracle(P0, name, 2, 2, oracle)
        .with_mana_cost(cost)
        .id();
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    let entered = outcome
        .find_object(|o| o.name == name && o.zone == Zone::Battlefield)
        .unwrap_or_else(|| panic!("reach guard: {name} must resolve onto the battlefield"));
    outcome.counters(entered, CounterType::Plus1Plus1)
}

/// Boreal Outrider's rider names the creature spell its trigger watches, not
/// Boreal Outrider. It is an honest gap (the snow-colour payment metric does
/// not exist), and Boreal Outrider itself enters with no counter: the line is
/// never a `Moved`/`SelfRef` replacement on Boreal Outrider.
#[test]
fn boreal_outrider_is_an_honest_gap() {
    let parsed = parse_oracle_text(
        BOREAL_OUTRIDER,
        "Boreal Outrider",
        &[],
        &["Creature".to_string()],
        &["Elf".to_string(), "Warrior".to_string()],
    );
    assert_eq!(parsed.triggers.len(), 1, "reach guard: {parsed:?}");
    let trigger = &parsed.triggers[0];
    assert_eq!(trigger.mode, TriggerMode::SpellCast);
    assert!(
        matches!(
            trigger.execute.as_deref().map(|e| e.effect.as_ref()),
            Some(Effect::Unimplemented { .. })
        ),
        "the body is one explicit gap leaf, got {:?}",
        trigger.execute
    );
    assert_no_counter_carrier("Boreal Outrider", &parsed);

    let green_and_two = || {
        let mut pool = colorless(2);
        pool.push(ManaUnit::new(
            ManaType::Green,
            ObjectId(0),
            false,
            Vec::new(),
        ));
        pool
    };
    let cost = || ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 2,
    };
    assert_eq!(
        cast_creature_and_count("Boreal Outrider", BOREAL_OUTRIDER, cost(), green_and_two()),
        0,
        "Boreal Outrider's text gives no counter to itself"
    );
    // Control: a real self enters-with line still gives its counter.
    assert_eq!(
        cast_creature_and_count("Test Grower", SELF_COUNTER, cost(), green_and_two()),
        1
    );
}

/// Hosting Season's second paragraph doubles created tokens and gives each of
/// THEM a counter; it is not a counter on Hosting Season. The line is not
/// claimed by the enters-with parser or by the token parser behind it, so it
/// stays an explicit gap, and Hosting Season enters with no counter.
#[test]
fn hosting_season_is_not_a_self_counter_replacement() {
    let parsed = parse_oracle_text(
        HOSTING_SEASON,
        "Hosting Season",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    assert!(
        parsed
            .replacements
            .iter()
            .all(|r| r.valid_card != Some(TargetFilter::SelfRef)
                && r.event != ReplacementEvent::CreateToken),
        "no self counter replacement and no partial token doubler, got {:?}",
        parsed.replacements
    );
    assert!(
        all_effects(&parsed)
            .iter()
            .all(|effect| !matches!(effect, Effect::AddTargetReplacement { .. })),
        "no floating doubler"
    );
    assert!(
        all_effects(&parsed).iter().any(|effect| effect
            .unimplemented_description()
            .is_some_and(|text| text.contains("each of them enters with"))),
        "reach guard: the doubling line is an explicit gap, got {parsed:?}"
    );

    // Control: Doubling Season's verbatim token line is still a token replacement.
    let doubling = parse_oracle_text(
        DOUBLING_SEASON,
        "Doubling Season",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    assert!(doubling
        .replacements
        .iter()
        .any(|r| r.event == ReplacementEvent::CreateToken));

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let season = scenario
        .add_spell_to_hand(P0, "Hosting Season", false)
        .as_enchantment()
        .from_oracle_text(HOSTING_SEASON)
        .with_mana_cost(generic_cost(1))
        .id();
    scenario.with_mana_pool(P0, colorless(1));
    let mut runner = scenario.build();
    let outcome = runner.cast(season).resolve();
    let entered = outcome
        .find_object(|o| o.name == "Hosting Season" && o.zone == Zone::Battlefield)
        .expect("reach guard: Hosting Season resolves onto the battlefield");
    assert_eq!(outcome.counters(entered, CounterType::Plus1Plus1), 0);
}

// ── Dark Intimations (CR 603.1 + CR 608.2c) ──

const BOLAS_LOYALTY: u32 = 4;

/// A synthetic test planeswalker (no abilities) of the given subtype in hand,
/// printed loyalty `loyalty`, mana value 1.
fn add_test_planeswalker(
    scenario: &mut GameScenario,
    owner: engine::types::player::PlayerId,
    name: &str,
    subtype: &str,
    loyalty: u32,
) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(owner, name, 0, 0, "")
        .as_planeswalker_with_loyalty(subtype, loyalty)
        .with_mana_cost(generic_cost(1))
        .id()
}

fn loyalty_of(state: &engine::types::game_state::GameState, name: &str) -> u32 {
    let obj = state
        .objects
        .values()
        .find(|o| o.name == name && o.zone == Zone::Battlefield)
        .unwrap_or_else(|| panic!("reach guard: {name} must be on the battlefield"));
    obj.counters
        .get(&CounterType::Loyalty)
        .copied()
        .unwrap_or(0)
}

/// CR 113.6m + CR 608.2c: after the printed first paragraph resolves, the
/// graveyard-only cast trigger exiles Dark Intimations, then installs the rider.
#[test]
fn dark_intimations_self_exiles_and_adds_loyalty_after_its_spell_resolves() {
    let parsed = parse_oracle_text(
        DARK_INTIMATIONS,
        "Dark Intimations",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    assert_eq!(parsed.triggers.len(), 1, "{parsed:?}");
    assert_eq!(parsed.triggers[0].mode, TriggerMode::SpellCast);
    assert_eq!(parsed.triggers[0].trigger_zones, vec![Zone::Graveyard]);
    assert!(parsed.replacements.is_empty(), "{parsed:?}");
    assert!(
        all_effects(&parsed)
            .iter()
            .all(|e| !matches!(e, Effect::Unimplemented { .. })),
        "{parsed:?}"
    );
    let exile = parsed.triggers[0].execute.as_deref().unwrap();
    assert!(matches!(
        exile.effect.as_ref(),
        Effect::ChangeZone {
            origin: Some(Zone::Graveyard),
            destination: Zone::Exile,
            target: TargetFilter::SelfRef,
            ..
        }
    ));
    assert!(matches!(
        exile.sub_ability.as_deref().unwrap().effect.as_ref(),
        Effect::AddTargetReplacement {
            target: TargetFilter::None,
            ..
        }
    ));

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dark = scenario
        .add_spell_to_hand_from_oracle(P0, "Dark Intimations", false, DARK_INTIMATIONS)
        .with_mana_cost(generic_cost(1))
        .id();
    // What the first paragraph acts on: an opposing creature to sacrifice, an
    // opposing card to discard, a creature card to return, and a card to draw.
    let sacrificed = scenario.add_vanilla(P1, 2, 2);
    let discarded = scenario.add_card_to_hand(P1, "Test Opposing Card");
    let returned = scenario
        .add_creature_to_graveyard(P0, "Test Returned Creature", 1, 1)
        .id();
    let drawn = scenario.add_card_to_library_top(P0, "Test Drawn Card");
    let walker = add_test_planeswalker(
        &mut scenario,
        P0,
        "Test Bolas Walker",
        "Bolas",
        BOLAS_LOYALTY,
    );
    scenario.with_mana_pool(P0, colorless(2));
    let mut runner = scenario.build();

    let outcome = runner.cast(dark).resolve();
    // Reach guards: the first paragraph resolved as printed.
    assert_eq!(outcome.zone_of(sacrificed), Zone::Graveyard, "sacrificed");
    assert_eq!(outcome.zone_of(discarded), Zone::Graveyard, "discarded");
    assert_eq!(outcome.zone_of(returned), Zone::Hand, "returned");
    assert_eq!(outcome.zone_of(drawn), Zone::Hand, "drawn");
    assert_eq!(
        outcome.zone_of(dark),
        Zone::Graveyard,
        "the sorcery resolved"
    );
    assert!(outcome.state().stack.is_empty());
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().objects[&dark]
        .trigger_definitions
        .as_slice()
        .iter()
        .any(|entry| entry.definition.mode == TriggerMode::SpellCast));

    runner.cast(walker).commit();
    assert_eq!(
        runner.state().stack.len(),
        2,
        "cast spell plus graveyard trigger"
    );
    assert_eq!(runner.state().objects[&dark].zone, Zone::Graveyard);
    runner.resolve_top();
    assert_eq!(
        runner.state().objects[&dark].zone,
        Zone::Exile,
        "exile happens before entry"
    );
    assert_eq!(runner.state().objects[&walker].zone, Zone::Stack);
    runner.advance_until_stack_empty();
    // CR 306.5b + CR 306.5c: intrinsic four plus one additional loyalty.
    assert_eq!(
        loyalty_of(runner.state(), "Test Bolas Walker"),
        BOLAS_LOYALTY + 1
    );
    assert_eq!(
        runner.state().objects[&walker].loyalty,
        Some(BOLAS_LOYALTY + 1)
    );
    assert_eq!(
        runner.state().objects[&walker]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        0
    );
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

// Synthetic (no corpus member): a keyword-qualified distributive subject.
/// Derived: each creature you control with flying enters with one +1/+1
/// counter for each creature you control; a creature without flying gets none.
/// The line is a distributive replacement, not a `static_structure` gap.
#[test]
fn keyword_qualified_distributive_subject_counts_at_entry() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(
        P0,
        "Test Aerie",
        "Each creature you control with flying enters with an additional +1/+1 counter on it for each creature you control.",
    );
    scenario.add_vanilla(P0, 1, 1);
    scenario.add_vanilla(P0, 1, 1);
    let flyer = scenario
        .add_creature_to_hand_from_oracle(P0, "Test Flyer", 1, 1, "Flying")
        .with_mana_cost(generic_cost(1))
        .id();
    let walker = scenario
        .add_creature_to_hand_from_oracle(P0, "Test Walker", 1, 1, "")
        .with_mana_cost(generic_cost(1))
        .id();
    scenario.with_mana_pool(P0, colorless(2));
    let mut runner = scenario.build();
    let outcome = runner.cast(flyer).resolve();
    let entered = outcome
        .find_object(|o| o.name == "Test Flyer" && o.zone == Zone::Battlefield)
        .expect("reach guard: the flyer resolves");
    assert_eq!(
        outcome.counters(entered, CounterType::Plus1Plus1),
        2,
        "one counter per creature you control as it enters"
    );
    let outcome = runner.cast(walker).resolve();
    let entered = outcome
        .find_object(|o| o.name == "Test Walker" && o.zone == Zone::Battlefield)
        .expect("reach guard: the non-flyer resolves");
    assert_eq!(outcome.counters(entered, CounterType::Plus1Plus1), 0);
}

/// Synthetic (no corpus member): a flavor-labeled leading gate on a
/// keyword-qualified distributive subject. A flavor word has no rules meaning
/// (CR 207.2d), so the gate still applies. Derived: while you control an
/// artifact, each creature you control with flying enters with one +1/+1
/// counter for each creature you control; with no artifact it gets none.
#[test]
fn labeled_leading_gate_conditions_the_distributive_entry() {
    for (controls_artifact, expected) in [(true, 2), (false, 0)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_enchantment_from_oracle(
            P0,
            "Test Muster",
            "Mystic Muster — If you control an artifact, each creature you control with flying enters with an additional +1/+1 counter on it for each creature you control.",
        );
        if controls_artifact {
            scenario.add_artifact_from_oracle(P0, "Test Relic", "");
        }
        scenario.add_vanilla(P0, 1, 1);
        scenario.add_vanilla(P0, 1, 1);
        let flyer = scenario
            .add_creature_to_hand_from_oracle(P0, "Test Flyer", 1, 1, "Flying")
            .with_mana_cost(generic_cost(1))
            .id();
        scenario.with_mana_pool(P0, colorless(1));
        let mut runner = scenario.build();
        let outcome = runner.cast(flyer).resolve();
        let entered = outcome
            .find_object(|o| o.name == "Test Flyer" && o.zone == Zone::Battlefield)
            .expect("reach guard: the flyer resolves");
        assert_eq!(
            outcome.counters(entered, CounterType::Plus1Plus1),
            expected,
            "controls an artifact: {controls_artifact}"
        );
    }
}
