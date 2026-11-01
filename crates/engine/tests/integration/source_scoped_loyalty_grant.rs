//! Source-scoped extra loyalty-activation grant: "You may activate ~'s loyalty
//! ability N more times this turn." (CR 606.3 + CR 101.1)
//!
//! No printed card carries this grant alone, so the rows below use synthetic
//! toy planeswalkers whose text is built from the grant clause, and drive the
//! real activation pipeline (`GameRunner::activate` / `GameAction`). The ruling
//! on the printed class (the grant applies only to the permanent's normal loyalty
//! ability) is not CR text.
//!
//! The Comet, Stellar Pup runtime rows (the verbatim face-6 roll) are covered end
//! to end in `comet_stellar_pup.rs`.

use engine::game::planeswalker::can_activate_loyalty_ability;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::{layers, turns, zones};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, AbilityProvenance, ContinuousModification,
    Duration, Effect, EffectKind, ObjectScope, PtValue, QuantityExpr, QuantityRef, TargetFilter,
    TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// The grant on a loyalty ability, plus a second ability to spend the extra
/// activations on. Gain life, not draw, so no row can lose to an empty library.
const WALKER_A: &str = "[0]: You may activate Probe Walker's loyalty ability two more times \
this turn.\n[+1]: You gain 1 life.";

/// A bystander that carries no grant.
const WALKER_B: &str = "[+1]: You gain 1 life.";

/// A `{T}` die ability whose full-range row is the grant, so no RNG pinning is
/// needed: the grant resolves inside the die-result branch frame whatever is rolled.
const DIE_WALKER: &str = "{T}: Roll a six-sided die.\n\
1\u{2014}6 | You may activate Probe Walker's loyalty ability two more times this turn.\n\
[+1]: You gain 1 life.";

/// A `{T}` ability whose second clause is the grant: the grant is a sub-ability of
/// an activated ability's stack entry.
const CHAIN_WALKER: &str = "{T}: You gain 1 life, and you may activate Probe Walker's loyalty \
ability two more times this turn.\n[+1]: You gain 1 life.";

/// The player-wide axis of the same grammar.
const PLAYER_WIDE_RELIC: &str =
    "{T}: You may activate each planeswalker's loyalty ability an additional time this turn.";

fn chain_has_unimplemented(def: &AbilityDefinition) -> bool {
    let mut next = Some(def);
    while let Some(d) = next {
        if matches!(&*d.effect, Effect::Unimplemented { .. }) {
            return true;
        }
        next = d.sub_ability.as_deref();
    }
    false
}

fn is_grant(ability: &AbilityDefinition) -> bool {
    matches!(
        &*ability.effect,
        Effect::GrantExtraLoyaltyActivations { .. }
    )
}

fn is_loyalty(ability: &AbilityDefinition) -> bool {
    matches!(ability.cost, Some(AbilityCost::Loyalty { .. }))
}

/// The index of the first parsed ability of `id` satisfying `pred`; read from the
/// parsed abilities, never hard-coded.
fn ability_index(
    runner: &GameRunner,
    id: ObjectId,
    pred: impl Fn(&AbilityDefinition) -> bool,
) -> usize {
    runner.state().objects[&id]
        .abilities
        .iter()
        .position(pred)
        .expect("the toy walker carries the expected ability")
}

/// The loyalty ability that carries the grant.
fn grant_index(runner: &GameRunner, id: ObjectId) -> usize {
    ability_index(runner, id, |a| is_loyalty(a) && is_grant(a))
}

/// The loyalty ability that spends the extra activations: `[+1]: You gain 1 life.`
fn spender_index(runner: &GameRunner, id: ObjectId) -> usize {
    ability_index(runner, id, |a| {
        is_loyalty(a) && matches!(&*a.effect, Effect::GainLife { .. })
    })
}

fn extra(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id].extra_loyalty_activations_this_turn
}

fn count(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id].loyalty_activations_this_turn
}

fn gate_admits(runner: &GameRunner, id: ObjectId, index: usize) -> bool {
    let controller = runner.state().objects[&id].controller;
    can_activate_loyalty_ability(runner.state(), id, controller, index)
}

/// A refused activation is an `Err` from the real action path, not just a false gate.
fn activation_is_refused(runner: &mut GameRunner, id: ObjectId, index: usize) -> bool {
    runner
        .act(GameAction::ActivateAbility {
            source_id: id,
            ability_index: index,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .is_err()
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .life
}

/// Pass priority until the stack empties, collecting every emitted event.
fn drain_stack(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..20 {
        if runner.state().stack.is_empty() {
            break;
        }
        let result = runner
            .act(GameAction::PassPriority)
            .expect("priority passes while the stack resolves");
        events.extend(result.events);
    }
    events
}

fn grant_events(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::GrantExtraLoyaltyActivations,
                    ..
                }
            )
        })
        .count()
}

/// Re-seed the loyalty a freshly re-entered planeswalker would otherwise lack
/// (its counters were on the previous object). Without it CR 704.5i removes the
/// zero-loyalty new object, which would make a "no grant" reading vacuous: the
/// permanent would be gone for a reason that has nothing to do with the grant.
fn reseed_loyalty(runner: &mut GameRunner, id: ObjectId, loyalty: u32) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.loyalty = Some(loyalty);
    obj.counters.insert(CounterType::Loyalty, loyalty);
}

/// Blink `id`: exile it and return it, so the same storage id is a new object.
fn blink(runner: &mut GameRunner, id: ObjectId) {
    let mut events = Vec::new();
    zones::move_to_zone(runner.state_mut(), id, Zone::Exile, &mut events);
    zones::move_to_zone(runner.state_mut(), id, Zone::Battlefield, &mut events);
    reseed_loyalty(runner, id, 5);
}

fn scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

/// Row-body census: the token-plus-haste row body is an existing
/// building block (Comet's `[+2]` row).
#[test]
fn row_body_census_token_with_haste_rider() {
    let def = parse_effect_chain(
        "Create two 1/1 green Squirrel creature tokens. They gain haste until end of turn.",
        AbilityKind::Activated,
    );
    assert!(!chain_has_unimplemented(&def), "{def:#?}");
    let Effect::Token {
        name,
        power,
        toughness,
        colors,
        count,
        ..
    } = &*def.effect
    else {
        panic!("expected Token, got {def:#?}");
    };
    assert_eq!(name, "Squirrel");
    assert_eq!((power, toughness), (&PtValue::Fixed(1), &PtValue::Fixed(1)));
    assert_eq!(colors.len(), 1);
    assert_eq!(count, &QuantityExpr::Fixed { value: 2 });
    let sub = def.sub_ability.as_deref().expect("haste rider sub-ability");
    let Effect::GenericEffect {
        static_abilities,
        duration,
        target,
        ..
    } = &*sub.effect
    else {
        panic!("expected GenericEffect rider, got {sub:#?}");
    };
    assert_eq!(target, &Some(TargetFilter::LastCreated));
    assert_eq!(duration, &Some(Duration::UntilEndOfTurn));
    assert!(static_abilities.iter().any(|s| s.modifications.iter().any(
        |m| matches!(m, ContinuousModification::AddKeyword { keyword } if *keyword == Keyword::Haste)
    )));
}

/// Row-body census: the MV <= 2 graveyard return is an existing building block
/// (Comet's `[−1]` row).
#[test]
fn row_body_census_graveyard_return_mana_value_two_or_less() {
    let def = parse_effect_chain(
        "Return a card with mana value 2 or less from your graveyard to your hand.",
        AbilityKind::Activated,
    );
    assert!(!chain_has_unimplemented(&def), "{def:#?}");
    let Effect::ChangeZone {
        origin,
        destination,
        target,
        ..
    } = &*def.effect
    else {
        panic!("expected ChangeZone, got {def:#?}");
    };
    assert_eq!(origin, &Some(Zone::Graveyard));
    assert_eq!(destination, &Zone::Hand);
    assert!(
        matches!(target, TargetFilter::Typed(tf) if tf.type_filters.is_empty()
            || tf.type_filters == vec![TypeFilter::Card]),
        "{target:?}"
    );
}

/// Row-body census: "loyalty counters on him" is an existing quantity (Comet's
/// `[−2]` row).
#[test]
fn row_body_census_loyalty_counters_on_the_source() {
    let def = parse_effect_chain(
        "~ deals damage equal to the number of loyalty counters on him to a creature or player.",
        AbilityKind::Activated,
    );
    let Effect::DealDamage { amount, .. } = &*def.effect else {
        panic!("expected DealDamage, got {def:#?}");
    };
    assert_eq!(
        amount,
        &QuantityExpr::Ref {
            qty: QuantityRef::CountersOn {
                scope: ObjectScope::Source,
                counter_type: Some(CounterType::Loyalty),
            }
        }
    );
}

/// The grant is mandatory and scoped to the source permanent, through the
/// real activation pipeline. The SHAPE reach-guard proves the toy parsed to the
/// grant (not `Unimplemented`, not optional); `resolve()` answering any optional
/// prompt by declining means an optional grant would leave the field at 0.
#[test]
fn printed_style_source_grant_is_mandatory_and_scoped_to_the_source_permanent() {
    let mut scenario = scenario();
    let a = scenario
        .add_planeswalker_from_oracle(P0, "Probe Walker", "Probe", 5, WALKER_A)
        .id();
    let b = scenario
        .add_planeswalker_from_oracle(P0, "Second Walker", "Probe", 5, WALKER_B)
        .id();
    let mut runner = scenario.build();

    // SHAPE reach-guard.
    let grant = grant_index(&runner, a);
    let spend = spender_index(&runner, a);
    let def = &runner.state().objects[&a].abilities[grant];
    assert!(
        matches!(
            &*def.effect,
            Effect::GrantExtraLoyaltyActivations {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::SelfRef,
            }
        ),
        "{def:#?}"
    );
    assert!(!def.optional && !chain_has_unimplemented(def), "{def:#?}");

    let outcome = runner.activate(a, grant).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the mandatory grant resolves with no prompt"
    );
    assert_eq!(extra(&runner, a), 2);
    assert_eq!(count(&runner, a), 1);
    assert!(
        runner
            .state()
            .extra_loyalty_activations_this_turn
            .is_empty(),
        "the source-scoped grant does not write the player-wide map"
    );

    // Two more activations of the source's own ability, then the cap.
    for expected in [2, 3] {
        assert!(
            gate_admits(&runner, a, spend),
            "count {}",
            count(&runner, a)
        );
        runner.activate(a, spend).resolve();
        assert_eq!(count(&runner, a), expected);
    }
    assert!(!gate_admits(&runner, a, spend));
    assert!(activation_is_refused(&mut runner, a, spend));

    // Hostile: a second own planeswalker gets no extra activation. Its first
    // activation succeeded (so the walker is activatable at all) and A's grant
    // exists, so the refusal of the second is the cap and not a harness limit.
    runner.activate(b, spender_index(&runner, b)).resolve();
    assert_eq!(count(&runner, b), 1);
    assert_eq!(extra(&runner, a), 2);
    assert_eq!(extra(&runner, b), 0);
    assert!(!gate_admits(&runner, b, spender_index(&runner, b)));
    let b_spend = spender_index(&runner, b);
    assert!(activation_is_refused(&mut runner, b, b_spend));
}

/// The permanent-scoped grant is additive with the player-wide grant.
/// The player-wide axis comes from the same grammar ("each planeswalker's ... an
/// additional time"), resolved from a real activated ability. A's cap is
/// 1 + 1 + 2 = 4 and the bystander's is 1 + 1 = 2.
#[test]
fn source_grant_is_additive_with_a_player_wide_grant() {
    let mut scenario = scenario();
    let a = scenario
        .add_planeswalker_from_oracle(P0, "Probe Walker", "Probe", 5, WALKER_A)
        .id();
    let b = scenario
        .add_planeswalker_from_oracle(P0, "Second Walker", "Probe", 5, WALKER_B)
        .id();
    let relic = scenario
        .add_artifact_from_oracle(P0, "Probe Relic", PLAYER_WIDE_RELIC)
        .id();
    let mut runner = scenario.build();

    let relic_def = &runner.state().objects[&relic].abilities[0];
    assert!(
        matches!(
            &*relic_def.effect,
            Effect::GrantExtraLoyaltyActivations {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            }
        ) && !relic_def.optional,
        "SHAPE reach-guard: {relic_def:#?}"
    );

    runner.activate(a, grant_index(&runner, a)).resolve();
    runner.activate(relic, 0).resolve();
    assert_eq!(extra(&runner, a), 2);
    assert_eq!(
        runner
            .state()
            .extra_loyalty_activations_this_turn
            .get(&P0)
            .copied(),
        Some(1),
        "reach-guard: both grants exist"
    );

    // A is at count 1 (the grant ability); cap 4 admits two more...
    let a_spend = spender_index(&runner, a);
    for expected in [2, 3, 4] {
        assert!(
            gate_admits(&runner, a, a_spend),
            "count {}",
            count(&runner, a)
        );
        runner.activate(a, a_spend).resolve();
        assert_eq!(count(&runner, a), expected);
    }
    assert!(!gate_admits(&runner, a, a_spend), "A reaches its cap of 4");

    // ...and the bystander gets only the player-wide extra: cap 2.
    let b_spend = spender_index(&runner, b);
    for expected in [1, 2] {
        assert!(gate_admits(&runner, b, b_spend));
        runner.activate(b, b_spend).resolve();
        assert_eq!(count(&runner, b), expected);
    }
    assert!(!gate_admits(&runner, b, b_spend), "B is refused at 2");
}

/// An opponent's planeswalker is unaffected. The turn is flipped to the
/// opponent DIRECTLY (as `chain_veil_grant_does_not_leak_to_opponent` does), never
/// through `start_next_turn`, whose sweep zeroes the field and would make a leak
/// check pass vacuously. The no-sweep reach-guard shows the grant is still live.
#[test]
fn an_opponents_planeswalker_is_unaffected_by_the_source_grant() {
    let mut scenario = scenario();
    let a = scenario
        .add_planeswalker_from_oracle(P0, "Probe Walker", "Probe", 5, WALKER_A)
        .id();
    let opposing = scenario
        .add_planeswalker_from_oracle(P1, "Opposing Walker", "Probe", 5, WALKER_B)
        .id();
    let mut runner = scenario.build();

    runner.activate(a, grant_index(&runner, a)).resolve();

    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    assert_eq!(
        extra(&runner, a),
        2,
        "no-sweep reach-guard: the grant is live"
    );
    assert!(runner
        .state()
        .extra_loyalty_activations_this_turn
        .is_empty());

    let spend = spender_index(&runner, opposing);
    assert!(
        gate_admits(&runner, opposing, spend),
        "paired control: the same walker at count 0 is activatable"
    );
    runner
        .state_mut()
        .objects
        .get_mut(&opposing)
        .unwrap()
        .loyalty_activations_this_turn = 1;
    assert!(
        !gate_admits(&runner, opposing, spend),
        "the opponent's walker is held to the base cap despite P0's grant"
    );
    assert!(activation_is_refused(&mut runner, opposing, spend));
}

/// The grant expires with the turn (CR 514.2). The pre-rollover guard
/// shows the field was live and the gate admitted a count of 2; after the
/// rollover one activation succeeds and the second is refused. The turn is handed
/// back to P0 directly so the sorcery-speed gate can be exercised.
#[test]
fn source_grant_expires_at_turn_rollover() {
    let mut scenario = scenario();
    let a = scenario
        .add_planeswalker_from_oracle(P0, "Probe Walker", "Probe", 5, WALKER_A)
        .id();
    let mut runner = scenario.build();
    let spend = spender_index(&runner, a);

    runner.activate(a, grant_index(&runner, a)).resolve();
    runner
        .state_mut()
        .objects
        .get_mut(&a)
        .unwrap()
        .loyalty_activations_this_turn = 2;
    assert_eq!(extra(&runner, a), 2, "pre-rollover reach-guard");
    assert!(gate_admits(&runner, a, spend), "cap 3 admits count 2");

    let mut events = Vec::new();
    turns::start_next_turn(runner.state_mut(), &mut events);
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.phase = Phase::PreCombatMain;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    assert_eq!(extra(&runner, a), 0);
    assert_eq!(count(&runner, a), 0);

    assert!(gate_admits(&runner, a, spend));
    runner.activate(a, spend).resolve();
    assert_eq!(count(&runner, a), 1);
    assert!(!gate_admits(&runner, a, spend), "the new turn's cap is 1");
    assert!(activation_is_refused(&mut runner, a, spend));
}

/// The grant is lost when the permanent re-enters (CR 400.7). Scenario-
/// built objects are used because the zone-exit revert resets a hand-built
/// object's abilities. Loyalty is re-seeded and the gate is called directly so no
/// state-based action runs. Paired control: before the blink the same count is
/// admitted (cap 3).
#[test]
fn source_grant_is_lost_when_the_permanent_re_enters() {
    let mut scenario = scenario();
    let a = scenario
        .add_planeswalker_from_oracle(P0, "Probe Walker", "Probe", 5, WALKER_A)
        .id();
    let mut runner = scenario.build();
    let spend = spender_index(&runner, a);

    runner.activate(a, grant_index(&runner, a)).resolve();
    assert_eq!(extra(&runner, a), 2);
    assert_eq!(
        (count(&runner, a), gate_admits(&runner, a, spend)),
        (1, true)
    );

    let incarnation = runner.state().objects[&a].incarnation;
    blink(&mut runner, a);
    let obj = &runner.state().objects[&a];
    assert!(obj.incarnation > incarnation, "reach-guard: a new object");
    assert_eq!(obj.zone, Zone::Battlefield);

    assert_eq!(extra(&runner, a), 0);
    runner
        .state_mut()
        .objects
        .get_mut(&a)
        .unwrap()
        .loyalty_activations_this_turn = 1;
    assert!(
        !gate_admits(&runner, a, spend),
        "the re-entered object has the base cap of 1"
    );
}

/// The grant covers the printed loyalty ability but not one the
/// permanent gained (the card's ruling; CR 613.1f puts ability-adding effects in
/// layer 6). The gained ability is installed by a real Layer-6 grant.
#[test]
fn source_grant_covers_the_printed_ability_but_not_a_gained_one() {
    let mut scenario = scenario();
    let a = scenario
        .add_planeswalker_from_oracle(P0, "Probe Walker", "Probe", 5, WALKER_A)
        .id();
    let granter = scenario
        .add_artifact_from_oracle(P0, "Probe Granter", "")
        .id();
    let mut runner = scenario.build();
    let spend = spender_index(&runner, a);

    runner.activate(a, grant_index(&runner, a)).resolve();
    assert_eq!((extra(&runner, a), count(&runner, a)), (2, 1));

    // The layer pass dedupes a granted definition structurally equal to an
    // existing ability, so the gained loyalty ability must differ from the
    // printed `[+1]`: a distinct `[+2]` ability.
    let mut gained = runner.state().objects[&a].abilities[spend].clone();
    gained.cost = Some(AbilityCost::Loyalty { amount: 2 });
    runner
        .state_mut()
        .add_transient_continuous_effect(
            granter,
            P0,
            Duration::Permanent,
            TargetFilter::SpecificObject { id: a },
            vec![ContinuousModification::GrantAbility {
                definition: Box::new(gained),
            }],
            None,
        )
        .expect("the fixture's duration begins");
    layers::mark_layers_full(runner.state_mut());
    layers::flush_layers(runner.state_mut());

    let obj = &runner.state().objects[&a];
    let gained_index = obj.abilities.len() - 1;
    assert_eq!(obj.abilities.len(), 3, "reach-guard: the grant is live");
    assert_eq!(obj.granted_abilities_from, Some(2));
    assert_eq!(
        obj.activated_ability_provenance(gained_index),
        AbilityProvenance::Granted
    );
    assert!(matches!(
        obj.activated_ability_provenance(spend),
        AbilityProvenance::Characteristic(_)
    ));

    assert!(
        gate_admits(&runner, a, spend),
        "the printed ability is covered (count 1, cap 3)"
    );
    assert!(
        !gate_admits(&runner, a, gained_index),
        "the gained ability is not covered (count 1, cap 1)"
    );
}

/// The grant resolves inside a die-result branch frame (a full-range
/// row, so no RNG pinning). No blink is involved: this row asserts only that the
/// building block works in that frame. The reach-guards show the die really rolled
/// and the row's grant parsed as a mandatory grant.
#[test]
fn source_grant_resolves_inside_a_die_result_branch_frame() {
    let mut scenario = scenario();
    let w = scenario
        .add_planeswalker_from_oracle(P0, "Probe Walker", "Probe", 3, DIE_WALKER)
        .id();
    let mut runner = scenario.build();

    let die_index = ability_index(&runner, w, |a| matches!(&*a.effect, Effect::RollDie { .. }));
    let Effect::RollDie { results, .. } = &*runner.state().objects[&w].abilities[die_index].effect
    else {
        unreachable!("the index was found by that shape");
    };
    assert_eq!(results.len(), 1);
    assert_eq!((results[0].min, results[0].max), (1, 6));
    let row = &results[0].effect;
    assert!(is_grant(row) && !row.optional, "{row:#?}");

    let outcome = runner.activate(w, die_index).resolve();
    assert!(
        outcome
            .events()
            .iter()
            .any(|e| matches!(e, GameEvent::DieRolled { sides: 6, .. })),
        "reach-guard: the die rolled"
    );
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert_eq!(extra(&runner, w), 2);
    assert!(runner
        .state()
        .extra_loyalty_activations_this_turn
        .is_empty());
    assert_eq!(grant_events(outcome.events()), 1);
}

/// What the chain walker's `{T}` ability did when it resolved.
struct ChainRun {
    extra: u32,
    player_wide_empty: bool,
    grant_events: usize,
    life_gained: i32,
}

/// Activate the chain walker's `{T}` ability, leave it on the stack, optionally
/// blink the walker in response, then let the stack resolve.
fn run_chain_walker(blink_in_response: bool) -> ChainRun {
    let mut scenario = scenario();
    let w = scenario
        .add_planeswalker_from_oracle(P0, "Probe Walker", "Probe", 5, CHAIN_WALKER)
        .id();
    let mut runner = scenario.build();

    let chain_index = ability_index(&runner, w, |a| {
        matches!(a.cost, Some(AbilityCost::Tap)) && a.sub_ability.is_some()
    });
    let grant = runner.state().objects[&w].abilities[chain_index]
        .sub_ability
        .as_deref()
        .expect("the grant rides the ability");
    assert!(is_grant(grant) && !grant.optional, "{grant:#?}");

    let life_before = life(&runner, P0);
    runner
        .act(GameAction::ActivateAbility {
            source_id: w,
            ability_index: chain_index,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("the {T} ability activates");

    // Reach-guards: one activated-ability entry, stamped with the walker's
    // current incarnation down to the grant sub-ability.
    let inc0 = runner.state().objects[&w].incarnation;
    assert_eq!(runner.state().stack.len(), 1);
    let StackEntryKind::ActivatedAbility { ability, .. } = &runner.state().stack[0].kind else {
        panic!("expected an activated ability entry");
    };
    assert_eq!(ability.source_incarnation, Some(inc0));
    assert_eq!(
        ability.sub_ability.as_deref().unwrap().source_incarnation,
        Some(inc0),
        "the recursive stamp reaches the grant"
    );

    if blink_in_response {
        blink(&mut runner, w);
        let obj = &runner.state().objects[&w];
        assert_ne!(obj.incarnation, inc0, "reach-guard: a new object");
        assert_eq!(obj.zone, Zone::Battlefield);
    }

    let events = drain_stack(&mut runner);
    ChainRun {
        extra: extra(&runner, w),
        player_wide_empty: runner
            .state()
            .extra_loyalty_activations_this_turn
            .is_empty(),
        grant_events: grant_events(&events),
        life_gained: life(&runner, P0) - life_before,
    }
}

/// CR 400.7 on a stack-entry chain, the real-stack discriminator. The
/// walker is blinked while its `{T}` ability (gain life, then the grant) is on the
/// stack. The gain-life clause before the grant resolved (so the chain ran and the
/// absence of a grant is not a fizzle), but the new object receives nothing. The
/// no-blink variant in the same test grants 2 and emits the event.
///
/// Revert-failing: binding the raw `source_id` instead of `self_ref_binding(state)`
/// in the `SelfRef` arm makes the blinked variant read 2.
#[test]
fn source_grant_in_a_stack_entry_chain_is_not_given_to_a_blinked_source() {
    let unblinked = run_chain_walker(false);
    assert_eq!(unblinked.life_gained, 1);
    assert_eq!(
        unblinked.extra, 2,
        "paired positive: without a blink the grant applies"
    );
    assert_eq!(unblinked.grant_events, 1);
    assert!(unblinked.player_wide_empty);

    let blinked = run_chain_walker(true);
    assert_eq!(
        blinked.life_gained, 1,
        "reach-guard: the chain resolved after the blink"
    );
    assert_eq!(blinked.extra, 0, "CR 400.7: the new object is not granted");
    assert_eq!(blinked.grant_events, 0);
    assert!(blinked.player_wide_empty);
}
