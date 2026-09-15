//! Issue #8795 (part 2): `counter::resolve` consumed the CR 614.1a exile rider
//! as a destination only. Delay's "exile it with three time counters on it" is
//! carried by the parse (`enter_with_counters: [(time, 3)]` on the rider) and
//! was dropped — the countered card landed in exile with no counters — and its
//! "If it doesn't have suspend, it gains suspend." tail (a `GenericEffect` on
//! `ParentTarget`) was outside the counter rider branch's allowlist, so the
//! exiled card never became a suspended card (CR 702.62b).
//!
//! Four seams, each with its own counter-probe:
//! 1. the rider's entry counters travel with the countered spell's move
//!    (`cast_from_zone::graveyard_exile_rider_entry_counters`, asked only where
//!    the rider applies);
//! 2. the rider branch admits the `GenericEffect` family and hands the tail the
//!    parent's targets restricted to the cards the rider exiled
//!    (`exile_rider_countered_ids`), so "it" is the exiled card and a refused
//!    counter (CR 101.2) hands it nothing;
//! 3. found once the counters existed: an `AddCounter` replacement describing
//!    its object by type without a zone ("a permanent you control", Doubling
//!    Season) matched the card in exile and doubled the counters —
//!    `replacement_valid_card_matches` now reads the object's zone for that
//!    shape (CR 109.2 + CR 110.1);
//! 4. found once the grant existed: a grant bound to a card outside the
//!    battlefield followed the storage id across zones — after the free cast
//!    the card in the graveyard still had suspend. `apply_zone_exit_cleanup`
//!    now prunes `SpecificObject` grants on every zone exit but two: to the
//!    stack (CR 400.7g), and a permanent spell's stack → battlefield (CR 400.7a).
//!
//! Corpus (`client/public/card-data.json`): of the 20 counter heads carrying
//! the exile rider, Delay is the only one whose rider names counters, and its
//! tail is the only conditioned tail; 21 of the 33 `AddCounter` replacements
//! carry a zone-less type description in `valid_card`, 9 carry no `valid_card`
//! and read as a permanent (or a player).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::apply_resolved_zone_change;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, ControllerRef, Duration, Effect, ObjectScope, QuantityExpr,
    QuantityModification, QuantityRef, ReplacementDefinition, TargetFilter, TypeFilter,
    TypedFilter,
};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastingVariant, GameState, StackEntry, StackEntryKind};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::{Keyword, KeywordKind};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::resolved_commands::ResolvedRulesCommand;
use engine::types::zones::Zone;

// Oracle text verbatim from `client/public/card-data.json`.
const DELAY: &str = "Counter target spell. If the spell is countered this way, exile it with \
                     three time counters on it instead of putting it into its owner's \
                     graveyard. If it doesn't have suspend, it gains suspend. (At the \
                     beginning of its owner's upkeep, they remove a time counter. When the \
                     last is removed, they may play it without paying its mana cost. If it's \
                     a creature, it has haste.)";
const RHYTHM_OF_THE_WILD: &str = "Creature spells you control can't be countered.\nNontoken \
                                  creatures you control have riot. (They enter with your \
                                  choice of a +1/+1 counter or haste.)";

/// An opponent spell on the stack, mirroring `counter_rider_tail_8762.rs`;
/// `printed` are the keywords the card itself carries (Rift Bolt's `Suspend
/// 1—{R}` for the already-has-suspend case).
fn put_spell_on_stack(
    runner: &mut GameRunner,
    controller: PlayerId,
    core: CoreType,
    printed: &[Keyword],
) -> ObjectId {
    let spell = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(701),
        controller,
        "Shock".to_string(),
        Zone::Stack,
    );
    if let Some(obj) = runner.state_mut().objects.get_mut(&spell) {
        obj.card_types.core_types = vec![core];
        obj.keywords = printed.to_vec();
        obj.base_keywords = printed.to_vec();
        // Mana value 2 ({1}{R}) — read by the target-relative counter test.
        obj.mana_cost = ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Red],
        };
        obj.base_mana_cost = obj.mana_cost.clone();
    }
    runner.state_mut().stack.push_back(StackEntry {
        id: spell,
        source_id: spell,
        controller,
        kind: StackEntryKind::Spell {
            card_id: CardId(701),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    spell
}

/// Replace the `enter_with_counters` of the exile rider (the `ChangeZone`
/// directly under the `Counter` head) in a parsed Delay chain.
fn with_rider_counters(
    mut ability: AbilityDefinition,
    counters: &[(CounterType, QuantityExpr)],
) -> AbilityDefinition {
    let rider = ability
        .sub_ability
        .as_deref_mut()
        .expect("reach guard: Delay's counter carries the exile rider");
    let Effect::ChangeZone {
        enter_with_counters,
        ..
    } = &mut *rider.effect
    else {
        panic!(
            "reach guard: the rider is the exile ChangeZone, got {:?}",
            rider.effect
        );
    };
    *enter_with_counters = counters.to_vec();
    ability
}

/// P0 casts Delay at an opponent spell of type `core` carrying `printed`
/// keywords and resolves it. Returns the runner and the countered spell.
fn delay_against(
    core: CoreType,
    printed: &[Keyword],
    setup: impl FnOnce(&mut GameScenario),
) -> (GameRunner, ObjectId) {
    delay_against_with(core, printed, None, setup)
}

/// As `delay_against`; `rider_counters`, when given, replaces the parsed
/// rider's `enter_with_counters` (Delay's `Fixed` 3) before the card is
/// built, so a target-relative count can be driven through the production
/// path without a corpus card that prints one.
fn delay_against_with(
    core: CoreType,
    printed: &[Keyword],
    rider_counters: Option<Vec<(CounterType, QuantityExpr)>>,
    setup: impl FnOnce(&mut GameScenario),
) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let delay = match rider_counters {
        None => {
            let mut cs = scenario.add_spell_to_hand_from_oracle(P0, "Delay", true, DELAY);
            cs.with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Blue],
            });
            cs.id()
        }
        Some(counters) => {
            let parsed = parse_oracle_text(DELAY, "Delay", &[], &["Instant".to_string()], &[]);
            let mut cs = scenario.add_spell_to_hand(P0, "Delay", true);
            cs.with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Blue],
            });
            for ability in parsed.abilities {
                cs.with_ability_definition(with_rider_counters(ability, &counters));
            }
            cs.id()
        }
    };
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);
    setup(&mut scenario);
    let mut runner = scenario.build();
    let opponent_spell = put_spell_on_stack(&mut runner, P1, core, printed);
    runner
        .cast(delay)
        .target_objects(&[opponent_spell])
        .try_resolve()
        .expect("Delay must cast and resolve");
    (runner, opponent_spell)
}

fn time_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Time)
        .copied()
        .unwrap_or(0)
}

/// Suspend as the game sees it on a card OFF the battlefield: the printed
/// keywords plus every granted one (`object_has_effective_keyword_kind`, the
/// authority the "without suspend" filter and the suspend triggers read). The
/// raw `has_keyword_kind` reads printed keywords only and is blind to a grant
/// on a card in exile.
fn has_suspend(runner: &GameRunner, id: ObjectId) -> bool {
    engine::game::keywords::object_has_effective_keyword_kind(
        runner.state(),
        id,
        KeywordKind::Suspend,
    )
}

/// Reach guard shared by the exiling tests: Delay countered the spell and its
/// rider exiled it, so a failure below is about the counters or the tail.
fn assert_countered_into_exile(runner: &GameRunner, countered: ObjectId) {
    assert!(
        runner.state().stack.is_empty(),
        "the spell must be countered (off the stack)"
    );
    assert_eq!(
        runner.state().objects[&countered].zone,
        Zone::Exile,
        "the rider must exile the countered spell instead of the graveyard"
    );
}

/// CR 614.1a + CR 122.1 + CR 702.62b: Delay exiles the countered spell WITH
/// three time counters, and its tail grants suspend to the exiled card — for
/// good (CR 611.2a: no stated duration), so the card is a suspended card
/// (in exile, has suspend, has a time counter) and suspend's second ability
/// removes a time counter at the beginning of its owner's upkeep
/// (CR 702.62a). On `main` the card was exiled with no counters and no suspend.
#[test]
fn delay_exiles_the_countered_spell_with_three_time_counters_and_suspend() {
    let (mut runner, countered) = delay_against(CoreType::Creature, &[], |_| {});
    assert_countered_into_exile(&runner, countered);

    assert_eq!(
        time_counters(&runner, countered),
        3,
        "the rider's \"with three time counters on it\" must travel with the exile (issue \
         #8795)"
    );
    assert!(
        has_suspend(&runner, countered),
        "\"it gains suspend\" must reach the exiled card (issue #8795)"
    );
    let grants: Vec<_> = runner
        .state()
        .transient_continuous_effects
        .iter()
        .filter(|effect| {
            effect.affected
                == engine::types::ability::TargetFilter::SpecificObject { id: countered }
        })
        .collect();
    assert_eq!(
        grants.len(),
        1,
        "exactly one grant on the exiled card: {grants:#?}"
    );
    assert_eq!(
        grants[0].duration,
        Duration::Permanent,
        "CR 611.2a: the grant states no duration and lasts until the game ends"
    );

    // CR 702.62a: the granted suspend's upkeep trigger removes a time counter
    // at the beginning of the OWNER's (P1's) upkeep — the next turn's.
    let turn_before = runner.state().turn_number;
    runner.advance_to_upkeep();
    assert!(
        runner.state().phase == Phase::Upkeep
            && runner.state().active_player == P1
            && runner.state().turn_number > turn_before,
        "reach guard: the advance must land in P1's upkeep, got turn {} {:?} active {:?}",
        runner.state().turn_number,
        runner.state().phase,
        runner.state().active_player
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        time_counters(&runner, countered),
        2,
        "the suspend upkeep trigger must tick the exiled card 3 → 2 at its owner's upkeep"
    );
    assert!(
        has_suspend(&runner, countered),
        "the grant must survive the cleanup step (CR 611.2a)"
    );
}

/// The tail's condition, at the same entrance: "If it doesn't have suspend" —
/// a countered card that already has suspend (Rift Bolt cast for its mana
/// cost) is exiled with Delay's three time counters and gains nothing; its own
/// suspend is the one the game reads.
#[test]
fn delay_does_not_grant_suspend_to_a_card_that_has_it() {
    let rift_bolt_suspend = Keyword::Suspend {
        count: 1,
        cost: ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Red],
        },
    };
    let (runner, countered) = delay_against(CoreType::Instant, &[rift_bolt_suspend], |_| {});
    assert_countered_into_exile(&runner, countered);

    assert_eq!(
        time_counters(&runner, countered),
        3,
        "the rider's counters do not depend on the tail's condition"
    );
    assert!(
        has_suspend(&runner, countered),
        "reach guard: the card's printed suspend is what the condition reads"
    );
    assert!(
        runner.state().transient_continuous_effects.is_empty(),
        "\"If it doesn't have suspend\" is false, so the tail must grant nothing: {:#?}",
        runner.state().transient_continuous_effects
    );
}

/// CR 101.2: against an uncounterable spell Delay counters nothing, so nothing
/// is exiled — no counters, and the tail's "it" is no spell at all: the spell
/// on the stack must not gain suspend. The tail is handed the parent's targets
/// restricted to `exile_rider_countered_ids`; without that restriction the
/// bare parent target would be the surviving spell.
#[test]
fn delay_leaves_an_uncounterable_spell_alone() {
    let (runner, uncounterable) = delay_against(CoreType::Creature, &[], |scenario| {
        scenario.add_enchantment_from_oracle(P1, "Rhythm of the Wild", RHYTHM_OF_THE_WILD);
    });
    assert_eq!(
        runner.state().objects[&uncounterable].zone,
        Zone::Stack,
        "reach guard: the creature spell must survive the counter (CR 101.2)"
    );
    assert_eq!(
        time_counters(&runner, uncounterable),
        0,
        "no counter, no rider, no time counters"
    );
    assert!(
        !has_suspend(&runner, uncounterable),
        "the spell on the stack must not gain suspend"
    );
    assert!(
        runner.state().transient_continuous_effects.is_empty(),
        "the tail must grant nothing when the rider exiled nothing: {:#?}",
        runner.state().transient_continuous_effects
    );
}

const DOUBLING_SEASON: &str =
    "If an effect would create one or more tokens under your control, it \
                               creates twice that many of those tokens instead.\nIf an effect \
                               would put one or more counters on a permanent you control, it puts \
                               twice that many of those counters on that permanent instead.";

/// CR 109.2 + CR 110.1: Doubling Season doubles counters put on "a permanent
/// you control" — a card in exile is not a permanent, so Delay's three time
/// counters stay three under the countered spell's controller's Doubling
/// Season. Measured before the gate in `replacement_valid_card_matches`: six.
#[test]
fn delays_time_counters_are_not_doubled_by_the_owners_doubling_season() {
    let (runner, countered) = delay_against(CoreType::Creature, &[], |scenario| {
        scenario.add_enchantment_from_oracle(P1, "Doubling Season", DOUBLING_SEASON);
    });
    assert_countered_into_exile(&runner, countered);
    assert_eq!(
        time_counters(&runner, countered),
        3,
        "CR 110.1: a card in exile is not a permanent, so Doubling Season does not apply"
    );
    assert!(has_suspend(&runner, countered));
}

/// The gate's positive partner at the same entrance: the same Doubling Season
/// still doubles counters an effect puts on a permanent its controller
/// controls on the battlefield.
#[test]
fn doubling_season_still_doubles_counters_on_a_permanent_on_the_battlefield() {
    const GROW: &str = "Put a +1/+1 counter on target creature.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut cs = scenario.add_spell_to_hand_from_oracle(P0, "Grow", true, GROW);
    cs.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Green],
    });
    let grow = cs.id();
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_enchantment_from_oracle(P1, "Doubling Season", DOUBLING_SEASON);
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    runner
        .cast(grow)
        .target_objects(&[bear])
        .try_resolve()
        .expect("the counter spell must cast and resolve");
    assert_eq!(
        runner.state().objects[&bear]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        2,
        "CR 110.1: the creature on the battlefield is a permanent P1 controls, so the \
         counter is doubled"
    );
}

const VORINCLEX: &str = "Trample, haste\nIf you would put one or more counters on a permanent or \
                         player, put twice that many of each of those kinds of counters on \
                         that permanent or player instead.\nIf an opponent would put one or \
                         more counters on a permanent or player, they put half that many of \
                         each of those kinds of counters on that permanent or player instead, \
                         rounded down.";

/// The same rule for a replacement that names no `valid_card` ("a permanent
/// or player" — Vorinclex): the exiled card is not a permanent, so neither
/// the doubling nor the halving applies, whichever player the engine
/// attributes the placement to (it reads the card's controller as the actor).
/// Measured before the gate: six with Vorinclex on P1's side, one on P0's.
#[test]
fn delays_time_counters_are_not_doubled_or_halved_by_vorinclex() {
    for (vorinclex_controller, expected_wrong) in [(P1, "six"), (P0, "one")] {
        let (runner, countered) = delay_against(CoreType::Creature, &[], |scenario| {
            scenario.add_creature_from_oracle(
                vorinclex_controller,
                "Vorinclex, Monstrous Raider",
                6,
                6,
                VORINCLEX,
            );
        });
        assert_countered_into_exile(&runner, countered);
        assert_eq!(
            time_counters(&runner, countered),
            3,
            "CR 110.1: a card in exile is not a permanent, so Vorinclex on {vorinclex_controller:?}'s \
             side does not apply (was {expected_wrong})"
        );
    }
}

/// Vorinclex's positive partner at the same entrance: P0's counter on P1's
/// creature on the battlefield is halved by P1's Vorinclex (1 → 0, rounded down).
#[test]
fn vorinclex_still_halves_an_opponents_counter_on_a_permanent_on_the_battlefield() {
    const GROW: &str = "Put two +1/+1 counters on target creature.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut cs = scenario.add_spell_to_hand_from_oracle(P0, "Grow", true, GROW);
    cs.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Green],
    });
    let grow = cs.id();
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_creature_from_oracle(P1, "Vorinclex, Monstrous Raider", 6, 6, VORINCLEX);
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    runner
        .cast(grow)
        .target_objects(&[bear])
        .try_resolve()
        .expect("the counter spell must cast and resolve");
    assert_eq!(
        runner.state().objects[&bear]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "CR 110.1: the creature on the battlefield is a permanent, so P1's Vorinclex halves \
         the opponent's two counters to one"
    );
}

/// Drive the real turn structure to the beginning of `player`'s next upkeep,
/// passing every priority window and declaring no attackers or blockers on
/// the way (the shape `tenth_doctor_allons_y_grants_working_suspend` uses).
fn advance_to_next_upkeep_of(runner: &mut GameRunner, player: PlayerId) {
    use engine::game::engine::apply_as_current;
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    let start_turn = runner.state().turn_number;
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(
            guard < 300,
            "turn progression stalled before {player:?}'s upkeep"
        );
        let state = runner.state_mut();
        if state.phase == Phase::Upkeep
            && state.active_player == player
            && state.turn_number > start_turn
        {
            return;
        }
        match &state.waiting_for {
            WaitingFor::Priority { .. } => {
                apply_as_current(state, GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::DeclareAttackers { .. } => {
                apply_as_current(
                    state,
                    GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    },
                )
                .expect("declare no attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                apply_as_current(
                    state,
                    GameAction::DeclareBlockers {
                        assignments: vec![],
                    },
                )
                .expect("declare no blockers");
            }
            other => panic!("unexpected waiting state during turn progression: {other:?}"),
        }
    }
}

/// CR 702.62a + CR 400.7g + CR 400.7: the whole suspend run under the granted
/// keyword — one time counter removed at each of the owner's upkeeps (turns 3,
/// 5, 7), the "you may play it" offer when the last is removed, the free cast
/// — the spell on the stack still has suspend (CR 400.7g: an ability granted
/// to a card that allows it to be cast continues to apply to the new object
/// on the stack), and once it resolves the card in the graveyard is a new
/// object with no grant (CR 400.7). Measured before the prune in
/// `apply_zone_exit_cleanup`: the grant followed the storage id into the
/// graveyard.
#[test]
fn delays_granted_suspend_runs_to_the_free_cast_and_ends_with_the_spell() {
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    let (mut runner, countered) = delay_against(CoreType::Instant, &[], |scenario| {
        // CR 104.3c: stock both libraries so nobody decks out on the way.
        for _ in 0..10 {
            scenario.add_card_to_library_top(P0, "Island");
            scenario.add_card_to_library_top(P1, "Forest");
        }
    });
    assert_countered_into_exile(&runner, countered);
    assert!(has_suspend(&runner, countered));

    let mut last_turn = runner.state().turn_number;
    for expected in [2, 1, 0] {
        advance_to_next_upkeep_of(&mut runner, P1);
        assert!(
            runner.state().turn_number > last_turn,
            "reach guard: each tick is a later upkeep of the owner"
        );
        last_turn = runner.state().turn_number;
        runner.advance_until_stack_empty();
        assert_eq!(
            time_counters(&runner, countered),
            expected,
            "CR 702.62a: one time counter removed at the owner's upkeep"
        );
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::OptionalEffectChoice { player, .. } if player == P1),
        "CR 702.62a: the last counter's removal offers the owner the free cast, got {:?}",
        runner.state().waiting_for
    );
    engine::game::engine::apply_as_current(
        runner.state_mut(),
        GameAction::DecideOptionalEffect { accept: true },
    )
    .expect("accept the free cast");
    assert_eq!(
        runner.state().objects[&countered].zone,
        Zone::Stack,
        "CR 702.62a: accepting the offer casts the card from exile"
    );
    assert!(
        has_suspend(&runner, countered),
        "CR 400.7g: the granted suspend continues to apply to the spell on the stack"
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&countered].zone,
        Zone::Graveyard,
        "the instant resolves into its owner's graveyard"
    );
    assert!(
        !has_suspend(&runner, countered),
        "CR 400.7: the card in the graveyard is a new object; the grant ended with the spell"
    );
    assert!(
        runner.state().transient_continuous_effects.is_empty(),
        "the grant is gone, not merely inert: {:#?}",
        runner.state().transient_continuous_effects
    );
}

/// A rider whose count is target-relative — "exile it with X time counters
/// on it, where X is its mana value" (`QuantityRef::ObjectManaValue { Target }`,
/// no corpus card prints one; Delay's parse with its `Fixed` 3 replaced) —
/// counts the countered spell: mana value 2 → 2 time counters.
/// `graveyard_exile_rider_entry_counters` binds the countered object into
/// the rider's context before resolving; the rider sub starts without
/// targets, so an unbound resolution reads no target and counts 0 (measured).
#[test]
fn a_target_relative_rider_count_reads_the_countered_spell() {
    let (runner, countered) = delay_against_with(
        CoreType::Creature,
        &[],
        Some(vec![(
            CounterType::Time,
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Target,
                },
            },
        )]),
        |_| {},
    );
    assert_countered_into_exile(&runner, countered);
    assert_eq!(
        runner.state().objects[&countered].effective_mana_value(),
        2,
        "reach guard: the countered spell's mana value is the count"
    );
    assert_eq!(
        time_counters(&runner, countered),
        2,
        "the rider's target-relative count must read the countered spell (issue #8795 review)"
    );
}

/// CR 109.2 at the replacement seam, the gate's siblings: a counter
/// replacement whose `valid_card` only SOMETIMES describes a permanent —
/// "creature or instant" (a mixed `Or`), "nonland" (`Non`) — is not
/// battlefield-only, so the gate stays out of its way and it applies to the
/// exiled card as its filter says (six time counters); the positive partner
/// "creature you control" is battlefield-only and leaves the exiled creature
/// spell's three counters alone. Hand-built definitions on a P1 permanent,
/// the shape `vorinclex_loyalty_actor_scope` uses.
#[test]
fn counter_replacement_zone_gate_ignores_mixed_and_negated_type_descriptions() {
    fn typed(types: Vec<TypeFilter>) -> TargetFilter {
        TargetFilter::Typed(TypedFilter {
            type_filters: types,
            controller: Some(ControllerRef::You),
            ..Default::default()
        })
    }
    fn doubling(valid_card: TargetFilter) -> ReplacementDefinition {
        ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .valid_card(valid_card)
            .quantity_modification(QuantityModification::DOUBLE)
            .description("double".to_string())
    }
    let rows: [(&str, TargetFilter, u32); 4] = [
        (
            "creature or instant you control (Or)",
            TargetFilter::Or {
                filters: vec![
                    typed(vec![TypeFilter::Creature]),
                    typed(vec![TypeFilter::Instant]),
                ],
            },
            6,
        ),
        (
            "creature or instant you control (AnyOf)",
            typed(vec![TypeFilter::AnyOf(vec![
                TypeFilter::Creature,
                TypeFilter::Instant,
            ])]),
            6,
        ),
        (
            "nonland you control",
            typed(vec![TypeFilter::Non(Box::new(TypeFilter::Land))]),
            6,
        ),
        ("creature you control", typed(vec![TypeFilter::Creature]), 3),
    ];
    for (label, valid_card, expected) in rows {
        let (runner, countered) = delay_against(CoreType::Creature, &[], |scenario| {
            scenario
                .add_creature(P1, "Counter Doubler", 1, 1)
                .with_replacement_definition(doubling(valid_card));
        });
        assert_countered_into_exile(&runner, countered);
        assert_eq!(time_counters(&runner, countered), expected, "{label}");
    }
}

/// Replay equivalence (CR 733 P2 + CR 400.7): a recorded zone exit of the
/// suspended card, applied through `apply_resolved_zone_change` to the state
/// before it, ends the grant exactly where the live path did. Both paths call
/// `prune_object_bound_effects_on_exit`; measured before the shared authority,
/// replay retained the grant across the new object. The recorded move is an
/// exile → graveyard `move_to_zone` (the shape `cr733_resolved_zone_change`
/// records its commands with; Pull from Eternity's "target face-up exiled
/// card" does not parse to an object target today): none of Delay's own
/// moves (stack → exile, exile → stack, stack → graveyard) is journaled as a
/// zone command — stack routes are outside the zone-command family's first
/// cut (`move_to_zone_with_entry_flags`), and `apply_resolved_stack_removal`
/// moves no object — so the journaled exit of a suspended card is a
/// non-stack move.
#[test]
fn replaying_a_zone_exit_ends_the_suspend_grant_like_the_live_path() {
    let (mut runner, countered) = delay_against(CoreType::Instant, &[], |_| {});
    assert_countered_into_exile(&runner, countered);
    assert!(has_suspend(&runner, countered));
    let pre_exit: GameState = runner.state().clone();

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), countered, Zone::Graveyard, &mut events);
    assert_eq!(runner.state().objects[&countered].zone, Zone::Graveyard);
    assert!(
        !has_suspend(&runner, countered) && runner.state().transient_continuous_effects.is_empty(),
        "live: the grant ended with the exiled object (CR 400.7)"
    );

    let command = runner
        .state()
        .resolved_rules_journal
        .entries()
        .iter()
        .filter_map(|entry| entry.command.as_ref())
        .find_map(|command| match command {
            ResolvedRulesCommand::ZoneChange(command)
                if command.object.object_id == countered
                    && command.from == Zone::Exile
                    && command.to == Zone::Graveyard =>
            {
                Some(command.as_ref().clone())
            }
            _ => None,
        })
        .expect("the live exile → graveyard move journals its zone command");

    let mut replay = pre_exit;
    apply_resolved_zone_change(&mut replay, &command).expect("exile → graveyard replays");
    assert_eq!(replay.objects[&countered].zone, Zone::Graveyard);
    assert!(
        !engine::game::keywords::object_has_effective_keyword_kind(
            &replay,
            countered,
            KeywordKind::Suspend
        ) && replay.transient_continuous_effects.is_empty(),
        "CR 400.7: replay ends the grant like the live path: {:#?}",
        replay.transient_continuous_effects
    );
}
