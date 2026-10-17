//! Triggers caused by the cost payments of a paused activation announcement.
//!
//! CR 602.2a + CR 602.2b: an activated ability is announced like a spell and its
//! costs are paid in the announcement. CR 603.2 + CR 603.2c: an ability triggers
//! once per occurrence of its event; a simultaneous group is one occurrence.
//! CR 603.3b: every ability that triggered since a player last received priority
//! is put on the stack in one APNAP batch, ordered once per controller. CR 733.1:
//! an undone action triggers nothing.
//!
//! An activation whose announcement crosses two event-producing prompts (a
//! discard and a sacrifice, say) must put the triggers of its discard on the stack
//! with the activation's own triggers, as one batch, exactly as the unpaused path
//! does. Each row drives the real reducer action by action and asserts what is
//! on the stack and which prompts appear, never how the engine holds them in the
//! meantime. The N1 and N2 abilities are synthetic (`from_oracle_text`): the class
//! is a cost shape, not a printed card.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, Effect, QuantityExpr,
    ReplacementDefinition, SacrificeCost, TargetFilter, TargetRef, TriggerDefinition, TypeFilter,
    TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, PendingCostMoveResume, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::triggers::TriggerMode;
use engine::types::zones::{EtbTapState, Zone};

use super::support::shared_card_db;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Spell,
    Activated,
    Triggered,
    Other,
}

/// The stack, bottom to top, as (kind, source, controller).
fn stack(r: &GameRunner) -> Vec<(Kind, ObjectId, PlayerId)> {
    r.state()
        .stack
        .iter()
        .map(|entry| {
            let kind = match &entry.kind {
                StackEntryKind::Spell { .. } => Kind::Spell,
                StackEntryKind::ActivatedAbility { .. } => Kind::Activated,
                StackEntryKind::TriggeredAbility { .. } => Kind::Triggered,
                _ => Kind::Other,
            };
            (kind, entry.source_id, entry.controller)
        })
        .collect()
}

fn stack_triggers_from(r: &GameRunner, source: ObjectId) -> usize {
    stack(r)
        .iter()
        .filter(|(kind, src, _)| *kind == Kind::Triggered && *src == source)
        .count()
}

fn add_mana(r: &mut GameRunner, player: PlayerId, mana: ManaType, count: usize) {
    for _ in 0..count {
        r.state_mut().players[player.0 as usize]
            .mana_pool
            .add(ManaUnit::new(mana, ObjectId(0), false, vec![]));
    }
}

fn act(r: &mut GameRunner, action: GameAction) -> WaitingFor {
    if let Err(error) = r.act(action.clone()) {
        panic!(
            "{action:?} rejected at {:?}: {error:?}",
            r.state().waiting_for
        );
    }
    r.state().waiting_for.clone()
}

fn cast(r: &mut GameRunner, object: ObjectId, mode: CastPaymentMode) -> WaitingFor {
    let card_id = r.state().objects[&object].card_id;
    act(
        r,
        GameAction::CastSpell {
            object_id: object,
            card_id,
            targets: vec![],
            payment_mode: mode,
        },
    )
}

fn activate(r: &mut GameRunner, source: ObjectId) -> WaitingFor {
    act(
        r,
        GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        },
    )
}

fn select_cards(r: &mut GameRunner, cards: Vec<ObjectId>) -> WaitingFor {
    act(r, GameAction::SelectCards { cards })
}

fn board(players: u8) -> GameScenario {
    let mut scenario = if players > 2 {
        GameScenario::new_n_player(players, 7)
    } else {
        GameScenario::new()
    };
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn build(scenario: GameScenario) -> GameRunner {
    let db = shared_card_db().expect("card db");
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner
}

/// The sources of an ordering prompt, sorted; panics when `wf` is not one.
fn prompt_sources(wf: &WaitingFor, controller: PlayerId) -> Vec<ObjectId> {
    let WaitingFor::OrderTriggers { player, triggers } = wf else {
        panic!("expected one ordering prompt, got {wf:?}");
    };
    assert_eq!(*player, controller);
    let mut sources: Vec<ObjectId> = triggers.iter().map(|t| t.source_id).collect();
    sources.sort();
    sources
}

fn sorted(mut sources: Vec<ObjectId>) -> Vec<ObjectId> {
    sources.sort();
    sources
}

fn order_in_listed_order(r: &mut GameRunner) {
    let WaitingFor::OrderTriggers { triggers, .. } = r.state().waiting_for.clone() else {
        panic!(
            "expected an ordering prompt, got {:?}",
            r.state().waiting_for
        );
    };
    act(
        r,
        GameAction::OrderTriggers {
            order: (0..triggers.len()).collect(),
        },
    );
}

// ---------------------------------------------------------------------------
// N1, N2, N2b: a discard cost that is not the last cost prompt
// ---------------------------------------------------------------------------

const TWO_COSTS_TARGETED: &str =
    "Sacrifice a creature, Discard a card: This creature deals 1 damage to any target.";
const TWO_COSTS_UNTARGETED: &str = "Sacrifice a creature, Discard a card: You gain 1 life.";
const ONE_COST_TARGETED: &str = "Discard a card: This creature deals 1 damage to any target.";
const ONE_COST_UNTARGETED: &str = "Discard a card: You gain 1 life.";

struct CostBoard {
    r: GameRunner,
    src: ObjectId,
    fodder: ObjectId,
    card: ObjectId,
    haven: ObjectId,
    construct: ObjectId,
    zulaport: ObjectId,
    grimoire: Option<ObjectId>,
}

/// P0: the activating creature, a spare creature, Grizzly Bears in hand, Drake
/// Haven (cycle or discard), Crackdown Construct (activation) and Zulaport
/// Cutthroat (a creature of yours dies). With `players > 2`, P1 has Geth's
/// Grimoire (an opponent discards).
fn cost_board(text: &str, players: u8) -> CostBoard {
    let db = shared_card_db().expect("card db");
    let mut scenario = board(players);
    let src = scenario
        .add_creature(P0, "Src", 1, 1)
        .from_oracle_text(text)
        .id();
    let fodder = scenario.add_creature(P0, "Fodder", 1, 1).id();
    let card = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let zulaport = scenario.add_real_card(P0, "Zulaport Cutthroat", Zone::Battlefield, db);
    let construct = scenario.add_real_card(P0, "Crackdown Construct", Zone::Battlefield, db);
    let haven = scenario.add_real_card(P0, "Drake Haven", Zone::Battlefield, db);
    let grimoire =
        (players > 2).then(|| scenario.add_real_card(P1, "Geth's Grimoire", Zone::Battlefield, db));
    CostBoard {
        r: build(scenario),
        src,
        fodder,
        card,
        haven,
        construct,
        zulaport,
        grimoire,
    }
}

/// Activates, aims at P1 when the ability has a target, and answers the discard
/// prompt. Returns the prompt that follows it.
fn activate_and_discard(b: &mut CostBoard, targeted: bool) -> WaitingFor {
    activate(&mut b.r, b.src);
    if targeted {
        act(
            &mut b.r,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            },
        );
    }
    let wf = b.r.state().waiting_for.clone();
    let WaitingFor::PayCost { choices, .. } = &wf else {
        panic!("expected the discard prompt, got {wf:?}");
    };
    assert!(
        choices.contains(&b.card) && !choices.contains(&b.fodder),
        "the first cost prompt is the discard: {choices:?}"
    );
    select_cards(&mut b.r, vec![b.card])
}

/// The single-prompt reference: `Discard a card: ...` on the same board.
fn reference_prompt(text: &str, targeted: bool) -> (CostBoard, WaitingFor) {
    let mut b = cost_board(text, 2);
    let wf = activate_and_discard(&mut b, targeted);
    (b, wf)
}

/// N1 — a TARGETED activation with two cost prompts keeps the trigger of its
/// discard (the first prompt) alongside the activation's own (CR 603.3b).
#[test]
fn n1_targeted_discard_trigger_survives_a_later_cost_prompt() {
    // Positive control: the single-prompt reference reaches the machinery and
    // yields one ordering prompt of {Haven, Construct}.
    let (b, wf) = reference_prompt(ONE_COST_TARGETED, true);
    assert_eq!(
        prompt_sources(&wf, P0),
        sorted(vec![b.haven, b.construct]),
        "reference: the unpaused activation triggers Haven and Construct as one batch"
    );

    let mut b = cost_board(TWO_COSTS_TARGETED, 2);
    let wf = activate_and_discard(&mut b, true);
    // Reach guard: the discard was paid and the activation waits at the sacrifice.
    assert_eq!(b.r.state().objects[&b.card].zone, Zone::Graveyard);
    let WaitingFor::PayCost { choices, .. } = &wf else {
        panic!("expected the sacrifice prompt, got {wf:?}");
    };
    assert!(choices.contains(&b.fodder), "{choices:?}");
    assert!(b.r.state().stack.is_empty());

    let wf = select_cards(&mut b.r, vec![b.fodder]);
    assert_eq!(
        prompt_sources(&wf, P0),
        sorted(vec![b.haven, b.construct, b.zulaport]),
        "one batch: the discard's Haven, the activation's Construct, the sacrifice's Zulaport"
    );
    order_in_listed_order(&mut b.r);
    let on_stack = stack(&b.r);
    assert_eq!(on_stack[0], (Kind::Activated, b.src, P0));
    assert_eq!(on_stack.len(), 4, "{on_stack:?}");
    assert_eq!(stack_triggers_from(&b.r, b.haven), 1);
    assert_eq!(stack_triggers_from(&b.r, b.construct), 1);
    assert_eq!(stack_triggers_from(&b.r, b.zulaport), 1);
}

/// N1 at three players: a non-active opponent's discard observer (Geth's
/// Grimoire) is in the same batch, controlled by its own player (APNAP).
#[test]
fn n1_three_player_opponent_discard_observer_joins_the_batch() {
    let mut b = cost_board(TWO_COSTS_TARGETED, 3);
    let grimoire = b.grimoire.unwrap();
    let wf = activate_and_discard(&mut b, true);
    assert_eq!(b.r.state().objects[&b.card].zone, Zone::Graveyard);
    assert!(matches!(wf, WaitingFor::PayCost { .. }), "{wf:?}");

    let wf = select_cards(&mut b.r, vec![b.fodder]);
    assert_eq!(
        prompt_sources(&wf, P0),
        sorted(vec![b.haven, b.construct, b.zulaport])
    );
    order_in_listed_order(&mut b.r);
    // APNAP: the active player's triggers go on first, the opponent's on top.
    let on_stack = stack(&b.r);
    assert_eq!(on_stack[0], (Kind::Activated, b.src, P0));
    assert_eq!(stack_triggers_from(&b.r, b.haven), 1);
    assert_eq!(stack_triggers_from(&b.r, grimoire), 1);
    assert_eq!(*on_stack.last().unwrap(), (Kind::Triggered, grimoire, P1));
}

/// N2 — an UNTARGETED activation with two cost prompts is one CR 603.3b batch.
#[test]
fn n2_untargeted_discard_trigger_is_in_the_activation_batch() {
    // Positive control: the single-prompt reference.
    let (b, wf) = reference_prompt(ONE_COST_UNTARGETED, false);
    assert_eq!(
        prompt_sources(&wf, P0),
        sorted(vec![b.haven, b.construct]),
        "reference: the unpaused activation triggers Haven and Construct as one batch"
    );

    let mut b = cost_board(TWO_COSTS_UNTARGETED, 2);
    let wf = activate_and_discard(&mut b, false);
    assert_eq!(b.r.state().objects[&b.card].zone, Zone::Graveyard);
    assert!(matches!(wf, WaitingFor::PayCost { .. }), "{wf:?}");
    assert!(
        b.r.state().stack.is_empty(),
        "nothing is on the stack while the activation is announced"
    );

    let wf = select_cards(&mut b.r, vec![b.fodder]);
    assert_eq!(
        prompt_sources(&wf, P0),
        sorted(vec![b.haven, b.construct, b.zulaport]),
        "one ordering prompt for the whole batch, not Haven alone first"
    );
    order_in_listed_order(&mut b.r);
    let on_stack = stack(&b.r);
    assert_eq!(on_stack[0], (Kind::Activated, b.src, P0));
    assert_eq!(on_stack.len(), 4, "{on_stack:?}");
    assert!(
        matches!(b.r.state().waiting_for, WaitingFor::Priority { .. }),
        "no second ordering prompt follows"
    );
}

/// N2b — Drake Haven's discard trigger is on the stack once, in either shape.
#[test]
fn n2b_discard_triggers_exactly_once() {
    for (text, targeted) in [(TWO_COSTS_TARGETED, true), (TWO_COSTS_UNTARGETED, false)] {
        // The reference counts what one discard produces.
        let (mut reference, wf) = reference_prompt(
            if targeted {
                ONE_COST_TARGETED
            } else {
                ONE_COST_UNTARGETED
            },
            targeted,
        );
        assert!(matches!(wf, WaitingFor::OrderTriggers { .. }), "{wf:?}");
        order_in_listed_order(&mut reference.r);
        assert_eq!(stack_triggers_from(&reference.r, reference.haven), 1);

        let mut b = cost_board(text, 2);
        activate_and_discard(&mut b, targeted);
        let wf = select_cards(&mut b.r, vec![b.fodder]);
        assert!(matches!(wf, WaitingFor::OrderTriggers { .. }), "{wf:?}");
        order_in_listed_order(&mut b.r);
        assert_eq!(
            stack_triggers_from(&b.r, b.haven),
            1,
            "targeted={targeted}: never zero, never two"
        );
    }
}

/// CR 601.2h + CR 602.2b: once an activation's discard is paid the cost is
/// committed. The engine refuses the cancel at the next cost prompt (a paid
/// cost is not a free undo), the prompt, the discarded card and the discard's
/// pending trigger all stay, and finishing the sacrifice puts the whole batch on
/// the stack exactly once (CR 603.2 + CR 603.3b), in either shape. The control
/// is the cancel BEFORE any payment, which is still accepted and puts nothing
/// on the stack.
#[test]
fn n1_cancel_after_a_paid_discard_is_refused_and_keeps_its_triggers() {
    for (text, targeted) in [(TWO_COSTS_TARGETED, true), (TWO_COSTS_UNTARGETED, false)] {
        let mut b = cost_board(text, 2);
        let wf = activate_and_discard(&mut b, targeted);
        // Reach guard: the discard is paid; the sacrifice prompt is pending.
        assert!(matches!(wf, WaitingFor::PayCost { .. }), "{wf:?}");
        assert_eq!(b.r.state().objects[&b.card].zone, Zone::Graveyard);

        let refused = b.r.act(GameAction::CancelCast);
        assert!(
            refused.is_err(),
            "targeted={targeted}: a paid cost is committed, the cancel is refused"
        );
        assert_eq!(
            b.r.state().waiting_for,
            wf,
            "targeted={targeted}: the sacrifice prompt is preserved"
        );
        assert_eq!(b.r.state().objects[&b.card].zone, Zone::Graveyard);
        assert!(b.r.state().stack.is_empty());

        let wf = select_cards(&mut b.r, vec![b.fodder]);
        assert_eq!(
            prompt_sources(&wf, P0),
            sorted(vec![b.haven, b.construct, b.zulaport]),
            "targeted={targeted}"
        );
        order_in_listed_order(&mut b.r);
        assert_eq!(stack(&b.r)[0], (Kind::Activated, b.src, P0));
        assert_eq!(stack_triggers_from(&b.r, b.haven), 1, "targeted={targeted}");
        assert_eq!(
            stack_triggers_from(&b.r, b.construct),
            1,
            "targeted={targeted}"
        );
        assert_eq!(
            stack_triggers_from(&b.r, b.zulaport),
            1,
            "targeted={targeted}"
        );
        assert_eq!(b.r.state().stack.len(), 4, "targeted={targeted}");
    }
}

/// Control for the above: cancelling at the FIRST prompt, before anything is
/// paid, is still accepted and leaves nothing behind.
#[test]
fn n1_cancel_before_any_payment_is_accepted() {
    for (text, targeted) in [(TWO_COSTS_TARGETED, true), (TWO_COSTS_UNTARGETED, false)] {
        let mut b = cost_board(text, 2);
        activate(&mut b.r, b.src);
        if targeted {
            act(
                &mut b.r,
                GameAction::SelectTargets {
                    targets: vec![TargetRef::Player(P1)],
                },
            );
        }
        assert!(matches!(
            b.r.state().waiting_for,
            WaitingFor::PayCost { .. }
        ));
        let wf = act(&mut b.r, GameAction::CancelCast);
        assert!(matches!(wf, WaitingFor::Priority { .. }), "{wf:?}");
        assert_eq!(b.r.state().objects[&b.card].zone, Zone::Hand);
        assert!(b.r.state().stack.is_empty(), "targeted={targeted}");
    }
}

// ---------------------------------------------------------------------------
// V21: a count-two sacrifice group paused at a ReplacementChoice
// ---------------------------------------------------------------------------

fn redirect_self(destination: Zone, redirected_to: Zone) -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(destination)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination: redirected_to,
                origin: None,
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
        ))
        .valid_card(TargetFilter::SelfRef)
}

/// A batched "one or more creatures you control leave the battlefield" observer.
fn batched_leave_observer() -> TriggerDefinition {
    let mut trigger = TriggerDefinition::new(TriggerMode::ChangesZoneAll)
        .origin(Zone::Battlefield)
        .valid_card(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Creature).controller(ControllerRef::You),
        ))
        .trigger_zones(vec![Zone::Battlefield])
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        ));
    trigger.batched = true;
    trigger
}

/// Adds the observer and returns (first, second-with-two-redirects, observer).
/// `redirects` gives the second creature its two competing replacements.
fn sacrifice_pair(scenario: &mut GameScenario, redirects: bool) -> (ObjectId, ObjectId, ObjectId) {
    let first = scenario.add_creature(P0, "First", 1, 1).id();
    let mut second = scenario.add_creature(P0, "Second", 1, 1);
    if redirects {
        second
            .with_replacement_definition(redirect_self(Zone::Graveyard, Zone::Exile))
            .with_replacement_definition(redirect_self(Zone::Graveyard, Zone::Hand));
    }
    let second = second.id();
    let observer = scenario
        .add_creature(P0, "Observer", 0, 0)
        .as_enchantment()
        .id();
    (first, second, observer)
}

fn install_observer(r: &mut GameRunner, observer: ObjectId) {
    r.state_mut()
        .objects
        .get_mut(&observer)
        .unwrap()
        .trigger_definitions
        .push(batched_leave_observer());
}

/// The single observer trigger's event is a stamped member of the whole group.
fn assert_observer_batch(r: &GameRunner, observer: ObjectId, first: ObjectId, second: ObjectId) {
    let observed: Vec<&GameEvent> = r
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == observer)
        .filter_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility { trigger_event, .. } => trigger_event.as_ref(),
            _ => None,
        })
        .collect();
    assert_eq!(observed.len(), 1, "exactly one observer trigger");
    let GameEvent::ZoneChanged {
        object_id, record, ..
    } = observed[0]
    else {
        panic!("the observer answers a zone change, got {:?}", observed[0]);
    };
    let other = if *object_id == first { second } else { first };
    assert!([first, second].contains(object_id));
    assert_eq!(
        record.co_departed,
        vec![other],
        "the record names the group"
    );
}

fn sacrifice_two_effect(targeted: bool) -> Effect {
    if targeted {
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        }
    } else {
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        }
    }
}

fn sacrifice_two_source(scenario: &mut GameScenario, targeted: bool) -> ObjectId {
    scenario
        .add_creature(P0, "Src", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(AbilityKind::Activated, sacrifice_two_effect(targeted)).cost(
                AbilityCost::Sacrifice(SacrificeCost::count(
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
                    2,
                )),
            ),
        )
        .id()
}

/// V21 — an activation's count-two sacrifice group, paused at a replacement
/// choice, yields ONE batched observer trigger (CR 603.2c + CR 603.10a), targeted
/// or not, and the activation is on the stack under it.
#[test]
fn v21_activation_sacrifice_group_yields_one_observer_trigger() {
    for targeted in [true, false] {
        let mut scenario = board(2);
        let source = sacrifice_two_source(&mut scenario, targeted);
        let (first, second, observer) = sacrifice_pair(&mut scenario, true);
        let target = scenario.add_creature(P1, "Target", 2, 2).id();
        let mut r = scenario.build();
        install_observer(&mut r, observer);

        activate(&mut r, source);
        if targeted {
            act(
                &mut r,
                GameAction::SelectTargets {
                    targets: vec![TargetRef::Object(target)],
                },
            );
        }
        let wf = select_cards(&mut r, vec![first, second]);
        // Reach guard: the group is paused at the replacement choice.
        assert!(matches!(wf, WaitingFor::ReplacementChoice { .. }), "{wf:?}");
        assert!(matches!(
            r.state().pending_cost_move_resume,
            Some(PendingCostMoveResume::SacrificeForCost {
                paused_at_index: 1,
                ..
            })
        ));
        assert!(r.state().stack.is_empty());

        let wf = act(&mut r, GameAction::ChooseReplacement { index: 0 });
        assert!(matches!(wf, WaitingFor::Priority { .. }), "{wf:?}");
        assert_eq!(
            stack(&r)[0],
            (Kind::Activated, source, P0),
            "targeted={targeted}"
        );
        assert_observer_batch(&r, observer, first, second);
    }
}

/// V21 control — the same targeted group with no replacement pause: one trigger.
#[test]
fn v21_reference_unpaused_group_yields_one_observer_trigger() {
    let mut scenario = board(2);
    let source = sacrifice_two_source(&mut scenario, true);
    let (first, second, observer) = sacrifice_pair(&mut scenario, false);
    let target = scenario.add_creature(P1, "Target", 2, 2).id();
    let mut r = scenario.build();
    install_observer(&mut r, observer);

    activate(&mut r, source);
    act(
        &mut r,
        GameAction::SelectTargets {
            targets: vec![TargetRef::Object(target)],
        },
    );
    let wf = select_cards(&mut r, vec![first, second]);
    assert!(matches!(wf, WaitingFor::Priority { .. }), "{wf:?}");
    assert_eq!(stack(&r)[0], (Kind::Activated, source, P0));
    assert_observer_batch(&r, observer, first, second);
}

/// CR 603.7b + CR 603.3b: an untargeted activation whose paid discard precedes
/// a count-two sacrifice paused at a replacement choice. A delayed "when this
/// dies" ability on the FIRST sacrificed creature (whose zone change was retained
/// from before the pause) triggers exactly once, in the activation's batch.
#[test]
fn v21b_delayed_observer_of_a_retained_sacrifice_fragment_triggers_once() {
    for targeted in [false, true] {
        v21b_case(targeted);
    }
}

fn v21b_case(targeted: bool) {
    use engine::types::ability::{DelayedTriggerCondition, ResolvedAbility};
    use engine::types::game_state::DelayedTrigger;

    let db = shared_card_db().expect("card db");
    let mut scenario = board(2);
    let source = scenario
        .add_creature(P0, "Src", 1, 1)
        .from_oracle_text(if targeted {
            "Discard a card, Sacrifice two creatures: This creature deals 1 damage to any target."
        } else {
            "Discard a card, Sacrifice two creatures: You gain 1 life."
        })
        .id();
    let (first, second, observer) = sacrifice_pair(&mut scenario, true);
    let card = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let marker = scenario
        .add_creature(P0, "Marker", 0, 0)
        .as_enchantment()
        .id();
    let mut r = build(scenario);
    install_observer(&mut r, observer);
    r.state_mut().delayed_triggers.push(DelayedTrigger::new(
        DelayedTriggerCondition::WhenDiesOrExiled {
            filter: TargetFilter::SpecificObject { id: first },
        },
        Box::new(ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
            vec![],
            marker,
            P0,
        )),
        P0,
        marker,
        true,
    ));

    activate(&mut r, source);
    if targeted {
        act(
            &mut r,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            },
        );
    }
    let wf = select_cards(&mut r, vec![card]);
    // Reach guard: the discard is paid and the count-two sacrifice is asked.
    assert!(matches!(wf, WaitingFor::PayCost { .. }), "{wf:?}");
    assert_eq!(r.state().objects[&card].zone, Zone::Graveyard);
    let wf = select_cards(&mut r, vec![first, second]);
    // Reach guard: the group is paused at the replacement choice.
    assert!(matches!(wf, WaitingFor::ReplacementChoice { .. }), "{wf:?}");
    assert!(r.state().stack.is_empty());

    let mut wf = act(&mut r, GameAction::ChooseReplacement { index: 0 });
    if matches!(wf, WaitingFor::OrderTriggers { .. }) {
        order_in_listed_order(&mut r);
        wf = r.state().waiting_for.clone();
    }
    assert!(matches!(wf, WaitingFor::Priority { .. }), "{wf:?}");
    assert_eq!(stack(&r)[0], (Kind::Activated, source, P0));
    assert_eq!(
        stack_triggers_from(&r, marker),
        1,
        "targeted={targeted}: the delayed ability of the retained fragment triggers once: {:?}",
        stack(&r)
    );
    assert!(
        r.state().delayed_triggers.is_empty(),
        "the one-shot delayed trigger is consumed"
    );
}

// ---------------------------------------------------------------------------
// V25: a spell keeps its paid discard's trigger when it is cancelled
// ---------------------------------------------------------------------------

fn thrill_board(swiftspear: bool) -> (GameRunner, ObjectId, ObjectId, ObjectId, Option<ObjectId>) {
    let db = shared_card_db().expect("card db");
    let mut scenario = board(2);
    let thrill = scenario.add_real_card(P0, "Thrill of Possibility", Zone::Hand, db);
    let fodder = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    scenario.add_real_card(P0, "Grizzly Bears", Zone::Library, db);
    scenario.add_real_card(P0, "Grizzly Bears", Zone::Library, db);
    let haven = scenario.add_real_card(P0, "Drake Haven", Zone::Battlefield, db);
    let swiftspear = swiftspear
        .then(|| scenario.add_real_card(P0, "Monastery Swiftspear", Zone::Battlefield, db));
    let mut r = build(scenario);
    add_mana(&mut r, P0, ManaType::Red, 3);
    (r, thrill, fodder, haven, swiftspear)
}

/// V25 — Thrill of Possibility ("As an additional cost to cast this spell,
/// discard a card") with Drake Haven and Monastery Swiftspear. A spell's
/// `CancelCast` does not return a paid discard, so Haven's trigger stays; that
/// is the spell behavior this change leaves alone (it only changes activations).
#[test]
fn v25_spell_with_a_paid_discard_across_a_manual_payment_pause() {
    // Auto leg: the unpaused reference.
    let (mut r, thrill, fodder, haven, swiftspear) = thrill_board(true);
    let swiftspear = swiftspear.unwrap();
    cast(&mut r, thrill, CastPaymentMode::Auto);
    let wf = select_cards(&mut r, vec![fodder]);
    let reference = prompt_sources(&wf, P0);
    assert_eq!(reference, sorted(vec![haven, swiftspear]));

    // Pay leg.
    let (mut r, thrill, fodder, haven, swiftspear) = thrill_board(true);
    let swiftspear = swiftspear.unwrap();
    assert!(matches!(
        cast(&mut r, thrill, CastPaymentMode::Manual),
        WaitingFor::PayCost { .. }
    ));
    let wf = select_cards(&mut r, vec![fodder]);
    assert!(matches!(wf, WaitingFor::ManaPayment { .. }), "{wf:?}");
    assert_eq!(r.state().objects[&fodder].zone, Zone::Graveyard);
    let wf = act(&mut r, GameAction::PassPriority);
    assert_eq!(prompt_sources(&wf, P0), reference);
    order_in_listed_order(&mut r);
    assert_eq!(stack(&r)[0], (Kind::Spell, thrill, P0));
    assert_eq!(stack_triggers_from(&r, haven), 1);
    assert_eq!(stack_triggers_from(&r, swiftspear), 1);

    // Cancel leg.
    let (mut r, thrill, fodder, haven, _) = thrill_board(true);
    cast(&mut r, thrill, CastPaymentMode::Manual);
    select_cards(&mut r, vec![fodder]);
    let wf = act(&mut r, GameAction::CancelCast);
    assert!(matches!(wf, WaitingFor::Priority { .. }), "{wf:?}");
    assert_eq!(r.state().objects[&thrill].zone, Zone::Hand);
    assert_eq!(r.state().objects[&fodder].zone, Zone::Graveyard);
    assert_eq!(stack(&r), vec![(Kind::Triggered, haven, P0)]);
}

// ---------------------------------------------------------------------------
// Guards: shapes that already work and must keep working
// ---------------------------------------------------------------------------

const TORMOD: &str = "Whenever one or more cards leave your graveyard, create a tapped 2/2 black Zombie creature token.
Partner (You can have two commanders if both have partner.)";

/// An untargeted activation that pauses at a replacement choice after its
/// exile cost keeps that cost's trigger in the activation's batch: Coin of Fate
/// ("{3}{W}, {T}, Exile two creature cards from your graveyard, Sacrifice this
/// artifact: ..."), two Rest in Peace competing for the sacrificed Coin.
#[test]
fn guard_untargeted_exile_cost_trigger_is_in_the_stack_push_batch() {
    let db = shared_card_db().expect("card db");
    let mut scenario = board(2);
    let coin = scenario.add_real_card(P0, "Coin of Fate", Zone::Battlefield, db);
    scenario.add_creature_to_graveyard(P0, "Graveyard Creature A", 2, 2);
    scenario.add_creature_to_graveyard(P0, "Graveyard Creature B", 3, 3);
    let tormod = scenario
        .add_creature_from_oracle(P0, "Tormod, the Desecrator", 2, 2, TORMOD)
        .id();
    let construct = scenario.add_real_card(P0, "Crackdown Construct", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Rest in Peace", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Rest in Peace", Zone::Battlefield, db);
    let mut r = build(scenario);
    add_mana(&mut r, P0, ManaType::White, 4);

    let wf = activate(&mut r, coin);
    let WaitingFor::PayCost { choices, count, .. } = wf else {
        panic!("expected the graveyard exile cost, got {wf:?}");
    };
    let wf = select_cards(&mut r, choices.iter().copied().take(count).collect());
    // Reach guard: the exile was paid and the activation is paused mid-cost.
    assert!(matches!(wf, WaitingFor::ReplacementChoice { .. }), "{wf:?}");
    assert!(r.state().stack.is_empty());

    let wf = act(&mut r, GameAction::ChooseReplacement { index: 0 });
    assert_eq!(prompt_sources(&wf, P0), sorted(vec![tormod, construct]));
    order_in_listed_order(&mut r);
    assert_eq!(stack(&r)[0], (Kind::Activated, coin, P0));
    assert_eq!(stack_triggers_from(&r, tormod), 1);
    assert_eq!(stack_triggers_from(&r, construct), 1);
    assert_eq!(r.state().stack.len(), 3);
}

/// A mana ability's sacrifice does not use the stack (CR 605.3b), so Crackdown
/// Construct, which ignores mana abilities, does not trigger, but Zulaport
/// Cutthroat still sees the sacrifice and its trigger is on the stack at the
/// first priority boundary.
#[test]
fn guard_mana_ability_sacrifice_is_not_an_announcement() {
    let db = shared_card_db().expect("card db");
    let mut scenario = board(2);
    let altar = scenario.add_real_card(P0, "Ashnod's Altar", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let construct = scenario.add_real_card(P0, "Crackdown Construct", Zone::Battlefield, db);
    let zulaport = scenario.add_real_card(P0, "Zulaport Cutthroat", Zone::Battlefield, db);
    let mut r = build(scenario);

    let wf = activate(&mut r, altar);
    assert!(
        matches!(wf, WaitingFor::PayCost { .. }),
        "reach guard: the mana ability's sacrifice is a player choice, got {wf:?}"
    );
    let wf = select_cards(&mut r, vec![bears]);
    assert_eq!(r.state().objects[&bears].zone, Zone::Graveyard);
    assert!(matches!(wf, WaitingFor::Priority { .. }), "{wf:?}");
    assert!(
        stack(&r).iter().all(|(kind, ..)| *kind != Kind::Activated),
        "a mana ability uses no stack"
    );
    assert_eq!(stack_triggers_from(&r, zulaport), 1);
    assert_eq!(stack_triggers_from(&r, construct), 0);
}
