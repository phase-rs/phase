//! The prevented-this-way rider class (#8777): a prevention shield's leading
//! "for each 1 damage prevented this way, <effect>" rider must actually fire
//! against the referent the RIDER names — the parent's chosen target when the
//! rider anaphorically refers to it ("that creature"/"it"), or the shield's
//! own untargeted scope when the rider does not (Inkshield's token payoff).
//! Class members exercised here: Inkshield (untargeted, non-anaphoric —
//! H-12), Test of Faith (targeted, `Next(N)` shield — H-1/H-3), Brace for
//! Impact (targeted, `All` shield, combat-batch drain — H-2).
//!
//! Oracle text under test (verified against Scryfall 2026-09-12):
//!   Inkshield: "Prevent all combat damage that would be dealt to you this
//!     turn. For each 1 damage prevented this way, create a 2/1 white and
//!     black Inkling creature token with flying."
//!   Test of Faith: "Prevent the next 3 damage that would be dealt to target
//!     creature this turn. For each 1 damage prevented this way, put a +1/+1
//!     counter on that creature."
//!   Brace for Impact: "Prevent all damage that would be dealt to target
//!     multicolored creature this turn. For each 1 damage prevented this way,
//!     put a +1/+1 counter on that creature."
//!
//! CR 615 (prevention) + CR 615.5 (prevented-this-way follow-up) +
//! CR 510.2 (simultaneous combat damage) + CR 111.1 (token creation) +
//! CR 615.7 (`Next(N)` depletion shields) + CR 122.1a (+1/+1 counters) +
//! CR 704.5g (lethal damage SBA).

use engine::game::combat::AttackTarget;
use engine::game::effects::deal_damage;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};
use engine::types::ability::{
    ChosenGroupId, Effect, PreventionAmount, QuantityExpr, ReplacementDefinition,
    ReplacementOrigin, ResolvedAbility, ShieldKind, TargetFilter, TargetRef,
};
use engine::types::actions::{GameAction, ReplacementAutoChoice};
use engine::types::counter::CounterType;
use engine::types::game_state::{
    GameState, PersistedGameState, ReplacementAutoChoiceIdentity, WaitingFor,
};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::proposed_event::AppliedReplacementKey;
use engine::types::resolution::ResolutionStateWire;
use engine::types::zones::Zone;

const INKSHIELD_TEXT: &str = "Prevent all combat damage that would be dealt to you this turn. \
     For each 1 damage prevented this way, create a 2/1 white and black Inkling creature token \
     with flying.";

/// Cast Inkshield from P0's hand on P0's own pre-combat main, then flip the
/// active player to P1 so P1's combat runs into the turn-scoped shield.
/// Mirrors `comeuppance.rs`'s `cast_comeuppance_then_p1_turn`.
fn cast_inkshield_then_p1_turn(
    scenario_setup: impl FnOnce(&mut GameScenario),
) -> engine::game::scenario::GameRunner {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let inkshield = scenario
        .add_spell_to_hand_from_oracle(P0, "Inkshield", true, INKSHIELD_TEXT)
        .id();
    scenario_setup(&mut scenario);

    let mut runner = scenario.build();
    runner.cast(inkshield).resolve();
    runner.state_mut().active_player = P1;
    runner
}

fn run_combat(
    runner: &mut engine::game::scenario::GameRunner,
    attacks: &[(ObjectId, AttackTarget)],
) {
    let mut attacked = false;
    for _ in 0..400 {
        match runner.state().phase {
            Phase::EndCombat | Phase::PostCombatMain => break,
            _ => {}
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::OrderTriggers { .. } => {
                if runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .is_err()
                {
                    break;
                }
            }
            WaitingFor::DeclareAttackers { player, .. } if !attacked => {
                attacked = true;
                let a = if player == P1 {
                    attacks.to_vec()
                } else {
                    vec![]
                };
                if runner.declare_attackers(&a).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareAttackers { .. } => {
                if runner.declare_attackers(&[]).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareBlockers { .. } => {
                // The attacker is unblocked in every prompt: this fixture's
                // whole point is that its combat damage reaches P0 and is
                // prevented there.
                if runner.declare_blockers(&[]).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
}

fn inkling_count(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Battlefield && o.controller == P0 && o.name.contains("Inkling"))
        .count()
}

/// #8777 — a single unblocked 3/3 attacker: all 3 combat damage is prevented
/// AND three 2/1 flying Inklings are created (CR 615.5 + CR 510.2).
#[test]
fn inkshield_prevents_combat_damage_and_creates_one_inkling_per_damage() {
    let mut attacker_id = None;
    let mut runner = cast_inkshield_then_p1_turn(|sc| {
        attacker_id = Some(sc.add_creature(P1, "Raging Bear", 3, 3).id());
    });
    let attacker = attacker_id.unwrap();
    let p0_life_before = runner.life(P0);

    // Reach-guard: the shield really is installed before combat.
    assert!(
        runner
            .state()
            .pending_damage_replacements
            .iter()
            .any(|r| r.shield_kind.is_shield()),
        "reach guard: Inkshield's prevention shield must be installed before combat"
    );

    runner.advance_to_combat();
    run_combat(&mut runner, &[(attacker, AttackTarget::Player(P0))]);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P0),
        p0_life_before,
        "Inkshield prevents all combat damage dealt to its controller"
    );
    assert_eq!(
        inkling_count(&runner),
        3,
        "CR 615.5: one 2/1 Inkling per 1 damage prevented this way (3 prevented)"
    );
}

const TEST_OF_FAITH_TEXT: &str = "Prevent the next 3 damage that would be dealt to target \
     creature this turn. For each 1 damage prevented this way, put a +1/+1 counter on that \
     creature.";

const BRACE_FOR_IMPACT_TEXT: &str = "Prevent all damage that would be dealt to target \
     multicolored creature this turn. For each 1 damage prevented this way, put a +1/+1 \
     counter on that creature.";

/// Hand-built noncombat damage ability, mirroring
/// `awe_strike_prevention.rs`'s `damage_ability` helper: the spell under test
/// is cast through the real pipeline; this is unrelated scaffolding to
/// deliver damage afterward, not a `resolve_top` shortcut for the spell.
fn noncombat_damage_ability(
    source_id: ObjectId,
    target: TargetRef,
    amount: i32,
) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: amount },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
        vec![target],
        source_id,
        P1,
    )
}

fn plus1plus1_counters(runner: &engine::game::scenario::GameRunner, obj: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&obj)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

fn damage_marked(runner: &engine::game::scenario::GameRunner, obj: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&obj)
        .map(|o| o.damage_marked)
        .unwrap_or(0)
}

/// #8777 (H-1) — Test of Faith: real cast, targeted, `Next(3)` depletion
/// shield (CR 615.7), per-event drain. This is the maintainer-named
/// regression: pre-fix, the `PutCounter { target: ParentTarget }` rider is
/// detached into `runtime_execute` with an empty `targets` vector, so
/// `ParentTarget` resolves to nothing and zero counters are put on. Step T0
/// measures this RED at `BASE_SHA`.
///
/// CR 615.7 (Next(3) depletion shield) + CR 615.5 (one counter per 1 damage
/// prevented) + CR 122.1a (+1/+1 counters) + CR 608.2c (the rider's "that
/// creature" anaphor names the parent's chosen target).
#[test]
fn test_of_faith_puts_one_counter_per_damage_prevented() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Bear", 3, 3).id();
    let attacker = scenario.add_creature(P1, "Attacker", 3, 3).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test of Faith", true, TEST_OF_FAITH_TEXT)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_objects(&[bear]).resolve();

    // Reach-guards: the cast resolved cleanly to a priority window, and the
    // shield installed on the chosen creature.
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "reach guard: Test of Faith must resolve to a clean priority window, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(
        !outcome.state().objects[&bear]
            .replacement_definitions
            .as_slice()
            .is_empty(),
        "reach guard: the prevention shield must be installed on the targeted bear"
    );

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &noncombat_damage_ability(attacker, TargetRef::Object(bear), 3),
        &mut events,
    )
    .expect("3 noncombat damage from the attacker resolves");

    assert_eq!(
        damage_marked(&runner, bear),
        0,
        "CR 615.7: the Next(3) shield must prevent all 3 damage"
    );
    assert_eq!(
        plus1plus1_counters(&runner, bear),
        3,
        "CR 615.5: one +1/+1 counter per 1 damage prevented (3 prevented) — the rider must \
         resolve against the parent's chosen target, not an empty referent set"
    );
}

/// #8777 (H-3, negative) — Test of Faith: no damage is ever dealt, so zero
/// counters go on. Mandatory positive pairing: the same test asserts the
/// shield WAS installed and its `runtime_execute` names the bear, so the
/// zero cannot be satisfied by "the spell fizzled" (foot-gun 6).
#[test]
fn test_of_faith_puts_no_counters_when_no_damage_is_prevented() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Bear", 3, 3).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test of Faith", true, TEST_OF_FAITH_TEXT)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_objects(&[bear]).resolve();

    // Mandatory positive pairing: the shield was installed AND its
    // runtime_execute names the bear.
    let shield = outcome.state().objects[&bear]
        .replacement_definitions
        .as_slice()
        .iter()
        .find(|r| r.shield_kind.is_shield())
        .expect("reach guard: the prevention shield must be installed on the bear");
    let rider = outcome
        .state()
        .replacement_rider(shield)
        .expect("reach guard: the shield must carry a runtime_execute rider");
    assert_eq!(
        rider.targets,
        vec![TargetRef::Object(bear)],
        "reach guard: the installed rider must name the bear as its referent"
    );

    // Advance to the end step without ever dealing damage.
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    assert_eq!(
        plus1plus1_counters(&runner, bear),
        0,
        "no damage was prevented, so zero +1/+1 counters must be put on"
    );
}

/// CR 616.1 + CR 616.1e + CR 615.5 + CR 615.7: Test of Faith cast twice on one
/// Bear gives two object-hosted `Next(3)` shields, each with its own rider. When
/// Lightning Bolt would deal 3 damage to the Bear, both apply, so the Bear's
/// controller chooses one: it prevents all 3 and puts 3 +1/+1 counters on the Bear;
/// no damage remains, so the other is not used and keeps its 3 (CR 616.1f, CR 615.3).
/// The CR 616.1 choice's saved-preference identity holds each shield's definition
/// with its handle cleared and the rider by value.
///
/// Lightning Bolt (verbatim): "Lightning Bolt deals 3 damage to any target."
#[test]
fn test_of_faith_twice_offers_a_rider_bearing_saved_choice() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Bear", 3, 3).id();
    let faiths = [0, 1].map(|_| {
        scenario
            .add_spell_to_hand_from_oracle(P0, "Test of Faith", true, TEST_OF_FAITH_TEXT)
            .id()
    });
    let bolt = scenario
        .add_spell_to_hand_from_oracle(
            P1,
            "Lightning Bolt",
            true,
            "Lightning Bolt deals 3 damage to any target.",
        )
        .id();
    let mut runner = scenario.build();
    let arena = runner.state().replacement_continuations.len();
    for faith in faiths {
        let outcome = runner.cast(faith).target_objects(&[bear]).resolve();
        assert!(
            matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
            "reach guard: Test of Faith resolves to a priority window, got {:?}",
            outcome.final_waiting_for()
        );
    }
    let installed_shields = runner.state().objects[&bear]
        .replacement_definitions
        .as_slice()
        .to_vec();
    assert!(
        installed_shields.len() == 2
            && installed_shields[0].runtime_execute.is_some()
            && installed_shields[0].runtime_execute != installed_shields[1].runtime_execute,
        "reach guard: the Bear holds two shields under distinct handles: {installed_shields:?}"
    );
    assert_eq!(runner.state().replacement_continuations.len(), arena + 2);

    // Harness shortcut, as in `gatta_and_luzzu_prevents_through_the_real_cast_pipeline`:
    // hand P1 priority so it can cast its Bolt this turn.
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    let outcome = runner.cast(bolt).target_object(bear).resolve();
    let WaitingFor::ReplacementChoice {
        player,
        candidate_count,
        remember_identity: Some(key),
        ..
    } = outcome.final_waiting_for().clone()
    else {
        panic!(
            "CR 616.1: both shields apply, so P0 chooses one; got {:?}",
            outcome.final_waiting_for()
        );
    };
    assert_eq!((player, candidate_count), (P0, 2));
    fn assert_identities(
        state: &GameState,
        bear: ObjectId,
        installed_shields: &[ReplacementDefinition],
        identities: &[ReplacementAutoChoiceIdentity],
        label: &str,
    ) {
        let mut indices = Vec::new();
        for identity in identities {
            let ReplacementAutoChoiceIdentity::Definition {
                source,
                index,
                occurrence,
                definition,
                rider,
            } = identity
            else {
                panic!("{label}: a shield identity is a definition: {identity:?}");
            };
            let object = &state.objects[&bear];
            let host = ObjectIncarnationRef::from_object(object);
            assert_eq!(source, &host, "{label}: exact source incarnation");
            let AppliedReplacementKey::Object {
                source: key_source,
                index: ordinal,
                copy_effect: None,
                resolution_host: Some(key_host),
            } = occurrence.expect("a resolution-installed shield has its full occurrence key")
            else {
                panic!(
                    "{label}: expected an exact resolution-installed object key: {occurrence:?}"
                );
            };
            assert_eq!(
                (key_source, key_host, ordinal),
                (bear, host, *index),
                "{label}: exact key/index/host"
            );
            let has_ordinal = |candidate: &&ReplacementDefinition| matches!(candidate.origin, ReplacementOrigin::Resolution { installation: Some(found) } if found == ordinal);
            let live_definitions: Vec<_> = object
                .replacement_definitions
                .as_slice()
                .iter()
                .filter(has_ordinal)
                .collect();
            let [live_definition] = live_definitions.as_slice() else {
                panic!("{label}: the stable ordinal resolves exactly once: {live_definitions:?}");
            };
            let originals: Vec<_> = installed_shields.iter().filter(has_ordinal).collect();
            let [original] = originals.as_slice() else {
                panic!("{label}: the original installation resolves exactly once: {originals:?}");
            };
            assert_eq!(definition.runtime_execute, None, "{label}: handle cleared");
            let mut original_definition = (**original).clone();
            original_definition.runtime_execute = None;
            assert_eq!(
                definition.as_ref(),
                &original_definition,
                "{label}: exact installed definition by value"
            );
            assert_eq!(
                live_definition.origin, original.origin,
                "{label}: consumption preserves the installation"
            );
            // The remembered program stays original; only depletion is live.
            let mut live_program = (**live_definition).clone();
            live_program.runtime_execute = None;
            live_program.is_consumed = original_definition.is_consumed;
            live_program.shield_kind = original_definition.shield_kind;
            assert_eq!(
                live_program, original_definition,
                "{label}: live program changes only its shield depletion state"
            );
            let live = state.replacement_rider(live_definition);
            assert!(live.is_some(), "{label}: the shield names a rider");
            assert_eq!(
                rider.as_deref(),
                live,
                "{label}: the rider is held by value"
            );
            indices.push(*index);
        }
        assert!(
            indices.len() == 2 && indices[0] != indices[1],
            "{label}: {indices:?}"
        );
    }
    assert_identities(
        runner.state(),
        bear,
        &installed_shields,
        &key.candidates,
        "prompt",
    );

    runner
        .act(GameAction::ChooseReplacementAndRemember {
            choice: ReplacementAutoChoice::Order { order: vec![0, 1] },
        })
        .expect("P0 orders the two shields and remembers the order");
    runner.advance_until_stack_empty();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(
        damage_marked(&runner, bear),
        0,
        "CR 615.7: all 3 damage prevented"
    );
    assert_eq!(
        plus1plus1_counters(&runner, bear),
        3,
        "CR 615.5: one counter per 1 damage prevented"
    );
    let shields = runner.state().objects[&bear]
        .replacement_definitions
        .as_slice()
        .to_vec();
    assert_eq!(
        shields.iter().filter(|shield| shield.is_consumed).count(),
        1,
        "CR 615.7: the chosen shield is used up"
    );
    assert!(
        shields.iter().any(|shield| !shield.is_consumed
            && shield.shield_kind
                == ShieldKind::Prevention {
                    amount: PreventionAmount::Next(3)
                }),
        "CR 616.1f + CR 615.3: the other shield is not used and keeps its 3"
    );
    let records = &runner.state().replacement_auto_choices;
    assert_eq!(records.len(), 1);
    assert_identities(
        runner.state(),
        bear,
        &installed_shields,
        &records[0].key.candidates,
        "record",
    );
}

/// Downgrade only the representation of actual installed riders. Keep the
/// existing arena beside the inline carriers, including its exact slots.
fn inline_rider_roundtrips(state: &GameState) -> Vec<(&'static str, GameState)> {
    fn inline(value: &mut serde_json::Value, arena: &[serde_json::Value]) -> usize {
        match value {
            serde_json::Value::Array(values) => {
                values.iter_mut().map(|value| inline(value, arena)).sum()
            }
            serde_json::Value::Object(object) => {
                let mut count = 0;
                if let Some(slot) = object.get_mut("runtime_execute") {
                    if let Some(index) = slot.as_u64() {
                        *slot = arena[index as usize].clone();
                        count += 1;
                    }
                }
                count
                    + object
                        .iter_mut()
                        .filter(|(key, _)| {
                            *key != "runtime_execute" && *key != "replacement_continuations"
                        })
                        .map(|(_, value)| inline(value, arena))
                        .sum::<usize>()
            }
            _ => 0,
        }
    }

    let arena: Vec<_> = state
        .replacement_continuations
        .iter()
        .map(|rider| serde_json::to_value(rider).unwrap())
        .collect();
    assert!(!arena.is_empty(), "real cast installed an arena rider");
    ["bare", "persisted raw", "trusted", "resolution wire"]
        .into_iter()
        .map(|ingress| {
            let mut value = match ingress {
                "bare" => serde_json::to_value(state),
                "persisted raw" => {
                    serde_json::to_value(PersistedGameState::Raw(Box::new(state.clone())))
                }
                "trusted" => serde_json::to_value(PersistedGameState::capture(state.clone())),
                "resolution wire" => {
                    serde_json::to_value(ResolutionStateWire::from_game_state(state.clone()))
                }
                _ => unreachable!(),
            }
            .unwrap();
            assert!(
                inline(&mut value, &arena) > 0,
                "{ingress}: real inline carrier"
            );
            let restored = match ingress {
                "bare" => serde_json::from_value::<GameState>(value).unwrap(),
                "persisted raw" | "trusted" => serde_json::from_value::<PersistedGameState>(value)
                    .unwrap()
                    .into_game_state()
                    .unwrap(),
                "resolution wire" => serde_json::from_value::<ResolutionStateWire>(value)
                    .unwrap()
                    .into_game_state(),
                _ => unreachable!(),
            };
            assert_eq!(
                restored
                    .replacement_continuations
                    .iter()
                    .take(arena.len())
                    .collect::<Vec<_>>(),
                state.replacement_continuations.iter().collect::<Vec<_>>(),
                "{ingress}: existing arena slots stay exact"
            );
            assert!(restored.replacement_continuations.len() > arena.len());
            let canonical = serde_json::to_value(&restored).unwrap();
            let reloaded: GameState = serde_json::from_value(canonical.clone()).unwrap();
            assert_eq!(serde_json::to_value(&reloaded).unwrap(), canonical);
            (ingress, reloaded)
        })
        .collect()
}

/// Supplemental private map on a clone of the REAL installed rider: this is
/// projection plumbing coverage, not extra Oracle text or an invented origin.
fn assert_installed_rider_projection(state: &GameState, definition: &ReplacementDefinition) {
    let original = serde_json::to_value(state).unwrap();
    let rider = state.replacement_rider(definition).unwrap().clone();
    let handle = definition.runtime_execute.unwrap();
    let mut private = state.clone();
    let unused = private
        .install_replacement_continuation(rider.clone())
        .expect("a copy of the actual independently admitted rider is valid");
    let private_rider = private
        .replacement_continuations
        .get_mut(handle.0 as usize)
        .unwrap();
    private_rider.context.chosen_group_targets = vec![(
        ChosenGroupId(89),
        vec![(TargetRef::Object(ObjectId(9_891_891)), None)],
    )];
    let private_snapshot = serde_json::to_value(&private).unwrap();
    assert!(private_snapshot.to_string().contains("9891891"));
    assert!(!private.replacement_continuations[handle.0 as usize]
        .context
        .chosen_group_targets
        .is_empty());
    for viewer in [Some(P0), Some(P1), None] {
        let projected = viewer.map_or_else(
            || filter_state_for_unseated_viewer(&private),
            |viewer| filter_state_for_viewer(&private, viewer),
        );
        assert_eq!(
            projected.replacement_continuations.len(),
            private.replacement_continuations.len()
        );
        assert!(projected.replacement_continuations[handle.0 as usize]
            .context
            .chosen_group_targets
            .is_empty());
        assert_eq!(
            projected.replacement_continuations[unused.0 as usize], rider,
            "{viewer:?}: an unreferenced public entry keeps its slot and program"
        );
        let wire = serde_json::to_value(&projected).unwrap();
        assert!(!wire.to_string().contains("9891891"));
        let decoded: GameState = serde_json::from_value(wire.clone())
            .expect("seated and unseated display projections transport-decode");
        assert_eq!(
            decoded.replacement_continuations,
            projected.replacement_continuations
        );
        assert!(
            serde_json::from_value::<PersistedGameState>(wire).is_err(),
            "a display projection cannot become persistence authority"
        );
    }
    assert_eq!(serde_json::to_value(&private).unwrap(), private_snapshot);
    assert_eq!(serde_json::to_value(state).unwrap(), original);
}

/// CR 616.1: two real shields produce an actual affected-controller choice.
/// CR 615.5 + CR 615.7: the restored public answer prevents Bolt's three
/// damage and immediately puts exactly three counters on the original target.
#[test]
fn test_of_faith_inline_legacy_choice_reloads_and_publicly_prevents_three() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Bear", 3, 3).id();
    let faiths = [0, 1].map(|_| {
        scenario
            .add_spell_to_hand_from_oracle(P0, "Test of Faith", true, TEST_OF_FAITH_TEXT)
            .id()
    });
    let bolt = scenario
        .add_spell_to_hand_from_oracle(
            P1,
            "Lightning Bolt",
            true,
            "Lightning Bolt deals 3 damage to any target.",
        )
        .id();
    let mut runner = scenario.build();
    for faith in faiths {
        let outcome = runner.cast(faith).target_object(bear).resolve();
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
    }
    let host = ObjectIncarnationRef::from_object(&runner.state().objects[&bear]);
    let installed = runner.state().objects[&bear]
        .replacement_definitions
        .as_slice()
        .to_vec();
    assert_eq!(installed.len(), 2);
    let programs: Vec<_> = installed
        .iter()
        .map(|definition| {
            runner
                .state()
                .replacement_rider(definition)
                .unwrap()
                .clone()
        })
        .collect();
    assert!(programs
        .iter()
        .all(|rider| rider.targets == vec![TargetRef::Object(bear)]));
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    let outcome = runner.cast(bolt).target_object(bear).resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::ReplacementChoice {
                player: P0,
                candidate_count: 2,
                remember_identity: Some(_),
                ..
            }
        ),
        "both actual shields must pause the public Bolt pipeline"
    );
    assert_eq!(damage_marked(&runner, bear), 0);
    assert_eq!(plus1plus1_counters(&runner, bear), 0);
    let original = serde_json::to_value(runner.state()).unwrap();
    for (ingress, restored) in inline_rider_roundtrips(runner.state()) {
        assert_eq!(
            ObjectIncarnationRef::from_object(&restored.objects[&bear]),
            host
        );
        let definitions = restored.objects[&bear].replacement_definitions.as_slice();
        assert_eq!(definitions.len(), 2);
        for ((definition, original), program) in definitions.iter().zip(&installed).zip(&programs) {
            let mut cleared = definition.clone();
            cleared.runtime_execute = None;
            let mut expected = original.clone();
            expected.runtime_execute = None;
            assert_eq!(
                cleared, expected,
                "{ingress}: full installation/lifetime survives"
            );
            assert_eq!(restored.replacement_rider(definition), Some(program));
        }
        assert!(matches!(
            restored.waiting_for,
            WaitingFor::ReplacementChoice {
                player: P0,
                candidate_count: 2,
                remember_identity: Some(_),
                ..
            }
        ));
        assert_eq!(
            restored.waiting_for,
            runner.state().waiting_for,
            "{ingress}: exact live offer keys and by-value programs survive migration"
        );
        let WaitingFor::ReplacementChoice {
            remember_identity: Some(key),
            ..
        } = &restored.waiting_for
        else {
            unreachable!("the real offer was asserted above")
        };
        let saved = serde_json::to_value(&restored).unwrap();
        for order in [vec![0, 1], vec![1, 0]] {
            let mut branch = GameRunner::from_state(restored.clone());
            branch
                .act(GameAction::ChooseReplacementAndRemember {
                    choice: ReplacementAutoChoice::Order { order },
                })
                .expect("the restored affected controller answers through apply");
            branch.advance_until_stack_empty();
            assert_eq!(
                damage_marked(&branch, bear),
                0,
                "{ingress}: three prevented"
            );
            assert_eq!(
                plus1plus1_counters(&branch, bear),
                3,
                "{ingress}: immediate rider exactly once"
            );
            assert_eq!(
                ObjectIncarnationRef::from_object(&branch.state().objects[&bear]),
                host
            );
            let shields = branch.state().objects[&bear]
                .replacement_definitions
                .as_slice();
            assert_eq!(
                shields.iter().filter(|shield| shield.is_consumed).count(),
                1
            );
            assert!(shields.iter().any(|shield| !shield.is_consumed
                && shield.shield_kind
                    == ShieldKind::Prevention {
                        amount: PreventionAmount::Next(3)
                    }));
            assert_eq!(branch.state().replacement_auto_choices.len(), 1);
            assert_eq!(
                &branch.state().replacement_auto_choices[0].key,
                key,
                "{ingress}: remembering retains the exact offered source/ordinal/host/program"
            );
            assert_eq!(
                branch.state().replacement_continuations,
                restored.replacement_continuations,
                "executing a consumed clone preserves the canonical arena"
            );
            assert!(matches!(
                branch.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert!(branch.state().stack.is_empty());
            assert!(branch.state().resolution_stack.is_empty());
            assert!(branch.state().return_result_frames.is_empty());
            assert_eq!(
                serde_json::to_value(&restored).unwrap(),
                saved,
                "the unexecuted state clone retains its original shields and inputs"
            );
        }
        assert_installed_rider_projection(&restored, &definitions[0]);
    }
    assert_eq!(serde_json::to_value(runner.state()).unwrap(), original);
}

/// CR 615.5 + CR 510.2: a migrated real Inkshield rider keeps its untargeted
/// controller scope and creates one token for each of three prevented damage.
#[test]
fn inkshield_inline_legacy_rider_reloads_before_public_combat() {
    let mut attacker = None;
    let runner = cast_inkshield_then_p1_turn(|scenario| {
        attacker = Some(scenario.add_creature(P1, "Attacker", 3, 3).id());
    });
    let attacker = attacker.unwrap();
    let attacker_birth = ObjectIncarnationRef::from_object(&runner.state().objects[&attacker]);
    let shield = runner
        .state()
        .pending_damage_replacements
        .iter()
        .find(|definition| definition.shield_kind.is_shield())
        .unwrap()
        .clone();
    let rider = runner.state().replacement_rider(&shield).unwrap().clone();
    assert_eq!(rider.controller, P0);
    assert_eq!(inkling_count(&runner), 0);
    let life = runner.life(P0);
    for (ingress, restored) in inline_rider_roundtrips(runner.state()) {
        let migrated = restored
            .pending_damage_replacements
            .iter()
            .find(|definition| definition.shield_kind.is_shield())
            .unwrap();
        assert_eq!(restored.replacement_rider(migrated), Some(&rider));
        let mut cleared = migrated.clone();
        cleared.runtime_execute = None;
        let mut expected = shield.clone();
        expected.runtime_execute = None;
        assert_eq!(
            cleared, expected,
            "{ingress}: controller, scope and duration preserved"
        );
        let arena = restored.replacement_continuations.clone();
        let mut branch = GameRunner::from_state(restored);
        branch.advance_to_combat();
        run_combat(&mut branch, &[(attacker, AttackTarget::Player(P0))]);
        branch.advance_until_stack_empty();
        assert_eq!(
            branch.life(P0),
            life,
            "{ingress}: all three combat damage prevented"
        );
        assert_eq!(
            inkling_count(&branch),
            3,
            "{ingress}: three actual prevented-this-way tokens"
        );
        assert_eq!(
            ObjectIncarnationRef::from_object(&branch.state().objects[&attacker]),
            attacker_birth
        );
        assert_eq!(branch.state().replacement_continuations, arena);
        assert!(matches!(
            branch.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(branch.state().stack.is_empty());
        assert!(branch.state().resolution_stack.is_empty());
        assert!(branch.state().return_result_frames.is_empty());
    }
}

/// #8777 (H-2) — Brace for Impact: real cast, targeted (`ColorCount {GE,2}`),
/// `All` shield, combat-batch aggregate drain. A multicolored 1/1 blocks a
/// 3/3 attacker: all 3 damage is prevented, 3 +1/+1 counters go on, and the
/// blocker survives SBAs (CR 704.5g).
///
/// CR 510.2 (simultaneous combat damage) + CR 615.5 + CR 704.5g.
#[test]
fn brace_for_impact_puts_one_counter_per_combat_damage_prevented() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let mut blocker_builder = scenario.add_creature(P0, "Multicolored Blocker", 1, 1);
    blocker_builder.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::Red, ManaCostShard::White],
        generic: 0,
    });
    let blocker = blocker_builder.id();
    let attacker = scenario.add_creature(P1, "Attacker", 3, 3).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Brace for Impact", true, BRACE_FOR_IMPACT_TEXT)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_objects(&[blocker]).resolve();

    // Reach-guard: the cast did NOT halt on TargetSelection — the
    // ColorCount{GE,2} slot accepted the two-color blocker.
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "reach guard: Brace for Impact must resolve past target selection onto a clean \
         priority window, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(
        !outcome.state().objects[&blocker]
            .replacement_definitions
            .as_slice()
            .is_empty(),
        "reach guard: the prevention shield must be installed on the targeted blocker"
    );

    runner.state_mut().active_player = P1;
    let mut attacked = false;
    let mut blocked = false;
    for _ in 0..200 {
        match runner.state().phase {
            Phase::EndCombat | Phase::PostCombatMain => break,
            _ => {}
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::OrderTriggers { .. } => {
                if runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .is_err()
                {
                    break;
                }
            }
            WaitingFor::DeclareAttackers { player, .. } if !attacked => {
                attacked = true;
                let a = if player == P1 {
                    vec![(attacker, AttackTarget::Player(P0))]
                } else {
                    vec![]
                };
                if runner.declare_attackers(&a).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareAttackers { .. } => {
                if runner.declare_attackers(&[]).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareBlockers { player, .. } if !blocked => {
                blocked = true;
                let b = if player == P0 {
                    vec![(blocker, attacker)]
                } else {
                    vec![]
                };
                if runner.declare_blockers(&b).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareBlockers { .. } => {
                if runner.declare_blockers(&[]).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
    runner.advance_until_stack_empty();
    // Reach-guard: the loop reached BOTH declaration prompts. This exists for
    // the two assertions below that pass TRIVIALLY when no combat damage step
    // ran — `damage_marked == 0` and the SBA-survival check are both satisfied
    // by a board where the attacker never attacked. (The `== 3` counter
    // assertion needs no such guard: it reads 0 and fails outright on that
    // board, so it is the load-bearing positive.)
    //
    // Scope, stated precisely: this proves the prompts were REACHED, not that
    // `declare_attackers`/`declare_blockers` succeeded — each flag is set
    // before its call, which may error and break. The counter assertion is
    // what covers that residue. Note also that merely observing the attacker
    // still alive with its printed power — which this guard replaced — would
    // establish nothing, being equally true of a run that never reached
    // `DeclareAttackers` at all.
    assert!(
        attacked && blocked,
        "reach guard: combat must have reached both declaration prompts \
         (attacked={attacked}, blocked={blocked})"
    );

    assert_eq!(
        damage_marked(&runner, blocker),
        0,
        "Brace for Impact's All shield must prevent all combat damage dealt to the blocker"
    );
    assert_eq!(
        plus1plus1_counters(&runner, blocker),
        3,
        "CR 615.5: one +1/+1 counter per 1 damage prevented (3 prevented)"
    );
    assert!(
        runner.state().objects.contains_key(&blocker),
        "CR 704.5g: the blocker must survive SBAs (0 damage marked, no lethal check trips)"
    );
}
