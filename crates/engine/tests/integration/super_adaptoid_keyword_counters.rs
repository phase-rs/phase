//! Super-Adaptoid (MSH) — keyword-counter mirror + characteristic-defining power.
//!
//! Oracle text:
//!   "Super-Adaptoid's power is equal to the number of legendary creatures you
//!    control.
//!    Whenever Super-Adaptoid enters or attacks, choose another target
//!    creature. If that creature has haste and Super-Adaptoid doesn't, put a
//!    haste counter on Super-Adaptoid. Do the same for flying, first strike,
//!    double strike, deathtouch, indestructible, lifelink, menace, reach,
//!    trample, and vigilance."
//!
//! This drives the REAL parse → trigger-resolution → layer pipeline:
//!   - Line 1 is a characteristic-defining `SetDynamicPower` over the
//!     legendary-creature `ObjectCount` (CR 604.3a). The CDA-power test asserts
//!     the layer-computed power equals the live legendary-creature count and
//!     tracks it.
//!   - The trigger's execute chain is one `TargetOnly` (choose another target
//!     creature, CR 601.2c) followed by 11 conditional keyword-counter
//!     placements (CR 122.1b): each `PutCounter { Keyword(K), target: SelfRef }`
//!     gated by `And([TargetHasKeywordInstead{K}, SourceLacksKeyword{K}])`.
//!
//! Discriminating coverage (each assertion FLIPS on a specific revert):
//!   (1) target has flying + lifelink (SA lacks both) → SA gains a flying
//!       counter AND a lifelink counter, and layers grant both keywords.
//!       Reverting the "Do the same for <list>" expansion drops every counter
//!       past haste, so flying/lifelink would never be placed.
//!   (2) a keyword the TARGET lacks (deathtouch) → no deathtouch counter.
//!       Reverting the `TargetHasKeywordInstead` conjunct would place it
//!       unconditionally.
//!   (3) a keyword SUPER-ADAPTOID ALREADY HAS (haste) → no second haste counter.
//!       Reverting the `SourceLacksKeyword` conjunct (the antecedent `And` fix)
//!       would place a redundant counter.
//!   (4) CDA power = legendary-creature count, and it tracks the count.
//!
//! The trigger is resolved through the public `resolve_ability_chain`
//! production seam with the chosen target supplied as a root `TargetRef` — the
//! exact path the ETB/attack trigger uses in production.

use engine::game::ability_utils::build_resolved_from_def_with_targets;
use engine::game::effects::resolve_ability_chain;
use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityDefinition, TargetRef};
use engine::types::card_type::Supertype;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, KeywordKind};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);

const ORACLE: &str = "Super-Adaptoid's power is equal to the number of legendary creatures you control.\n\
Whenever Super-Adaptoid enters or attacks, choose another target creature. If that creature has haste and Super-Adaptoid doesn't, put a haste counter on Super-Adaptoid. Do the same for flying, first strike, double strike, deathtouch, indestructible, lifelink, menace, reach, trample, and vigilance.";

fn artifact_creature_types() -> Vec<String> {
    vec!["Artifact".to_string(), "Creature".to_string()]
}

/// Re-parse the oracle text to get the trigger's execute `AbilityDefinition`
/// (the harness needs the definition to build a resolved ability with a chosen
/// target — same definition the object carries in production).
fn super_adaptoid_trigger_execute() -> AbilityDefinition {
    let parsed = parse_oracle_text(
        ORACLE,
        "Super-Adaptoid",
        &[],
        &artifact_creature_types(),
        &[],
    );
    assert_eq!(parsed.triggers.len(), 1, "exactly one ETB/attack trigger");
    parsed
        .triggers
        .into_iter()
        .next()
        .unwrap()
        .execute
        .map(|b| *b)
        .expect("trigger must carry an execute ability")
}

fn keyword_counter_count(runner: &GameRunner, id: ObjectId, kind: KeywordKind) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Keyword(kind))
        .copied()
        .unwrap_or(0)
}

fn has_kw(runner: &mut GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], keyword)
}

/// The parse must leave ZERO residual `Unimplemented` nodes — the "Do the same
/// for <list>" continuation expands into 11 conditional keyword-counter clauses
/// (pre-fix it stayed `Unimplemented[do]`).
#[test]
fn super_adaptoid_parses_with_zero_unimplemented() {
    let parsed = parse_oracle_text(
        ORACLE,
        "Super-Adaptoid",
        &[],
        &artifact_creature_types(),
        &[],
    );
    let dbg = format!("{parsed:#?}");
    assert!(
        !dbg.contains("Unimplemented"),
        "Super-Adaptoid must parse to zero Unimplemented nodes, parse was:\n{dbg}"
    );

    // Shape guard: the execute chain must contain exactly the 11 keyword
    // counters in order. Pre-fix only the haste counter existed (the rest were
    // swallowed by Unimplemented[do]).
    let execute = super_adaptoid_trigger_execute();
    let mut node = Some(&execute);
    let mut placed: Vec<KeywordKind> = Vec::new();
    while let Some(def) = node {
        if let engine::types::ability::Effect::PutCounter {
            counter_type: CounterType::Keyword(kind),
            ..
        } = &*def.effect
        {
            placed.push(*kind);
        }
        node = def.sub_ability.as_deref();
    }
    assert_eq!(
        placed,
        vec![
            KeywordKind::Haste,
            KeywordKind::Flying,
            KeywordKind::FirstStrike,
            KeywordKind::DoubleStrike,
            KeywordKind::Deathtouch,
            KeywordKind::Indestructible,
            KeywordKind::Lifelink,
            KeywordKind::Menace,
            KeywordKind::Reach,
            KeywordKind::Trample,
            KeywordKind::Vigilance,
        ],
        "all 11 keyword counters present, in Oracle order (haste + the 10 repeated)"
    );
}

/// (1)+(2)+(3): drive the trigger through the production resolver against a
/// target with a partial keyword set, and assert which counters land.
#[test]
fn super_adaptoid_mirrors_only_keywords_target_has_and_source_lacks() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Super-Adaptoid built from Oracle text through the real parse pipeline.
    // Give it haste up front (case 3: it already has haste, so the haste
    // counter must NOT be placed).
    let sa = scenario
        .add_creature_from_oracle(P0, "Super-Adaptoid", 0, 5, ORACLE)
        .haste()
        .id();

    // Target creature: HAS flying + lifelink + haste; LACKS deathtouch (and the
    // rest). Super-Adaptoid lacks flying/lifelink but already has haste.
    let target = scenario
        .add_creature(P0, "Donor Beast", 3, 3)
        .flying()
        .lifelink()
        .haste()
        .id();

    let mut runner = scenario.build();

    let execute = super_adaptoid_trigger_execute();
    let ability =
        build_resolved_from_def_with_targets(&execute, sa, P0, vec![TargetRef::Object(target)]);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("Super-Adaptoid trigger must resolve");

    // (1) Target HAS flying + lifelink, SA LACKS both → both counters placed.
    // Flips on revert of the "Do the same for <list>" expansion (only haste
    // would be considered; flying/lifelink counters never placed).
    assert_eq!(
        keyword_counter_count(&runner, sa, KeywordKind::Flying),
        1,
        "target has flying & SA lacks it → one flying counter placed"
    );
    assert_eq!(
        keyword_counter_count(&runner, sa, KeywordKind::Lifelink),
        1,
        "target has lifelink & SA lacks it → one lifelink counter placed"
    );

    // (2) Target LACKS deathtouch → no deathtouch counter. Flips on revert of
    // the `TargetHasKeywordInstead` conjunct (would place unconditionally).
    assert_eq!(
        keyword_counter_count(&runner, sa, KeywordKind::Deathtouch),
        0,
        "target lacks deathtouch → no deathtouch counter"
    );

    // (3) SA ALREADY HAS haste → no haste counter. Flips on revert of the
    // `SourceLacksKeyword` conjunct (antecedent `And` fix): pre-fix the
    // condition was a bare/Unknown keyword check that ignored "and ~ doesn't",
    // so a redundant haste counter would land.
    assert_eq!(
        keyword_counter_count(&runner, sa, KeywordKind::Haste),
        0,
        "SA already has haste → no redundant haste counter"
    );

    // The placed keyword counters actually GRANT their keywords via layers
    // (CR 122.1b). SA had neither flying nor lifelink before resolution.
    assert!(
        has_kw(&mut runner, sa, &Keyword::Flying),
        "the flying counter grants flying to Super-Adaptoid (CR 122.1b)"
    );
    assert!(
        has_kw(&mut runner, sa, &Keyword::Lifelink),
        "the lifelink counter grants lifelink to Super-Adaptoid (CR 122.1b)"
    );
    // No deathtouch keyword was granted (no counter placed).
    assert!(
        !has_kw(&mut runner, sa, &Keyword::Deathtouch),
        "no deathtouch counter → Super-Adaptoid does not gain deathtouch"
    );
}

/// (4): the characteristic-defining power equals the count of legendary
/// creatures the controller controls, and tracks the count (CR 604.3a).
#[test]
fn super_adaptoid_power_tracks_legendary_creature_count() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Super-Adaptoid (NOT legendary itself) and three other P0 creatures, two
    // of them legendary. SA must count legendary creatures regardless of which
    // one it is.
    let sa = scenario
        .add_creature_from_oracle(P0, "Super-Adaptoid", 0, 5, ORACLE)
        .id();
    scenario.add_creature(P0, "Legend One", 2, 2).as_legendary();
    scenario.add_creature(P0, "Legend Two", 2, 2).as_legendary();
    let plain = scenario.add_creature(P0, "Plain Bear", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    // Two legendary creatures controlled → SA power = 2. Flips if the CDA power
    // line fails to parse or counts the wrong objects.
    assert_eq!(
        runner.state().objects[&sa].power,
        Some(2),
        "CDA power = number of legendary creatures you control (2)"
    );

    // Make the previously-plain creature legendary → count becomes 3, power
    // tracks it (CDA is continuous, CR 604.3). Push onto `base_card_types` so
    // the supertype survives the layer reset (the layer system reverts
    // `card_types` to `base_card_types` at the top of each evaluation).
    {
        let obj = runner.state_mut().objects.get_mut(&plain).unwrap();
        obj.base_card_types.supertypes.push(Supertype::Legendary);
        obj.card_types.supertypes.push(Supertype::Legendary);
    }
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&sa].power,
        Some(3),
        "CDA power tracks the live legendary-creature count (now 3)"
    );
}
