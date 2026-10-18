//! CR 606.2 + CR 606.3: a loyalty ability granted through quoted text is a
//! loyalty ability, so it carries the same activation timing as a printed one —
//! "any time they have priority and the stack is empty during a main phase of
//! their turn" — and shares the once-per-permanent-per-turn cap (issue #9936).
//!
//! Way of the Pyromancer grants `"[+1]: Add {R}."` to every planeswalker its
//! controller controls. Before the fix the granted ability carried no timing
//! restriction and could be activated on an opponent's turn, during combat, or
//! with a nonempty stack. Every real card below uses its verbatim Oracle text.

use engine::ai_support::legal_actions;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityCost, Effect};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// Way of the Pyromancer (verbatim Oracle text).
const WAY_OF_THE_PYROMANCER: &str = "When Way of the Pyromancer enters, empower Jace 2. (Put two loyalty counters on a Jace token you control. If you don't control one, first create a blue Jace planeswalker token with \"[−1]: Surveil 1\" and \"[−3]: Draw a card.\")\nPlaneswalkers you control have \"[+1]: Add {R}.\"";

/// The Wandering Emperor (verbatim Oracle text).
const THE_WANDERING_EMPEROR: &str = "Flash\nAs long as The Wandering Emperor entered this turn, you may activate her loyalty abilities any time you could cast an instant.\n[+1]: Put a +1/+1 counter on up to one target creature. It gains first strike until end of turn.\n[−1]: Create a 2/2 white Samurai creature token with vigilance.\n[−2]: Exile target tapped creature. You gain 2 life.";

/// Synthetic host planeswalker with one printed, untargeted loyalty ability —
/// a control for the printed path and the per-permanent cap.
const HOST_WALKER: &str = "[+2]: You gain 2 life.";

/// Board: Way of the Pyromancer plus `hosts` copies of the synthetic host
/// walker, all under P0, at P0's precombat main with an empty stack.
fn board(hosts: usize) -> (GameScenario, Vec<ObjectId>) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_enchantment_from_oracle(P0, "Way of the Pyromancer", WAY_OF_THE_PYROMANCER);
    let ids = (0..hosts)
        .map(|i| {
            s.add_planeswalker_from_oracle(
                P0,
                &format!("Host Walker {i}"),
                "Chandra",
                3,
                HOST_WALKER,
            )
            .id()
        })
        .collect();
    (s, ids)
}

/// Put the game in a priority window: `active` player's turn, `phase`, P0 to
/// act, empty stack; then run the layer system so the grant lands.
fn set_window(runner: &mut GameRunner, active: PlayerId, phase: Phase) {
    let state = runner.state_mut();
    state.active_player = active;
    state.phase = phase;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
    assert!(state.stack.is_empty(), "fixture starts with an empty stack");
    evaluate_layers(state);
}

fn index_where(runner: &GameRunner, id: ObjectId, amount: i32, mana: bool) -> Option<usize> {
    runner.state().objects[&id].abilities.iter().position(|a| {
        matches!(a.cost, Some(AbilityCost::Loyalty { amount: n }) if n == amount)
            && matches!(*a.effect, Effect::Mana { .. }) == mana
    })
}

/// Index of the granted `[+1]: Add {R}.` on `id`, with a reach guard proving it
/// is the layer-6 granted ability (not a printed one).
fn granted_index(runner: &GameRunner, id: ObjectId) -> usize {
    let idx = index_where(runner, id, 1, true)
        .expect("reach-guard: Way of the Pyromancer granted [+1]: Add {R}.");
    let from = runner.state().objects[&id]
        .granted_abilities_from
        .expect("reach-guard: object carries granted abilities");
    assert!(idx >= from, "reach-guard: [+1] is a granted ability");
    idx
}

fn printed_plus_two(runner: &GameRunner, id: ObjectId) -> usize {
    index_where(runner, id, 2, false).expect("printed [+2]")
}

fn activation(id: ObjectId, ability_index: usize) -> GameAction {
    GameAction::ActivateAbility {
        source_id: id,
        ability_index,
    }
}

fn offered(runner: &GameRunner, id: ObjectId, ability_index: usize) -> bool {
    legal_actions(runner.state()).contains(&activation(id, ability_index))
}

fn loyalty(runner: &GameRunner, id: ObjectId) -> Option<u32> {
    runner.state().objects[&id].loyalty
}

fn red(runner: &GameRunner) -> usize {
    runner.state().players[P0.0 as usize]
        .mana_pool
        .count_color(ManaType::Red)
}

/// Assert the granted [+1] on `id` is neither offered nor accepted, and that a
/// rejected attempt changed nothing.
fn assert_granted_refused(runner: &mut GameRunner, id: ObjectId, why: &str) {
    let idx = granted_index(runner, id);
    assert!(!offered(runner, id, idx), "{why}: granted [+1] offered");
    let before = loyalty(runner, id);
    let red_before = red(runner);
    let stack_before = runner.state().stack.len();
    assert!(
        runner.act(activation(id, idx)).is_err(),
        "{why}: granted [+1] activation accepted"
    );
    assert_eq!(loyalty(runner, id), before, "{why}: loyalty changed");
    assert_eq!(red(runner), red_before, "{why}: red mana added");
    assert_eq!(
        runner.state().stack.len(),
        stack_before,
        "{why}: stack grew"
    );
}

/// T1 (positive): own precombat main, empty stack — the granted [+1] is
/// offered, uses the stack (CR 605.1a: a loyalty ability is never a mana
/// ability), and adds {R} on resolution; then the per-permanent cap
/// (CR 606.3) blocks the printed [+2].
#[test]
fn granted_loyalty_ability_activates_in_own_main_phase() {
    let (s, ids) = board(1);
    let host = ids[0];
    let mut r = s.build();
    set_window(&mut r, P0, Phase::PreCombatMain);
    let idx = granted_index(&r, host);
    assert!(offered(&r, host, idx), "granted [+1] offered in own main");
    r.act(activation(host, idx))
        .expect("granted [+1] accepted in own main");
    assert_eq!(loyalty(&r, host), Some(4), "loyalty cost +1 paid");
    assert_eq!(r.state().stack.len(), 1, "loyalty ability uses the stack");
    assert_eq!(red(&r), 0, "no mana before resolution");
    r.resolve_top();
    assert!(r.state().stack.is_empty());
    assert_eq!(red(&r), 1, "resolution adds {{R}}");
    let plus_two = printed_plus_two(&r, host);
    assert!(
        !offered(&r, host, plus_two),
        "CR 606.3: one loyalty activation per permanent per turn"
    );
}

/// T2: opponent's turn — neither the granted [+1] nor the printed [+2] may be
/// activated.
#[test]
fn granted_loyalty_ability_not_activatable_on_opponents_turn() {
    let (s, ids) = board(1);
    let host = ids[0];
    let mut r = s.build();
    set_window(&mut r, P1, Phase::PreCombatMain);
    let plus_two = printed_plus_two(&r, host);
    assert!(
        !offered(&r, host, plus_two),
        "control: printed [+2] not offered on opponent's turn"
    );
    assert_granted_refused(&mut r, host, "opponent's turn");
}

/// T3: own turn but beginning of combat — not a main phase.
#[test]
fn granted_loyalty_ability_not_activatable_outside_main_phase() {
    let (s, ids) = board(1);
    let host = ids[0];
    let mut r = s.build();
    set_window(&mut r, P0, Phase::BeginCombat);
    let plus_two = printed_plus_two(&r, host);
    assert!(
        !offered(&r, host, plus_two),
        "control: printed [+2] not offered in combat"
    );
    assert_granted_refused(&mut r, host, "beginning of combat");
}

/// T4: with a loyalty ability of host A on the stack, host B's granted [+1]
/// may not be activated (nonempty stack); once the stack empties it may.
#[test]
fn granted_loyalty_ability_requires_empty_stack() {
    let (s, ids) = board(2);
    let (a, b) = (ids[0], ids[1]);
    let mut r = s.build();
    set_window(&mut r, P0, Phase::PreCombatMain);
    let a_idx = granted_index(&r, a);
    r.act(activation(a, a_idx)).expect("host A activates");
    assert_eq!(r.state().stack.len(), 1, "host A's ability on the stack");
    assert!(
        matches!(r.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "P0 holds priority with a nonempty stack"
    );
    assert_granted_refused(&mut r, b, "nonempty stack");
    r.resolve_top();
    assert!(r.state().stack.is_empty(), "stack drained");
    let b_idx = granted_index(&r, b);
    assert!(
        offered(&r, b, b_idx),
        "host B offered once the stack empties"
    );
    r.act(activation(b, b_idx))
        .expect("host B accepted once the stack empties");
    assert_eq!(loyalty(&r, b), Some(4));
}

/// The Wandering Emperor under P0, entered this turn if `entered_this_turn` (else the previous turn), with the game
/// at P1's declare-attackers step and P0 holding priority.
fn emperor_board(entered_this_turn: bool) -> (GameRunner, ObjectId, ObjectId) {
    let (mut s, ids) = board(1);
    let host = ids[0];
    let emperor = s
        .add_planeswalker_from_oracle(P0, "The Wandering Emperor", "", 3, THE_WANDERING_EMPEROR)
        .id();
    let mut r = s.build();
    {
        let state = r.state_mut();
        let turn = state.turn_number;
        let obj = state.objects.get_mut(&emperor).unwrap();
        // The Wandering Emperor has no planeswalker subtype.
        obj.card_types.subtypes.clear();
        obj.base_card_types = obj.card_types.clone();
        obj.entered_battlefield_turn = Some(if entered_this_turn { turn } else { turn - 1 });
    }
    set_window(&mut r, P1, Phase::DeclareAttackers);
    (r, emperor, host)
}

/// T5a (preservation): The Wandering Emperor's ActivateAsInstant permission
/// covers her granted [+1] too — it is one of "her loyalty abilities".
#[test]
fn wandering_emperor_flash_permission_covers_granted_loyalty_ability() {
    let (mut r, emperor, _) = emperor_board(true);
    let idx = granted_index(&r, emperor);
    assert!(
        offered(&r, emperor, idx),
        "Emperor's granted [+1] offered at instant speed the turn she entered"
    );
    r.act(activation(emperor, idx))
        .expect("Emperor's granted [+1] accepted");
    assert_eq!(loyalty(&r, emperor), Some(4));
}

/// T5b: the Emperor's permission is hers alone — another planeswalker's
/// granted [+1] stays at CR 606.3 timing in the same window.
#[test]
fn wandering_emperor_permission_does_not_extend_to_other_planeswalkers() {
    let (mut r, emperor, host) = emperor_board(true);
    let emperor_idx = granted_index(&r, emperor);
    assert!(
        offered(&r, emperor, emperor_idx),
        "reach-guard: the window is one where the Emperor's permission applies"
    );
    assert_granted_refused(&mut r, host, "other walker on opponent's turn");
}

/// T5c: the Emperor entered on a previous turn — her permission is off, so her
/// granted [+1] is refused on the opponent's turn.
#[test]
fn wandering_emperor_granted_loyalty_ability_refused_when_not_entered_this_turn() {
    let (mut r, emperor, _) = emperor_board(false);
    assert_granted_refused(&mut r, emperor, "Emperor entered last turn");
}
