//! CR 118.3: the "you can't pay this cost right now" read-out published on the
//! legal-actions channel by `ai_support::activation_block_reasons`.
//!
//! The reported defect: `can_activate_ability_now_with_restriction_gates` drops
//! an activated ability when its cost is unpayable, so an unaffordable ability
//! never reaches `legal_actions_by_object` and is silently omitted from the
//! picker — no greyed-out row, no explanation. Spells do not behave this way
//! (`spell_costs` exists precisely so an unaffordable spell cost still
//! displays).
//!
//! Every test here drives the production entry point
//! (`ai_support::activation_block_reasons` / `..._for_viewer` →
//! `casting::activation_cost_block_reason` → the shared `activation_verdict`
//! core), never a helper in isolation.
//!
//! Sliver Overlord's Oracle text is byte-identical to the constant the shipped
//! `sliver_overlord_activation_offer.rs` uses. It is NOT verbatim `card-data.json`:
//! that source ends `…Gain control of target Sliver. (This effect lasts
//! indefinitely.)`, and the reminder text is dropped here. Immaterial — the parser
//! strips reminder text — but the two are not the same string.

use std::collections::HashMap;

use engine::ai_support::{
    activation_block_reasons, activation_block_reasons_for_viewer, legal_actions_full,
    MAX_ACTIVATION_BLOCK_TOTAL,
};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityBlockEntry, AbilityBlockKind, AbilityCost, AbilityDefinition, AbilityKind, Effect,
    EffectScope, GameRestriction, ManaContribution, ManaProduction, ProhibitedActivity,
    QuantityExpr, RestrictionExpiry, RestrictionPlayerScope, SacrificeCost, TapStateChange,
    TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::statics::ActivationExemption;
use engine::types::zones::Zone;

const OVERLORD: &str = "{3}: Search your library for a Sliver card, reveal that card, put it into your hand, then shuffle.\n{3}: Gain control of target Sliver.";

// ── helpers ─────────────────────────────────────────────────────────────────

/// The production read-out for `id`, as `(ability_index, sources, kind)` triples
/// sorted by index so assertions are order-independent.
fn blocked(runner: &GameRunner, id: ObjectId) -> Vec<(usize, Vec<ObjectId>, AbilityBlockKind)> {
    entries_of(&activation_block_reasons(runner.state()), id)
}

fn entries_of(
    map: &HashMap<ObjectId, Vec<AbilityBlockEntry>>,
    id: ObjectId,
) -> Vec<(usize, Vec<ObjectId>, AbilityBlockKind)> {
    let mut out: Vec<_> = map
        .get(&id)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .map(|e| (e.ability_index, e.reason.sources.clone(), e.reason.kind))
        .collect();
    out.sort_by_key(|(i, _, _)| *i);
    out
}

/// The ability indices the engine actually OFFERS for `id` — the dispatchable
/// set, which must stay disjoint from the read-out.
fn offered_indices(runner: &GameRunner, id: ObjectId) -> Vec<usize> {
    let (_, _, grouped) = legal_actions_full(runner.state());
    let mut out: Vec<usize> = grouped
        .get(&id)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter_map(|a| match a {
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } if *source_id == id => Some(*ability_index),
            _ => None,
        })
        .collect();
    out.sort_unstable();
    out
}

/// `{cost} generic: draw a card` — a plain, resource-verdict mana cost.
fn draw_for_generic(cost: u32) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Mana {
        cost: ManaCost::generic(cost),
    })
}

/// `Pay N life: draw a card` — the canonical resource-verdict non-mana cost,
/// used as the paired positive control in every negative test below.
fn draw_for_life(amount: i32) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::PayLife {
        amount: QuantityExpr::Fixed { value: amount },
    })
}

/// CR 113.6b: the same `Pay N life: draw` ability, but activatable from the
/// graveyard. `AbilityDefinition` has no `activation_zone` builder, so the field
/// is set directly.
fn graveyard_draw_for_life(amount: i32) -> AbilityDefinition {
    let mut def = draw_for_life(amount);
    def.activation_zone = Some(Zone::Graveyard);
    def
}

fn draw_effect() -> Effect {
    Effect::Draw {
        count: QuantityExpr::Fixed { value: 1 },
        target: TargetFilter::Controller,
    }
}

// ── Row 1 — the reported defect ─────────────────────────────────────────────

/// Row 1: both of Sliver Overlord's printed `{3}` abilities are read out on a
/// board that cannot produce three mana.
///
/// REVERT: drop the `CostNotPayableNow` arm (or restore the plain
/// `return false` at the CR 118.3 exit) and the map has no entry for the
/// Overlord, failing `len() == 2`.
#[test]
fn overlord_unaffordable_printed_abilities_are_read_out() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let overlord = scenario
        .add_creature(P0, "Sliver Overlord", 7, 7)
        .with_subtypes(vec!["Sliver", "Mutant"])
        .from_oracle_text(OVERLORD)
        .id();
    // A second Sliver so ability 1 ("Gain control of target Sliver") has a
    // legal target — otherwise row 7's tail would correctly suppress it.
    scenario
        .add_creature(P0, "Sliver Drone", 1, 1)
        .with_subtypes(vec!["Sliver"]);
    let runner = scenario.build();

    let entries = blocked(&runner, overlord);
    assert_eq!(
        entries.len(),
        2,
        "both printed {{3}} abilities are read out on a tapped-out board; got {entries:?}"
    );
    assert_eq!(
        entries,
        vec![
            (0, vec![], AbilityBlockKind::CostNotPayableNow),
            (1, vec![], AbilityBlockKind::CostNotPayableNow),
        ],
        "each entry names the CR 118.3 arm and carries no prohibiting source"
    );

    // Reach-guard / sibling: give the controller three lands and the SAME
    // abilities become offered instead of read out.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let overlord_b = scenario
        .add_creature(P0, "Sliver Overlord", 7, 7)
        .with_subtypes(vec!["Sliver", "Mutant"])
        .from_oracle_text(OVERLORD)
        .id();
    scenario
        .add_creature(P0, "Sliver Drone", 1, 1)
        .with_subtypes(vec!["Sliver"]);
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let runner_b = scenario.build();
    assert!(
        blocked(&runner_b, overlord_b).is_empty(),
        "with three untapped lands the abilities are affordable, so nothing is read out"
    );
    assert!(
        offered_indices(&runner_b, overlord_b).contains(&0),
        "paired positive: the affordable board genuinely OFFERS ability 0"
    );
}

// ── Row 2b — offered and blocked are disjoint ───────────────────────────────

/// Row 2b: the read-out and the dispatchable set never intersect on
/// `(ObjectId, ability_index)`. Both sets are non-empty in the same assertion,
/// so an empty-vs-empty pass is impossible.
#[test]
fn offered_and_blocked_ability_indices_are_disjoint() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    let obj = scenario
        .add_creature(P0, "Two Ability Engine", 2, 2)
        // index 0: affordable (1 life at 20 life)
        .with_ability_definition(draw_for_life(1))
        // index 1: unaffordable ({7} with no lands)
        .with_ability_definition(draw_for_generic(7))
        .id();
    let runner = scenario.build();

    let offered = offered_indices(&runner, obj);
    let read_out: Vec<usize> = blocked(&runner, obj)
        .into_iter()
        .map(|(i, _, _)| i)
        .collect();

    assert!(
        !offered.is_empty(),
        "paired positive: the affordable ability IS offered; got {offered:?}"
    );
    assert!(
        !read_out.is_empty(),
        "paired positive: the unaffordable ability IS read out; got {read_out:?}"
    );
    assert!(
        offered.iter().all(|i| !read_out.contains(i)),
        "offered {offered:?} and read-out {read_out:?} must be disjoint"
    );
}

// ── Rows 3 / 3b / 3c — the carve-out, both arms ─────────────────────────────

/// Row 3: a tap-only cost is not read out — the `Some(inner)` arm of
/// `cost_conclusively_payable_by_cheap_gate`. A tapped source's `{T}` ability is
/// unpayable, but the card face already shows the tap state.
///
/// Row 3c: a COSTLESS ability is not read out either — the predicate's
/// `None => true` arm, which no shipped test exercised before.
///
/// Both negatives share one fixture with a mandatory positive: the same board
/// carries a `Pay 5 life` ability at 1 life which IS read out, so neither
/// negative can pass on an empty map.
#[test]
fn carve_out_suppresses_costless_and_tap_only_abilities_but_not_resource_costs() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 1);

    // `{T}: draw` on a TAPPED source — unpayable, carved out by the tap arm.
    let tap_only = scenario
        .add_creature(P0, "Gemhide Sliver", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(AbilityKind::Activated, draw_effect()).cost(AbilityCost::Tap),
        )
        .id();
    // A costless activated ability — the `None` arm.
    let costless = scenario
        .add_creature(P0, "Costless Engine", 1, 1)
        .with_ability_definition(AbilityDefinition::new(
            AbilityKind::Activated,
            draw_effect(),
        ))
        .id();
    // The mandatory paired positive: a resource cost the player cannot meet.
    let life = scenario
        .add_creature(P0, "Bloodletter", 1, 1)
        .with_ability_definition(draw_for_life(5))
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&tap_only)
        .expect("tap-only source exists")
        .tapped = true;

    assert!(
        !blocked(&runner, life).is_empty(),
        "paired positive (mandatory): Pay 5 life at 1 life IS read out — the map is not empty"
    );
    assert!(
        blocked(&runner, tap_only).is_empty(),
        "row 3: a tap-only cost on a tapped source is carved out"
    );
    assert!(
        blocked(&runner, costless).is_empty(),
        "row 3c: a costless ability is carved out by the `None` arm"
    );

    // Sibling for row 3c: give the SAME costless ability a PayLife cost and it
    // IS read out — proving the exclusion tracks `cost: None`, not the ability.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 1);
    let now_costed = scenario
        .add_creature(P0, "Costless Engine", 1, 1)
        .with_ability_definition(draw_for_life(5))
        .id();
    let runner = scenario.build();
    assert_eq!(
        blocked(&runner, now_costed)
            .into_iter()
            .map(|(i, _, k)| (i, k))
            .collect::<Vec<_>>(),
        vec![(0, AbilityBlockKind::CostNotPayableNow)],
        "sibling: the same ability with a PayLife cost IS read out"
    );

    // Row 3 sibling: UNTAP the source — still no entry, now because the ability
    // is affordable rather than carved out. Asserted so the carve-out is never
    // confused with the affordability gate.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let untapped = scenario
        .add_creature(P0, "Gemhide Sliver", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(AbilityKind::Activated, draw_effect()).cost(AbilityCost::Tap),
        )
        .id();
    let runner = scenario.build();
    assert!(
        blocked(&runner, untapped).is_empty(),
        "row 3 sibling: an untapped tap-only ability is affordable, so still no entry"
    );
    assert!(
        offered_indices(&runner, untapped).contains(&0),
        "reach-guard: and it is genuinely OFFERED, so the empty read-out is not an invisible ability"
    );
}

/// Row 3b: the carve-out does not swallow a MIXED cost. `Composite[Tap, PayLife 5]`
/// on a tapped source at 1 life IS read out, because
/// `all_components_cheap_gate_covered` returns false for `PayLife`.
#[test]
fn carve_out_does_not_swallow_a_mixed_tap_plus_life_cost() {
    let build = |life: i32| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_life(P0, life);
        let obj = scenario
            .add_creature(P0, "Mixed Cost Engine", 1, 1)
            .with_ability_definition(
                AbilityDefinition::new(AbilityKind::Activated, draw_effect()).cost(
                    AbilityCost::Composite {
                        costs: vec![
                            AbilityCost::Tap,
                            AbilityCost::PayLife {
                                amount: QuantityExpr::Fixed { value: 5 },
                            },
                        ],
                    },
                ),
            )
            .id();
        (scenario.build(), obj)
    };

    let (runner, obj) = build(1);
    assert_eq!(
        blocked(&runner, obj)
            .into_iter()
            .map(|(i, _, k)| (i, k))
            .collect::<Vec<_>>(),
        vec![(0, AbilityBlockKind::CostNotPayableNow)],
        "a mixed Tap+PayLife cost is NOT carved out and is read out at 1 life"
    );

    let (runner, obj) = build(20);
    assert!(
        blocked(&runner, obj).is_empty(),
        "sibling: the same cost at 20 life is affordable, so no entry"
    );
}

// ── Rows 4 / 4b — the membership predicate (B1) ─────────────────────────────

/// Row 4: a structurally-refused `EffectCost` is NEVER read out, on any board —
/// its `can_pay` refusal is a limit of the engine's payment authority, not a
/// resource shortfall, so a row would be a permanent lie.
///
/// Two mandatory controls, because a constant-false predicate would pass the
/// negative alone:
///   * a `PayLife` ability on the SAME object at 0 life IS read out;
///   * Devoted Druid's supported `EffectCost{PutCounter{SelfRef}}` shape IS read
///     out when its accompanying mana is unaffordable — so the predicate is not
///     a constant-false on `EffectCost`.
#[test]
fn structurally_refused_effect_cost_is_not_read_out() {
    // Crackleburr's untap-two-red-creatures cost is an `EffectCost{SetTapState}`,
    // a shape `supports_effect_cost_payment` refuses on every board.
    let refused_effect_cost = || {
        AbilityDefinition::new(AbilityKind::Activated, draw_effect()).cost(
            AbilityCost::EffectCost {
                effect: Box::new(Effect::SetTapState {
                    target: TargetFilter::Any,
                    scope: EffectScope::Single,
                    state: TapStateChange::Untap,
                }),
            },
        )
    };

    for (life, label) in [(0, "0 life"), (20, "20 life")] {
        for tapped in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            scenario.with_life(P0, life);
            let obj = scenario
                .add_creature(P0, "Crackleburr", 2, 2)
                // index 0: the structurally-refused EffectCost
                .with_ability_definition(refused_effect_cost())
                // index 1: the MANDATORY paired positive on the same object
                .with_ability_definition(draw_for_life(5))
                .id();
            let mut runner = scenario.build();
            runner
                .state_mut()
                .objects
                .get_mut(&obj)
                .expect("Crackleburr exists")
                .tapped = tapped;

            let read_out: Vec<usize> = blocked(&runner, obj)
                .into_iter()
                .map(|(i, _, _)| i)
                .collect();
            assert!(
                !read_out.contains(&0),
                "the structurally-refused EffectCost is never read out ({label}, tapped={tapped}); got {read_out:?}"
            );
            if life == 0 {
                assert!(
                    read_out.contains(&1),
                    "paired positive (mandatory): PayLife 5 at 0 life on the SAME object IS read out ({label}, tapped={tapped})"
                );
            }
        }
    }

    // Second control: a SUPPORTED `EffectCost` shape is read out, so the
    // predicate is not a constant-false on the `EffectCost` variant. Devoted
    // Druid's "{T}, Put a -1/-1 counter on this: Add {G}" cost shape, paired
    // with an unaffordable mana component so `can_pay` genuinely refuses.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let druid = scenario
        .add_creature(P0, "Devoted Druid", 0, 2)
        .with_ability_definition(
            AbilityDefinition::new(AbilityKind::Activated, draw_effect()).cost(
                AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::EffectCost {
                            effect: Box::new(Effect::PutCounter {
                                target: TargetFilter::SelfRef,
                                counter_type: CounterType::Minus1Minus1,
                                count: QuantityExpr::Fixed { value: 1 },
                            }),
                        },
                        AbilityCost::Mana {
                            cost: ManaCost::generic(7),
                        },
                    ],
                },
            ),
        )
        .id();
    let runner = scenario.build();
    assert_eq!(
        blocked(&runner, druid)
            .into_iter()
            .map(|(i, _, k)| (i, k))
            .collect::<Vec<_>>(),
        vec![(0, AbilityBlockKind::CostNotPayableNow)],
        "second control: a SUPPORTED EffectCost shape IS read out — the predicate is not constant-false"
    );
}

/// Row 4b: `PerCounter` and `Unimplemented` are excluded on the same axis. The
/// bare `PayLife` control from row 4 rides along on the same object.
#[test]
fn per_counter_and_unimplemented_costs_are_not_read_out() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 0);
    let obj = scenario
        .add_creature(P0, "Structural Refusal Engine", 1, 1)
        // index 0: PerCounter wrapping a resource-verdict base
        .with_ability_definition(
            AbilityDefinition::new(AbilityKind::Activated, draw_effect()).cost(
                AbilityCost::PerCounter {
                    counter: CounterType::Age,
                    target: TargetFilter::SelfRef,
                    base: Box::new(AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                    }),
                },
            ),
        )
        // index 1: Unimplemented
        .with_ability_definition(
            AbilityDefinition::new(AbilityKind::Activated, draw_effect()).cost(
                AbilityCost::Unimplemented {
                    description: "some cost the parser could not classify".to_string(),
                },
            ),
        )
        // index 2: the paired positive
        .with_ability_definition(draw_for_life(5))
        .id();
    let runner = scenario.build();

    let read_out: Vec<usize> = blocked(&runner, obj)
        .into_iter()
        .map(|(i, _, _)| i)
        .collect();
    assert!(
        read_out.contains(&2),
        "paired positive: the bare PayLife ability at 0 life IS read out; got {read_out:?}"
    );
    assert!(
        !read_out.contains(&0),
        "PerCounter is excluded on the structural axis; got {read_out:?}"
    );
    assert!(
        !read_out.contains(&1),
        "Unimplemented is excluded on the structural axis; got {read_out:?}"
    );
}

// ── Row 5 — mana abilities are never read out ───────────────────────────────

/// Row 5: a NON-tap-only mana ability — the exact class the carve-out does not
/// hide — gets no row, because a different authority
/// (`mana_abilities::can_activate_mana_ability_now`) decides mana abilities.
///
/// Paired positive (mandatory): a NON-mana ability with the same
/// `Composite[Tap, Sacrifice]` cost on the same board IS read out, so the
/// negative cannot be satisfied by the carve-out or by an empty map.
#[test]
fn mana_abilities_are_never_read_out() {
    let tap_sac_cost = || AbilityCost::Composite {
        costs: vec![
            AbilityCost::Tap,
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
        ],
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // A Treasure-shaped mana ability: `{T}, Sacrifice this: Add one mana`.
    let treasure = scenario
        .add_creature(P0, "Treasure Token", 0, 0)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(tap_sac_cost()),
        )
        .id();
    // The same cost shape on a NON-mana ability.
    let non_mana = scenario
        .add_creature(P0, "Sacrificial Engine", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(AbilityKind::Activated, draw_effect()).cost(tap_sac_cost()),
        )
        .id();
    let mut runner = scenario.build();
    // Tap both so neither cost is payable.
    for id in [treasure, non_mana] {
        runner
            .state_mut()
            .objects
            .get_mut(&id)
            .expect("source exists")
            .tapped = true;
    }

    assert!(
        !blocked(&runner, non_mana).is_empty(),
        "paired positive (mandatory): the NON-mana Composite[Tap, Sacrifice] ability IS read out"
    );
    assert!(
        blocked(&runner, treasure).is_empty(),
        "row 5: the mana ability with the identical cost shape gets NO row"
    );
}

// ── Row 6 — prohibition arms keep precedence ────────────────────────────────

/// Row 6: an ability that is BOTH prohibited and unaffordable reports the
/// CR 602.5 prohibition on the object field, and does not appear in the CR 118.3
/// read-out — the prohibition gate returns `Illegal` before the payability probe
/// is ever reached.
///
/// Sibling: remove the prohibition and the SAME ability reads
/// `CostNotPayableNow` with empty `sources`.
#[test]
fn prohibitions_take_precedence_over_the_payability_read_out() {
    let build = |prohibit: bool| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_life(P0, 1);
        let source = scenario.add_creature(P0, "Kang", 0, 0).id();
        let obj = scenario
            .add_creature(P0, "Costly Engine", 1, 1)
            .with_ability_definition(draw_for_life(5))
            .id();
        let mut runner = scenario.build();
        if prohibit {
            runner
                .state_mut()
                .restrictions
                .push(GameRestriction::ProhibitActivity {
                    source,
                    affected_players: RestrictionPlayerScope::AllPlayers,
                    expiry: RestrictionExpiry::EndOfTurn,
                    activity: ProhibitedActivity::ActivateAbilities {
                        exemption: ActivationExemption::None,
                        only_tag: None,
                    },
                });
        }
        (runner, obj)
    };

    let (runner, obj) = build(false);
    assert_eq!(
        blocked(&runner, obj),
        vec![(0, vec![], AbilityBlockKind::CostNotPayableNow)],
        "sibling: with no prohibition the ability reads CostNotPayableNow with empty sources"
    );

    let (runner, obj) = build(true);
    assert!(
        blocked(&runner, obj).is_empty(),
        "under a prohibition the payability read-out yields nothing — the CR 602.5 gate wins"
    );
}

// ── Row 7 — the target-legality tail ────────────────────────────────────────

/// Row 7: an ability that is unaffordable AND has no legal target is NOT read
/// out. Reporting "you can't pay this cost" for an ability that could not be
/// activated anyway names the wrong cause, and this row is what makes
/// `CostNotPayableNow` mean what its name says.
///
/// This row IS the target-legality tail's discriminating test: delete the tail
/// from `ActivationQuery::BlockReason` and index 1 is read out, failing the
/// negative below.
///
/// Fixture note: the source is deliberately NOT a Sliver. A "target Sliver"
/// ability on Sliver Overlord itself has a legal target — the Overlord — because
/// gaining control of a permanent you already control is a legal targeting
/// choice. The tail can only be exercised by a filter that genuinely matches
/// nothing on the board.
#[test]
fn unaffordable_ability_with_no_legal_target_is_not_read_out() {
    let sliver_filter = TargetFilter::Typed(TypedFilter {
        type_filters: vec![TypeFilter::Subtype("Sliver".to_string())],
        controller: None,
        properties: vec![],
    });

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 1);
    let obj = scenario
        .add_creature(P0, "Targeting Engine", 2, 2)
        // index 0: untargeted and unaffordable → read out.
        .with_ability_definition(draw_for_life(5))
        // index 1: unaffordable AND no legal target (no Sliver exists) → NOT read out.
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Destroy {
                    target: sliver_filter,
                    cant_regenerate: false,
                },
            )
            .cost(AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 5 },
            }),
        )
        .id();
    // A non-Sliver creature, so the board is not empty of creatures either.
    scenario.add_creature(P1, "Grizzly Bears", 2, 2);
    let runner = scenario.build();

    let read_out: Vec<usize> = blocked(&runner, obj)
        .into_iter()
        .map(|(i, _, _)| i)
        .collect();
    assert!(
        read_out.contains(&0),
        "paired positive: the untargeted unaffordable ability IS read out — the \
         instrument fires; got {read_out:?}"
    );
    assert!(
        !read_out.contains(&1),
        "row 7: the unaffordable ability with NO legal target is NOT read out; got {read_out:?}"
    );

    // Sibling: add a Sliver and index 1 gains a legal target, so the SAME
    // ability now IS read out — the exclusion tracks target legality, not the
    // ability.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 1);
    let obj = scenario
        .add_creature(P0, "Targeting Engine", 2, 2)
        .with_ability_definition(draw_for_life(5))
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Destroy {
                    target: TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Subtype("Sliver".to_string())],
                        controller: None,
                        properties: vec![],
                    }),
                    cant_regenerate: false,
                },
            )
            .cost(AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 5 },
            }),
        )
        .id();
    scenario
        .add_creature(P1, "Sliver Drone", 1, 1)
        .with_subtypes(vec!["Sliver"]);
    let runner = scenario.build();
    let read_out: Vec<usize> = blocked(&runner, obj)
        .into_iter()
        .map(|(i, _, _)| i)
        .collect();
    assert!(
        read_out.contains(&1),
        "sibling: with a legal Sliver target on the board the same ability IS read out; \
         got {read_out:?}"
    );
}

// ── Row 8 — non-battlefield zones ───────────────────────────────────────────

/// Row 8: the read-out covers the graveyard zone, not just the battlefield
/// (r3's Deferral 2, free on this channel).
#[test]
fn graveyard_activated_abilities_are_read_out() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 1);
    // A battlefield entry in the same map proves the traversal ran at all.
    let battlefield = scenario
        .add_creature(P0, "Battlefield Engine", 1, 1)
        .with_ability_definition(draw_for_life(5))
        .id();
    let unaffordable = scenario
        .add_creature_to_graveyard(P0, "Bloodsoaked Champion", 2, 1)
        .with_ability_definition(graveyard_draw_for_life(5))
        .id();
    let affordable = scenario
        .add_creature_to_graveyard(P0, "Dread Wanderer", 2, 1)
        .with_ability_definition(graveyard_draw_for_life(1))
        .id();
    let runner = scenario.build();

    assert!(
        !blocked(&runner, battlefield).is_empty(),
        "reach-guard: the battlefield loop ran and produced an entry"
    );
    assert_eq!(
        blocked(&runner, unaffordable)
            .into_iter()
            .map(|(i, _, k)| (i, k))
            .collect::<Vec<_>>(),
        vec![(0, AbilityBlockKind::CostNotPayableNow)],
        "row 8: an unaffordable graveyard-activated ability IS read out"
    );
    assert!(
        blocked(&runner, affordable).is_empty(),
        "paired positive: an affordable graveyard ability on the same board is absent"
    );
}

// ── Row 9 — two sources sharing an ability index ────────────────────────────

/// Row 9: the entry binds to the MAP KEY (`ObjectId`), not to `ability_index`
/// alone. Two objects each carry an ability at index 0; only the unaffordable
/// one appears, and swapping which is affordable inverts the assertion.
#[test]
fn two_sources_same_ability_index_are_disambiguated() {
    let build = |cheap_first: bool| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_life(P0, 3);
        let a = scenario
            .add_creature(P0, "Sliver A", 1, 1)
            .with_ability_definition(draw_for_life(if cheap_first { 1 } else { 5 }))
            .id();
        let b = scenario
            .add_creature(P0, "Sliver B", 1, 1)
            .with_ability_definition(draw_for_life(if cheap_first { 5 } else { 1 }))
            .id();
        (scenario.build(), a, b)
    };

    let (runner, a, b) = build(true);
    assert!(
        blocked(&runner, a).is_empty(),
        "the affordable source is absent from the map"
    );
    assert_eq!(
        blocked(&runner, b)
            .into_iter()
            .map(|(i, _, _)| i)
            .collect::<Vec<_>>(),
        vec![0],
        "the unaffordable source carries index 0"
    );

    // Sibling: swap which is affordable → the assertion inverts.
    let (runner, a, b) = build(false);
    assert_eq!(
        blocked(&runner, a)
            .into_iter()
            .map(|(i, _, _)| i)
            .collect::<Vec<_>>(),
        vec![0],
        "swapped: now source A carries the entry"
    );
    assert!(
        blocked(&runner, b).is_empty(),
        "swapped: source B is now affordable and absent"
    );
}

// ── Row 10 — viewer scoping ─────────────────────────────────────────────────

/// Row 10: the viewer-scoped sibling is empty for a viewer without action
/// authority, and non-empty for the acting seat on the SAME state.
#[test]
fn read_out_is_empty_for_a_non_acting_viewer() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 1);
    let obj = scenario
        .add_creature(P0, "Costly Engine", 1, 1)
        .with_ability_definition(draw_for_life(5))
        .id();
    let runner = scenario.build();

    let acting = activation_block_reasons_for_viewer(runner.state(), P0);
    let other = activation_block_reasons_for_viewer(runner.state(), P1);

    assert!(
        !entries_of(&acting, obj).is_empty(),
        "paired positive (mandatory): the acting seat receives a non-empty read-out"
    );
    assert!(
        other.is_empty(),
        "row 10: a viewer without action authority receives an empty map; got {other:?}"
    );
}

// ── Row 18 — the producer-side bound ────────────────────────────────────────

/// Row 18: the read-out is bounded AT THE PRODUCER on the TOTAL across buckets,
/// and `activation_block_reasons` returns a map — it has no `Result` and cannot
/// fail, so no new broadcast-suppressing `Err` path exists.
///
/// Hostile fixture (mandatory): ONE object carrying more blocked abilities than
/// the cap — the case a KEY-COUNT bound would wave through and a TOTAL bound
/// must catch.
///
/// Paired positive (mandatory): a board just UNDER the cap is returned whole
/// with its true count, so "truncated" is never "always returns the cap".
#[test]
fn read_out_is_bounded_on_the_total_across_buckets() {
    let build = |abilities: usize| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_life(P0, 1);
        let builder = scenario.add_creature(P0, "Hydra of Many Costs", 1, 1);
        let mut builder = builder;
        for _ in 0..abilities {
            builder.with_ability_definition(draw_for_life(5));
        }
        let obj = builder.id();
        (scenario.build(), obj)
    };

    // Paired positive: just under the cap, returned whole.
    let under = MAX_ACTIVATION_BLOCK_TOTAL - 1;
    let (runner, obj) = build(under);
    let map = activation_block_reasons(runner.state());
    let total: usize = map.values().map(Vec::len).sum();
    assert_eq!(
        total, under,
        "under the cap the map is returned whole with its true count"
    );
    assert_eq!(
        map.get(&obj).map(Vec::len),
        Some(under),
        "and every entry belongs to the single object bucket"
    );

    // Hostile fixture: ONE object over the cap.
    let (runner, obj) = build(MAX_ACTIVATION_BLOCK_TOTAL + 25);
    let map = activation_block_reasons(runner.state());
    let total: usize = map.values().map(Vec::len).sum();
    assert!(
        total <= MAX_ACTIVATION_BLOCK_TOTAL,
        "the TOTAL across buckets is capped even when ONE object exceeds it; got {total}"
    );
    assert_eq!(
        total, MAX_ACTIVATION_BLOCK_TOTAL,
        "truncation keeps the cap's worth rather than dropping the bucket"
    );
    assert!(
        map.get(&obj)
            .is_some_and(|entries| entries.iter().all(|e| e.reason.sources.is_empty())),
        "every returned CR 118.3 entry carries empty `sources`"
    );

    // Determinism: the same board truncates identically across calls.
    let again = activation_block_reasons(runner.state());
    assert_eq!(
        entries_of(&map, obj),
        entries_of(&again, obj),
        "truncation is deterministic (ObjectId order, then ability_index order)"
    );
}

/// CR 118.12a: a DISJUNCTIVE cost is a resource verdict iff ANY alternative is.
///
/// `can_pay` answers `OneOf` with `.any()` at Activation scope (`costs.rs`), so a
/// refusal means every alternative was refused. An alternative refused for lack of
/// resources could still have been paid on a richer board, so the disjunction's
/// refusal is a statement about resources — even though a sibling alternative is
/// refused structurally on every board.
///
/// Revert probe: writing the `OneOf` arm of
/// `AbilityCost::payability_verdict_is_resource_based` as `.all()` (which is
/// correct for `Composite` and was originally shared with it) makes the mixed
/// disjunction answer `false` and drops index 0 from the read-out, failing the
/// first assertion while the all-structural negative control below still passes.
#[test]
fn mixed_disjunctive_cost_is_read_out_but_an_all_structural_one_is_not() {
    // A shape `supports_effect_cost_payment` refuses on every board, so its
    // refusal is structural rather than a statement about resources.
    let structural = || AbilityCost::EffectCost {
        effect: Box::new(Effect::SetTapState {
            target: TargetFilter::Any,
            scope: EffectScope::Single,
            state: TapStateChange::Untap,
        }),
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 0);
    let obj = scenario
        .add_creature(P0, "Disjunctive Source", 2, 2)
        // index 0: MIXED — one structural alternative, one resource alternative
        // the player cannot currently afford (no mana sources on the board).
        .with_ability_definition(
            AbilityDefinition::new(AbilityKind::Activated, draw_effect()).cost(
                AbilityCost::OneOf {
                    costs: vec![
                        structural(),
                        AbilityCost::Mana {
                            cost: ManaCost::generic(7),
                        },
                    ],
                },
            ),
        )
        // index 1: NEGATIVE CONTROL — every alternative is structural, so the
        // whole refusal is structural and no row may be produced.
        .with_ability_definition(
            AbilityDefinition::new(AbilityKind::Activated, draw_effect()).cost(
                AbilityCost::OneOf {
                    costs: vec![structural(), structural()],
                },
            ),
        )
        // index 2: PAIRED POSITIVE on the same object, so an empty read-out
        // cannot pass this test vacuously.
        .with_ability_definition(draw_for_life(5))
        .id();
    let runner = scenario.build();

    let read_out: Vec<usize> = blocked(&runner, obj)
        .into_iter()
        .map(|(i, _, _)| i)
        .collect();

    assert!(
        read_out.contains(&0),
        "a disjunction with ONE resource-refused alternative is a resource verdict \
         (CR 118.12a); got {read_out:?}"
    );
    assert!(
        !read_out.contains(&1),
        "negative control: an all-structural disjunction is refused on every board \
         and must NOT be read out; got {read_out:?}"
    );
    assert!(
        read_out.contains(&2),
        "paired positive (mandatory): PayLife 5 at 0 life on the SAME object is read out"
    );
}
