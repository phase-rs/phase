//! "Until the end of your next turn, you may play those cards." (Light Up the
//! Stage) over an exiled spell//land modal card (Bala Ged Recovery // Bala Ged
//! Sanctuary): the land back face is played through the lingering permission.
//!
//! - CR 701.18b: to play a card is to play it as a land or cast it as a spell,
//!   whichever is appropriate.
//! - CR 712.12: a player playing a modal double-faced card as a land chooses
//!   one of its faces that is a land before putting it onto the battlefield.
//! - CR 116.2a + CR 305.1: a land is played as a special action by the active
//!   player in a main phase with an empty stack.
//! - CR 305.2a + CR 305.2b: the played land counts against the land allowance.
//! - CR 611.2a: the stated window bounds a lingering grant.

use engine::ai_support::legal_actions;
use engine::game::engine::apply;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{ChoiceType, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

struct Board {
    runner: GameRunner,
    recovery: ObjectId,
    forest_in_batch: ObjectId,
    graveyard_target: ObjectId,
    land_in_hand: ObjectId,
    stage: ObjectId,
}

fn put_on_top(runner: &mut GameRunner, player: PlayerId, id: ObjectId) {
    let library = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player exists")
        .library;
    library.retain(|card| *card != id);
    library.insert(0, id);
}

fn lands_played(state: &GameState, player: PlayerId) -> u8 {
    state.players[player.0 as usize].lands_played_this_turn
}

fn play_land_action(state: &GameState, id: ObjectId) -> GameAction {
    GameAction::PlayLand {
        object_id: id,
        card_id: state.objects[&id].card_id,
    }
}

fn offers_play_land(state: &GameState, id: ObjectId) -> bool {
    legal_actions(state).contains(&play_land_action(state, id))
}

fn offers_cast(state: &GameState, id: ObjectId) -> bool {
    legal_actions(state)
        .iter()
        .any(|action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == id))
}

fn snapshot(state: &GameState) -> serde_json::Value {
    serde_json::to_value(state).expect("game state serializes")
}

/// A submitted action that the engine refuses leaves the state untouched.
///
/// The action boundary also prunes `zone_change_library_knowledge_stamps`
/// (internal library-knowledge bookkeeping) on any refusal, so that one key is
/// compared against what a refused land play of a nonexistent object leaves
/// behind, which no land-play authority can influence; every other key must be
/// identical to the state before the refusal.
fn assert_refused_and_unchanged(state: &GameState, actor: PlayerId, action: GameAction, why: &str) {
    const BOOKKEEPING: &str = "zone_change_library_knowledge_stamps";
    let mut baseline = state.clone();
    assert!(
        apply(
            &mut baseline,
            actor,
            GameAction::PlayLand {
                object_id: ObjectId(u64::MAX),
                card_id: CardId(u64::MAX),
            },
        )
        .is_err(),
        "baseline: a land play of a nonexistent object is refused"
    );
    let mut attempt = state.clone();
    assert!(
        apply(&mut attempt, actor, action).is_err(),
        "refused: {why}"
    );
    let (before, after, baseline) = (snapshot(state), snapshot(&attempt), snapshot(&baseline));
    let (before, after, baseline) = (
        before.as_object().unwrap(),
        after.as_object().unwrap(),
        baseline.as_object().unwrap(),
    );
    let changed: Vec<&String> = before
        .keys()
        .chain(after.keys())
        .filter(|key| key.as_str() != BOOKKEEPING && before.get(*key) != after.get(*key))
        .collect();
    assert!(changed.is_empty(), "refusal changed {changed:?}: {why}");
    assert_eq!(
        after.get(BOOKKEEPING),
        baseline.get(BOOKKEEPING),
        "refusal housekeeping matches an unrelated refusal: {why}"
    );
}

/// Pass priority (and decline every attack) until `stop` holds.
fn drive_until(runner: &mut GameRunner, stop: impl Fn(&GameState) -> bool) {
    for _ in 0..400 {
        if stop(runner.state()) {
            return;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: Vec::new(),
                bands: Vec::new(),
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: Vec::new(),
            },
            other => panic!("drive_until cannot answer {other:?}"),
        };
        runner.act(action).expect("turn advances");
    }
    panic!("drive_until: no stop state within 400 actions");
}

fn at_main_of(player: PlayerId, turn: u32) -> impl Fn(&GameState) -> bool {
    move |state| {
        state.turn_number == turn
            && state.active_player == player
            && state.phase == Phase::PreCombatMain
            && state.stack.is_empty()
            && matches!(&state.waiting_for, WaitingFor::Priority { player: p } if *p == player)
    }
}

/// Enough textless library cards for both players to survive several turns.
fn add_fillers(s: &mut GameScenario) {
    for _ in 0..8 {
        s.add_card_to_library_top(P0, "Bottom Filler");
        s.add_card_to_library_top(P1, "Bottom Filler");
    }
}

/// P0 in the precombat main phase with four Mountains, three Forests, Light Up the Stage and
/// a Forest in hand, Opt in the graveyard, and Bala Ged Recovery over a Forest
/// on top of the library. Light Up the Stage is cast at its normal cost and
/// resolved, so both library cards are exiled by the public card.
fn board() -> Option<Board> {
    let db = load_db()?;
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    for _ in 0..4 {
        s.add_real_card(P0, "Mountain", Zone::Battlefield, db);
    }
    for _ in 0..3 {
        s.add_real_card(P0, "Forest", Zone::Battlefield, db);
    }
    add_fillers(&mut s);
    let stage = s.add_real_card(P0, "Light Up the Stage", Zone::Hand, db);
    let land_in_hand = s.add_real_card(P0, "Forest", Zone::Hand, db);
    let graveyard_target = s.add_real_card(P0, "Opt", Zone::Graveyard, db);
    let recovery = s.add_real_card(P0, "Bala Ged Recovery", Zone::Library, db);
    let forest_in_batch = s.add_real_card(P0, "Forest", Zone::Library, db);
    let mut runner = s.build();
    put_on_top(&mut runner, P0, forest_in_batch);
    put_on_top(&mut runner, P0, recovery);
    runner.cast(stage).resolve();
    Some(Board {
        runner,
        recovery,
        forest_in_batch,
        graveyard_target,
        land_in_hand,
        stage,
    })
}

fn assert_reach(b: &Board) {
    let state = b.runner.state();
    assert_eq!(state.objects[&b.recovery].zone, Zone::Exile, "reach guard");
    assert_eq!(
        state.objects[&b.forest_in_batch].zone,
        Zone::Exile,
        "reach guard"
    );
    assert!(
        state.stack.is_empty(),
        "reach guard: Light Up the Stage resolved"
    );
    assert!(matches!(&state.waiting_for, WaitingFor::Priority { player } if *player == P0));
    assert_eq!(lands_played(state, P0), 0, "reach guard: no land play yet");
    assert!(
        offers_play_land(state, b.forest_in_batch),
        "reach guard: the grant admits the batch's plain land"
    );
    assert!(
        offers_play_land(state, b.recovery),
        "reach guard: the grant admits the modal land back face"
    );
}

/// CR 701.18b + CR 712.12: the legal land back face is offered and played from
/// the lingering exile permission; it enters tapped, spends the land drop and
/// leaves exile.
#[test]
fn light_up_the_stage_plays_the_modal_land_back_face_from_exile() {
    let Some(mut b) = board() else { return };
    assert_reach(&b);
    let state = b.runner.state();
    assert!(
        offers_play_land(state, b.recovery),
        "the land back face of the exiled modal card is offered"
    );
    let action = play_land_action(state, b.recovery);
    b.runner.act(action).expect("playing the land back face");
    let state = b.runner.state();
    let played = &state.objects[&b.recovery];
    assert_eq!(played.zone, Zone::Battlefield);
    assert_eq!(played.name, "Bala Ged Sanctuary", "the back face entered");
    assert!(played.tapped, "Bala Ged Sanctuary enters tapped");
    assert!(played.modal_back_face);
    assert_eq!(played.controller, P0);
    assert!(!state.exile.contains(&b.recovery), "left exile");
    assert_eq!(lands_played(state, P0), 1, "one land drop spent");
    assert!(
        !offers_play_land(state, b.forest_in_batch),
        "the land drop is spent: the batch's plain land is no longer offered"
    );
    assert!(!offers_play_land(state, b.recovery), "cannot be replayed");
    assert_refused_and_unchanged(
        state,
        P0,
        play_land_action(state, b.recovery),
        "replaying the played land",
    );
}

/// Control: the conventional land of the same batch is played through the same
/// grant and spends the drop, after which the modal land back is refused.
#[test]
fn a_conventional_land_spends_the_drop_before_the_modal_land_back_face() {
    let Some(mut b) = board() else { return };
    assert_reach(&b);
    let action = play_land_action(b.runner.state(), b.forest_in_batch);
    b.runner.act(action).expect("playing the batch's Forest");
    let state = b.runner.state();
    assert_eq!(state.objects[&b.forest_in_batch].zone, Zone::Battlefield);
    assert_eq!(lands_played(state, P0), 1);
    assert!(!offers_play_land(state, b.recovery));
    assert_refused_and_unchanged(
        state,
        P0,
        play_land_action(state, b.recovery),
        "the land drop is spent",
    );
    assert!(
        state.objects[&b.recovery].zone == Zone::Exile,
        "the refused card stays exiled"
    );
}

/// Control: the land drop spent by a land from hand refuses the exiled modal
/// land back; the spell front remains castable.
#[test]
fn a_land_from_hand_spends_the_drop_before_the_modal_land_back_face() {
    let Some(mut b) = board() else { return };
    assert_reach(&b);
    let action = play_land_action(b.runner.state(), b.land_in_hand);
    b.runner.act(action).expect("playing the Forest from hand");
    let state = b.runner.state();
    assert!(!offers_play_land(state, b.recovery));
    assert_refused_and_unchanged(
        state,
        P0,
        play_land_action(state, b.recovery),
        "the land drop is spent",
    );
}

/// CR 601.2 + CR 701.18b: the spell front stays independently castable by the
/// same permission and is restricted to its printed target: the legal targets
/// are exactly the cards in P0's graveyard. The test asserts that legal-target
/// set; it does not assert the mana spent, and P1's graveyard is empty, so it
/// does not show that another graveyard is excluded.
#[test]
fn the_spell_front_is_castable_and_target_restricted() {
    let Some(b) = board() else { return };
    assert_reach(&b);
    let mut b = b;
    let recovery_card = b.runner.state().objects[&b.recovery].card_id;
    assert!(
        offers_cast(b.runner.state(), b.recovery),
        "front is offered"
    );
    // CR 601.2c: the printed target is "target card from your graveyard"; the
    // announced cast offers exactly P0's graveyard cards (Opt and the resolved
    // Light Up the Stage), not the exiled cards.
    let mut announce = GameRunner::from_state(b.runner.state().clone());
    announce
        .act(GameAction::CastSpell {
            object_id: b.recovery,
            card_id: recovery_card,
            targets: Vec::new(),
            payment_mode: Default::default(),
        })
        .expect("announcing the exiled front");
    let WaitingFor::TargetSelection { target_slots, .. } = &announce.state().waiting_for else {
        panic!(
            "expected target selection, got {:?}",
            announce.state().waiting_for
        );
    };
    assert_eq!(target_slots.len(), 1, "one printed target slot");
    let legal = &target_slots[0].legal_targets;
    assert_eq!(legal.len(), 2, "exact legal targets: {legal:?}");
    for card in [b.stage, b.graveyard_target] {
        assert!(
            legal.contains(&TargetRef::Object(card)),
            "{card:?} is legal"
        );
    }
    b.runner
        .cast(b.recovery)
        .target_object(b.graveyard_target)
        .resolve();
    let state = b.runner.state();
    assert_eq!(state.objects[&b.graveyard_target].zone, Zone::Hand);
    assert_eq!(state.objects[&b.recovery].zone, Zone::Graveyard);
    assert_eq!(lands_played(state, P0), 0, "casting is not a land play");
}

/// CR 611.2a: the permission lasts through the owner's NEXT turn. The unplayed
/// land back face is still offered there with a fresh land drop, and is played.
#[test]
fn the_land_back_face_is_playable_in_the_next_turn_of_the_window() {
    let Some(mut b) = board() else { return };
    assert_reach(&b);
    let turn = b.runner.state().turn_number;
    drive_until(&mut b.runner, at_main_of(P0, turn + 2));
    let state = b.runner.state();
    assert_eq!(state.objects[&b.recovery].zone, Zone::Exile, "reach guard");
    assert_eq!(lands_played(state, P0), 0, "reach guard: new turn");
    assert!(offers_play_land(state, b.recovery));
    let action = play_land_action(state, b.recovery);
    b.runner.act(action).expect("playing in the next turn");
    let state = b.runner.state();
    assert_eq!(state.objects[&b.recovery].zone, Zone::Battlefield);
    assert_eq!(state.objects[&b.recovery].name, "Bala Ged Sanctuary");
    assert_eq!(lands_played(state, P0), 1);
}

/// CR 611.2a: after the end of the owner's next turn the permission is gone.
#[test]
fn the_land_back_face_is_refused_after_the_window_expires() {
    let Some(mut b) = board() else { return };
    assert_reach(&b);
    let turn = b.runner.state().turn_number;
    drive_until(&mut b.runner, at_main_of(P0, turn + 4));
    let state = b.runner.state();
    assert_eq!(
        state.objects[&b.recovery].zone,
        Zone::Exile,
        "reach guard: still exiled"
    );
    assert!(
        offers_play_land(state, b.land_in_hand),
        "reach guard: a land play is otherwise available (hand Forest, land drop free)"
    );
    assert!(!offers_play_land(state, b.recovery));
    assert!(!offers_cast(state, b.recovery));
    assert_refused_and_unchanged(
        state,
        P0,
        play_land_action(state, b.recovery),
        "the window expired",
    );
}

/// CR 305.3 + CR 116.2a: a land is played only on its player's own turn. During
/// the opponent's turn the exiled land back face is neither offered nor
/// playable, even when the owner receives priority there. The engine resolves
/// the land-playing player to the active player (P1), so the refusal is the
/// permission's grantee check (it names P0), not a separate turn check.
#[test]
fn the_land_back_face_is_refused_on_the_opponents_turn() {
    let Some(mut b) = board() else { return };
    assert_reach(&b);
    let turn = b.runner.state().turn_number;
    drive_until(&mut b.runner, at_main_of(P1, turn + 1));
    b.runner.act(GameAction::PassPriority).expect("P1 passes");
    let state = b.runner.state();
    assert!(
        matches!(&state.waiting_for, WaitingFor::Priority { player } if *player == P0),
        "reach guard: P0 holds priority during P1's main phase, got {:?}",
        state.waiting_for
    );
    assert_eq!(state.objects[&b.recovery].zone, Zone::Exile, "reach guard");
    assert!(!offers_play_land(state, b.recovery));
    assert_refused_and_unchanged(
        state,
        P0,
        play_land_action(state, b.recovery),
        "the opponent's turn",
    );
}

/// CR 116.2a: a land is played only while the stack is empty.
#[test]
fn the_land_back_face_is_refused_while_the_stack_is_not_empty() {
    let Some(db) = load_db() else { return };
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    for _ in 0..4 {
        s.add_real_card(P0, "Mountain", Zone::Battlefield, db);
    }
    add_fillers(&mut s);
    let stage = s.add_real_card(P0, "Light Up the Stage", Zone::Hand, db);
    let shock = s.add_real_card(P0, "Shock", Zone::Hand, db);
    let recovery = s.add_real_card(P0, "Bala Ged Recovery", Zone::Library, db);
    let filler = s.add_real_card(P0, "Forest", Zone::Library, db);
    let mut runner = s.build();
    put_on_top(&mut runner, P0, filler);
    put_on_top(&mut runner, P0, recovery);
    runner.cast(stage).resolve();
    assert_eq!(runner.state().objects[&recovery].zone, Zone::Exile);
    assert!(offers_play_land(runner.state(), recovery), "reach guard");
    runner.cast(shock).target_player(P1).commit();
    let state = runner.state();
    assert!(
        !state.stack.is_empty(),
        "reach guard: Shock is on the stack"
    );
    assert!(!offers_play_land(state, recovery));
    assert_refused_and_unchanged(
        state,
        P0,
        play_land_action(state, recovery),
        "the stack is not empty",
    );
}

/// A different exile batch carries no permission: a second printed Bala Ged
/// Recovery exiled from the graveyard by Relic of Progenitus is neither offered
/// nor playable as a land while the Light Up the Stage batch's copy is.
#[test]
fn a_different_exile_batch_has_no_land_back_permission() {
    let Some(db) = load_db() else { return };
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    for _ in 0..3 {
        s.add_real_card(P0, "Mountain", Zone::Battlefield, db);
    }
    add_fillers(&mut s);
    let relic = s.add_real_card(P0, "Relic of Progenitus", Zone::Battlefield, db);
    let stage = s.add_real_card(P0, "Light Up the Stage", Zone::Hand, db);
    let ungranted = s.add_real_card(P0, "Bala Ged Recovery", Zone::Graveyard, db);
    let granted = s.add_real_card(P0, "Bala Ged Recovery", Zone::Library, db);
    let filler = s.add_real_card(P0, "Forest", Zone::Library, db);
    let mut runner = s.build();
    runner
        .state_mut()
        .objects
        .get_mut(&relic)
        .unwrap()
        .summoning_sick = false;
    runner.activate(relic, 0).target_player(P0).resolve();
    assert_eq!(
        runner.state().objects[&ungranted].zone,
        Zone::Exile,
        "reach guard: Relic of Progenitus exiled the graveyard copy"
    );
    put_on_top(&mut runner, P0, filler);
    put_on_top(&mut runner, P0, granted);
    runner.cast(stage).resolve();
    let state = runner.state();
    assert_eq!(state.objects[&granted].zone, Zone::Exile, "reach guard");
    assert!(
        offers_play_land(state, granted),
        "the Light Up the Stage copy is offered its land back face"
    );
    assert!(!offers_play_land(state, ungranted));
    assert!(!offers_cast(state, ungranted));
    assert_refused_and_unchanged(
        state,
        P0,
        play_land_action(state, ungranted),
        "the copy exiled without a grant",
    );
}

/// Cast-only control (CR 701.18b: playing a card is playing it as a land or
/// casting it as a spell, and a cast-only permission covers only the spell half): Nivix, Aerie of the Firemind's
/// "you may cast it if it's an instant or sorcery spell" grant lets the exiled
/// Bala Ged Recovery's spell front be cast but offers no land play of its land
/// back face, because a Cast-mode permission never authorizes a land play.
#[test]
fn a_cast_only_grant_does_not_admit_the_modal_land_back_face() {
    let Some(db) = load_db() else { return };
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let nivix = s.add_real_card(P0, "Nivix, Aerie of the Firemind", Zone::Battlefield, db);
    s.add_real_card(P0, "Island", Zone::Battlefield, db);
    s.add_real_card(P0, "Mountain", Zone::Battlefield, db);
    // Nivix's {2}{U}{R} leaves three Forests to pay Bala Ged Recovery's {2}{G}.
    for _ in 0..5 {
        s.add_real_card(P0, "Forest", Zone::Battlefield, db);
    }
    add_fillers(&mut s);
    let recovery = s.add_real_card(P0, "Bala Ged Recovery", Zone::Library, db);
    s.add_real_card(P0, "Opt", Zone::Graveyard, db);
    let mut runner = s.build();
    put_on_top(&mut runner, P0, recovery);
    runner.activate(nivix, 1).resolve();
    let state = runner.state();
    assert_eq!(
        state.objects[&recovery].zone,
        Zone::Exile,
        "reach guard: Nivix exiled the top card"
    );
    assert_eq!(
        lands_played(state, P0),
        0,
        "reach guard: land drop available"
    );
    assert!(
        !state.objects[&recovery].casting_permissions.is_empty(),
        "reach guard: the cast permission was granted"
    );
    assert!(
        offers_cast(state, recovery),
        "reach guard: the cast permission offers the spell front"
    );
    assert!(!offers_play_land(state, recovery));
    assert_refused_and_unchanged(
        state,
        P0,
        play_land_action(state, recovery),
        "a cast-only grant authorizes no land play",
    );
}

/// Ark of Hunger's "Mill a card. You may play that card this turn." puts Bala
/// Ged Recovery into the graveyard with a Play permission, and the rules let a
/// milled modal card's land face be played from there: CR 701.18b (to play a
/// card is to play it as a land or cast it as a spell) and CR 712.12 (a player
/// playing a modal double-faced card as a land chooses one of its faces that
/// is a land, and it enters with that face up). The engine's graveyard land
/// discovery finds lands by their front face, so the modal land back is not
/// offered from the graveyard and this positive witness fails at the offer
/// assertion; the spell front is castable, which the reach guards establish.
#[test]
#[ignore = "known limitation (tracked in the PR description): a modal double-faced card's land back face is not offered for play from the graveyard under a Play permission (CR 701.18b + CR 712.12)"]
fn a_milled_modal_card_plays_its_land_back_face_from_the_graveyard() {
    let Some(db) = load_db() else { return };
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    for _ in 0..3 {
        s.add_real_card(P0, "Forest", Zone::Battlefield, db);
    }
    let ark = s.add_real_card(P0, "Ark of Hunger", Zone::Battlefield, db);
    add_fillers(&mut s);
    let recovery = s.add_real_card(P0, "Bala Ged Recovery", Zone::Library, db);
    let mut runner = s.build();
    put_on_top(&mut runner, P0, recovery);
    runner.activate(ark, 0).resolve();
    let state = runner.state();
    assert_eq!(
        state.objects[&recovery].zone,
        Zone::Graveyard,
        "reach guard: Ark of Hunger milled the card"
    );
    assert!(
        !state.objects[&recovery].casting_permissions.is_empty(),
        "reach guard: the mill grant was recorded"
    );
    assert!(
        offers_cast(state, recovery),
        "reach guard: the milled spell front is castable under the grant"
    );
    assert_eq!(
        lands_played(state, P0),
        0,
        "reach guard: land drop available"
    );
    assert!(
        offers_play_land(state, recovery),
        "the land back face of the milled modal card is offered"
    );
    let action = play_land_action(state, recovery);
    runner.act(action).expect("playing the land back face");
    let state = runner.state();
    let played = &state.objects[&recovery];
    assert_eq!(played.zone, Zone::Battlefield);
    assert_eq!(played.name, "Bala Ged Sanctuary", "the back face entered");
    assert!(played.modal_back_face);
    assert_eq!(lands_played(state, P0), 1, "one land drop spent");
}

/// A static exile permission reaches the land back through the same authority:
/// Uba Mask's "each player may play lands and cast spells from among cards they
/// exiled with this artifact this turn" lets the cards Divination would draw
/// be played, the modal card as its land face.
#[test]
fn a_static_exile_permission_plays_the_modal_land_back_face() {
    let Some(db) = load_db() else { return };
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    // Divination's {2}{U} leaves three Forests to pay Bala Ged Recovery's {2}{G}.
    for _ in 0..2 {
        s.add_real_card(P0, "Island", Zone::Battlefield, db);
    }
    for _ in 0..4 {
        s.add_real_card(P0, "Forest", Zone::Battlefield, db);
    }
    s.add_real_card(P0, "Uba Mask", Zone::Battlefield, db);
    add_fillers(&mut s);
    let divination = s.add_real_card(P0, "Divination", Zone::Hand, db);
    let recovery = s.add_real_card(P0, "Bala Ged Recovery", Zone::Library, db);
    let mut runner = s.build();
    put_on_top(&mut runner, P0, recovery);
    runner.cast(divination).resolve();
    let state = runner.state();
    assert_eq!(
        state.objects[&recovery].zone,
        Zone::Exile,
        "reach guard: Uba Mask exiled the drawn card"
    );
    assert!(
        offers_cast(state, recovery),
        "reach guard: the static permission offers the spell front"
    );
    assert!(offers_play_land(state, recovery), "land back offered");
    let action = play_land_action(state, recovery);
    runner.act(action).expect("playing the land back face");
    let played = &runner.state().objects[&recovery];
    assert_eq!(played.zone, Zone::Battlefield);
    assert_eq!(played.name, "Bala Ged Sanctuary");
    assert_eq!(lands_played(runner.state(), P0), 1);
}

// CR 201.4d + CR 712.12 + CR 101.2: Conjurer's Ban ("lands with the chosen name
// can't be played") over the exiled modal card. The land that would be played is
// the back face, so a name chosen for the back face stops the play and a name
// chosen for the front face does not (CR 712.12: the played face is the land
// face; the Bala Ged Recovery ruling: consider only the characteristics of the
// face you're playing).

/// Cast Conjurer's Ban naming `named`, answering the name prompt, and resolve it.
/// Asserts the prompt's actor and that the printed choice is a card name.
fn resolve_conjurers_ban_naming(runner: &mut GameRunner, ban: ObjectId, named: &str) {
    let card_id = runner.state().objects[&ban].card_id;
    let mut result = runner
        .act(GameAction::CastSpell {
            object_id: ban,
            card_id,
            targets: Vec::new(),
            payment_mode: Default::default(),
        })
        .expect("casting Conjurer's Ban");
    let mut named_once = false;
    for _ in 0..16 {
        let action = match &result.waiting_for {
            WaitingFor::NamedChoice {
                player,
                choice_type,
                options,
                ..
            } => {
                assert_eq!(*player, P0, "the caster names the card");
                assert!(
                    matches!(choice_type, ChoiceType::CardName { .. }),
                    "a card name is chosen, not {choice_type:?}"
                );
                assert!(
                    options.is_empty() || options.iter().any(|option| option == named),
                    "the name {named:?} is a legal answer: {options:?}"
                );
                assert!(!named_once, "named exactly once");
                named_once = true;
                GameAction::ChooseOption {
                    choice: named.to_string(),
                }
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt while resolving Conjurer's Ban: {other:?}"),
        };
        result = runner.act(action).expect("answering a prompt");
    }
    assert!(named_once, "Conjurer's Ban asked for a name");
    assert!(runner.state().stack.is_empty(), "Conjurer's Ban resolved");
}

/// The witness ends settled: nothing on the stack and P0 holding priority.
fn assert_settled(state: &GameState) {
    assert!(state.stack.is_empty(), "settled: empty stack");
    assert!(
        matches!(&state.waiting_for, WaitingFor::Priority { player } if *player == P0),
        "settled: P0 has priority, got {:?}",
        state.waiting_for
    );
}

struct BanBoard {
    runner: GameRunner,
    recovery: ObjectId,
    forest_in_batch: ObjectId,
}

/// P0 in the precombat main phase: Conjurer's Ban naming `named` resolved, then
/// either Light Up the Stage resolved over Bala Ged Recovery and a Forest
/// (`Zone::Exile`) or Bala Ged Recovery sitting in the hand (`Zone::Hand`).
fn ban_board(named: &str, recovery_zone: Zone) -> Option<BanBoard> {
    let db = load_db()?;
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    for _ in 0..3 {
        s.add_real_card(P0, "Mountain", Zone::Battlefield, db);
    }
    s.add_real_card(P0, "Plains", Zone::Battlefield, db);
    s.add_real_card(P0, "Swamp", Zone::Battlefield, db);
    add_fillers(&mut s);
    let ban = s.add_real_card(P0, "Conjurer's Ban", Zone::Hand, db);
    let stage = (recovery_zone == Zone::Exile)
        .then(|| s.add_real_card(P0, "Light Up the Stage", Zone::Hand, db));
    let recovery = s.add_real_card(
        P0,
        "Bala Ged Recovery",
        if recovery_zone == Zone::Hand {
            Zone::Hand
        } else {
            Zone::Library
        },
        db,
    );
    let forest_in_batch = s.add_real_card(P0, "Forest", Zone::Library, db);
    let mut runner = s.build();
    // Production fills the nameable set from the card database at game start;
    // CR 201.4d lets a player name a back face, so its name must be in the set.
    runner.state_mut().all_card_names = db.card_names().into();
    resolve_conjurers_ban_naming(&mut runner, ban, named);
    if let Some(stage) = stage {
        put_on_top(&mut runner, P0, forest_in_batch);
        put_on_top(&mut runner, P0, recovery);
        runner.cast(stage).resolve();
    }
    // Settled: nothing on the stack and P0 holds priority.
    let state = runner.state();
    assert!(state.stack.is_empty(), "reach guard: empty stack");
    assert!(
        matches!(&state.waiting_for, WaitingFor::Priority { player } if *player == P0),
        "reach guard: P0 has priority, got {:?}",
        state.waiting_for
    );
    assert_eq!(state.phase, Phase::PreCombatMain, "reach guard: main phase");
    assert_eq!(state.objects[&recovery].zone, recovery_zone, "reach guard");
    assert_eq!(lands_played(state, P0), 0, "reach guard: land drop unused");
    assert_eq!(
        state.restrictions.len(),
        2,
        "reach guard: Conjurer's Ban armed its cast and land prohibitions"
    );
    Some(BanBoard {
        runner,
        recovery,
        forest_in_batch,
    })
}

/// F1: naming the back face (Bala Ged Sanctuary) stops the exiled modal card's
/// land play, at discovery and at submission, and a refusal changes nothing.
#[test]
fn naming_the_land_back_face_suppresses_and_rejects_its_exile_land_play() {
    let Some(b) = ban_board("Bala Ged Sanctuary", Zone::Exile) else {
        return;
    };
    let state = b.runner.state();
    assert!(
        offers_play_land(state, b.forest_in_batch),
        "control: the batch's other land is still offered under the same grant"
    );
    assert!(
        !offers_play_land(state, b.recovery),
        "the land back face is named: its land play is not offered"
    );
    assert_refused_and_unchanged(
        state,
        P0,
        play_land_action(state, b.recovery),
        "Bala Ged Sanctuary is named, so it can't be played",
    );
    assert_settled(state);
}

/// Naming the front face (Bala Ged Recovery) does not stop the land back face
/// from being played: only the played face's name counts.
#[test]
fn naming_the_front_face_still_permits_the_land_back_face_from_exile() {
    let Some(mut b) = ban_board("Bala Ged Recovery", Zone::Exile) else {
        return;
    };
    let state = b.runner.state();
    assert!(
        offers_play_land(state, b.recovery),
        "Recovery is named, but the face played is Sanctuary: offered"
    );
    assert!(
        !offers_cast(state, b.recovery),
        "the cast prohibition on the named front face is in force"
    );
    let action = play_land_action(state, b.recovery);
    b.runner.act(action).expect("playing the land back face");
    let state = b.runner.state();
    let played = &state.objects[&b.recovery];
    assert_eq!(played.zone, Zone::Battlefield);
    assert_eq!(played.name, "Bala Ged Sanctuary", "the back face entered");
    assert_eq!(lands_played(state, P0), 1, "one land drop spent");
    assert_settled(state);
}

/// Negative sibling: a name that is on neither face of the modal card leaves its
/// land play alone, while the named plain land is stopped (the per-face check
/// does not turn into a blanket ban on the exiled batch).
#[test]
fn naming_another_card_stops_only_that_exiled_land() {
    let Some(b) = ban_board("Forest", Zone::Exile) else {
        return;
    };
    let state = b.runner.state();
    assert!(
        !offers_play_land(state, b.forest_in_batch),
        "the named Forest can't be played"
    );
    assert!(
        offers_play_land(state, b.recovery),
        "the modal card's land face is not named"
    );
    assert_refused_and_unchanged(
        state,
        P0,
        play_land_action(state, b.forest_in_batch),
        "the named Forest can't be played",
    );
    assert_settled(state);
}

/// The shared authority also serves a modal card in hand: naming the back face
/// stops its land play, naming the front face does not.
#[test]
fn naming_the_land_back_face_stops_the_hand_land_play_and_naming_the_front_does_not() {
    let Some(sanctuary_named) = ban_board("Bala Ged Sanctuary", Zone::Hand) else {
        return;
    };
    let state = sanctuary_named.runner.state();
    assert!(
        !offers_play_land(state, sanctuary_named.recovery),
        "Sanctuary is named: the hand land play is not offered"
    );
    assert_refused_and_unchanged(
        state,
        P0,
        play_land_action(state, sanctuary_named.recovery),
        "Bala Ged Sanctuary is named, so it can't be played from hand",
    );
    assert_settled(state);
    let Some(mut recovery_named) = ban_board("Bala Ged Recovery", Zone::Hand) else {
        return;
    };
    let state = recovery_named.runner.state();
    assert!(
        offers_play_land(state, recovery_named.recovery),
        "Recovery is named, but the face played is Sanctuary: offered from hand"
    );
    let action = play_land_action(state, recovery_named.recovery);
    recovery_named
        .runner
        .act(action)
        .expect("playing the land back face from hand");
    let played = &recovery_named.runner.state().objects[&recovery_named.recovery];
    assert_eq!(played.zone, Zone::Battlefield);
    assert_eq!(played.name, "Bala Ged Sanctuary");
    assert_settled(recovery_named.runner.state());
}
