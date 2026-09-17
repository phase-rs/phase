//! Issue #6902 — Sneak Attack activated twice in one turn must sacrifice BOTH
//! creatures at the beginning of the next end step.
//!
//! > {R}: You may put a creature card from your hand onto the battlefield. That
//! > creature gains haste. Sacrifice the creature at the beginning of the next
//! > end step.
//!
//! Reported (and reproduced on v0.81.2) as two delayed sacrifice triggers going
//! on the stack where the second carried an empty target list, stranding one
//! creature on the battlefield.

use std::sync::Arc;

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, DelayedTriggerCondition, Effect,
    EffectScope, FilterProp, MultiTargetSpec, QuantityExpr, ReplacementDefinition, TapStateChange,
    TargetChoiceTiming, TargetFilter, TargetRef, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};

// Verbatim Oracle text (Scryfall, 2026-09-15).
const SNEAK_ATTACK: &str = "{R}: You may put a creature card from your hand onto the battlefield. That creature gains haste. Sacrifice the creature at the beginning of the next end step.";

/// Drive one activation to completion on the real action path, choosing `pick`
/// from hand. Every prompt seen is recorded; an unexpected one fails loudly.
fn activate_putting(
    runner: &mut GameRunner,
    sneak_attack: ObjectId,
    pick: ObjectId,
) -> Vec<String> {
    resolve_putting(
        runner,
        GameAction::ActivateAbility {
            source_id: sneak_attack,
            ability_index: 0,
        },
        pick,
    )
}

/// Submit `first` (an activation or a cast) and drive it to completion on the
/// real action path, choosing `pick` from hand. An unexpected prompt fails loudly.
fn resolve_putting(runner: &mut GameRunner, first: GameAction, pick: ObjectId) -> Vec<String> {
    let mut waiting = runner
        .act(first)
        .expect("the put-from-hand instruction must be accepted")
        .waiting_for;
    let mut seen = Vec::new();
    for _ in 0..20 {
        seen.push(format!("{waiting:?}").chars().take(50).collect::<String>());
        let action = match &waiting {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return seen,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::ReplacementChoice { .. } => GameAction::ChooseReplacement { index: 0 },
            WaitingFor::CopyTargetChoice { valid_targets, .. } => GameAction::ChooseTarget {
                target: Some(TargetRef::Object(
                    *valid_targets
                        .first()
                        .expect("a creature to copy must be offered"),
                )),
            },
            WaitingFor::EffectZoneChoice { cards, .. }
            | WaitingFor::ChooseFromZoneChoice { cards, .. } => {
                assert!(
                    cards.contains(&pick),
                    "the chosen creature must be offered; offered {cards:?}"
                );
                GameAction::SelectCards { cards: vec![pick] }
            }
            other => {
                panic!("unexpected prompt during activation: {other:?}; prompts so far: {seen:?}")
            }
        };
        waiting = runner
            .act(action)
            .unwrap_or_else(|e| panic!("action rejected: {e:?}; prompts so far: {seen:?}"))
            .waiting_for;
    }
    panic!("activation did not settle; prompts: {seen:?}");
}

/// The constructed `forward_result` producer these forwarding tests drive.
///
/// Root: `PutCounter` on a DECLARED creature target. That declared target is
/// exactly what tier 2 of `parent_chain_referents` inherits when tier 1 is left
/// empty, and its counter doubles as a reach-guard proving the target really
/// resolved. Sub-ability: a hand-to-battlefield `ChangeZone` carrying
/// `forward_result`, whose own sub-ability installs the delayed "sacrifice that
/// permanent" rider.
///
/// `hand_type` selects what the zone choice offers, so one producer serves both
/// a creature put (the declined-selection case) and an Aura put (the CR 303.4f
/// host-pause sibling) without duplicating the chain.
///
/// CR 601.2c + CR 608.2c: "put a card from your hand onto the battlefield" is
/// NOT targeting. Hand is a hidden zone with no legal stack-time targets, so
/// under the default `Stack` timing the slot resolves empty and the producer
/// completes having moved nothing — silently, with no prompt at all. Resolution
/// timing plus an unlimited-from-zero spec is the shape the engine uses for a
/// DECLINABLE zone selection; it is pinned by the `up_to` unit test in
/// `effects/change_zone.rs`, which asserts the resulting `EffectZoneChoice`
/// carries count = eligible, min_count = 0, up_to = true.
fn forwarding_producer_ability(hand_type: TypeFilter) -> AbilityDefinition {
    let delayed_sacrifice = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::CreateDelayedTrigger {
            condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
            effect: Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Sacrifice {
                    target: TargetFilter::ParentTarget,
                    count: QuantityExpr::Fixed { value: 1 },
                    // CR 107.1c: `min_count` is the floor for RANGED sacrifice
                    // choices ("one or more"); a plain "sacrifice that permanent"
                    // takes the 0 default.
                    min_count: 0,
                },
            )),
            uses_tracked_set: false,
        },
    );
    let mut producer = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChangeZone {
            origin: Some(Zone::Hand),
            destination: Zone::Battlefield,
            target: TargetFilter::Typed(
                TypedFilter::new(hand_type)
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::InZone { zone: Zone::Hand }]),
            ),
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: true,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
    )
    .sub_ability(delayed_sacrifice)
    .target_choice_timing(TargetChoiceTiming::Resolution)
    .multi_target(MultiTargetSpec::unlimited(0));
    producer.forward_result = true;

    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Creature).controller(ControllerRef::You),
            ),
        },
    )
    .cost(AbilityCost::Mana {
        cost: ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        },
    })
    .sub_ability(producer)
}

/// Install `ability` as `source`'s only activated ability. Layers reset
/// `abilities` from `base_abilities` on every pass, so both must be set.
fn install_ability(runner: &mut GameRunner, source: ObjectId, ability: AbilityDefinition) {
    let obj = runner
        .state_mut()
        .objects
        .get_mut(&source)
        .expect("the scenario source object exists");
    obj.abilities = Arc::new(vec![ability.clone()]);
    obj.base_abilities = Arc::new(vec![ability]);
}

/// CR 608.2c + CR 400.7: a TERMINAL EMPTY `up_to` selection is a COMPLETED
/// producer that moved nothing. The forwarded-result contract spells that
/// `Some([])`, NOT `None` — and `targeting::parent_chain_referents` documents the
/// distinction at its tier 1: *"a forward-result producer is the most recent
/// antecedent. `Some([])` there is a real zero-result and must not fall through."*
///
/// Left at `None`, tier 1 is skipped and tier 2
/// (`flatten_targets_in_chain(resolving_root_ability(..))`) hands the chain's
/// DECLARED targets to a `ParentTarget` anaphor that names an object the selection
/// never moved — the "incorrectly fall back to inherited targets" case.
///
/// The ability is constructed because the corpus has no card that reaches this:
/// the producers that can be DECLINED (`ChangeZone` + `up_to` + `forward_result` —
/// Journey to the Oracle, The Great Aurora, Worlds Within Worlds, Yawgmoth's Vile
/// Offering) carry no `ParentTarget`/`CreateDelayedTrigger` consumer, while the
/// cards that DO carry that consumer (Sneak Attack, Through the Breach) parse with
/// no `up_to` and so cannot be declined. Everything beneath the ability is
/// production: a real `ActivateAbility`, real target selection, the real
/// `EffectZoneChoice` answered with an empty `SelectCards`, and the real delayed
/// trigger firing at the end step.
///
/// The root's `PutCounter` is load-bearing twice over: it gives the chain a
/// DECLARED target (so tier 2 is non-empty and the unfixed path has something
/// wrong to inherit), and its counter is a reach-guard proving that target was
/// really declared and resolved.
///
/// Revert-proof: drop the `Some([])` publish from the terminal-empty branch in
/// `engine_resolution_choices` and the decoy — never put onto the battlefield by
/// this spell — is SACRIFICED at the end step.
#[test]
fn a_declined_up_to_zone_choice_publishes_a_completed_empty_forwarded_result() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let source = scenario.add_creature(P0, "Forwarding Source", 2, 2).id();
    let decoy = scenario.add_creature(P0, "Decoy Bear", 2, 2).id();
    // Two creature cards in hand so the zone choice offers a real, non-empty pool
    // — declining must be a genuine choice, not an empty-pool no-op.
    let hand_a = scenario.add_creature_to_hand(P0, "Hand One", 1, 1).id();
    let hand_b = scenario.add_creature_to_hand(P0, "Hand Two", 1, 1).id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, source, false, Vec::new())],
    );

    let mut runner = scenario.build();

    install_ability(
        &mut runner,
        source,
        forwarding_producer_ability(TypeFilter::Creature),
    );

    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("the constructed activation must be accepted");

    // Declare the decoy as the chain's target. This is what the unfixed path
    // inherits, so the test would prove nothing if the prompt never appeared.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "reach-guard: the root must declare a target; got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(decoy)],
        })
        .expect("declaring the decoy as the chain's target must succeed");

    // CR 608.2: the activated ability is on the stack — it only reaches its
    // sub-ability producer once both players pass. Stop at the first non-priority
    // prompt (the zone choice) or at an empty stack (which the reach-guard below
    // reports as the producer never having prompted at all).
    for _ in 0..8 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority toward resolution must be accepted");
            }
            _ => break,
        }
    }

    // The producer pauses on its up-to zone choice with a real pool; DECLINE it.
    let offered = match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice { cards, .. } => cards.clone(),
        other => panic!(
            "reach-guard: expected the up-to zone choice; got {other:?} (stack {}, \
             decoy +1/+1 {:?}, hand zones {:?} / {:?}). An empty eligible set \
             completes an `up_to` producer with no prompt at all.",
            runner.state().stack.len(),
            runner.state().objects[&decoy]
                .counters
                .get(&CounterType::Plus1Plus1),
            runner.state().objects[&hand_a].zone,
            runner.state().objects[&hand_b].zone,
        ),
    };
    assert!(
        offered.contains(&hand_a) && offered.contains(&hand_b),
        "reach-guard: both hand creatures must be offered; got {offered:?}"
    );
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("declining an up-to zone choice must be accepted");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&decoy]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        Some(1),
        "reach-guard: the declared target really resolved, so tier 2 has something \
         to inherit"
    );
    for card in [hand_a, hand_b] {
        assert_eq!(
            runner.state().objects[&card].zone,
            Zone::Hand,
            "reach-guard: declining moved nothing out of hand"
        );
    }

    pass_priority_into_end_step_of(&mut runner, P0);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&decoy].zone,
        Zone::Battlefield,
        "CR 608.2c: a declined up-to selection is a COMPLETED producer that moved \
         nothing, so the delayed \"that creature\" rider has no referent. Left at \
         `None` it inherits the chain's declared target and sacrifices the decoy, \
         which this spell never put onto the battlefield"
    );
}

/// CR 608.2: the activated ability is on the stack — it reaches its sub-ability
/// producer only once both players pass. Stops at the first non-priority prompt,
/// or at an empty stack so a producer that never prompted is reported by the
/// caller's reach-guard rather than silently passing turns.
fn pass_priority_to_prompt(runner: &mut GameRunner) {
    for _ in 0..8 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority toward resolution must be accepted");
            }
            _ => break,
        }
    }
}

/// CR 303.4f + CR 608.2c: the SIBLING of the declined-selection case. An Aura put
/// onto the battlefield by a NON-SPELL effect with two or more legal hosts pauses
/// mid-entry on the CR 303.4f host choice — the same out-of-band delivery shape as
/// the as-enters copy choice. That member DID move, so the `forward_result`
/// producer must still forward it: the completed-empty publish must never swallow
/// a real entry.
///
/// Revert-proof: publish `&[]` for every paused member and this test fails — the
/// delayed rider falls back to the chain's declared target and sacrifices the decoy
/// this spell never put onto the battlefield (MEASURED).
///
/// It does NOT discriminate the choice of classifier: gating the forward on the
/// inferring `terminal_completion_after_resume()` leaves this test green and breaks
/// only the copy-choice sibling (also MEASURED). The two re-pause routes fail under
/// different reverts, which is why both are kept.
#[test]
fn an_aura_host_pause_still_forwards_the_member_it_moved() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let source = scenario.add_creature(P0, "Forwarding Source", 2, 2).id();
    let decoy = scenario.add_creature(P0, "Decoy Bear", 2, 2).id();
    // CR 303.4f: two or more legal hosts is what makes the entry PAUSE. With
    // exactly one the resolver auto-attaches and never installs the prompt —
    // `old_growth_troll_return_as_aura` pins that branch.
    let host = scenario.add_creature(P0, "Host Bear", 1, 1).id();
    let aura = scenario
        .add_spell_to_hand(P0, "Clinging Vines", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(TypedFilter::new(
            TypeFilter::Creature,
        ))))
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, source, false, Vec::new())],
    );

    let mut runner = scenario.build();
    install_ability(
        &mut runner,
        source,
        forwarding_producer_ability(TypeFilter::Enchantment),
    );

    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("the constructed activation must be accepted");
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(decoy)],
        })
        .expect("declaring the decoy as the chain's target must succeed");
    pass_priority_to_prompt(&mut runner);

    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice { cards, .. } => assert!(
            cards.contains(&aura),
            "reach-guard: the Aura must be offered by the zone choice; got {cards:?}"
        ),
        other => panic!("reach-guard: expected the up-to zone choice; got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards { cards: vec![aura] })
        .expect("choosing the Aura must be accepted");

    // The entry pauses MID-DELIVERY on the CR 303.4f host choice. This is the
    // reach-guard that makes the test discriminating: without this pause the
    // member would be delivered through the ordinary synchronous path and would
    // never reach the paused-member forward seam under test.
    let hosts = match &runner.state().waiting_for {
        WaitingFor::ReturnAsAuraTarget { legal_targets, .. } => legal_targets.clone(),
        other => panic!("reach-guard: expected the CR 303.4f host choice; got {other:?}"),
    };
    assert!(
        hosts.len() >= 2,
        "reach-guard: the host pause requires two or more legal hosts; got {hosts:?}"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(host)),
        })
        .expect("answering the CR 303.4f host choice must be accepted");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&aura].zone,
        Zone::Battlefield,
        "reach-guard: the Aura really entered the battlefield"
    );

    pass_priority_into_end_step_of(&mut runner, P0);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&aura].zone,
        Zone::Graveyard,
        "CR 608.2c: the host pause delivered the Aura out-of-band, but it MOVED, so \
         the producer must forward it and the delayed rider names the Aura"
    );
    assert_eq!(
        runner.state().objects[&decoy].zone,
        Zone::Battlefield,
        "the chain's DECLARED target must not be inherited when a real member moved"
    );
}

/// CR 614.6 + CR 616.1 + CR 608.2c: a REDIRECTED re-paused delivery must not fall
/// back to inherited targets. A "would enter the battlefield, exile it instead"
/// replacement rewrites the destination, so nothing was put onto the battlefield by
/// this effect and the delayed "that creature" rider has no referent — it must not
/// name the chain's DECLARED target instead.
///
/// Scope, stated precisely because it is narrower than it looks: this test
/// discriminates against the `None` fallback only. Both correct policies — publish
/// `Some([])` (what this seam does: the redirect's delivery events show no arrival
/// at the requested destination) and forwarding the redirected object — block the
/// tier-2 fallback identically, and `ParentTarget` resolves at trigger-resolution
/// time, so neither board state nor the installed `DelayedTrigger` can tell them
/// apart. MEASURED: this test stays green when the seam publishes `&[]` for every
/// paused member.
///
/// Reaching the paused-member seam takes BOTH replacements. A lone redirect takes
/// the `candidates.len() == 1` path and applies synchronously, never pausing; the
/// enters-tapped sibling is what makes `candidates.len() > 1`. The redirect is then
/// what makes the CR 616.1 ordering choice MATERIAL rather than degenerate —
/// `candidate_materiality` returns `Unconditional` for a stored `ChangeZone` whose
/// destination differs from the proposed one (`proposed_to != Some(destination)`),
/// and a degenerate ordering would auto-resolve with no prompt at all.
///
/// Revert-proof: publish `&[]` for a redirected member and the delayed rider falls
/// back to the chain's declared target, sacrificing the decoy.
#[test]
fn a_redirected_delivery_still_forwards_the_member_it_moved() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let source = scenario.add_creature(P0, "Forwarding Source", 2, 2).id();
    let decoy = scenario.add_creature(P0, "Decoy Bear", 2, 2).id();

    // CR 614.6: "If this creature would enter the battlefield, exile it instead."
    let mut redirect = ReplacementDefinition::new(ReplacementEvent::ChangeZone);
    redirect.valid_card = Some(TargetFilter::SelfRef);
    redirect.execute = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChangeZone {
            origin: None,
            destination: Zone::Exile,
            target: TargetFilter::SelfRef,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
    )));
    // CR 614.1c: the second applicable replacement. Without it the pipeline takes
    // the single-candidate path, applies the redirect synchronously, and never
    // pauses — the seam under test would not be reached at all.
    let mut enters_tapped = ReplacementDefinition::new(ReplacementEvent::ChangeZone);
    enters_tapped.valid_card = Some(TargetFilter::SelfRef);
    enters_tapped.execute = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::SetTapState {
            target: TargetFilter::SelfRef,
            scope: EffectScope::Single,
            state: TapStateChange::Tap,
        },
    )));

    let redirected = scenario
        .add_creature_to_hand(P0, "Redirected Bear", 2, 2)
        .with_replacement_definition(redirect)
        .with_replacement_definition(enters_tapped)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, source, false, Vec::new())],
    );

    let mut runner = scenario.build();
    install_ability(
        &mut runner,
        source,
        forwarding_producer_ability(TypeFilter::Creature),
    );

    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("the constructed activation must be accepted");
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(decoy)],
        })
        .expect("declaring the decoy as the chain's target must succeed");
    pass_priority_to_prompt(&mut runner);

    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice { cards, .. } => assert!(
            cards.contains(&redirected),
            "reach-guard: the redirected creature must be offered; got {cards:?}"
        ),
        other => panic!("reach-guard: expected the up-to zone choice; got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards {
            cards: vec![redirected],
        })
        .expect("choosing the creature must be accepted");

    // CR 616.1: the ordering prompt. This reach-guard is what proves the entry
    // PAUSED mid-delivery; without it the test would pass through the synchronous
    // path and assert nothing about the seam.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "reach-guard: expected the CR 616.1 ordering choice; got {:?}",
        runner.state().waiting_for
    );
    for _ in 0..4 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            runner
                .act(GameAction::ChooseReplacement { index: 0 })
                .expect("ordering the applicable replacements must be accepted");
        } else {
            break;
        }
    }
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&redirected].zone,
        Zone::Exile,
        "reach-guard: the redirect really rewrote the destination"
    );

    pass_priority_into_end_step_of(&mut runner, P0);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&decoy].zone,
        Zone::Battlefield,
        "CR 614.6 + CR 608.2c: a redirected member still MOVED, so the producer \
         forwards it and the chain's declared target is never inherited"
    );
}

/// Pass priority on the real action path until `player`'s end step has begun.
fn pass_priority_into_end_step_of(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..60 {
        let state = runner.state();
        if state.active_player == player && state.phase == Phase::End {
            return;
        }
        match &state.waiting_for {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            // The creatures have haste; this test does not attack.
            WaitingFor::DeclareAttackers { .. } => {
                runner.declare_attackers(&[]).expect("declare no attackers");
            }
            other => panic!("unexpected prompt while advancing: {other:?}"),
        }
    }
    panic!("did not reach {player:?}'s end step");
}

#[test]
fn sneak_attack_twice_sacrifices_both_creatures_at_the_next_end_step() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Lib A", "Lib B"]);

    let sneak_attack = scenario
        .add_creature(P0, "Sneak Attack", 0, 0)
        .as_enchantment()
        .from_oracle_text(SNEAK_ATTACK)
        .id();
    let first = scenario
        .add_creature_to_hand(P0, "First Bear", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();
    let second = scenario
        .add_creature_to_hand(P0, "Second Bear", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Red, sneak_attack, false, Vec::new()),
            ManaUnit::new(ManaType::Red, sneak_attack, false, Vec::new()),
        ],
    );
    let mut runner = scenario.build();
    activate_putting(&mut runner, sneak_attack, first);
    activate_putting(&mut runner, sneak_attack, second);

    // Reach-guards: both creatures entered, and each activation installed its
    // own delayed sacrifice trigger.
    for bear in [first, second] {
        assert_eq!(
            runner.state().objects[&bear].zone,
            Zone::Battlefield,
            "reach-guard: {bear:?} entered"
        );
    }
    assert_eq!(
        runner.state().delayed_triggers.len(),
        2,
        "reach-guard: one delayed trigger per activation"
    );

    pass_priority_into_end_step_of(&mut runner, P0);
    runner.advance_until_stack_empty();

    for bear in [first, second] {
        assert_eq!(
            runner.state().objects[&bear].zone,
            Zone::Graveyard,
            "{bear:?} must be sacrificed at the beginning of the next end step (CR 603.7)"
        );
    }
}

// Verbatim Oracle text (Scryfall, 2026-09-15).
const THROUGH_THE_BREACH: &str = "You may put a creature card from your hand onto the battlefield. That creature gains haste. Sacrifice that creature at the beginning of the next end step.\nSplice onto Arcane {2}{R}{R}";

/// CR 603.7 + CR 608.2c: the same forwarding gap reached from a SPELL with the
/// "that creature" phrasing. Two creature cards in hand force the choice prompt
/// that used to drop the referent; the one not chosen stays in hand.
#[test]
fn through_the_breach_sacrifices_the_chosen_creature_after_a_choice_prompt() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Lib A", "Lib B"]);

    let breach = scenario
        .add_spell_to_hand_from_oracle(P0, "Through the Breach", true, THROUGH_THE_BREACH)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 4,
        })
        .id();
    let chosen = scenario
        .add_creature_to_hand(P0, "Chosen Bear", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();
    let left_behind = scenario
        .add_creature_to_hand(P0, "Other Bear", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        (0..5)
            .map(|_| ManaUnit::new(ManaType::Red, breach, false, Vec::new()))
            .collect(),
    );
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&breach].card_id;

    resolve_putting(
        &mut runner,
        GameAction::CastSpell {
            object_id: breach,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        },
        chosen,
    );
    assert_eq!(
        runner.state().objects[&chosen].zone,
        Zone::Battlefield,
        "reach-guard: the chosen creature entered"
    );
    assert_eq!(
        runner.state().objects[&left_behind].zone,
        Zone::Hand,
        "reach-guard: the other creature stayed in hand"
    );

    pass_priority_into_end_step_of(&mut runner, P0);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&chosen].zone,
        Zone::Graveyard,
        "the creature chosen through the prompt must be sacrificed at the next end step"
    );
    assert_eq!(
        runner.state().objects[&left_behind].zone,
        Zone::Hand,
        "the unchosen creature is untouched"
    );
}
// Verbatim Oracle text (Scryfall, 2026-09-15).
const CLONE: &str =
    "You may have this creature enter as a copy of any creature on the battlefield.";

/// CR 608.2c + CR 614.12 + CR 707.9: the chosen creature's own "enter as a copy"
/// replacement re-pauses the put mid-delivery, so the selection completes
/// through the change-zone iteration drain instead of the choice prompt's own
/// completion. The delayed sacrifice must still name that creature.
#[test]
fn sneak_attack_sacrifices_a_creature_whose_entry_paused_on_a_copy_choice() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Lib A", "Lib B"]);

    let sneak_attack = scenario
        .add_creature(P0, "Sneak Attack", 0, 0)
        .as_enchantment()
        .from_oracle_text(SNEAK_ATTACK)
        .id();
    let copy_source = scenario.add_creature(P0, "Copy Source", 3, 3).id();
    let clone = scenario
        .add_creature_to_hand(P0, "Clone", 0, 0)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 3,
        })
        .from_oracle_text(CLONE)
        .id();
    let left_behind = scenario
        .add_creature_to_hand(P0, "Other Bear", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Red,
            sneak_attack,
            false,
            Vec::new(),
        )],
    );
    let mut runner = scenario.build();
    let prompts = activate_putting(&mut runner, sneak_attack, clone);

    // Reach-guards: the creature was chosen through the selection prompt, and
    // its entry then paused on the copy choice, so the drain path is the one
    // under test.
    assert!(
        prompts.iter().any(|p| p.starts_with("EffectZoneChoice")),
        "reach-guard: the creature was chosen through the selection prompt; {prompts:?}"
    );
    assert!(
        prompts.iter().any(|p| p.starts_with("CopyTargetChoice")),
        "reach-guard: the entry paused on the COPY choice specifically. A bare \
         `ReplacementChoice` is a different pause and would satisfy a disjunctive \
         guard without ever exercising the as-enters copy path; {prompts:?}"
    );
    assert_eq!(
        runner.state().objects[&clone].zone,
        Zone::Battlefield,
        "reach-guard: the chosen creature entered"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach-guard: the activation installed its delayed sacrifice trigger"
    );

    pass_priority_into_end_step_of(&mut runner, P0);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&clone].zone,
        Zone::Graveyard,
        "the creature whose entry paused on a copy choice must be sacrificed (CR 603.7)"
    );
    assert_eq!(
        runner.state().objects[&copy_source].zone,
        Zone::Battlefield,
        "the copied creature is untouched"
    );
    assert_eq!(
        runner.state().objects[&left_behind].zone,
        Zone::Hand,
        "the unchosen creature stays in hand"
    );
}
