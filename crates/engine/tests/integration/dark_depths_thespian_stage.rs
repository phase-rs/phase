//! GitHub issue #1514 — Dark Depths + Thespian's Stage combo doesn't work.
//!
//! Oracle text:
//!   Dark Depths: "Dark Depths enters with ten ice counters on it.
//!     {3}: Remove an ice counter from Dark Depths.
//!     When Dark Depths has no ice counters on it, sacrifice it. If you do,
//!     create Marit Lage, a legendary 20/20 black Avatar creature token with
//!     flying and indestructible."
//!   Thespian's Stage: "{T}: Add {C}.
//!     {2}, {T}: This land becomes a copy of target land, except it has this
//!     ability."
//!
//! The classic combo: P0 controls Dark Depths (10 ice counters) and Thespian's
//! Stage. Activate Stage's BecomeCopy targeting Depths. After resolution the
//! Stage's intrinsic copiable values are now Dark Depths' — including the
//! "has no ice counters" state-trigger — but per CR 707.10 / CR 122.1c the
//! copy has zero counters (a copy does not inherit counters; the
//! "enters with ten ice counters" replacement only applies on enter, and the
//! Stage doesn't re-enter). So the copy's state trigger immediately fires.
//!
//! What the reporter observed: no Marit Lage token appeared. This test drives
//! the real pipeline (activate → BecomeCopy resolve → state trigger scan →
//! resolve sacrifice + Marit Lage creation) and asserts the combo works
//! end-to-end.
//!
//! CR references (verified against `docs/MagicCompRules.txt`):
//!   - CR 121.1c: a permanent enters with no counters except as the result of
//!     a replacement / sub-effect specifying otherwise.
//!   - CR 122.1: counter handling.
//!   - CR 603.8: state triggers re-evaluate after each SBA cycle.
//!   - CR 707.2: a copy effect copies the source's copiable values.
//!   - CR 707.10: counters on the original are not part of copiable values.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Duration, Effect, TargetFilter, TypeFilter,
    TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const DARK_DEPTHS_ORACLE: &str = "Dark Depths enters with ten ice counters on it.\n\
     {3}: Remove an ice counter from Dark Depths.\n\
     When Dark Depths has no ice counters on it, sacrifice it. If you do, \
     create Marit Lage, a legendary 20/20 black Avatar creature token with flying \
     and indestructible.";

/// Fill a player's mana pool directly. Mirrors the helper used in other
/// activation-pipeline integration tests (see `urzas_saga_chapter_two`).
fn add_mana(
    runner: &mut engine::game::scenario::GameRunner,
    player: PlayerId,
    color: ManaType,
    count: usize,
) {
    let state = runner.state_mut();
    let p = state.players.iter_mut().find(|p| p.id == player).unwrap();
    for _ in 0..count {
        p.mana_pool
            .add(ManaUnit::new(color, ObjectId(0), false, Vec::new()));
    }
}

/// Convert a battlefield creature object into a Land permanent (strip
/// `Creature` from core types, clear P/T, sync base values). `add_creature*`
/// is the only path that parses Oracle text into abilities, so lands have to
/// post-process the resulting object to drop the Creature scaffolding.
fn convert_to_land(runner: &mut engine::game::scenario::GameRunner, id: ObjectId) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types
        .core_types
        .retain(|t| *t != CoreType::Creature);
    obj.card_types.core_types.push(CoreType::Land);
    obj.power = None;
    obj.toughness = None;
    obj.base_power = None;
    obj.base_toughness = None;
    obj.base_card_types = obj.card_types.clone();
}

/// Drive priority + targeting + legend-rule prompts until the engine settles.
/// Picks the supplied target whenever a `TargetSelection` / `TriggerTargetSelection`
/// prompt appears; picks `legend_keep` whenever the legend rule asks which of
/// the duplicate legendaries to keep (CR 704.5j). Bounded loop.
fn drain_until_settled(
    runner: &mut engine::game::scenario::GameRunner,
    auto_target: engine::types::ability::TargetRef,
    legend_keep: ObjectId,
) {
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                if runner
                    .act(GameAction::SelectTargets {
                        targets: vec![auto_target.clone()],
                    })
                    .is_err()
                {
                    break;
                }
            }
            // CR 704.5j: legend rule asks the controller which of the same-name
            // legendaries to keep. The Dark Depths combo turns on picking the
            // *copy* (zero counters) so the state trigger fires next SBA cycle.
            WaitingFor::ChooseLegend { .. } => {
                if runner
                    .act(GameAction::ChooseLegend { keep: legend_keep })
                    .is_err()
                {
                    break;
                }
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
}

/// CR 707.10 + CR 603.8: After Thespian's Stage becomes a copy of Dark Depths
/// (which carries the "has no ice counters" state trigger) the Stage has zero
/// counters — copies don't inherit counters from the original. The state
/// trigger fires on the next SBA / state-trigger scan, the Stage is
/// sacrificed, and a Marit Lage token enters the battlefield.
#[test]
fn thespians_stage_copies_dark_depths_then_state_trigger_creates_marit_lage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);

    // "Dark Depths" — printed state trigger comes from the Oracle parser; we
    // seed the 10 ice counters directly (already on the battlefield in the
    // scenario, so the ETB replacement does not run).
    let depths = scenario
        .add_creature_from_oracle(P0, "Dark Depths", 0, 0, DARK_DEPTHS_ORACLE)
        .as_legendary()
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.with_counter(depths, CounterType::Generic("ice".to_string()), 10);

    // "Thespian's Stage" — the BecomeCopy ability is added manually with
    // `TargetFilter::Any` so the test selects Dark Depths via SelectTargets.
    let stage = scenario
        .add_creature(P0, "Thespian's Stage", 0, 0)
        .with_mana_cost(ManaCost::zero())
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::BecomeCopy {
                    recipient: TargetFilter::SelfRef,
                    target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)),
                    duration: Some(Duration::Permanent),
                    mana_value_limit: None,
                    additional_modifications: Vec::new(),
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            shards: vec![],
                            generic: 2,
                        },
                    },
                    AbilityCost::Tap,
                ],
            }),
        )
        .id();

    let mut runner = scenario.build();
    convert_to_land(&mut runner, depths);
    convert_to_land(&mut runner, stage);

    // Pre-pay the {2} cost; the {T} cost is paid automatically.
    add_mana(&mut runner, P0, ManaType::Colorless, 2);

    // Sanity: Depths has 10 ice counters, Stage has 0.
    assert_eq!(
        runner.state().objects[&depths]
            .counters
            .get(&CounterType::Generic("ice".to_string()))
            .copied()
            .unwrap_or(0),
        10,
        "Dark Depths must start the scenario with 10 ice counters"
    );
    assert_eq!(
        runner.state().objects[&stage]
            .counters
            .get(&CounterType::Generic("ice".to_string()))
            .copied()
            .unwrap_or(0),
        0,
        "Thespian's Stage must start with zero counters"
    );

    // Activate the BecomeCopy ability on the Stage. The ability is the only
    // ability we added, so its index is 0.
    let activate_index = runner.state().objects[&stage]
        .abilities
        .iter()
        .position(|a| matches!(*a.effect, Effect::BecomeCopy { .. }))
        .expect("Stage must have a BecomeCopy ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: stage,
            ability_index: activate_index,
        })
        .expect("activating Thespian's Stage's BecomeCopy must succeed");

    // Drive targeting + resolution. The Stage targets Dark Depths, BecomeCopy
    // resolves, layer 1 swaps Stage's copiable values for Depths' — including
    // the state trigger. The Stage has zero ice counters, so the state
    // trigger immediately fires and resolves (sacrifice Stage + create Marit
    // Lage).
    // CR 704.5j: when the legend prompt comes up (two "Dark Depths" share a
    // name), keep the copy — the Stage. That's the side the combo wants on
    // the battlefield with zero counters so the state trigger can fire.
    drain_until_settled(
        &mut runner,
        engine::types::ability::TargetRef::Object(depths),
        stage,
    );

    let state = runner.state();

    // CR 704.5j: the controller kept the Stage in the legend prompt, so the
    // ORIGINAL Dark Depths was put into the graveyard by the legend rule.
    // The Stage (the surviving copy) has zero ice counters, fires the copied
    // state trigger, and sacrifices itself — also ending up in the graveyard.
    assert_eq!(
        state.objects[&depths].zone,
        Zone::Graveyard,
        "Dark Depths (the original) must be in the graveyard after the legend \
         rule resolved with Stage chosen as the kept copy"
    );
    assert_eq!(
        state.objects[&stage].zone,
        Zone::Graveyard,
        "Thespian's Stage (the copy with zero ice counters) must be sacrificed \
         by the copied 'has no ice counters' state trigger; instead it is in {:?}",
        state.objects[&stage].zone
    );

    // Marit Lage: a 20/20 black Avatar creature token controlled by P0.
    let marit_lage: Vec<_> = state
        .objects
        .values()
        .filter(|o| o.is_token && o.name == "Marit Lage")
        .collect();
    assert_eq!(
        marit_lage.len(),
        1,
        "exactly one Marit Lage token must be created — found {} (objects on \
         battlefield: {:?})",
        marit_lage.len(),
        state
            .battlefield
            .iter()
            .map(|id| state.objects[id].name.clone())
            .collect::<Vec<_>>()
    );
    let token = marit_lage[0];
    assert_eq!(token.controller, P0);
    assert_eq!(token.zone, Zone::Battlefield);
    assert_eq!(
        token.power,
        Some(20),
        "Marit Lage must be 20/20 (got {:?})",
        token.power
    );
    assert_eq!(token.toughness, Some(20));
    assert!(
        token.card_types.core_types.contains(&CoreType::Creature),
        "Marit Lage must be a Creature, got {:?}",
        token.card_types.core_types
    );
}
