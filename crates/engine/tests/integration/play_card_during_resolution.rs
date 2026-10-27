//! A free, no-duration "you may play [the exiled card / that card] without
//! paying its mana cost" plays the card AS THE ABILITY RESOLVES.
//!
//! - CR 701.18b: to play a card means to play it as a land or to cast it as a
//!   spell, whichever is appropriate.
//! - CR 608.2g: a spell an effect allows to be cast during resolution is cast
//!   then, ignoring timing permissions.
//! - CR 305.2a + CR 305.2b + CR 305.3: a land played during resolution counts
//!   against the turn's land allowance, and the instruction is ignored when the
//!   allowance is used up or it isn't the player's turn.
//! - Djinn of Wishes ruling (2018-07-13): "If you wish to play the exiled card,
//!   you must play it while the last ability of Djinn of Wishes is resolving.
//!   You can't play it later in the turn." Explicit durations (Mind's Desire,
//!   CR 611.2a) keep their lingering permission.
//! - CR 712.12 bounded fallback: a land//land modal card keeps the lingering
//!   grant, whose `PlayLand` opens the land-face choice; a spell//land modal
//!   card is cast by its front face (CR 712.8a). A resolution-owned land-face
//!   election is a tracked follow-up.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{CastingPermission, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card::LayoutKind;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{ExileLinkKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const SHELLDOCK_ISLE: &str = "Hideaway 4 (When this land enters, look at the top four cards of your library, exile one face down, then put the rest on the bottom in a random order.)\nThis land enters tapped.\n{T}: Add {U}.\n{U}, {T}: You may play the exiled card without paying its mana cost if a library has twenty or fewer cards in it.";
const MOSSWORT_BRIDGE: &str = "Hideaway 4 (When this land enters, look at the top four cards of your library, exile one face down, then put the rest on the bottom in a random order.)\nThis land enters tapped.\n{T}: Add {G}.\n{G}, {T}: You may play the exiled card without paying its mana cost if creatures you control have total power 10 or greater.";
const DJINN_OF_WISHES: &str = "Flying\nThis creature enters with three wish counters on it.\n{2}{U}{U}, Remove a wish counter from this creature: Reveal the top card of your library. You may play that card without paying its mana cost. If you don't, exile it.";
const MINDS_DESIRE: &str = "Shuffle your library. Then exile the top card of your library. Until end of turn, you may play that card without paying its mana cost.\nStorm (When you cast this spell, copy it for each spell cast before it this turn.)";
const HORN_OF_GREED: &str = "Whenever a player plays a land, that player draws a card.";
const EXTRA_LAND: &str = "You may play an additional land on each of your turns.";
const PAY_LIFE_LAND: &str =
    "As this land enters, you may pay 2 life. If you don't, it enters tapped.\n{T}: Add {U}.";
const COLOR_CHOICE_LAND: &str = "As this land enters, choose a color.\n{T}: Add {U}.";
const STEAL_PERMANENT: &str = "Gain control of target permanent.";
const GAIN_THREE: &str = "You gain 3 life.";

fn fund(runner: &mut GameRunner, player: PlayerId, mana: ManaType, count: usize) {
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player exists")
        .mana_pool;
    for _ in 0..count {
        pool.add(ManaUnit::new(mana, ObjectId(0), false, vec![]));
    }
}

fn lands_played(runner: &GameRunner, player: PlayerId) -> u32 {
    u32::from(
        runner
            .state()
            .players
            .iter()
            .find(|p| p.id == player)
            .expect("player exists")
            .lands_played_this_turn,
    )
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .len()
}

/// Every object's casting permissions sourced from `source`.
fn permissions_from(runner: &GameRunner, source: ObjectId) -> Vec<(ObjectId, CastingPermission)> {
    runner
        .state()
        .objects
        .values()
        .flat_map(|obj| {
            obj.casting_permissions
                .iter()
                .filter(|p| match p {
                    CastingPermission::PlayFromExile { source_id, .. } => {
                        *source_id == Some(source)
                    }
                    CastingPermission::ExileWithAltCost { .. } => true,
                    _ => false,
                })
                .map(|p| (obj.id, p.clone()))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn has_lingering_play_grant(runner: &GameRunner, id: ObjectId, source: ObjectId) -> bool {
    let permissions = &runner.state().objects[&id].casting_permissions;
    permissions.iter().any(|p| {
        matches!(
            p,
            CastingPermission::ExileWithAltCost { duration: None, .. }
        )
    }) && permissions.iter().any(|p| {
        matches!(
            p,
            CastingPermission::PlayFromExile { source_id, .. } if *source_id == Some(source)
        )
    })
}

/// What one drive of a resolving "you may play" instruction observed.
#[derive(Default)]
struct Drive {
    offered: bool,
    /// The watched card's zone immediately after the optional decision.
    zone_after_decision: Option<Zone>,
    /// Every zone the watched card occupied after any action of the drive.
    zones_seen: Vec<Zone>,
    events: Vec<GameEvent>,
    replacement_choice_seen: bool,
    named_choice_seen: bool,
    target_prompt_seen: bool,
    /// The source of the stack entry being resolved (popped off the stack into
    /// `resolving_stack_entry`) when the target prompt was answered.
    resolving_source_at_target: Option<ObjectId>,
}

/// Pass priority until the stack is empty, answering the optional offer with
/// `accept`, a target prompt with `target`, a replacement choice with its
/// non-decline candidate, and a named choice with its first option.
fn settle(
    runner: &mut GameRunner,
    watched: ObjectId,
    accept: bool,
    target: Option<TargetRef>,
) -> Drive {
    let mut drive = Drive::default();
    for _ in 0..120 {
        let result = match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                drive.offered = true;
                let result = runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("answering the offer");
                drive.zone_after_decision = runner.state().objects.get(&watched).map(|o| o.zone);
                result
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                drive.target_prompt_seen = true;
                drive.resolving_source_at_target = runner
                    .state()
                    .resolving_stack_entry
                    .as_ref()
                    .map(|e| e.source_id);
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(target.clone().expect("a target prompt was expected")),
                    })
                    .expect("choosing the target")
            }
            WaitingFor::ReplacementChoice { candidates, .. } => {
                drive.replacement_choice_seen = true;
                let index = candidates
                    .iter()
                    .position(|c| c.description != "Decline")
                    .unwrap_or(0);
                runner
                    .act(GameAction::ChooseReplacement { index })
                    .expect("choosing the replacement")
            }
            WaitingFor::NamedChoice { options, .. } => {
                drive.named_choice_seen = true;
                let choice = options.first().expect("named-choice options").clone();
                runner
                    .act(GameAction::ChooseOption { choice })
                    .expect("answering the named choice")
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => runner
                .act(GameAction::PassPriority)
                .expect("passing priority"),
            other => panic!("unexpected prompt while settling: {other:?}"),
        };
        drive.events.extend(result.events);
        if let Some(obj) = runner.state().objects.get(&watched) {
            drive.zones_seen.push(obj.zone);
        }
    }
    assert!(runner.state().stack.is_empty(), "the stack settles");
    drive
}

/// Pass the game forward until `player`'s precombat main phase with an empty
/// stack, then hand `holder` priority there.
fn advance_to_main_of(runner: &mut GameRunner, player: PlayerId, holder: PlayerId) {
    let start_turn = runner.state().turn_number;
    for _ in 0..300 {
        let state = runner.state();
        if state.turn_number > start_turn
            && state.active_player == player
            && state.phase == Phase::PreCombatMain
            && state.stack.is_empty()
            && matches!(state.waiting_for, WaitingFor::Priority { player: p } if p == holder)
        {
            return;
        }
        match state.waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => runner
                .act(GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                })
                .expect("declaring no attackers"),
            _ => runner
                .act(GameAction::PassPriority)
                .expect("passing priority"),
        };
    }
    panic!("never reached {player:?}'s main phase with {holder:?} holding priority");
}

// ---------------------------------------------------------------------------
// Shelldock Isle (Hideaway) boards
// ---------------------------------------------------------------------------

struct ShelldockBoard {
    runner: GameRunner,
    land: ObjectId,
    hidden: ObjectId,
}

/// P0 plays Shelldock Isle (this turn's land drop, LP 1) and hides the card
/// `stage` puts on top of P0's library. Libraries stay well under twenty cards,
/// so the activated ability's condition holds. `extras` stages anything else.
fn shelldock_board(
    stage: impl FnOnce(&mut GameScenario) -> ObjectId,
    extras: impl FnOnce(&mut GameScenario),
    real_db: bool,
) -> Option<ShelldockBoard> {
    shelldock_board_at_depth(stage, extras, real_db, 0)
}

/// [`shelldock_board`] with the staged card `hidden_depth` cards below the top,
/// for boards whose setup draws a card before the Hideaway trigger resolves.
fn shelldock_board_at_depth(
    stage: impl FnOnce(&mut GameScenario) -> ObjectId,
    extras: impl FnOnce(&mut GameScenario),
    real_db: bool,
    hidden_depth: usize,
) -> Option<ShelldockBoard> {
    hideaway_board_at_depth(
        "Shelldock Isle",
        SHELLDOCK_ISLE,
        stage,
        extras,
        real_db,
        hidden_depth,
    )
}

/// [`shelldock_board_at_depth`] for any Hideaway land built from `oracle`.
fn hideaway_board_at_depth(
    name: &str,
    oracle: &str,
    stage: impl FnOnce(&mut GameScenario) -> ObjectId,
    extras: impl FnOnce(&mut GameScenario),
    real_db: bool,
    hidden_depth: usize,
) -> Option<ShelldockBoard> {
    let db = if real_db { Some(load_db()?) } else { None };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario
        .add_land_to_hand(P0, name)
        .from_oracle_text(oracle)
        .id();
    for i in 0..3 {
        scenario.add_card_to_library_top(P0, &format!("Library Card {i}"));
        scenario.add_card_to_library_top(P1, &format!("Opponent Card {i}"));
    }
    let hidden = stage(&mut scenario);
    extras(&mut scenario);
    let mut runner = scenario.build();
    if let Some(db) = db {
        engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    }
    put_in_library(&mut runner, P0, hidden, hidden_depth);

    let card_id = runner.state().objects[&land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect("playing the Hideaway land");
    let mut hid = false;
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DigChoice { cards, .. } => {
                assert!(
                    cards.contains(&hidden),
                    "reach-guard: the staged card is seen"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![hidden],
                    })
                    .expect("hiding the staged card");
                hid = true;
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() && hid => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority");
            }
            other => panic!("unexpected prompt while hiding a card: {other:?}"),
        }
    }
    assert!(hid, "the Hideaway trigger offered a DigChoice");
    assert_eq!(runner.state().objects[&hidden].zone, Zone::Exile);
    assert!(runner.state().objects[&hidden].face_down);
    // "This land enters tapped." Stay on this turn; untap directly.
    runner.state_mut().objects.get_mut(&land).unwrap().tapped = false;
    Some(ShelldockBoard {
        runner,
        land,
        hidden,
    })
}

fn put_on_top(runner: &mut GameRunner, player: PlayerId, id: ObjectId) {
    put_in_library(runner, player, id, 0);
}

/// Re-seat `id` `depth` cards below the top of `player`'s library
/// (`library[0]` is the top).
fn put_in_library(runner: &mut GameRunner, player: PlayerId, id: ObjectId, depth: usize) {
    let library = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player exists")
        .library;
    library.retain(|card| *card != id);
    library.insert(depth.min(library.len()), id);
}

fn activate_shelldock(
    board: &mut ShelldockBoard,
    accept: bool,
    target: Option<TargetRef>,
) -> Drive {
    activate_hideaway(board, ManaType::Blue, accept, target)
}

/// Activate a Hideaway land's `{C}, {T}` play ability, paying `mana`.
fn activate_hideaway(
    board: &mut ShelldockBoard,
    mana: ManaType,
    accept: bool,
    target: Option<TargetRef>,
) -> Drive {
    fund(&mut board.runner, P0, mana, 1);
    board
        .runner
        .act(GameAction::ActivateAbility {
            source_id: board.land,
            ability_index: 1,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("activating the Hideaway land's play ability");
    settle(&mut board.runner, board.hidden, accept, target)
}

fn stage_land(scenario: &mut GameScenario, name: &str) -> ObjectId {
    scenario.add_land_to_library_top(P0, name).id()
}

fn hidden_is_shelldocks_face_down_card(board: &ShelldockBoard) {
    let state = board.runner.state();
    assert_eq!(state.objects[&board.hidden].zone, Zone::Exile);
    assert!(state.objects[&board.hidden].face_down);
    assert!(state
        .exile_links
        .iter()
        .any(|link| link.exiled_id == board.hidden
            && link.source_id == board.land
            && matches!(link.kind, ExileLinkKind::HideawayLookable { .. })));
}

/// R2 — CR 701.18b + CR 305.2a + CR 116.2a: with a land drop left, the hidden
/// land is played during the resolution: it enters face up under P0's control
/// from exile and counts as a land play; nothing lingers.
#[test]
fn shelldock_plays_its_hidden_land_during_resolution() {
    let mut board = shelldock_board(
        |s| stage_land(s, "Hidden Land"),
        |s| {
            s.add_enchantment_from_oracle(P0, "Extra Land", EXTRA_LAND);
        },
        false,
    )
    .unwrap();
    assert_eq!(
        lands_played(&board.runner, P0),
        1,
        "reach-guard: Shelldock was played"
    );
    let drive = activate_shelldock(&mut board, true, None);
    assert!(drive.offered);
    let state = board.runner.state();
    let land = &state.objects[&board.hidden];
    assert_eq!(land.zone, Zone::Battlefield);
    assert!(!land.face_down, "a played land enters face up");
    assert_eq!(land.controller, P0);
    assert_eq!(lands_played(&board.runner, P0), 2);
    assert!(drive.events.iter().any(|e| matches!(
        e,
        GameEvent::LandPlayed { object_id, from_zone: Zone::Exile, .. } if *object_id == board.hidden
    )));
    assert!(permissions_from(&board.runner, board.land).is_empty());
    assert!(board.runner.state().stack.is_empty());
}

/// R3 — CR 305.2b + CR 608.2d: with no land drop left, the instruction can't be
/// followed, so no "you may play" offer is made; the hidden land stays hidden.
#[test]
fn shelldock_offers_nothing_for_a_hidden_land_at_the_allowance() {
    let mut board = shelldock_board(|s| stage_land(s, "Hidden Land"), |_| {}, false).unwrap();
    assert_eq!(lands_played(&board.runner, P0), 1);
    let drive = activate_shelldock(&mut board, true, None);
    assert!(!drive.offered, "no offer for an unplayable land");
    let state = board.runner.state();
    assert!(
        state.objects[&board.land].tapped,
        "reach-guard: the ability resolved"
    );
    assert_eq!(
        state.players[0].mana_pool.total(),
        0,
        "reach-guard: {{U}} was spent"
    );
    assert!(state.stack.is_empty());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    hidden_is_shelldocks_face_down_card(&board);
    assert_eq!(lands_played(&board.runner, P0), 1);
    assert!(state.objects[&board.hidden].casting_permissions.is_empty());
}

/// R13 — CR 110.2a + CR 305.2a: P1 steals Shelldock Isle and activates it on
/// P1's turn; the hidden land (owned by P0) enters under P1's control and
/// counts as P1's land play.
#[test]
fn a_stolen_shelldock_plays_its_hidden_land_for_its_controller() {
    let mut steal = None;
    let mut board = shelldock_board(
        |s| stage_land(s, "Hidden Land"),
        |s| {
            steal = Some(
                s.add_spell_to_hand_from_oracle(P1, "Take", true, STEAL_PERMANENT)
                    .with_mana_cost(ManaCost::zero())
                    .id(),
            );
        },
        false,
    )
    .unwrap();
    let steal = steal.unwrap();
    board
        .runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority to P1");
    board
        .runner
        .cast(steal)
        .target_objects(&[board.land])
        .resolve();
    assert_eq!(board.runner.state().objects[&board.land].controller, P1);
    advance_to_main_of(&mut board.runner, P1, P1);
    assert!(
        !board.runner.state().objects[&board.land].tapped,
        "P1 untapped it"
    );
    let p0_lands = lands_played(&board.runner, P0);

    fund(&mut board.runner, P1, ManaType::Blue, 1);
    board
        .runner
        .act(GameAction::ActivateAbility {
            source_id: board.land,
            ability_index: 1,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("P1 activates the stolen Shelldock");
    let drive = settle(&mut board.runner, board.hidden, true, None);
    assert!(drive.offered);
    let land = &board.runner.state().objects[&board.hidden];
    assert_eq!(land.zone, Zone::Battlefield);
    assert_eq!(land.owner, P0);
    assert_eq!(land.controller, P1);
    assert_eq!(lands_played(&board.runner, P1), 1);
    assert_eq!(lands_played(&board.runner, P0), p0_lands);
}

/// R14 — CR 614.12a + CR 603.2: an entry choice of the land played during the
/// resolution pauses and resumes; the stack settles and a "whenever a player
/// plays a land" observer fires exactly once.
#[test]
fn a_hidden_land_with_an_entry_choice_resumes_and_triggers_once() {
    let mut board = shelldock_board_at_depth(
        |s| {
            s.add_land_to_library_top(P0, "Pay Life Land")
                .from_oracle_text(PAY_LIFE_LAND)
                .id()
        },
        |s| {
            s.add_enchantment_from_oracle(P0, "Extra Land", EXTRA_LAND);
            // Under P1, so the observer and P0's Hideaway trigger never share a
            // controller-ordering prompt while the board is set up.
            s.add_artifact_from_oracle(P1, "Horn of Greed", HORN_OF_GREED);
        },
        false,
        // Horn of Greed's draw for Shelldock's own land play resolves before
        // the Hideaway trigger, so the hidden land sits one card down.
        1,
    )
    .unwrap();
    let life_before = board.runner.state().players[0].life;
    let hand_before = hand_size(&board.runner, P0);
    let drive = activate_shelldock(&mut board, true, None);
    assert!(drive.offered);
    assert!(
        drive.replacement_choice_seen,
        "reach-guard: the entry choice paused"
    );
    let state = board.runner.state();
    let land = &state.objects[&board.hidden];
    assert_eq!(land.zone, Zone::Battlefield);
    assert!(!land.tapped, "the life was paid");
    assert_eq!(state.players[0].life, life_before - 2);
    assert_eq!(lands_played(&board.runner, P0), 2);
    assert!(state.stack.is_empty());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(
        hand_size(&board.runner, P0),
        hand_before + 1,
        "Horn of Greed fired exactly once for the land play"
    );
    assert!(drive.events.iter().any(|e| matches!(
        e,
        GameEvent::LandPlayed { object_id, .. } if *object_id == board.hidden
    )));
}

/// R14n — CR 614.12a + CR 603.2c: a land played during the resolution whose
/// as-enters choice ("choose a color", the Thriving-land / Valgavoth's Lair
/// shape) pauses the entry on a `NamedChoice` resumes, and a "whenever a player
/// plays a land" observer fires exactly once. This is the post-replacement
/// drain route, whose mid-entry capture parks the land's entry events for the
/// replay that owns their single trigger scan.
#[test]
fn a_hidden_land_with_an_as_enters_named_choice_triggers_once() {
    let mut board = shelldock_board_at_depth(
        |s| {
            s.add_land_to_library_top(P0, "Color Choice Land")
                .from_oracle_text(COLOR_CHOICE_LAND)
                .id()
        },
        |s| {
            s.add_enchantment_from_oracle(P0, "Extra Land", EXTRA_LAND);
            // Under P1, as in R14: no controller-ordering prompt with Hideaway.
            s.add_artifact_from_oracle(P1, "Horn of Greed", HORN_OF_GREED);
        },
        false,
        // Horn of Greed draws for Shelldock's own land play first.
        1,
    )
    .unwrap();
    let hand_before = hand_size(&board.runner, P0);
    let drive = activate_shelldock(&mut board, true, None);
    assert!(drive.offered, "reach-guard: the play was offered");
    assert!(
        drive.named_choice_seen,
        "reach-guard: the entry paused on its as-enters color choice"
    );
    let state = board.runner.state();
    assert_eq!(state.objects[&board.hidden].zone, Zone::Battlefield);
    assert_eq!(lands_played(&board.runner, P0), 2);
    assert!(state.stack.is_empty());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(
        hand_size(&board.runner, P0),
        hand_before + 1,
        "CR 603.2c: Horn of Greed fired exactly once for the land play"
    );
}

// ---------------------------------------------------------------------------
// Hideaway modal cards (R15). Real card faces from the card database; the
// Hideaway source is built from its verbatim text.
// ---------------------------------------------------------------------------

fn stage_real<'a>(
    db: &'a engine::database::card_db::CardDatabase,
    name: &str,
) -> impl FnOnce(&mut GameScenario) -> ObjectId + 'a {
    let name = name.to_string();
    move |s: &mut GameScenario| s.add_real_card(P0, &name, Zone::Library, db)
}

/// R15a — CR 712.8a + CR 608.2g + CR 608.2n: a spell//land modal card (Hagra
/// Mauling // Hagra Broodpit) is cast by its front face during the resolution
/// and ends in its owner's graveyard; nothing lingers. Hagra Mauling stands in
/// for the plan's Bala Ged Recovery // Bala Ged Sanctuary, which the test card
/// fixture lacks (same shape: targeted sorcery front, land back).
#[test]
fn shelldock_casts_a_hidden_spell_land_modal_card_by_its_front_face() {
    let Some(db) = load_db() else { return };
    let mut victim = None;
    let Some(mut board) = shelldock_board(
        stage_real(db, "Hagra Mauling"),
        |s| victim = Some(s.add_creature(P1, "Victim", 2, 2).id()),
        true,
    ) else {
        return;
    };
    let victim = victim.unwrap();
    let back = board.runner.state().objects[&board.hidden]
        .back_face
        .clone()
        .expect("reach-guard: the modal back face is present");
    assert_eq!(back.layout_kind, Some(LayoutKind::Modal));
    let lands_before = lands_played(&board.runner, P0);

    let drive = activate_shelldock(&mut board, true, Some(TargetRef::Object(victim)));
    assert!(drive.offered);
    assert_eq!(
        drive.zone_after_decision,
        Some(Zone::Stack),
        "the front face is cast during the resolution"
    );
    let state = board.runner.state();
    assert_eq!(
        state.objects[&victim].zone,
        Zone::Graveyard,
        "Destroy target creature"
    );
    assert_eq!(state.objects[&board.hidden].zone, Zone::Graveyard);
    assert_eq!(lands_played(&board.runner, P0), lands_before);
    assert!(permissions_from(&board.runner, board.land).is_empty());
    assert!(!state
        .exile_links
        .iter()
        .any(|link| link.exiled_id == board.hidden));
}

/// R15a′ — declining leaves the hidden modal card hidden, with no grant. Hagra
/// Mauling stands in for Bala Ged Recovery, as in R15a.
#[test]
fn declining_a_hidden_spell_land_modal_card_leaves_it_hidden() {
    let Some(db) = load_db() else { return };
    let Some(mut board) = shelldock_board(stage_real(db, "Hagra Mauling"), |_| {}, true) else {
        return;
    };
    let drive = activate_shelldock(&mut board, false, None);
    assert!(drive.offered, "reach-guard: the offer appeared");
    hidden_is_shelldocks_face_down_card(&board);
    assert!(board.runner.state().objects[&board.hidden]
        .casting_permissions
        .is_empty());
}

/// R5 — CR 608.2c + CR 608.2g: declining a non-modal Hideaway play (Mosswort
/// Bridge, its condition met) leaves the hidden spell face down in exile, linked
/// to the land, with no lingering grant; the instruction does not wait for a
/// later priority window.
#[test]
fn declining_mosswort_bridges_hidden_spell_leaves_it_hidden_without_a_grant() {
    let mut board = hideaway_board_at_depth(
        "Mosswort Bridge",
        MOSSWORT_BRIDGE,
        stage_instant,
        |s| {
            s.add_creature(P0, "Power Ten", 10, 1);
        },
        false,
        0,
    )
    .unwrap();
    let drive = activate_hideaway(&mut board, ManaType::Green, false, None);
    assert!(drive.offered, "reach-guard: the offer appeared");
    assert!(
        board.runner.state().objects[&board.land].tapped,
        "reach-guard: the ability resolved"
    );
    hidden_is_shelldocks_face_down_card(&board);
    assert!(
        permissions_from(&board.runner, board.land).is_empty(),
        "declining grants no permission"
    );
    assert!(board.runner.state().objects[&board.hidden]
        .casting_permissions
        .is_empty());
    assert_eq!(board.runner.state().players[0].life, 20, "nothing was cast");
}

/// R15b — CR 712.12: a land//land modal card (Branchloft Pathway // Boulderloft
/// Pathway) keeps the lingering grant; playing it opens the face choice and the
/// back face can be chosen. Branchloft Pathway stands in for the plan's
/// Brightclimb Pathway // Grimclimb Pathway, which the test card fixture lacks.
#[test]
fn shelldock_keeps_a_hidden_land_land_modal_card_on_the_lingering_grant() {
    let Some(db) = load_db() else { return };
    let Some(mut board) = shelldock_board(
        stage_real(db, "Branchloft Pathway"),
        |s| {
            s.add_enchantment_from_oracle(P0, "Extra Land", EXTRA_LAND);
        },
        true,
    ) else {
        return;
    };
    let back = board.runner.state().objects[&board.hidden]
        .back_face
        .clone()
        .expect("reach-guard: the modal back face is present");
    assert_eq!(back.layout_kind, Some(LayoutKind::Modal));
    let lands_before = lands_played(&board.runner, P0);
    let drive = activate_shelldock(&mut board, true, None);
    assert!(drive.offered);
    assert_eq!(
        board.runner.state().objects[&board.hidden].zone,
        Zone::Exile
    );
    assert!(has_lingering_play_grant(
        &board.runner,
        board.hidden,
        board.land
    ));
    assert_eq!(lands_played(&board.runner, P0), lands_before);

    let card_id = board.runner.state().objects[&board.hidden].card_id;
    board
        .runner
        .act(GameAction::PlayLand {
            object_id: board.hidden,
            card_id,
        })
        .expect("playing the hidden Pathway");
    assert!(matches!(
        board.runner.state().waiting_for,
        WaitingFor::ModalFaceChoice { .. }
    ));
    board
        .runner
        .act(GameAction::ChooseModalFace { back_face: true })
        .expect("choosing the back face");
    let obj = &board.runner.state().objects[&board.hidden];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(obj.name, "Boulderloft Pathway");
}

/// R15c — CR 712.8a: a transform DFC (Hadana's Climb // Winged Temple of
/// Orazca) is not modal; its front face is cast during the resolution. Hadana's
/// Climb stands in for the plan's Search for Azcanta, which the test card
/// fixture lacks (same shape: transform DFC with a nonland front).
#[test]
fn shelldock_casts_a_hidden_transform_card_by_its_front_face() {
    let Some(db) = load_db() else { return };
    let Some(mut board) = shelldock_board(stage_real(db, "Hadana's Climb"), |_| {}, true) else {
        return;
    };
    let back = board.runner.state().objects[&board.hidden]
        .back_face
        .clone()
        .expect("reach-guard: the transform back face is present");
    assert_eq!(back.layout_kind, Some(LayoutKind::Transform));
    let drive = activate_shelldock(&mut board, true, None);
    assert!(drive.offered);
    assert_eq!(drive.zone_after_decision, Some(Zone::Stack));
    assert_eq!(
        board.runner.state().objects[&board.hidden].zone,
        Zone::Battlefield,
        "the enchantment resolved"
    );
}

// ---------------------------------------------------------------------------
// Djinn of Wishes boards (library-revealed card)
// ---------------------------------------------------------------------------

struct DjinnBoard {
    runner: GameRunner,
    djinn: ObjectId,
    top: ObjectId,
}

/// Djinn of Wishes (verbatim) with three wish counters on P0's battlefield, and
/// `stage` putting a card on top of P0's library.
fn djinn_board(
    stage: impl FnOnce(&mut GameScenario) -> ObjectId,
    extras: impl FnOnce(&mut GameScenario),
    real_db: bool,
) -> Option<DjinnBoard> {
    let db = if real_db { Some(load_db()?) } else { None };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let djinn = scenario
        .add_creature_from_oracle(P0, "Djinn of Wishes", 4, 4, DJINN_OF_WISHES)
        .id();
    scenario.with_counter(djinn, CounterType::Generic("wish".to_string()), 3);
    for i in 0..3 {
        scenario.add_card_to_library_top(P0, &format!("Library Card {i}"));
        scenario.add_card_to_library_top(P1, &format!("Opponent Card {i}"));
    }
    let top = stage(&mut scenario);
    extras(&mut scenario);
    let mut runner = scenario.build();
    if let Some(db) = db {
        engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    }
    put_on_top(&mut runner, P0, top);
    Some(DjinnBoard { runner, djinn, top })
}

fn djinn_ability_index(board: &DjinnBoard) -> usize {
    board.runner.state().objects[&board.djinn]
        .abilities
        .iter()
        .position(|ability| ability.cost.is_some())
        .expect("Djinn's activated ability")
}

fn activate_djinn(board: &mut DjinnBoard, accept: bool, target: Option<TargetRef>) -> Drive {
    fund(&mut board.runner, P0, ManaType::Blue, 4);
    let ability_index = djinn_ability_index(board);
    board
        .runner
        .act(GameAction::ActivateAbility {
            source_id: board.djinn,
            ability_index,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("activating Djinn of Wishes");
    settle(&mut board.runner, board.top, accept, target)
}

fn stage_instant(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_library_top(P0, "Revealed Insight", true)
        .from_oracle_text(GAIN_THREE)
        .with_mana_cost(ManaCost::generic(3))
        .id()
}

/// R7a — CR 608.2g + CR 608.2n: a revealed instant is cast during the
/// resolution for free and ends in the graveyard, not exile ("If you don't,
/// exile it" applies only when it was not played).
#[test]
fn djinn_casts_a_revealed_instant_during_resolution() {
    let mut board = djinn_board(stage_instant, |_| {}, false).unwrap();
    let drive = activate_djinn(&mut board, true, None);
    assert!(drive.offered);
    assert_eq!(drive.zone_after_decision, Some(Zone::Stack));
    let state = board.runner.state();
    assert_eq!(state.players[0].life, 23);
    assert_eq!(state.players[0].mana_pool.total(), 0, "the cast was free");
    assert_eq!(state.objects[&board.top].zone, Zone::Graveyard);
    assert!(!drive.zones_seen.contains(&Zone::Exile), "never exiled");
    assert!(permissions_from(&board.runner, board.djinn).is_empty());
}

/// R7b — CR 305.2a: a revealed land is played during the resolution.
#[test]
fn djinn_plays_a_revealed_land_during_resolution() {
    let mut board = djinn_board(|s| stage_land(s, "Revealed Land"), |_| {}, false).unwrap();
    assert_eq!(lands_played(&board.runner, P0), 0);
    let drive = activate_djinn(&mut board, true, None);
    assert!(drive.offered);
    assert_eq!(
        board.runner.state().objects[&board.top].zone,
        Zone::Battlefield
    );
    assert_eq!(lands_played(&board.runner, P0), 1);
    assert!(!drive.zones_seen.contains(&Zone::Exile), "not exiled");
}

/// R7c — CR 305.2b: with the land drop used, the revealed land can't be played:
/// no offer, and "If you don't, exile it" exiles it.
#[test]
fn djinn_exiles_a_revealed_land_it_cannot_play() {
    let mut hand_land = None;
    let mut board = djinn_board(
        |s| stage_land(s, "Revealed Land"),
        |s| hand_land = Some(s.add_land_to_hand(P0, "Hand Land").id()),
        false,
    )
    .unwrap();
    let hand_land = hand_land.unwrap();
    let card_id = board.runner.state().objects[&hand_land].card_id;
    board
        .runner
        .act(GameAction::PlayLand {
            object_id: hand_land,
            card_id,
        })
        .expect("playing the hand land first");
    assert_eq!(lands_played(&board.runner, P0), 1);
    let drive = activate_djinn(&mut board, true, None);
    assert!(!drive.offered, "no offer for an unplayable land");
    assert_eq!(board.runner.state().objects[&board.top].zone, Zone::Exile);
    assert_eq!(lands_played(&board.runner, P0), 1);
    assert!(board.runner.state().objects[&board.top]
        .casting_permissions
        .is_empty());
}

/// R7d — CR 608.2c: declining exiles the revealed card and grants nothing.
#[test]
fn djinn_exiles_a_declined_card_without_a_grant() {
    let mut board = djinn_board(stage_instant, |_| {}, false).unwrap();
    let drive = activate_djinn(&mut board, false, None);
    assert!(drive.offered, "reach-guard: the offer appeared");
    assert_eq!(board.runner.state().objects[&board.top].zone, Zone::Exile);
    assert!(board.runner.state().objects[&board.top]
        .casting_permissions
        .is_empty());
    assert_eq!(board.runner.state().players[0].life, 20);
}

/// R4 — CR 305.3: on an opponent's turn the revealed land can't be played: no
/// offer, and it is exiled. R4s — CR 608.2g: a revealed instant is still cast
/// then, whatever the turn.
#[test]
fn djinn_on_an_opponents_turn_plays_spells_but_not_lands() {
    let mut board = djinn_board(|s| stage_land(s, "Revealed Land"), |_| {}, false).unwrap();
    advance_to_main_of(&mut board.runner, P1, P0);
    assert_eq!(board.runner.state().active_player, P1);
    let drive = activate_djinn(&mut board, true, None);
    assert!(!drive.offered, "CR 305.3: no offer for a land on P1's turn");
    assert_eq!(board.runner.state().objects[&board.top].zone, Zone::Exile);
    assert_eq!(lands_played(&board.runner, P0), 0);
    assert!(board.runner.state().objects[&board.top]
        .casting_permissions
        .is_empty());

    let mut board = djinn_board(stage_instant, |_| {}, false).unwrap();
    advance_to_main_of(&mut board.runner, P1, P0);
    // P0 drew nothing on P1's turn, so the staged instant is still on top.
    let drive = activate_djinn(&mut board, true, None);
    assert!(drive.offered);
    assert_eq!(drive.zone_after_decision, Some(Zone::Stack));
    assert_eq!(
        board.runner.state().objects[&board.top].zone,
        Zone::Graveyard
    );
}

/// R16a — CR 712.8a + CR 608.2g + CR 608.2n: a revealed spell//land modal card
/// is cast by its front face during the resolution and ends in the graveyard;
/// the decline branch never exiles it. Hagra Mauling // Hagra Broodpit stands in
/// for the plan's Emeria's Call // Emeria, Shattered Skyclave, which the test
/// card fixture lacks (same shape: sorcery front, land back).
#[test]
fn djinn_casts_a_revealed_spell_land_modal_card_by_its_front_face() {
    let Some(db) = load_db() else { return };
    let mut victim = None;
    let Some(mut board) = djinn_board(
        stage_real(db, "Hagra Mauling"),
        |s| victim = Some(s.add_creature(P1, "Victim", 2, 2).id()),
        true,
    ) else {
        return;
    };
    let victim = victim.unwrap();
    assert_eq!(
        board.runner.state().objects[&board.top]
            .back_face
            .as_ref()
            .and_then(|bf| bf.layout_kind),
        Some(LayoutKind::Modal),
        "reach-guard: a real modal card"
    );
    let drive = activate_djinn(&mut board, true, Some(TargetRef::Object(victim)));
    assert!(drive.offered);
    // A targeted spell cast from the library announces its target before it
    // reaches the stack; the cast is in progress right after the decision.
    assert!(
        drive.target_prompt_seen,
        "reach-guard: Hagra Mauling's own target prompt opened during the resolution"
    );
    assert_eq!(
        drive.resolving_source_at_target,
        Some(board.djinn),
        "CR 608.2g: the target is chosen while Djinn's ability is still resolving"
    );
    assert!(
        drive.zones_seen.contains(&Zone::Stack),
        "the front face was cast during the resolution"
    );
    let state = board.runner.state();
    assert_eq!(state.objects[&victim].zone, Zone::Graveyard);
    assert_eq!(state.objects[&board.top].zone, Zone::Graveyard);
    assert!(!drive.zones_seen.contains(&Zone::Exile), "never exiled");
    assert!(permissions_from(&board.runner, board.djinn).is_empty());
    assert_eq!(lands_played(&board.runner, P0), 0);
    assert_eq!(state.players[0].mana_pool.total(), 0);
}

/// R16b — CR 712.12: a revealed land//land modal card keeps the lingering grant
/// (delivered to exile, the decline branch skipped); playing it opens the face
/// choice and the back face can be chosen. Branchloft Pathway stands in for the
/// plan's Brightclimb Pathway // Grimclimb Pathway, which the test card fixture
/// lacks.
#[test]
fn djinn_keeps_a_revealed_land_land_modal_card_on_the_lingering_grant() {
    let Some(db) = load_db() else { return };
    let Some(mut board) = djinn_board(stage_real(db, "Branchloft Pathway"), |_| {}, true) else {
        return;
    };
    let drive = activate_djinn(&mut board, true, None);
    assert!(drive.offered);
    assert_eq!(board.runner.state().objects[&board.top].zone, Zone::Exile);
    assert!(
        has_lingering_play_grant(&board.runner, board.top, board.djinn),
        "one exiled object carries both halves of the grant"
    );
    assert_eq!(
        lands_played(&board.runner, P0),
        0,
        "not played during resolution"
    );

    let card_id = board.runner.state().objects[&board.top].card_id;
    board
        .runner
        .act(GameAction::PlayLand {
            object_id: board.top,
            card_id,
        })
        .expect("playing the exiled Pathway");
    assert!(matches!(
        board.runner.state().waiting_for,
        WaitingFor::ModalFaceChoice { .. }
    ));
    board
        .runner
        .act(GameAction::ChooseModalFace { back_face: true })
        .expect("choosing the back face");
    let obj = &board.runner.state().objects[&board.top];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(obj.name, "Boulderloft Pathway");
    assert_eq!(lands_played(&board.runner, P0), 1);
}

/// R6 — CR 611.2a: an explicit duration keeps the lingering permission (Mind's
/// Desire, verbatim): the exiled land can be played afterwards.
#[test]
fn minds_desire_keeps_its_until_end_of_turn_play_permission() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let desire = scenario
        .add_spell_to_hand_from_oracle(P0, "Mind's Desire", false, MINDS_DESIRE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let land = scenario.add_land_to_library_top(P0, "Desired Land").id();
    scenario.add_card_to_library_top(P1, "Opponent Card");
    let mut runner = scenario.build();
    runner.cast(desire).resolve();
    assert_eq!(runner.state().objects[&land].zone, Zone::Exile);
    assert!(
        runner.state().objects[&land]
            .casting_permissions
            .iter()
            .any(|p| matches!(
                p,
                CastingPermission::PlayFromExile {
                    duration: engine::types::ability::Duration::UntilEndOfTurn,
                    ..
                }
            )),
        "the lingering grant stays, got {:?}",
        runner.state().objects[&land].casting_permissions
    );
    let card_id = runner.state().objects[&land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect("playing the exiled land later");
    assert_eq!(runner.state().objects[&land].zone, Zone::Battlefield);
    assert_eq!(lands_played(&runner, P0), 1);
}
