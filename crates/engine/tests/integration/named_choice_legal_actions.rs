//! CR 608.2d + CR 614.12a: a named choice made while an effect applies, or as a
//! permanent enters, offers its enumerated domain without simulating each
//! answer.
//!
//! Before this, every `ChooseOption` answer of a `WaitingFor::NamedChoice` went
//! through `SimulationFilter`'s clone-and-apply probe — one full `GameState`
//! clone per answer, which on a creature-type prompt is several hundred clones
//! for a list whose every member the reducer accepts.
//!
//! Each test drives a real card to its prompt through the production pipeline
//! (`PlayLand`, or a cast resolved through the scenario runner), then checks:
//!
//! 1. an independent per-answer reference — one `apply_interaction_for_simulation`
//!    per option on its own clone — accepts exactly the prompt's options (the
//!    CR 608.2d derived value: every enumerated answer is legal);
//! 2. `legal_actions_full` offers exactly that set and records zero raw
//!    validation clones;
//! 3. `flat_priority_actions` offers the same set, also with zero clones, so
//!    the two consumers cannot drift.
//!
//! Fail-on-revert: without the `SimulationFilter` structural check each answer
//! is simulated, so `raw_validation_state_clones == options.len() > 0` and the
//! zero-clone assertion in (2) fails. The set checks are the reach guard: they
//! prove the prompt was really enumerated, so the zero is not an empty list.
//!
//! Scope: the cast-in-progress refusal (CR 601.2f + CR 601.2h + CR 602.2b) is
//! covered as ROUTING evidence only, by the `filter.rs` unit tests — no current
//! card raises a `NamedChoice` mid-cast whose answers differ in payability.

use std::collections::BTreeSet;

use engine::ai_support::{flat_priority_actions, legal_actions_full};
use engine::game::engine::apply_interaction_for_simulation;
use engine::game::perf_counters;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::ChoiceType;
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, NamedChoiceSourceBinding, WaitingFor};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// Thriving Grove, verbatim.
const THRIVING_GROVE: &str =
    "This land enters tapped. As it enters, choose a color other than green.\n{T}: Add {G} or one mana of the chosen color.";

/// Vanquisher's Banner, verbatim.
const VANQUISHERS_BANNER: &str = "As this artifact enters, choose a creature type.\nCreatures you control of the chosen type get +1/+1.\nWhenever you cast a creature spell of the chosen type, draw a card.";

/// Distant Melody, verbatim.
const DISTANT_MELODY: &str =
    "Choose a creature type. Draw a card for each permanent you control of that type.";

/// Stave Off, verbatim.
const STAVE_OFF: &str =
    "Target creature gains protection from the color of your choice until end of turn.";

/// Brave the Elements, verbatim.
const BRAVE_THE_ELEMENTS: &str = "Choose a color. White creatures you control gain protection from the chosen color until end of turn.";

fn chosen_options(actions: &[GameAction]) -> BTreeSet<String> {
    actions
        .iter()
        .filter_map(|action| match action {
            GameAction::ChooseOption { choice } => Some(choice.clone()),
            _ => None,
        })
        .collect()
}

/// The shared checks (module doc, points 1–3) for a state halted on an
/// enumerated `NamedChoice` outside any cast or activation.
fn assert_offers_named_domain_without_simulation(state: &GameState) {
    let WaitingFor::NamedChoice {
        player, options, ..
    } = &state.waiting_for
    else {
        panic!("expected a NamedChoice prompt, got {:?}", state.waiting_for);
    };
    assert!(!options.is_empty(), "the prompt must enumerate its domain");
    assert!(
        state.pending_cast.is_none(),
        "the prompt must be raised outside a pending cast"
    );
    assert!(
        !state.withholds_priority(),
        "the prompt must be raised outside a cast, activation or cost payment"
    );
    let player = *player;
    let domain: BTreeSet<String> = options.iter().cloned().collect();

    // (1) Independent per-answer reference: the reducer, one clone per answer.
    let mut reference = BTreeSet::new();
    for option in options {
        let mut clone = state.clone();
        let applied = apply_interaction_for_simulation(
            &mut clone,
            player,
            player,
            GameAction::ChooseOption {
                choice: option.clone(),
            },
        );
        if applied.is_ok() {
            // No cast is resumed by the answer, so `fallback_simulation`'s
            // post-apply cost checks (`pending_spell_root` /
            // `post_origin_auto_payment_verdict`) are inert and this `Ok` is
            // its full verdict.
            assert!(
                clone.pending_cast.is_none(),
                "answer {option:?} must not leave a pending cast"
            );
            assert!(
                clone.waiting_for.pending_cast_ref().is_none(),
                "answer {option:?} must not raise a prompt carrying a pending cast"
            );
            reference.insert(option.clone());
        }
    }
    assert_eq!(
        reference, domain,
        "CR 608.2d: the reducer must accept every enumerated answer"
    );

    // (2) The measured call.
    perf_counters::reset();
    let (actions, _costs, _grouped) = legal_actions_full(state);
    let clones = perf_counters::snapshot().raw_validation_state_clones;
    assert_eq!(
        chosen_options(&actions),
        reference,
        "legal_actions_full must offer exactly the reducer-accepted answers"
    );
    assert_eq!(
        clones, 0,
        "legal_actions_full must not simulate named-choice answers; got {clones} clones"
    );

    // (3) The flat consumer agrees, with no simulation either.
    perf_counters::reset();
    let flat = flat_priority_actions(state);
    let flat_clones = perf_counters::snapshot().raw_validation_state_clones;
    assert_eq!(
        chosen_options(&flat),
        reference,
        "flat_priority_actions must offer the same answers as legal_actions_full"
    );
    assert_eq!(
        flat_clones, 0,
        "flat_priority_actions must not simulate named-choice answers; got {flat_clones} clones"
    );
}

fn prompt_binding(state: &GameState) -> Option<NamedChoiceSourceBinding> {
    match &state.waiting_for {
        WaitingFor::NamedChoice { source, .. } => source.as_ref().map(|source| source.binding),
        other => panic!("expected a NamedChoice prompt, got {other:?}"),
    }
}

fn colorless(source: engine::types::identifiers::ObjectId, count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Colorless, source, false, vec![]))
        .collect()
}

fn with_p0_priority(runner: &mut GameRunner) {
    let state = runner.state_mut();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
}

/// CR 614.12a: Thriving Grove's as-enters color choice, raised by `PlayLand`.
#[test]
fn as_enters_color_choice_offers_its_domain_without_simulation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let grove = scenario
        .add_land_to_hand(P0, "Thriving Grove")
        .from_oracle_text(THRIVING_GROVE)
        .id();
    let mut runner = scenario.build();
    with_p0_priority(&mut runner);
    let card_id = runner.state().objects[&grove].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: grove,
            card_id,
        })
        .expect("playing the land must start its entry");

    let state = runner.state();
    assert!(
        !state.battlefield.contains(&grove),
        "CR 614.12a: the color is chosen before Thriving Grove enters"
    );
    assert_eq!(
        prompt_binding(state),
        Some(NamedChoiceSourceBinding::PendingEntry),
        "the prompt must answer the parked entry"
    );
    let WaitingFor::NamedChoice {
        choice_type,
        options,
        ..
    } = &state.waiting_for
    else {
        unreachable!("prompt_binding checked the prompt shape");
    };
    assert!(matches!(choice_type, ChoiceType::Color { .. }));
    let offered: BTreeSet<&str> = options.iter().map(String::as_str).collect();
    assert_eq!(
        offered,
        BTreeSet::from(["White", "Blue", "Black", "Red"]),
        "\"a color other than green\" is the four non-green colors"
    );

    assert_offers_named_domain_without_simulation(state);
}

/// CR 614.12a: Vanquisher's Banner's as-enters creature-type choice, raised
/// when the artifact spell resolves and its entry is parked.
#[test]
fn as_enters_creature_type_choice_offers_its_domain_without_simulation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut builder =
        scenario.add_artifact_to_hand_from_oracle(P0, "Vanquisher's Banner", VANQUISHERS_BANNER);
    builder.with_mana_cost(ManaCost::Cost {
        generic: 5,
        shards: vec![],
    });
    let banner = builder.id();
    scenario.with_mana_pool(P0, colorless(banner, 5));
    let mut runner = scenario.build();
    with_p0_priority(&mut runner);

    // No `named_choice` policy: the driver halts on the prompt.
    let outcome = runner.cast(banner).resolve();
    let state = outcome.state();
    assert!(
        !state.battlefield.contains(&banner),
        "CR 614.12a: the creature type is chosen before the Banner enters"
    );
    assert_eq!(
        prompt_binding(state),
        Some(NamedChoiceSourceBinding::PendingEntry),
        "the prompt must answer the parked entry"
    );
    let WaitingFor::NamedChoice {
        choice_type,
        options,
        ..
    } = &state.waiting_for
    else {
        unreachable!("prompt_binding checked the prompt shape");
    };
    assert!(matches!(choice_type, ChoiceType::CreatureType { .. }));
    // CR 205.3m: the prompt offers the engine's whole creature-type domain. The
    // scenario loads no card database, so that is `compute_options`' built-in
    // fallback list rather than the full card-pool list a real game offers.
    assert!(
        options.len() > 1,
        "the creature-type prompt must offer more than one type; got {}",
        options.len()
    );

    assert_offers_named_domain_without_simulation(state);
}

/// CR 608.2d: Distant Melody's creature-type choice, raised while the
/// sorcery resolves. Its answer persists on the resolving spell ("that type"
/// is read back by the draw count), so the prompt is source-bound with
/// `ExactObjectAndResolution`.
#[test]
fn resolving_spell_creature_type_choice_offers_its_domain_without_simulation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Distant Melody", false, DISTANT_MELODY);
    builder.with_mana_cost(ManaCost::Cost {
        generic: 3,
        shards: vec![ManaCostShard::Blue],
    });
    let melody = builder.id();
    let mut pool = colorless(melody, 3);
    pool.push(ManaUnit::new(ManaType::Blue, melody, false, vec![]));
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();
    with_p0_priority(&mut runner);

    let outcome = runner.cast(melody).resolve();
    let state = outcome.state();
    assert_eq!(
        prompt_binding(state),
        Some(NamedChoiceSourceBinding::ExactObjectAndResolution),
        "a persisting resolution-time choice binds to the exact resolving source"
    );
    let WaitingFor::NamedChoice {
        choice_type,
        options,
        ..
    } = &state.waiting_for
    else {
        unreachable!("prompt_binding checked the prompt shape");
    };
    assert!(matches!(choice_type, ChoiceType::CreatureType { .. }));
    // CR 205.3m: the prompt offers the engine's whole creature-type domain. The
    // scenario loads no card database, so that is `compute_options`' built-in
    // fallback list rather than the full card-pool list a real game offers.
    assert!(
        options.len() > 1,
        "the creature-type prompt must offer more than one type; got {}",
        options.len()
    );

    assert_offers_named_domain_without_simulation(state);
}

/// CR 608.2d: Stave Off's color choice, raised while the instant resolves. Like
/// Distant Melody above, its answer persists on the resolving spell ("the color
/// of your choice" is read back by the protection grant), so the prompt is
/// source-bound with `ExactObjectAndResolution` and carries its matching
/// context — the arm the reducer answers through `bind_named_choice`.
///
/// The sibling `ResolutionContext` binding is reached in production by a
/// `CardPredicateGuess` prompt (Gollum, Scheming Guide's opponent guess); it
/// takes the same hatch arm and has no fixture here.
#[test]
fn resolving_spell_persisted_color_choice_offers_its_domain_without_simulation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut builder = scenario.add_spell_to_hand_from_oracle(P0, "Stave Off", true, STAVE_OFF);
    builder.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::White],
    });
    let stave_off = builder.id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::White, stave_off, false, vec![])],
    );
    let mut runner = scenario.build();
    with_p0_priority(&mut runner);

    let outcome = runner.cast(stave_off).target_object(bear).resolve();
    let state = outcome.state();
    assert_eq!(
        prompt_binding(state),
        Some(NamedChoiceSourceBinding::ExactObjectAndResolution),
        "a persisting resolution-time choice binds to the exact resolving source"
    );
    let WaitingFor::NamedChoice {
        choice_type,
        options,
        source,
        ..
    } = &state.waiting_for
    else {
        unreachable!("prompt_binding checked the prompt shape");
    };
    assert!(
        source
            .as_ref()
            .is_some_and(|source| source.has_matching_context()),
        "the source-bound prompt must carry its matching context"
    );
    assert!(matches!(choice_type, ChoiceType::Color { .. }));
    let offered: BTreeSet<&str> = options.iter().map(String::as_str).collect();
    assert_eq!(
        offered,
        BTreeSet::from(["White", "Blue", "Black", "Red", "Green"]),
        "\"the color of your choice\" is any of the five colors"
    );

    assert_offers_named_domain_without_simulation(state);
}

/// CR 608.2d: Brave the Elements' color choice, raised while the instant
/// resolves. Its `Choose` does not persist, so the prompt carries no source —
/// the source-less arm of the named-choice hatch.
#[test]
fn resolving_spell_unbound_color_choice_offers_its_domain_without_simulation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Brave the Elements", true, BRAVE_THE_ELEMENTS);
    builder.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::White],
    });
    let brave = builder.id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::White, brave, false, vec![])],
    );
    let mut runner = scenario.build();
    with_p0_priority(&mut runner);

    let outcome = runner.cast(brave).resolve();
    let state = outcome.state();
    assert_eq!(
        prompt_binding(state),
        None,
        "a non-persisting resolution-time choice carries no source"
    );
    let WaitingFor::NamedChoice {
        choice_type,
        options,
        ..
    } = &state.waiting_for
    else {
        unreachable!("prompt_binding checked the prompt shape");
    };
    assert!(matches!(choice_type, ChoiceType::Color { .. }));
    let offered: BTreeSet<&str> = options.iter().map(String::as_str).collect();
    assert_eq!(
        offered,
        BTreeSet::from(["White", "Blue", "Black", "Red", "Green"]),
        "\"choose a color\" is any of the five colors"
    );

    assert_offers_named_domain_without_simulation(state);
}
