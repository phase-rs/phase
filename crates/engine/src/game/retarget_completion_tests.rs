//! CR 115.7d + CR 115.7e + CR 115.3 + CR 707.10c: the completion search
//! against THE validator. Every prompt below is built fresh at its prefix by
//! the production walk (`copy_choice::walk_step`) and compared with a brute
//! force over every complete pick vector, each judged by
//! `engine::validate_retarget_submission`.

use std::collections::BTreeSet;

use super::*;
use crate::game::effects::copy_choice::{walk_of, walk_step, CopyWalk, CopyWalkStep};
use crate::game::scenario::{GameRunner, GameScenario, P0, P1};
use crate::parser::oracle::parse_oracle_text;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, Comparator, Effect, ObjectScope, QuantityRef, SharedQuality,
    SharedQualityRelation,
};
use crate::types::actions::GameAction;
use crate::types::counter::CounterType;
use crate::types::game_state::WaitingFor;
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaCost;
use crate::types::phase::Phase;

const TWINCAST: &str =
    "Copy target instant or sorcery spell. You may choose new targets for the copy.";
const FIERY_ANNIHILATION: &str = "Fiery Annihilation deals 5 damage to target creature. Exile up to one target Equipment attached to that creature. If that creature would die this turn, exile it instead.";

fn free_spell(s: &mut GameScenario, name: &str, text: &str) -> ObjectId {
    s.add_spell_to_hand_from_oracle(P0, name, true, text)
        .with_mana_cost(ManaCost::zero())
        .id()
}

fn equipment(s: &mut GameScenario, name: &str) -> ObjectId {
    s.add_artifact_from_oracle(P1, name, "Equipped creature gets +1/+0.")
        .with_subtypes(vec!["Equipment"])
        .id()
}

fn drive_to_copy_walk(r: &mut GameRunner) {
    for _ in 0..24 {
        match r.state().waiting_for {
            WaitingFor::CopyRetarget { .. } => return,
            WaitingFor::OptionalEffectChoice { .. } => {
                r.act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept");
            }
            _ => {
                r.act(GameAction::PassPriority).expect("pass");
            }
        }
    }
    panic!("no copy walk opened");
}

/// Cast `spell` at `targets`, then Twincast it, and open the copy's walk.
fn copy_walk_board(
    mut r: GameRunner,
    spell: ObjectId,
    targets: &[ObjectId],
    twincast: ObjectId,
) -> GameRunner {
    r.cast(spell).target_objects(targets).commit();
    r.cast(twincast).target_object(spell).commit();
    drive_to_copy_walk(&mut r);
    r
}

fn search_of<'s>(state: &'s GameState, walk: &CopyWalk) -> RetargetSearch<'s> {
    let index = state
        .stack
        .iter()
        .position(|entry| entry.id == walk.copy_id)
        .expect("the copy is on the stack");
    RetargetSearch::for_stack_entry(state, index).expect("the copy has addressed positions")
}

/// Brute force: some complete vector extending `prefix` is accepted by the
/// validator. Every remaining position ranges over keep and its whole pool.
fn brute_feasible(search: &RetargetSearch<'_>, prefix: &[RetargetPick]) -> bool {
    fn extend(search: &RetargetSearch<'_>, picks: &mut Vec<RetargetPick>) -> bool {
        if picks.len() == search.len() {
            return search.validate(picks).is_ok();
        }
        let position = picks.len();
        let domain: Vec<RetargetPick> = std::iter::once(None)
            .chain(search.slot_pools()[position].iter().cloned().map(Some))
            .collect();
        for pick in domain {
            picks.push(pick);
            if extend(search, picks) {
                return true;
            }
            picks.pop();
        }
        false
    }
    extend(search, &mut prefix.to_vec())
}

/// Walk every prefix the production prompt offers, rebuilding the prompt and
/// a fresh search at each, and compare its answers with the brute force.
/// Returns the number of prefixes checked.
fn assert_walk_matches_brute_force(state: &GameState) -> usize {
    let (walk, _) = walk_of(&state.waiting_for).expect("a normalized copy walk");
    fn explore(state: &GameState, walk: &CopyWalk, prefix: Vec<RetargetPick>) -> usize {
        let search = search_of(state, walk);
        if prefix.len() == search.len() {
            return 0;
        }
        let Some(CopyWalkStep::Prompt(prompt)) =
            walk_step(state, walk, prefix.clone()).expect("the walk steps")
        else {
            panic!("an undecided prefix prompts");
        };
        let WaitingFor::CopyRetarget {
            target_slots,
            current_slot,
            can_keep_rest,
            ..
        } = *prompt
        else {
            panic!("a copy walk prompt");
        };
        let position = prefix.len();
        assert_eq!(current_slot, position, "the cursor is the prefix length");
        let keep_is_distinct =
            retarget_keep_is_distinct(state, search.pre, search.slots())[position];
        let with = |pick: RetargetPick| {
            let mut extended = prefix.clone();
            extended.push(pick);
            extended
        };
        let brute_keep = brute_feasible(&search, &with(None));
        let brute_alternatives: BTreeSet<String> = search.slot_pools()[position]
            .iter()
            .filter(|target| keep_is_distinct || search.current_targets()[position] != **target)
            .filter(|target| brute_feasible(&search, &with(Some((*target).clone()))))
            .map(|target| format!("{target:?}"))
            .collect();
        let offered: BTreeSet<String> = target_slots[position]
            .legal_alternatives
            .iter()
            .map(|target| format!("{target:?}"))
            .collect();
        assert_eq!(
            target_slots[position].can_keep, brute_keep,
            "prefix {prefix:?}: keep permission"
        );
        assert_eq!(
            offered, brute_alternatives,
            "prefix {prefix:?}: alternatives"
        );
        let mut rest = prefix.clone();
        rest.resize(search.len(), None);
        assert_eq!(
            can_keep_rest,
            search.validate(&rest).is_ok(),
            "prefix {prefix:?}: keep the rest"
        );
        let mut checked = 1;
        let picks: Vec<RetargetPick> = target_slots[position]
            .can_keep
            .then_some(None)
            .into_iter()
            .chain(
                target_slots[position]
                    .legal_alternatives
                    .iter()
                    .cloned()
                    .map(Some),
            )
            .collect();
        for pick in picks {
            checked += explore(state, walk, with(pick));
        }
        checked
    }
    explore(state, &walk, Vec::new())
}

const FROST_BREATH: &str = "Tap up to two target creatures. Those creatures don't untap during their controller's next untap step.";

/// A two-creature uniform run (Frost Breath), Twincast: X1, X2 announced, G
/// and H also on the battlefield.
fn tap_two_board() -> (GameRunner, [ObjectId; 4]) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let ids = ["X1", "X2", "G", "H"].map(|name| s.add_creature(P1, name, 2, 2).id());
    let spell = free_spell(&mut s, "Frost Breath", FROST_BREATH);
    let twincast = free_spell(&mut s, "Twincast", TWINCAST);
    let r = copy_walk_board(s.build(), spell, &ids[..2], twincast);
    (r, ids)
}

/// CR 115.3 + CR 115.7d: in a uniform run with all-different announced
/// identities (the matching solver), the walk offers exactly what some
/// validator-accepted completion extends, at every prefix. The legacy Hex
/// counterexample (one object chosen at two positions of one run) is refused:
/// after choosing G at position 0, G is not offered at position 1.
#[test]
fn uniform_run_walk_matches_the_validator_at_every_prefix() {
    let (r, [x1, _x2, g, _h]) = tap_two_board();
    let state = r.state();
    let (walk, _) = walk_of(&state.waiting_for).unwrap();
    let search = search_of(state, &walk);
    assert_eq!(search.len(), 2, "reach: two addressed positions");
    assert_eq!(
        search.component_census(),
        (1, 1),
        "one run component, solved by matching"
    );
    let checked = assert_walk_matches_brute_force(state);
    assert!(checked > 4, "reach: the walk explored {checked} prefixes");

    let Some(CopyWalkStep::Prompt(prompt)) =
        walk_step(state, &walk, vec![Some(TargetRef::Object(g))]).unwrap()
    else {
        panic!("position 1 prompts");
    };
    let WaitingFor::CopyRetarget { target_slots, .. } = *prompt else {
        panic!("a copy walk prompt");
    };
    assert!(
        !target_slots[1]
            .legal_alternatives
            .contains(&TargetRef::Object(g)),
        "CR 115.3: G already named at position 0 is not offered at position 1"
    );
    assert!(
        !search.admits(&[Some(TargetRef::Object(g))], &Some(TargetRef::Object(g))),
        "the legacy [G, G] prefix is refused by the gate"
    );
    assert!(
        search.admits(&[Some(TargetRef::Object(g))], &Some(TargetRef::Object(x1))),
        "control: X1 (freed by position 0) is admitted at position 1"
    );
    let calls = search.validator_calls();
    assert!(calls > 0, "measurement: {calls} validator calls");
}

/// CR 400.7 + CR 115.3 (post-blink split): X1 blinks after the announcement.
/// Position 0 may keep the departed X1 (an unchanged illegal target, CR
/// 115.7d) while another position elects the returned X1, a different object.
/// The search agrees with the validator at every prefix.
#[test]
fn post_blink_split_walk_matches_the_validator_at_every_prefix() {
    let (mut r, [x1, ..]) = tap_two_board();
    let mut events = Vec::new();
    crate::game::zones::move_to_zone(
        r.state_mut(),
        x1,
        crate::types::zones::Zone::Exile,
        &mut events,
    );
    crate::game::zones::move_to_zone(
        r.state_mut(),
        x1,
        crate::types::zones::Zone::Battlefield,
        &mut events,
    );
    let (walk, _) = walk_of(&r.state().waiting_for).unwrap();
    // The prompt was built before the blink; rebuild it on the current board.
    let Some(CopyWalkStep::Prompt(prompt)) = walk_step(r.state(), &walk, Vec::new()).unwrap()
    else {
        panic!("the walk prompts");
    };
    r.state_mut().waiting_for = *prompt;
    let state = r.state();
    let search = search_of(state, &walk);
    assert!(
        search.admits(&[None], &Some(TargetRef::Object(x1))),
        "keep the departed X1, elect the returned X1 at position 1"
    );
    assert_walk_matches_brute_force(state);
}

/// CR 115.7e + CR 608.2b: Fiery Annihilation's Equipment position reads the
/// creature position (`AttachedTo { DeclaredTarget }`), so the two positions
/// are one component, solved by validator-checked search; the walk matches
/// the validator at every prefix.
#[test]
fn declared_slot_component_walk_matches_the_validator_at_every_prefix() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let a = s.add_creature(P1, "Creature A", 2, 7).id();
    let b = s.add_creature(P1, "Creature B", 2, 2).id();
    let _c = s.add_creature(P1, "Creature C", 2, 7).id();
    let eq1 = equipment(&mut s, "Equipment 1");
    let eq2 = equipment(&mut s, "Equipment 2");
    let _eq3 = equipment(&mut s, "Equipment 3");
    let fiery = free_spell(&mut s, "Fiery Annihilation", FIERY_ANNIHILATION);
    let twincast = free_spell(&mut s, "Twincast", TWINCAST);
    let mut r = s.build();
    crate::game::effects::attach::attach_to(r.state_mut(), eq1, a);
    crate::game::effects::attach::attach_to(r.state_mut(), eq2, b);
    let r = copy_walk_board(r, fiery, &[a, eq1], twincast);
    let state = r.state();
    let (walk, _) = walk_of(&state.waiting_for).unwrap();
    let search = search_of(state, &walk);
    assert_eq!(search.len(), 2, "reach: the creature and the Equipment");
    assert_eq!(search.component_census(), (1, 0), "one searched component");
    assert_walk_matches_brute_force(state);
}

/// CR 122.1 + CR 608.2c: a quantity read of the chain root's counters
/// (`QuantityRef::CountersOn { ChainRootTarget }`) is not modeled exactly, so
/// it routes the whole walk to the combined, validator-searched component.
/// The carrier (`chain_root_targets`) is nonempty, the read is nonzero (A has
/// two +1/+1 counters, so the MV-2 artifact is a legal second target and the
/// MV-3 one is not), and a third counter crosses the threshold: the MV-3
/// artifact becomes offered. At every prefix the walk matches the validator.
#[test]
fn counters_on_chain_root_falls_back_to_the_combined_search() {
    for counters in [2_u32, 3] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let a = s.add_creature(P1, "Creature A", 2, 2).id();
        let _b = s.add_creature(P1, "Creature B", 2, 2).id();
        let mv2 = s
            .add_artifact_from_oracle(P1, "Artifact MV2", "")
            .with_mana_cost(ManaCost::generic(2))
            .id();
        let mv3 = s
            .add_artifact_from_oracle(P1, "Artifact MV3", "")
            .with_mana_cost(ManaCost::generic(3))
            .id();
        // The announcement reads 0 (the chain-root carrier is latched only
        // after targets are chosen, CR 601.2c), so the declared artifact is
        // the MV-0 one; the copy's walk reads the latched carrier.
        let mv0 = s
            .add_artifact_from_oracle(P1, "Artifact MV0", "")
            .with_mana_cost(ManaCost::zero())
            .id();
        let types = vec!["Instant".to_string()];
        let mut root = parse_oracle_text("Tap target creature.", "Counter Probe", &[], &types, &[])
            .abilities
            .remove(0);
        root.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact).properties(
                    vec![FilterProp::Cmc {
                        comparator: Comparator::LE,
                        value: QuantityExpr::Ref {
                            qty: QuantityRef::CountersOn {
                                scope: ObjectScope::ChainRootTarget,
                                counter_type: Some(CounterType::Plus1Plus1),
                            },
                        },
                    }],
                )),
                cant_regenerate: false,
            },
        )));
        let spell = s
            .add_spell_to_hand(P0, "Counter Probe", true)
            .with_mana_cost(ManaCost::zero())
            .with_ability_definition(root)
            .id();
        let twincast = free_spell(&mut s, "Twincast", TWINCAST);
        let mut r = s.build();
        r.state_mut()
            .objects
            .get_mut(&a)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, counters);
        let mut r = copy_walk_board(r, spell, &[a, mv0], twincast);
        // Staged carrier: the cast latches `chain_root_targets` on the root
        // only (CR 601.2c) and resolution carries it down the chain, so on the
        // stack the destroy node's own carrier is empty and its read is 0. The
        // fixture stages the carrier through the chain so the counter read is
        // nonzero. Every prompt below is rebuilt fresh on this state.
        let copy_id = walk_of(&r.state().waiting_for).unwrap().0.copy_id;
        r.state_mut()
            .stack
            .iter_mut()
            .find(|entry| entry.id == copy_id)
            .and_then(|entry| entry.ability_mut())
            .expect("the copy on the stack")
            .set_chain_root_targets_recursive(vec![TargetRef::Object(a)]);
        let state = r.state();
        let (walk, _) = walk_of(&state.waiting_for).unwrap();
        let search = search_of(state, &walk);
        assert_eq!(
            search
                .pre
                .sub_ability
                .as_ref()
                .expect("the destroy node")
                .context
                .chain_root_targets,
            vec![TargetRef::Object(a)],
            "reach: the destroy node's chain-root carrier names A"
        );
        assert_eq!(search.len(), 2, "reach: the creature and the artifact");
        assert_eq!(
            retarget_dependencies(&TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Artifact).properties(vec![FilterProp::Cmc {
                    comparator: Comparator::LE,
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::CountersOn {
                            scope: ObjectScope::ChainRootTarget,
                            counter_type: Some(CounterType::Plus1Plus1),
                        },
                    },
                }]),
            )),
            RetargetDeps::AllPositions,
            "the counter read is classified AllPositions directly"
        );
        assert_eq!(
            search.component_census(),
            (1, 0),
            "one combined component, solved by validator-checked search"
        );
        assert_eq!(
            search.slot_pools()[1].contains(&TargetRef::Object(mv3)),
            counters >= 3,
            "{counters} counters: the MV-3 artifact crosses the threshold"
        );
        assert!(
            search.slot_pools()[1].contains(&TargetRef::Object(mv2)),
            "nonzero read: the MV-2 artifact is legal with {counters} counters"
        );
        assert_walk_matches_brute_force(state);
    }
}

/// The classifier is exact for run distinctness and `AttachedTo {
/// DeclaredTarget }` only; every other target-dependent read (Daring Thief's
/// `SharesQuality`, target-relative controllers, quantity references) returns
/// `AllPositions`, which dominates every combination.
#[test]
fn classifier_names_declared_slot_reads_and_fails_closed_otherwise() {
    let typed = |props: Vec<FilterProp>| {
        TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact).properties(props))
    };
    let attached = FilterProp::AttachedTo {
        to: AttachmentReferent::DeclaredTarget { slot: 0 },
    };
    let shares = FilterProp::SharesQuality {
        quality: SharedQuality::CardType,
        reference: Some(Box::new(TargetFilter::ParentTarget)),
        relation: SharedQualityRelation::Shares,
    };
    assert_eq!(
        retarget_dependencies(&TargetFilter::Typed(TypedFilter::creature())),
        RetargetDeps::Independent
    );
    assert_eq!(
        retarget_dependencies(&typed(vec![attached.clone()])),
        RetargetDeps::Slots(vec![0])
    );
    assert_eq!(
        retarget_dependencies(&typed(vec![shares.clone()])),
        RetargetDeps::AllPositions,
        "Daring Thief's shared-type read is not claimed as exact"
    );
    assert_eq!(
        retarget_dependencies(&typed(vec![FilterProp::AnyOf {
            props: vec![attached.clone(), shares],
        }])),
        RetargetDeps::AllPositions,
        "AllPositions dominates a combination"
    );
    assert_eq!(
        retarget_dependencies(&TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::ParentTargetController)
        )),
        RetargetDeps::AllPositions,
        "a target-relative controller fails closed"
    );
    assert_eq!(
        retarget_dependencies(&typed(vec![FilterProp::Not {
            prop: Box::new(attached),
        }])),
        RetargetDeps::Slots(vec![0]),
        "a negated declared-slot read still reads that slot"
    );
}

/// CR 115.3 + CR 115.7e (measurement): Hex ("Destroy six target creatures.")
/// copied by Twincast, with G chosen at position 0. Choosing G again at
/// position 1 names one object twice in one run: the fixed pair already
/// violates W2, so it is refused without any validator call, however large
/// the pool. Before the pre-check it took 840 / 1,680 / 3,024 validations for
/// pools of 8 / 9 / 10. The control, H at position 1, is admitted cheaply.
#[test]
fn an_impossible_fixed_pair_is_refused_without_search() {
    for extra in 0..3 {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let ids: Vec<ObjectId> = (0..8 + extra)
            .map(|n| s.add_creature(P1, &format!("Hex Creature {n}"), 2, 9).id())
            .collect();
        let hex = s
            .add_spell_to_hand_from_oracle(P0, "Hex", false, "Destroy six target creatures.")
            .with_mana_cost(ManaCost::zero())
            .id();
        let twincast = free_spell(&mut s, "Twincast", TWINCAST);
        let r = copy_walk_board(s.build(), hex, &ids[..6], twincast);
        let state = r.state();
        let (walk, _) = walk_of(&state.waiting_for).unwrap();
        let (g, h) = (TargetRef::Object(ids[6]), TargetRef::Object(ids[7]));

        let search = search_of(state, &walk);
        assert_eq!(search.len(), 6, "reach: six positions");
        assert_eq!(search.component_census(), (1, 1), "reach: one matched run");
        assert!(
            !search.admits(&[Some(g.clone())], &Some(g.clone())),
            "pool {}: G twice in one run is refused",
            8 + extra
        );
        assert_eq!(
            search.validator_calls(),
            0,
            "pool {}: refused by the fixed-pair check, not by search",
            8 + extra
        );

        let control = search_of(state, &walk);
        assert!(
            control.admits(&[Some(g.clone())], &Some(h)),
            "control: [G, H]"
        );
        assert!(
            control.validator_calls() <= 2,
            "pool {}: the control is cheap, took {}",
            8 + extra,
            control.validator_calls()
        );
    }
}

/// The UNSTAGED companion of `counters_on_chain_root_falls_back_to_the_combined_search`:
/// the counter read sits on the ROOT node, whose chain-root carrier the cast
/// itself latches (CR 601.2c), so nothing is staged. An engine-composed root
/// "exchange control of target creature and target artifact with mana value
/// less than or equal to the number of +1/+1 counters on that creature"
/// announces A (read 0 before the latch: the MV-0 artifact). On the copy the
/// carrier names A and the read is nonzero, the walk is one combined
/// component, a third counter crosses the MV-3 threshold, and every prefix
/// matches the validator.
#[test]
fn counters_on_chain_root_read_at_the_root_needs_no_staging() {
    let artifact_le_counters = || {
        TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact).properties(vec![
            FilterProp::Cmc {
                comparator: Comparator::LE,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::CountersOn {
                        scope: ObjectScope::ChainRootTarget,
                        counter_type: Some(CounterType::Plus1Plus1),
                    },
                },
            },
        ]))
    };
    for counters in [2_u32, 3] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let a = s.add_creature(P1, "Creature A", 2, 2).id();
        let _b = s.add_creature(P1, "Creature B", 2, 2).id();
        let mv0 = s
            .add_artifact_from_oracle(P1, "Artifact MV0", "")
            .with_mana_cost(ManaCost::zero())
            .id();
        let mv2 = s
            .add_artifact_from_oracle(P1, "Artifact MV2", "")
            .with_mana_cost(ManaCost::generic(2))
            .id();
        let mv3 = s
            .add_artifact_from_oracle(P1, "Artifact MV3", "")
            .with_mana_cost(ManaCost::generic(3))
            .id();
        let spell = s
            .add_spell_to_hand(P0, "Root Counter Probe", true)
            .with_mana_cost(ManaCost::zero())
            .with_ability_definition(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ExchangeControl {
                    target_a: TargetFilter::Typed(TypedFilter::creature()),
                    target_b: artifact_le_counters(),
                },
            ))
            .id();
        let twincast = free_spell(&mut s, "Twincast", TWINCAST);
        let mut r = s.build();
        r.state_mut()
            .objects
            .get_mut(&a)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, counters);
        let r = copy_walk_board(r, spell, &[a, mv0], twincast);
        let state = r.state();
        let (walk, _) = walk_of(&state.waiting_for).unwrap();
        let search = search_of(state, &walk);
        assert_eq!(
            search.pre.context.chain_root_targets.first(),
            Some(&TargetRef::Object(a)),
            "reach: the cast latched the root carrier; nothing is staged"
        );
        assert_eq!(search.len(), 2, "reach: the creature and the artifact");
        assert_eq!(
            retarget_dependencies(&artifact_le_counters()),
            RetargetDeps::AllPositions
        );
        assert_eq!(search.component_census(), (1, 0), "one combined component");
        assert!(
            search.slot_pools()[1].contains(&TargetRef::Object(mv2)),
            "nonzero read: MV 2 is legal with {counters} counters"
        );
        assert_eq!(
            search.slot_pools()[1].contains(&TargetRef::Object(mv3)),
            counters >= 3,
            "{counters} counters: the MV-3 threshold"
        );
        assert_walk_matches_brute_force(state);
    }
}
