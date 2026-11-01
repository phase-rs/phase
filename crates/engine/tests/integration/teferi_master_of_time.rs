//! CR 606.3 + CR 304.5: Teferi, Master of Time — "You may activate loyalty
//! abilities of Teferi on any player's turn any time you could cast an
//! instant." (verbatim Oracle line 1, Scryfall-verified; PR #10010).
//!
//! The line lowers to the shared `ActivateAsInstant { PaysLoyalty, None }`
//! timing permission, self-scoped — the same mode as the Wandering Emperor's
//! conditional form, without the condition and with the "of \<subject\>" axis.
//! CR 304.5 subsumes the "on any player's turn" infix: instant timing needs
//! only priority, so Teferi's abilities work on any player's turn AND with a
//! nonempty stack. The CR 606.3 once-per-permanent-per-turn cap is untouched:
//! a second activation in the same turn is still refused.
//!
//! Every test below uses Teferi's verbatim Oracle text.

use engine::ai_support::legal_actions;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCost, ControllerRef, CostCategory, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

/// Teferi, Master of Time (verbatim Oracle text, Scryfall-verified).
const TEFERI_MASTER_OF_TIME: &str = "You may activate loyalty abilities of Teferi on any player's turn any time you could cast an instant.\n+1: Draw a card, then discard a card.\n\u{2212}3: Target creature you don't control phases out. (Treat it and anything attached to it as though they don't exist until its controller's next turn.)\n\u{2212}10: Take two extra turns after this one.";

/// Synthetic control planeswalker with one printed, untargeted loyalty
/// ability — no timing permission, so CR 606.3 timing applies.
const CONTROL_WALKER: &str = "+1: You gain 1 life.";

/// Teferi, Temporal Archmage (verbatim Oracle text, Scryfall-verified) — the
/// second printed parent of the emblem, whose [−10] creates it through the
/// production resolution pipeline.
const TEFERI_TEMPORAL_ARCHMAGE: &str = "+1: Look at the top two cards of your library. Put one of them into your hand and the other on the bottom of your library.\n\u{2212}1: Untap up to four target permanents.\n\u{2212}10: You get an emblem with \"You may activate loyalty abilities of planeswalkers you control on any player's turn any time you could cast an instant.\"\nTeferi, Temporal Archmage can be your commander.";

/// Board: Teferi plus a control walker, both under P0.
fn board() -> (GameScenario, ObjectId, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let teferi = s
        .add_planeswalker_from_oracle(
            P0,
            "Teferi, Master of Time",
            "Teferi",
            3,
            TEFERI_MASTER_OF_TIME,
        )
        .id();
    let control = s
        .add_planeswalker_from_oracle(P0, "Control Walker", "Chandra", 3, CONTROL_WALKER)
        .id();
    (s, teferi, control)
}

/// Put the game in a priority window: `active` player's turn, `phase`, P0 to
/// act, empty stack; then run the layer system so statics function.
fn set_window(runner: &mut GameRunner, active: PlayerId, phase: Phase) {
    let state = runner.state_mut();
    state.active_player = active;
    state.phase = phase;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
    assert!(state.stack.is_empty(), "fixture starts with an empty stack");
    evaluate_layers(state);
}

fn plus_one_index(runner: &GameRunner, id: ObjectId) -> usize {
    runner.state().objects[&id]
        .abilities
        .iter()
        .position(|a| matches!(a.cost, Some(AbilityCost::Loyalty { amount: 1 })))
        .expect("a [+1] loyalty ability")
}

fn activation(id: ObjectId, ability_index: usize) -> GameAction {
    GameAction::ActivateAbility {
        source_id: id,
        ability_index,
        payment_mode: engine::types::game_state::CastPaymentMode::Auto,
    }
}

fn offered(runner: &GameRunner, id: ObjectId, ability_index: usize) -> bool {
    legal_actions(runner.state()).contains(&activation(id, ability_index))
}

fn loyalty(runner: &GameRunner, id: ObjectId) -> Option<u32> {
    runner.state().objects[&id].loyalty
}

/// Put a card of `core_type` directly into P0's hand.
fn add_hand_card(runner: &mut GameRunner, name: &str, core_type: CoreType) -> ObjectId {
    let state = runner.state_mut();
    let id = create_object(
        state,
        CardId(state.next_object_id),
        P0,
        name.to_string(),
        Zone::Hand,
    );
    state
        .objects
        .get_mut(&id)
        .unwrap()
        .card_types
        .core_types
        .push(core_type);
    id
}

/// Put a card of `core_type` on top of P0's library so the draw pulls it.
fn seed_library_top(runner: &mut GameRunner, name: &str, core_type: CoreType) {
    let state = runner.state_mut();
    let id = create_object(
        state,
        CardId(state.next_object_id),
        P0,
        name.to_string(),
        Zone::Library,
    );
    state
        .objects
        .get_mut(&id)
        .unwrap()
        .card_types
        .core_types
        .push(core_type);
    let player = state.players.iter_mut().find(|p| p.id == P0).unwrap();
    player.library.retain(|&oid| oid != id);
    player.library.insert(0, id);
}

/// Resolve Teferi's [+1] loot through the interactive discard, choosing
/// `chosen`, and assert the full post-resolution state.
fn resolve_loot_choosing_discard(
    runner: &mut GameRunner,
    teferi: ObjectId,
    creature: ObjectId,
    land: ObjectId,
    chosen: ObjectId,
) {
    for _ in 0..10 {
        if matches!(runner.state().waiting_for, WaitingFor::DiscardChoice { .. }) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("resolve Teferi's [+1]");
    }
    match &runner.state().waiting_for {
        WaitingFor::DiscardChoice { player, cards, .. } => {
            assert_eq!(*player, P0, "Teferi's controller chooses the discard");
            assert!(cards.contains(&creature) && cards.contains(&land));
        }
        other => panic!("expected a DiscardChoice prompt, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards {
            cards: vec![chosen],
        })
        .expect("discard the chosen card");
    assert!(
        runner.state().players[0].graveyard.contains(&chosen),
        "the chosen card must have reached the graveyard"
    );
    assert!(runner.state().stack.is_empty(), "the loot ability resolved");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "priority returns after the discard"
    );
    assert_eq!(loyalty(runner, teferi), Some(4), "loyalty cost +1 paid");
}

/// Assert the [+1] on `id` is neither offered nor accepted, and that a
/// rejected attempt changed nothing.
fn assert_plus_one_refused(runner: &mut GameRunner, id: ObjectId, why: &str) {
    let idx = plus_one_index(runner, id);
    assert!(!offered(runner, id, idx), "{why}: [+1] offered");
    let before = loyalty(runner, id);
    let stack_before = runner.state().stack.len();
    assert!(
        runner.act(activation(id, idx)).is_err(),
        "{why}: [+1] activation accepted"
    );
    assert_eq!(loyalty(runner, id), before, "{why}: loyalty changed");
    assert_eq!(
        runner.state().stack.len(),
        stack_before,
        "{why}: stack grew"
    );
}

/// T1 (positive): on the opponent's turn with priority, Teferi's [+1] is
/// offered, activates, uses the stack (CR 605.1a: a loyalty ability is never a
/// mana ability), and loots on resolution.
#[test]
fn teferi_plus_one_activates_on_opponents_turn() {
    let (s, teferi, _) = board();
    let mut r = s.build();
    set_window(&mut r, P1, Phase::DeclareAttackers);
    let creature = add_hand_card(&mut r, "Grizzly Bears", CoreType::Creature);
    let land = add_hand_card(&mut r, "Forest", CoreType::Land);
    seed_library_top(&mut r, "Island", CoreType::Land);

    let idx = plus_one_index(&r, teferi);
    assert!(
        offered(&r, teferi, idx),
        "Teferi's [+1] offered on the opponent's turn"
    );
    r.act(activation(teferi, idx))
        .expect("Teferi's [+1] accepted on the opponent's turn");
    assert_eq!(r.state().stack.len(), 1, "loyalty ability uses the stack");
    resolve_loot_choosing_discard(&mut r, teferi, creature, land, land);
}

/// T2: the CR 606.3 once-per-permanent-per-turn cap survives the permission —
///
/// after [+1] resolves on Teferi's controller's own turn, [+1] again is
/// refused on an empty stack.
#[test]
fn teferi_second_activation_same_turn_refused() {
    let (s, teferi, _) = board();
    let mut r = s.build();
    set_window(&mut r, P0, Phase::PreCombatMain);
    let creature = add_hand_card(&mut r, "Grizzly Bears", CoreType::Creature);
    let land = add_hand_card(&mut r, "Forest", CoreType::Land);
    seed_library_top(&mut r, "Island", CoreType::Land);

    let idx = plus_one_index(&r, teferi);
    assert!(
        offered(&r, teferi, idx),
        "reach-guard: [+1] offered on the controller's own turn"
    );
    r.act(activation(teferi, idx))
        .expect("reach-guard: [+1] accepted on the controller's own turn");
    resolve_loot_choosing_discard(&mut r, teferi, creature, land, creature);
    assert_plus_one_refused(&mut r, teferi, "CR 606.3 same-turn cap");
}

/// T3: the permission is Teferi's alone — the control walker's [+1] stays at
/// CR 606.3 timing (refused on the opponent's turn) in the same window where
/// Teferi's [+1] is offered.
#[test]
fn teferi_permission_does_not_extend_to_other_planeswalkers() {
    let (s, teferi, control) = board();
    let mut r = s.build();
    set_window(&mut r, P1, Phase::DeclareAttackers);
    let teferi_idx = plus_one_index(&r, teferi);
    assert!(
        offered(&r, teferi, teferi_idx),
        "reach-guard: the window is one where Teferi's permission applies"
    );
    assert_plus_one_refused(&mut r, control, "other walker on opponent's turn");
}

/// T4 (CR 304.5): instant timing needs only priority — with the control
/// walker's [+1] on the stack (nonempty stack, P0 priority), Teferi's [+1] is
/// still offered and accepted. A sorcery-timed loyalty ability would be
/// refused here (cf. the granted-loyalty stack test).
#[test]
fn teferi_plus_one_activates_with_nonempty_stack() {
    let (s, teferi, control) = board();
    let mut r = s.build();
    set_window(&mut r, P0, Phase::PreCombatMain);

    let control_idx = plus_one_index(&r, control);
    r.act(activation(control, control_idx))
        .expect("control walker activates at sorcery speed");
    assert_eq!(r.state().stack.len(), 1, "control [+1] on the stack");
    assert!(
        matches!(r.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "P0 holds priority with a nonempty stack"
    );

    let teferi_idx = plus_one_index(&r, teferi);
    assert!(
        offered(&r, teferi, teferi_idx),
        "Teferi's [+1] offered with a nonempty stack"
    );
    r.act(activation(teferi, teferi_idx))
        .expect("Teferi's [+1] accepted with a nonempty stack");
    assert_eq!(loyalty(&r, teferi), Some(4));
    assert_eq!(r.state().stack.len(), 2, "both abilities on the stack");
}

/// Board for the emblem tests: Temporal Archmage seeded at 10 loyalty so its
/// [−10] is payable, plus the control walker, both under P0.
fn emblem_board() -> (GameScenario, ObjectId, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let archmage = s
        .add_planeswalker_from_oracle(
            P0,
            "Teferi, Temporal Archmage",
            "Teferi",
            10,
            TEFERI_TEMPORAL_ARCHMAGE,
        )
        .id();
    let control = s
        .add_planeswalker_from_oracle(P0, "Control Walker", "Chandra", 3, CONTROL_WALKER)
        .id();
    (s, archmage, control)
}

fn minus_ten_index(runner: &GameRunner, id: ObjectId) -> usize {
    runner.state().objects[&id]
        .abilities
        .iter()
        .position(|a| matches!(a.cost, Some(AbilityCost::Loyalty { amount: -10 })))
        .expect("a [−10] loyalty ability")
}

/// Reach-guard: exactly one command-zone emblem carries a *functioning* parsed
/// timing permission scoped to planeswalkers its controller controls — the
/// exact source the runtime consult must read (CR 114.4). Uses the public
/// `active_static_definitions` gate stack, so this also proves the static
/// functions from the command zone rather than merely sitting on the object.
fn emblem_with_timing_permission(runner: &GameRunner) -> ObjectId {
    let state = runner.state();
    let found: Vec<ObjectId> = state
        .command_zone
        .iter()
        .copied()
        .filter(|id| {
            let obj = &state.objects[id];
            obj.is_emblem
                && obj.zone == Zone::Command
                && engine::game::functioning_abilities::active_static_definitions(state, obj).any(
                    |def| {
                        matches!(
                            def.mode,
                            StaticMode::ActivateAsInstant {
                                cost_category: CostCategory::PaysLoyalty,
                                keyword: None,
                            }
                        ) && def.affected
                            == Some(TargetFilter::Typed(
                                TypedFilter::new(TypeFilter::Planeswalker)
                                    .controller(ControllerRef::You),
                            ))
                    },
                )
        })
        .collect();
    assert_eq!(
        found.len(),
        1,
        "exactly one emblem carries the parsed timing permission"
    );
    found[0]
}

/// T5 (CR 114.4): the emblem created by Temporal Archmage's [−10] grants
/// instant timing to planeswalkers its controller controls — the control
/// walker's [+1] is offered and accepted on the opponent's turn.
#[test]
fn temporal_archmage_emblem_grants_loyalty_timing_on_opponents_turn() {
    let (s, archmage, control) = emblem_board();
    let mut r = s.build();
    set_window(&mut r, P0, Phase::PreCombatMain);

    let minus_ten = minus_ten_index(&r, archmage);
    assert!(
        offered(&r, archmage, minus_ten),
        "reach-guard: [−10] offered at sorcery speed on the controller's turn"
    );
    r.act(activation(archmage, minus_ten))
        .expect("reach-guard: [−10] accepted");
    assert_eq!(r.state().stack.len(), 1, "[−10] uses the stack");
    for _ in 0..10 {
        if r.state().stack.is_empty() {
            break;
        }
        r.act(GameAction::PassPriority).expect("resolve the [−10]");
    }
    assert!(r.state().stack.is_empty(), "[−10] resolved");
    emblem_with_timing_permission(&r);

    set_window(&mut r, P1, Phase::DeclareAttackers);
    let idx = plus_one_index(&r, control);
    assert!(
        offered(&r, control, idx),
        "control [+1] offered on the opponent's turn under the emblem"
    );
    r.act(activation(control, idx))
        .expect("control [+1] accepted on the opponent's turn under the emblem");
    assert_eq!(loyalty(&r, control), Some(4), "loyalty cost +1 paid");
}

/// T6 (control): without the emblem, the same window refuses the control
/// walker's [+1] — the permission in T5 comes from the emblem alone.
#[test]
fn loyalty_timing_refused_on_opponents_turn_without_emblem() {
    let (s, _, control) = emblem_board();
    let mut r = s.build();
    set_window(&mut r, P1, Phase::DeclareAttackers);
    assert!(
        r.state().command_zone.is_empty(),
        "reach-guard: no emblem exists"
    );
    assert_plus_one_refused(&mut r, control, "no emblem");
}
