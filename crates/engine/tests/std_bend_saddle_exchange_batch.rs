//! Standard "bend / saddle / exchange" bundle — three small singleton batches.
//!
//! Each test drives the PRODUCTION pipeline (`parse_oracle_text` →
//! `resolve_ability_chain` / the runtime trigger matcher / the runtime filter
//! evaluator) and every assertion FLIPS if the corresponding parser/engine
//! change is reverted:
//!
//! - Fatal Fissure ("you earthbend 4"): without the optional-"you" strip in the
//!   earthbend clause parser, the effect is `Unimplemented` — no Animate, no
//!   +1/+1 counters, no Earthbend event. The test resolves the parsed earthbend
//!   chain on a land and asserts the 0/0 animation, four +1/+1 counters, and the
//!   `GameEvent::Earthbend` emission.
//! - Avatar Aang ("whenever you waterbend, earthbend, firebend, or airbend"):
//!   without the batched bend-trigger parser the mode is `Unknown` and the
//!   runtime matcher never fires. The test asserts `TriggerMode::ElementalBend`
//!   and drives the production matcher (`trigger_matcher`) against a real
//!   `Earthbend` event for the source's controller.
//! - Alacrian Armory ("becomes saddled if it's a Mount and becomes an artifact
//!   creature if it's a Vehicle"): without the compound-become split + bare-become
//!   subject inference the whole clause is `Unimplemented`. The test resolves
//!   `BecomeSaddled` (gated TargetMatchesFilter(Mount)) on a Mount and asserts the
//!   saddled designation.
//! - Kitsune, Dragon's Daughter ("exchange control of two other target creatures
//!   controlled by different players"): without stripping the trailing
//!   "controlled by different players" target-set constraint the per-slot parse
//!   fails and the effect is `Unimplemented`. The test resolves `ExchangeControl`
//!   and asserts the two creatures swapped controllers.
//! - FilterProp::SaddledSource building block: the runtime filter evaluator
//!   matches exactly the creatures recorded in the source's `saddled_by`.

use engine::game::ability_utils::build_resolved_from_def_with_targets;
use engine::game::effects::resolve_ability_chain;
use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::triggers::trigger_matcher;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, Effect, FilterProp, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;

fn types_of(types: &[&str]) -> Vec<String> {
    types.iter().map(|s| s.to_string()).collect()
}

fn assert_zero_unimplemented(
    oracle: &str,
    name: &str,
    kws: &[&str],
    types: &[&str],
    subs: &[&str],
) {
    let parsed = parse_oracle_text(
        oracle,
        name,
        &types_of(kws),
        &types_of(types),
        &types_of(subs),
    );
    let dbg = format!("{parsed:#?}");
    assert!(
        !dbg.contains("Unimplemented"),
        "{name}: expected zero Unimplemented nodes, parse was:\n{dbg}"
    );
}

/// Walk a parsed ability/effect tree and return the first effect (incl. nested
/// sub-abilities and delayed-trigger bodies) matching `pred`.
fn find_effect_def<'a>(
    def: &'a AbilityDefinition,
    pred: &dyn Fn(&Effect) -> bool,
) -> Option<&'a AbilityDefinition> {
    if pred(&def.effect) {
        return Some(def);
    }
    if let Some(sub) = &def.sub_ability {
        if let Some(found) = find_effect_def(sub, pred) {
            return Some(found);
        }
    }
    if let Effect::CreateDelayedTrigger { effect, .. } = &*def.effect {
        if let Some(found) = find_effect_def(effect, pred) {
            return Some(found);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// §17 — Fatal Fissure: "you earthbend 4"
// ---------------------------------------------------------------------------

const FATAL_FISSURE: &str = "Choose target creature. When that creature dies this turn, you earthbend 4. (Target land you control becomes a 0/0 creature with haste that's still a land. Put four +1/+1 counters on it. When it dies or is exiled, return it to the battlefield tapped.)";

#[test]
fn fatal_fissure_parses_zero_unimplemented() {
    assert_zero_unimplemented(FATAL_FISSURE, "Fatal Fissure", &[], &["Instant"], &[]);
}

#[test]
fn fatal_fissure_earthbend_animates_land_with_four_counters() {
    let parsed = parse_oracle_text(
        FATAL_FISSURE,
        "Fatal Fissure",
        &[],
        &types_of(&["Instant"]),
        &[],
    );
    // The earthbend chain lives inside the delayed "when that creature dies"
    // trigger as an `Animate` whose sub-ability is the +1/+1 counter placement.
    let ability = &parsed.abilities[0];
    let earthbend = find_effect_def(ability, &|e| matches!(e, Effect::Animate { .. })).expect(
        "Fatal Fissure must parse 'you earthbend 4' into an Animate chain (not Unimplemented)",
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Earthbend default target is "target land you control" — give P0 a land.
    let land = scenario.add_creature(P0, "Mountain", 0, 0).id();
    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&land).unwrap();
        obj.card_types.core_types = vec![CoreType::Land];
        obj.base_card_types = obj.card_types.clone();
        obj.power = None;
        obj.toughness = None;
        obj.base_power = None;
        obj.base_toughness = None;
    }

    let resolved =
        build_resolved_from_def_with_targets(earthbend, land, P0, vec![TargetRef::Object(land)]);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("earthbend chain must resolve");

    let obj = &runner.state().objects[&land];
    assert!(
        obj.card_types.core_types.contains(&CoreType::Creature),
        "earthbent land must become a creature; got {:?}",
        obj.card_types.core_types
    );
    assert!(
        obj.card_types.core_types.contains(&CoreType::Land),
        "earthbent land is still a land"
    );
    let plus = obj
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0);
    assert_eq!(
        plus, 4,
        "earthbend 4 puts four +1/+1 counters on the land; got {plus}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, GameEvent::Earthbend { controller, .. } if *controller == P0)),
        "earthbend must emit a GameEvent::Earthbend for the controller; got {events:?}"
    );
}

// ---------------------------------------------------------------------------
// §17 — Avatar Aang: batched bend trigger
// ---------------------------------------------------------------------------

const AVATAR_AANG: &str = "Flying, firebending 2\nWhenever you waterbend, earthbend, firebend, or airbend, draw a card. Then if you've done all four this turn, transform Avatar Aang.";

#[test]
fn avatar_aang_parses_zero_unimplemented() {
    assert_zero_unimplemented(
        AVATAR_AANG,
        "Avatar Aang",
        &["Flying", "firebending"],
        &["Creature"],
        &["Avatar"],
    );
}

#[test]
fn avatar_aang_batched_bend_trigger_fires_on_any_bend() {
    let parsed = parse_oracle_text(
        AVATAR_AANG,
        "Avatar Aang",
        &types_of(&["Flying", "firebending"]),
        &types_of(&["Creature"]),
        &types_of(&["Avatar"]),
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::ElementalBend)
        .expect("Avatar Aang's batched bend trigger must lower to TriggerMode::ElementalBend");

    // The execute body must draw a card (the bend subject was consumed, not
    // swallowed into the effect).
    let execute = trigger
        .execute
        .as_ref()
        .expect("trigger has an execute body");
    assert!(
        find_effect_def(execute, &|e| matches!(e, Effect::Draw { .. })).is_some(),
        "ElementalBend trigger executes a Draw"
    );

    // Drive the PRODUCTION matcher: Aang on the battlefield, an Earthbend event
    // for its controller must match. Without the parser change the mode would be
    // Unknown and this matcher path would never be reached.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let aang = scenario.add_creature(P0, "Avatar Aang", 4, 4).id();
    let runner = scenario.build();

    let matcher = trigger_matcher(trigger.mode.clone())
        .expect("ElementalBend has a registered runtime matcher");
    // Any one of the four bend events fires the batched trigger for the controller.
    let earthbend = GameEvent::Earthbend {
        source_id: aang,
        controller: P0,
    };
    assert!(
        matcher(&earthbend, trigger, aang, runner.state()),
        "ElementalBend must match an Earthbend event for the source's controller"
    );
    let firebend = GameEvent::Firebend {
        source_id: aang,
        controller: P0,
    };
    assert!(
        matcher(&firebend, trigger, aang, runner.state()),
        "ElementalBend must match a Firebend event for the source's controller"
    );
    // An opponent's bend must NOT fire the controller-scoped trigger.
    let opp_bend = GameEvent::Earthbend {
        source_id: aang,
        controller: P1,
    };
    assert!(
        !matcher(&opp_bend, trigger, aang, runner.state()),
        "ElementalBend is controller-scoped: an opponent's bend must not fire it"
    );
}

/// Maintainer guard (CR 603.2): a PARTIAL bend disjunction must NOT collapse to
/// `TriggerMode::ElementalBend`. The only any-bend runtime matcher fires on ALL
/// four bend events; collapsing "whenever you waterbend or earthbend" to
/// `ElementalBend` would over-fire on the unlisted firebend/airbend events.
/// `try_parse_bend_trigger` returns `None` for any strict subset, so the trigger
/// fails closed (`Unknown`) instead — and crucially produces NO `ElementalBend`
/// trigger that the runtime matcher could fire on. This proves the parser never
/// ships a building block broader than the runtime semantics it preserves.
#[test]
fn partial_bend_disjunction_does_not_collapse_to_elemental_bend() {
    let parsed = parse_oracle_text(
        "Whenever you waterbend or earthbend, draw a card.",
        "Partial Bender",
        &types_of(&[]),
        &types_of(&["Creature"]),
        &types_of(&[]),
    );
    assert!(
        !parsed
            .triggers
            .iter()
            .any(|t| t.mode == TriggerMode::ElementalBend),
        "a partial two-bend disjunction must not lower to the any-bend ElementalBend \
         matcher (it would over-fire on the unlisted bend events)"
    );
    // Belt-and-suspenders: it must also not silently map to one of the listed
    // single-bend modes (that would drop the other listed event).
    assert!(
        !parsed
            .triggers
            .iter()
            .any(|t| matches!(t.mode, TriggerMode::Waterbend | TriggerMode::Earthbend)),
        "a partial bend disjunction must not collapse to a single-bend mode either"
    );
}

// ---------------------------------------------------------------------------
// §20 — Alacrian Armory: "becomes saddled if it's a Mount and becomes an
// artifact creature if it's a Vehicle"
// ---------------------------------------------------------------------------

const ALACRIAN_ARMORY: &str = "Creatures you control get +0/+1 and have vigilance.\nAt the beginning of combat on your turn, choose up to one target Mount or Vehicle you control. Until end of turn, that permanent becomes saddled if it's a Mount and becomes an artifact creature if it's a Vehicle.";

#[test]
fn alacrian_armory_parses_zero_unimplemented() {
    assert_zero_unimplemented(ALACRIAN_ARMORY, "Alacrian Armory", &[], &["Artifact"], &[]);
}

#[test]
fn alacrian_armory_become_saddled_branch_saddles_a_mount() {
    let parsed = parse_oracle_text(
        ALACRIAN_ARMORY,
        "Alacrian Armory",
        &[],
        &types_of(&["Artifact"]),
        &[],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.execute.is_some())
        .expect("Alacrian has a begin-combat trigger");
    let execute = trigger.execute.as_ref().unwrap();
    // The first become-conjunct must lower to BecomeSaddled (gated on Mount).
    let become_saddled = find_effect_def(execute, &|e| matches!(e, Effect::BecomeSaddled { .. }))
        .expect(
            "Alacrian must parse the 'becomes saddled if it's a Mount' conjunct into BecomeSaddled",
        );
    // The second conjunct must lower to a type-adding GenericEffect (artifact creature).
    assert!(
        find_effect_def(execute, &|e| matches!(e, Effect::GenericEffect { .. })).is_some(),
        "the 'becomes an artifact creature if it's a Vehicle' conjunct must also lower"
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::BeginCombat);
    let mount = scenario
        .add_creature(P0, "Mount", 2, 2)
        .with_subtypes(vec!["Mount"])
        .id();
    let mut runner = scenario.build();

    let resolved = build_resolved_from_def_with_targets(
        become_saddled,
        mount,
        P0,
        vec![TargetRef::Object(mount)],
    );
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("BecomeSaddled must resolve");

    assert!(
        runner.state().objects[&mount].is_saddled,
        "the Mount must acquire the saddled designation"
    );
}

// ---------------------------------------------------------------------------
// §23 — Kitsune, Dragon's Daughter: exchange control of two creatures
// ---------------------------------------------------------------------------

const KITSUNE: &str = "Vigilance\nWhenever Kitsune enters or deals combat damage to a player, you may exchange control of two other target creatures controlled by different players.";

#[test]
fn kitsune_parses_zero_unimplemented() {
    assert_zero_unimplemented(
        KITSUNE,
        "Kitsune, Dragon's Daughter",
        &["Vigilance"],
        &["Legendary", "Creature"],
        &["Fox", "Samurai"],
    );
}

#[test]
fn kitsune_exchange_control_swaps_two_creatures() {
    let parsed = parse_oracle_text(
        KITSUNE,
        "Kitsune, Dragon's Daughter",
        &types_of(&["Vigilance"]),
        &types_of(&["Legendary", "Creature"]),
        &types_of(&["Fox", "Samurai"]),
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| {
            t.execute.as_ref().is_some_and(|e| {
                find_effect_def(e, &|x| matches!(x, Effect::ExchangeControl { .. })).is_some()
            })
        })
        .expect("Kitsune must lower the exchange clause to Effect::ExchangeControl");
    let execute = trigger.execute.as_ref().unwrap();
    let exchange =
        find_effect_def(execute, &|e| matches!(e, Effect::ExchangeControl { .. })).unwrap();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kitsune = scenario.add_creature(P0, "Kitsune", 2, 3).id();
    let mine = scenario.add_creature(P0, "My Bear", 2, 2).id();
    let theirs = scenario.add_creature(P1, "Their Ogre", 3, 3).id();
    let mut runner = scenario.build();

    assert_eq!(runner.state().objects[&mine].controller, P0);
    assert_eq!(runner.state().objects[&theirs].controller, P1);

    let mut resolved = build_resolved_from_def_with_targets(
        exchange,
        kitsune,
        P0,
        vec![TargetRef::Object(mine), TargetRef::Object(theirs)],
    );
    // CR 603.5: the "you may" is an optional trigger — simulate the controller
    // electing to exchange so the resolver applies the swap (the optional-prompt
    // path is a separate concern; here we drive the resolved effect itself).
    resolved.optional = false;
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("ExchangeControl must resolve");

    // CR 701.12a + CR 613.1 (Layer 2): the swap is applied via two transient
    // ChangeController continuous effects; the controller change manifests only
    // after the layer evaluator runs.
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    assert_eq!(
        runner.state().objects[&mine].controller,
        P1,
        "my creature must now be controlled by the opponent"
    );
    assert_eq!(
        runner.state().objects[&theirs].controller,
        P0,
        "the opponent's creature must now be controlled by me"
    );
}

// ---------------------------------------------------------------------------
// §20 — FilterProp::SaddledSource building block (Calamity's saddler reference).
// The full Calamity card honest-defers (the non-targeting "choose a creature
// that saddled it" selection feeding a token-copy, plus "Repeat this process
// once" owned by the repeat cluster), but the saddler-reference FILTER primitive
// it anchors is shipped, reachable ("creature that saddled it this turn" target
// phrase), and runtime-evaluated here.
// ---------------------------------------------------------------------------

#[test]
fn saddled_source_filter_matches_only_creatures_that_saddled_the_source() {
    // The parser produces the filter from a clean targeting phrase.
    let parsed = parse_oracle_text(
        "Destroy target creature that saddled it this turn.",
        "Saddler Probe",
        &[],
        &types_of(&["Instant"]),
        &[],
    );
    let dbg = format!("{parsed:#?}");
    assert!(
        dbg.contains("SaddledSource"),
        "'creature that saddled it this turn' must parse to FilterProp::SaddledSource:\n{dbg}"
    );

    // Runtime evaluation: a creature recorded in the source's saddled_by matches;
    // one that is not does not.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mount = scenario
        .add_creature(P0, "Mount", 0, 4)
        .with_subtypes(vec!["Mount"])
        .id();
    let saddler = scenario.add_creature(P0, "Saddler", 3, 3).id();
    let bystander = scenario.add_creature(P0, "Bystander", 3, 3).id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&mount)
        .unwrap()
        .saddled_by = vec![saddler];

    let filter =
        TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::SaddledSource]));
    // `source` is the Mount (Calamity) — the creature that saddled IT.
    let ctx = FilterContext::from_source(runner.state(), mount);
    assert!(
        matches_target_filter(runner.state(), saddler, &filter, &ctx),
        "the creature recorded in the source's saddled_by must match SaddledSource"
    );
    assert!(
        !matches_target_filter(runner.state(), bystander, &filter, &ctx),
        "a creature that did not saddle the source must not match SaddledSource"
    );
}
