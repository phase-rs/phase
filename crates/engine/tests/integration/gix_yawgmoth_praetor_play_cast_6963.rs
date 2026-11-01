//! "You may play lands and cast spells from among cards exiled this way …"
//! (Gix, Yawgmoth Praetor; Magus of the Mind) and "… from among the exiled
//! cards …" (Elder Brain, #6963) grant BOTH actions.
//!
//! - CR 701.18b: to play a card means to play it as a land or cast it as a
//!   spell, whichever is appropriate.
//! - CR 608.2g: Gix's cards are played as the ability resolves ("You must play
//!   the cards as you resolve the last ability. You can't wait and play them
//!   later.").
//! - CR 305.2a + CR 305.2b + CR 305.3: a land played during the resolution
//!   counts against the land allowance and is refused off-turn or past it
//!   ("You may play a land this way only during your own turn and only if you
//!   have not yet played a land this turn.").
//! - CR 611.2a: a stated window ("Until end of turn") bounds a lingering grant.

use engine::ai_support::legal_actions;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, CardPlayMode, CastFromZoneDriver, CastingPermission, ControllerRef, Effect,
    ResolutionCastWindow, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastOfferKind, ExileLink, ExileLinkKind, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const GIX: &str = "Whenever a creature deals combat damage to one of your opponents, its controller may pay 1 life. If they do, they draw a card.\n{4}{B}{B}{B}, Discard X cards: Exile the top X cards of target opponent's library. You may play lands and cast spells from among cards exiled this way without paying their mana costs.";
const ELDER_BRAIN: &str = "Menace\nWhenever this creature attacks a player, exile all cards from that player's hand, then they draw that many cards. You may play lands and cast spells from among the exiled cards for as long as they remain exiled. If you cast a spell this way, you may spend mana as though it were mana of any color to cast it.";
const MAGUS_OF_THE_MIND: &str = "{U}, {T}, Sacrifice this creature: Shuffle your library, then exile the top X cards, where X is one plus the number of spells cast this turn. Until end of turn, you may play lands and cast spells from among cards exiled this way without paying their mana costs.";
const THOUGHT_DISTORTION: &str = "This spell can't be countered.\nTarget opponent reveals their hand. Exile all noncreature, nonland cards from that player's hand and graveyard.";
const EXTRA_LAND: &str = "You may play an additional land on each of your turns.";

// ---------------------------------------------------------------------------
// Parse helpers
// ---------------------------------------------------------------------------

fn parse(text: &str, name: &str, types: &[&str]) -> engine::parser::oracle::ParsedAbilities {
    let types: Vec<String> = types.iter().map(|t| t.to_string()).collect();
    parse_oracle_text(text, name, &[], &types, &[])
}

/// Every node of `def`'s sub-ability chain, head first.
fn chain(def: &AbilityDefinition) -> Vec<&AbilityDefinition> {
    std::iter::successors(Some(def), |node| node.sub_ability.as_deref()).collect()
}

fn is_land_play_gap(effect: &Effect) -> bool {
    matches!(effect, Effect::Unimplemented { description: Some(d), .. } if d == "play lands")
}

fn is_unimplemented(effect: &Effect) -> bool {
    matches!(effect, Effect::Unimplemented { .. })
}

// ---------------------------------------------------------------------------
// Parser: the coordinated land + cast permission lowers as ONE Play grant
// ---------------------------------------------------------------------------

/// CR 701.18b + CR 608.2g: Gix's activated ability lowers to `ExileTop` then ONE
/// free `CastFromZone` in `Play` mode over the batch, driven as a resolution
/// window; the refused "play lands" head is gone and no node is optional.
#[test]
fn gix_lowers_one_play_window_over_the_exiled_batch() {
    let parsed = parse(GIX, "Gix, Yawgmoth Praetor", &["Creature"]);
    let ability = parsed
        .abilities
        .first()
        .expect("reach-guard: the activated ability parses");
    let nodes = chain(ability);
    assert!(
        matches!(&*nodes[0].effect, Effect::ExileTop { .. }),
        "reach-guard: the exile producer heads the chain, got {:?}",
        nodes[0].effect
    );
    assert_eq!(nodes.len(), 2, "exile then one play window: {nodes:#?}");
    assert_eq!(
        &*nodes[1].effect,
        &Effect::CastFromZone {
            target: TargetFilter::ExiledBySource,
            without_paying_mana_cost: true,
            mode: CardPlayMode::Play,
            cast_transformed: false,
            alt_ability_cost: None,
            constraint: None,
            duration: None,
            driver: CastFromZoneDriver::ResolutionWindow {
                bounds: ResolutionCastWindow::UNBOUNDED,
            },
            mana_spend_permission: None,
            additional_cost: None,
            cast_cost_modifier: None,
        }
    );
    assert!(!nodes[1].optional, "the window's decline is the 'may'");
    assert!(nodes.iter().all(|node| !is_unimplemented(&node.effect)));
    assert!(
        parsed.parse_warnings.is_empty(),
        "no swallowed clause: {:?}",
        parsed.parse_warnings
    );
}

/// CR 611.2a + CR 701.18b: Magus of the Mind's "Until end of turn, you may
/// play lands and cast spells from among cards exiled this way …" lowers to a
/// lingering `Play` grant bounded to the turn. (The card stays unsupported for
/// its separate `where X` gap, which this change does not touch.)
#[test]
fn magus_of_the_mind_lowers_a_play_grant_until_end_of_turn() {
    let parsed = parse(MAGUS_OF_THE_MIND, "Magus of the Mind", &["Creature"]);
    let nodes = chain(parsed.abilities.first().expect("reach-guard: parses"));
    assert!(
        nodes.iter().all(|node| !is_land_play_gap(&node.effect)),
        "the land half is no longer refused: {nodes:#?}"
    );
    let grant = nodes
        .iter()
        .find(|node| matches!(&*node.effect, Effect::CastFromZone { .. }))
        .expect("reach-guard: the cast/play grant is present");
    assert!(
        matches!(
            &*grant.effect,
            Effect::CastFromZone {
                target: TargetFilter::ExiledBySource,
                without_paying_mana_cost: true,
                mode: CardPlayMode::Play,
                duration: Some(engine::types::ability::Duration::UntilEndOfTurn),
                driver: CastFromZoneDriver::LingeringPermission,
                ..
            }
        ),
        "got {:?}",
        grant.effect
    );
    assert!(!grant.optional);
}

/// CR 701.18b + CR 400.1: Elder Brain's permission lowers to `PlayFromExile` in
/// `Play` mode with its any-color rider, and no optional prompt. Its exile
/// clause ("exile all cards from that player's hand") binds no player, so it
/// stays an honest gap rather than exiling every hand; the draw half is kept.
#[test]
fn elder_brain_play_permission_lowers_and_unbound_exile_stays_a_gap() {
    let parsed = parse(ELDER_BRAIN, "Elder Brain", &["Creature"]);
    let execute = parsed
        .triggers
        .first()
        .and_then(|t| t.execute.as_ref())
        .expect("reach-guard: the attack trigger parses");
    let nodes = chain(execute);
    assert!(nodes.iter().all(|node| !is_land_play_gap(&node.effect)));
    let grant = nodes
        .iter()
        .find_map(|node| match &*node.effect {
            Effect::GrantCastingPermission {
                permission:
                    CastingPermission::PlayFromExile {
                        mode,
                        mana_spend_permission,
                        ..
                    },
                ..
            } => Some((node, *mode, *mana_spend_permission)),
            _ => None,
        })
        .expect("reach-guard: the permission node is present");
    assert_eq!(grant.1, CardPlayMode::Play);
    assert!(grant.2.is_some(), "the any-color rider rides the grant");
    assert!(!grant.0.optional, "no 'do you want to' prompt");
    assert!(
        nodes
            .iter()
            .any(|node| matches!(&*node.effect, Effect::Draw { .. })),
        "reach-guard: the draw half is intact"
    );
    assert!(
        !nodes
            .iter()
            .any(|node| matches!(&*node.effect, Effect::ChangeZoneAll { .. })),
        "the unbound exile must not lower to an every-hand sweep: {nodes:#?}"
    );
    assert!(
        nodes.iter().any(|node| is_unimplemented(&node.effect)),
        "the exile clause stays an honest gap"
    );
}

/// CR 400.1: Thought Distortion's "… from that player's hand and graveyard" is
/// owner-bound by the multi-zone path, so the unbound-possessive guard leaves it
/// untouched.
#[test]
fn thought_distortion_keeps_its_owner_bound_exile() {
    let parsed = parse(THOUGHT_DISTORTION, "Thought Distortion", &["Sorcery"]);
    let exile = parsed
        .abilities
        .iter()
        .flat_map(chain)
        .find_map(|node| match &*node.effect {
            Effect::ChangeZoneAll { target, .. } => Some(target.clone()),
            _ => None,
        })
        .expect("reach-guard: the mass exile lowers");
    let TargetFilter::Typed(typed) = exile else {
        panic!("typed exile filter, got {exile:?}");
    };
    assert_eq!(typed.controller, Some(ControllerRef::TargetPlayer));
}

/// The guard reads only its own clause: a possessive in a LATER sentence does
/// not refuse an earlier, own-zone mass exile.
#[test]
fn a_later_possessive_does_not_refuse_an_earlier_exile_clause() {
    let parsed = parse(
        "Whenever this creature attacks a player, exile all cards from your graveyard. That player reveals their hand.",
        "Probe",
        &["Creature"],
    );
    let execute = parsed.triggers[0].execute.as_ref().expect("parses");
    assert!(
        chain(execute)
            .iter()
            .any(|node| matches!(&*node.effect, Effect::ChangeZoneAll { .. })),
        "the own-graveyard exile still lowers"
    );
}

/// Fail closed: a type-gated spell half would exclude the lands under `Play`,
/// a printed cap has no Play window, and an undated lingering grant would last
/// until the end of the game — each keeps the refused land head.
#[test]
fn unfaithful_play_shapes_keep_the_refused_land_head() {
    for text in [
        "Exile the top two cards of target opponent's library. You may play lands and cast creature spells from among cards exiled this way without paying their mana costs.",
        "Exile the top two cards of target opponent's library. You may play lands and cast up to two spells from among cards exiled this way without paying their mana costs.",
        "Exile the top two cards of target opponent's library. You may play lands and cast spells from among cards exiled this way.",
    ] {
        let parsed = parse(text, "Probe", &["Sorcery"]);
        let nodes = chain(parsed.abilities.first().expect("parses"));
        assert!(
            nodes.iter().any(|node| is_land_play_gap(&node.effect)),
            "{text}: the land half must stay refused, got {nodes:#?}"
        );
        assert!(
            !nodes.iter().any(|node| matches!(
                &*node.effect,
                Effect::CastFromZone {
                    mode: CardPlayMode::Play,
                    ..
                }
            )),
            "{text}: no Play grant"
        );
    }
}

// ---------------------------------------------------------------------------
// Runtime: Gix's Play window
// ---------------------------------------------------------------------------

struct GixBoard {
    runner: GameRunner,
    gix: ObjectId,
    discard: Vec<ObjectId>,
}

/// P0 controls Gix in its precombat main phase with {4}{B}{B}{B} floating
/// (twice when `activations == 2`) and four cards to discard. `stage` adds the
/// cards Gix will exile and returns them top card first; they are put on top
/// of P1's library in that order after the build.
fn gix_board(
    activations: usize,
    stage: impl FnOnce(&mut GameScenario) -> Vec<ObjectId>,
) -> (GixBoard, Vec<ObjectId>) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let gix = s
        .add_creature_from_oracle(P0, "Gix, Yawgmoth Praetor", 3, 3, GIX)
        .id();
    let discard: Vec<ObjectId> = ["Paid One", "Paid Two", "Paid Three", "Paid Four"]
        .iter()
        .map(|n| s.add_card_to_hand(P0, n))
        .collect();
    s.with_library_top(P0, &["Draw A", "Draw B", "Draw C"]);
    s.add_card_to_library_top(P1, "Bottom Filler");
    let staged = stage(&mut s);
    let mut runner = s.build();
    for id in staged.iter().rev() {
        put_on_top(&mut runner, P1, *id);
    }
    for _ in 0..activations {
        fund_gix(&mut runner);
    }
    (
        GixBoard {
            runner,
            gix,
            discard,
        },
        staged,
    )
}

fn fund_gix(runner: &mut GameRunner) {
    let pool = &mut runner.state_mut().players[0].mana_pool;
    for mana in [
        ManaType::Black,
        ManaType::Black,
        ManaType::Black,
        ManaType::Colorless,
        ManaType::Colorless,
        ManaType::Colorless,
        ManaType::Colorless,
    ] {
        pool.add(ManaUnit::new(mana, ObjectId(0), false, vec![]));
    }
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

/// Activate Gix for `x` targeting P1, discarding the next `x` paid cards.
fn activate_gix(board: &mut GixBoard, x: usize) {
    let paid: Vec<ObjectId> = board.discard.drain(..x).collect();
    board
        .runner
        .activate(board.gix, 0)
        .x(x as u32)
        .target_player(P1)
        .pay_with(&paid)
        .resolve();
}

/// The open window's candidates (sorted) and policy mode; panics otherwise.
fn window(state: &GameState) -> (Vec<ObjectId>, CardPlayMode) {
    match &state.waiting_for {
        WaitingFor::CastOffer {
            player,
            kind:
                CastOfferKind::FreeCastWindow {
                    candidates,
                    face_policy,
                    ..
                },
        } => {
            assert_eq!(*player, P0, "the window is Gix's controller's");
            let mut candidates = candidates.clone();
            candidates.sort();
            (candidates, face_policy.play_mode)
        }
        other => panic!("expected the Play window, got {other:?}"),
    }
}

fn choose(runner: &mut GameRunner, selection: Option<ObjectId>) {
    runner
        .act(GameAction::FreeCastWindowChoice { selection })
        .expect("the window accepts the choice");
}

fn lands_played(state: &GameState, player: PlayerId) -> u8 {
    state.players[player.0 as usize].lands_played_this_turn
}

fn sorted(mut ids: Vec<ObjectId>) -> Vec<ObjectId> {
    ids.sort();
    ids
}

/// The ability that opened the window has finished: nothing of Gix's is on
/// the stack and the controller holds priority.
fn assert_parent_finished(runner: &GameRunner, gix: ObjectId) {
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the window closed: {:?}",
        state.waiting_for
    );
    assert!(
        state.stack.iter().all(|entry| entry.source_id != gix),
        "Gix's ability finished resolving"
    );
}

/// CR 701.18b + CR 608.2g + CR 305.2a + CR 118.9: Gix exiles a land and
/// Divination. The window is a Play window offering both; the land is played
/// as a land (P0 controls it, the land drop is spent), the window re-opens with
/// the spell, which is cast without paying its mana cost, and the ability
/// finishes.
#[test]
fn gix_plays_the_exiled_land_and_casts_the_exiled_spell_in_one_window() {
    let Some(db) = load_db() else { return };
    let (mut board, staged) = gix_board(1, |s| {
        vec![
            s.add_real_card(P1, "Forest", Zone::Library, db),
            s.add_real_card(P1, "Divination", Zone::Library, db),
        ]
    });
    let [forest, divination] = staged[..] else {
        unreachable!()
    };
    activate_gix(&mut board, 2);
    let state = board.runner.state();
    assert_eq!(state.objects[&forest].zone, Zone::Exile);
    assert_eq!(state.objects[&divination].zone, Zone::Exile);
    assert_eq!(
        window(state),
        (sorted(vec![forest, divination]), CardPlayMode::Play)
    );
    let offered = legal_actions(state);
    for selection in [Some(forest), Some(divination), None] {
        assert!(
            offered.contains(&GameAction::FreeCastWindowChoice { selection }),
            "AI legal actions offer {selection:?}"
        );
    }

    choose(&mut board.runner, Some(forest));
    let state = board.runner.state();
    assert_eq!(state.objects[&forest].zone, Zone::Battlefield);
    assert_eq!(state.objects[&forest].controller, P0, "CR 110.2a");
    assert_eq!(
        lands_played(state, P0),
        1,
        "CR 305.2a: the land drop is spent"
    );
    assert_eq!(window(state), (vec![divination], CardPlayMode::Play));

    choose(&mut board.runner, Some(divination));
    let state = board.runner.state();
    assert_eq!(state.objects[&divination].zone, Zone::Stack);
    assert_eq!(state.players[0].mana_pool.total(), 0, "nothing was paid");
    assert_parent_finished(&board.runner, board.gix);
}

/// CR 305.2b: with the land drop already spent, the exiled land is not
/// offered; the spell still is (reach guard), and declining finishes.
#[test]
fn gix_offers_no_land_once_the_land_drop_is_spent() {
    let Some(db) = load_db() else { return };
    let (mut board, staged) = gix_board(1, |s| {
        vec![
            s.add_real_card(P1, "Forest", Zone::Library, db),
            s.add_real_card(P1, "Divination", Zone::Library, db),
        ]
    });
    let [forest, divination] = staged[..] else {
        unreachable!()
    };
    {
        let state = board.runner.state_mut();
        state.lands_played_this_turn = 1;
        state.players[0].lands_played_this_turn = 1;
    }
    activate_gix(&mut board, 2);
    assert_eq!(
        window(board.runner.state()),
        (vec![divination], CardPlayMode::Play),
        "reach-guard: the spell is offered; the land is not"
    );
    assert!(board
        .runner
        .act(GameAction::FreeCastWindowChoice {
            selection: Some(forest)
        })
        .is_err());
    choose(&mut board.runner, None);
    let state = board.runner.state();
    assert_eq!(state.objects[&forest].zone, Zone::Exile);
    assert_eq!(state.objects[&divination].zone, Zone::Exile);
    assert_parent_finished(&board.runner, board.gix);
}

/// Hellcarver Demon: "Exile the top six cards of your library. You may cast any
/// number of spells from among cards exiled this way without paying their mana
/// costs." A Cast-only window (CR 601.2: casting never plays a land) offers the
/// exiled spell and none of the five exiled lands, on P0's own turn with the
/// land drop unused.
#[test]
fn hellcarvers_cast_only_window_offers_no_land() {
    let Some(db) = load_db() else { return };
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let demon = s.add_real_card(P0, "Hellcarver Demon", Zone::Battlefield, db);
    s.add_card_to_library_top(P0, "Bottom Filler");
    let lands: Vec<ObjectId> = ["Forest", "Island", "Plains", "Swamp", "Mountain"]
        .iter()
        .map(|name| s.add_real_card(P0, name, Zone::Library, db))
        .collect();
    let divination = s.add_real_card(P0, "Divination", Zone::Library, db);
    let mut runner = s.build();
    for id in lands.iter().rev() {
        put_on_top(&mut runner, P0, *id);
    }
    put_on_top(&mut runner, P0, divination);
    runner
        .state_mut()
        .objects
        .get_mut(&demon)
        .unwrap()
        .summoning_sick = false;

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(demon, AttackTarget::Player(P1))])
        .expect("P0 attacks with Hellcarver Demon");
    runner.combat_damage();
    let state = runner.state();
    for id in lands.iter().chain([&divination]) {
        assert_eq!(
            state.objects[id].zone,
            Zone::Exile,
            "reach-guard: the trigger exiled the top six"
        );
    }
    assert_eq!(lands_played(state, P0), 0, "the land drop is unused");
    assert_eq!(window(state), (vec![divination], CardPlayMode::Cast));
}

/// CR 614.12a + CR 305.2a: the exiled land is a shock land. Its entry pauses
/// on the pay-2-life replacement; once it settles the window re-opens with the
/// spell, which is cast, and the ability finishes.
#[test]
fn gix_window_reopens_after_a_shock_land_entry_choice() {
    let Some(db) = load_db() else { return };
    let (mut board, staged) = gix_board(1, |s| {
        vec![
            s.add_real_card(P1, "Stomping Ground", Zone::Library, db),
            s.add_real_card(P1, "Divination", Zone::Library, db),
        ]
    });
    let [shock, divination] = staged[..] else {
        unreachable!()
    };
    activate_gix(&mut board, 2);
    assert_eq!(
        window(board.runner.state()).0,
        sorted(vec![shock, divination])
    );
    let life_before = board.runner.state().players[0].life;
    choose(&mut board.runner, Some(shock));
    let WaitingFor::ReplacementChoice { candidates, .. } = board.runner.state().waiting_for.clone()
    else {
        panic!(
            "reach-guard: the shock land's entry paused, got {:?}",
            board.runner.state().waiting_for
        );
    };
    let pay = candidates
        .iter()
        .position(|c| c.description != "Decline")
        .unwrap_or(0);
    board
        .runner
        .act(GameAction::ChooseReplacement { index: pay })
        .expect("paying 2 life");
    let state = board.runner.state();
    assert_eq!(state.objects[&shock].zone, Zone::Battlefield);
    assert_eq!(state.players[0].life, life_before - 2);
    assert_eq!(lands_played(state, P0), 1);
    assert_eq!(window(state), (vec![divination], CardPlayMode::Play));

    choose(&mut board.runner, Some(divination));
    assert_eq!(board.runner.state().objects[&divination].zone, Zone::Stack);
    assert_parent_finished(&board.runner, board.gix);
}

/// The paused window survives a save/reload taken during the entry choice.
#[test]
fn gix_window_survives_a_reload_during_the_entry_choice() {
    let Some(db) = load_db() else { return };
    let (mut board, staged) = gix_board(1, |s| {
        vec![
            s.add_real_card(P1, "Stomping Ground", Zone::Library, db),
            s.add_real_card(P1, "Divination", Zone::Library, db),
        ]
    });
    let [shock, divination] = staged[..] else {
        unreachable!()
    };
    activate_gix(&mut board, 2);
    choose(&mut board.runner, Some(shock));
    assert!(
        matches!(
            board.runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "reach-guard: paused on the entry choice"
    );
    let saved = serde_json::to_string(board.runner.state()).expect("serializes");
    let restored: GameState = serde_json::from_str(&saved).expect("deserializes");
    let mut runner = GameRunner::from_state(restored);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("answering the entry choice after reload");
    assert_eq!(runner.state().objects[&shock].zone, Zone::Battlefield);
    assert_eq!(
        window(runner.state()),
        (vec![divination], CardPlayMode::Play)
    );
}

/// CR 305.2a + CR 614.12a: two shock lands with an extra land drop — each
/// entry pauses and the window re-opens after each; then the spell is cast.
#[test]
fn gix_window_reopens_after_each_of_two_paused_land_entries() {
    let Some(db) = load_db() else { return };
    let (mut board, staged) = gix_board(1, |s| {
        s.add_enchantment_from_oracle(P0, "Extra Land", EXTRA_LAND);
        vec![
            s.add_real_card(P1, "Stomping Ground", Zone::Library, db),
            s.add_real_card(P1, "Breeding Pool", Zone::Library, db),
            s.add_real_card(P1, "Divination", Zone::Library, db),
        ]
    });
    let [first, second, divination] = staged[..] else {
        unreachable!()
    };
    activate_gix(&mut board, 3);
    for (land, remaining) in [
        (first, sorted(vec![second, divination])),
        (second, vec![divination]),
    ] {
        choose(&mut board.runner, Some(land));
        assert!(
            matches!(
                board.runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ),
            "reach-guard: {land:?} paused"
        );
        board
            .runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("answering the entry choice");
        assert_eq!(board.runner.state().objects[&land].zone, Zone::Battlefield);
        assert_eq!(
            window(board.runner.state()),
            (remaining, CardPlayMode::Play)
        );
    }
    choose(&mut board.runner, Some(divination));
    assert_parent_finished(&board.runner, board.gix);
}

/// CR 701.18b + CR 712.12: a spell//land modal card (Hagra Mauling // Hagra
/// Broodpit) in the batch offers both faces (Gix is a legal target for the
/// spell face); electing the land face plays it and the window re-opens.
#[test]
fn gix_window_plays_a_spell_land_modal_cards_land_face() {
    let Some(db) = load_db() else { return };
    let (mut board, staged) = gix_board(1, |s| {
        vec![
            s.add_real_card(P1, "Hagra Mauling", Zone::Library, db),
            s.add_real_card(P1, "Divination", Zone::Library, db),
        ]
    });
    let [mdfc, divination] = staged[..] else {
        unreachable!()
    };
    activate_gix(&mut board, 2);
    assert_eq!(
        window(board.runner.state()).0,
        sorted(vec![mdfc, divination]),
        "reach-guard: the modal card is a candidate"
    );
    choose(&mut board.runner, Some(mdfc));
    assert!(
        matches!(
            board.runner.state().waiting_for,
            WaitingFor::ModalFaceChoice { object_id, .. } if object_id == mdfc
        ),
        "reach-guard: both faces are legal, got {:?}",
        board.runner.state().waiting_for
    );
    let offered = legal_actions(board.runner.state());
    assert!(offered.contains(&GameAction::ChooseModalFace { back_face: true }));
    board
        .runner
        .act(GameAction::ChooseModalFace { back_face: true })
        .expect("electing the land face");
    let state = board.runner.state();
    assert_eq!(state.objects[&mdfc].zone, Zone::Battlefield);
    assert_eq!(lands_played(state, P0), 1);
    assert_eq!(window(state), (vec![divination], CardPlayMode::Play));
    choose(&mut board.runner, Some(divination));
    assert_parent_finished(&board.runner, board.gix);
}

/// "… from among cards exiled this way" (CR 608.2c: the instruction refers back
/// to its own exile step): a second activation's window offers only ITS batch —
/// a land left in exile by the first activation (declined) is not re-offered.
#[test]
fn a_second_gix_window_offers_only_its_own_batch() {
    let Some(db) = load_db() else { return };
    let (mut board, staged) = gix_board(2, |s| {
        vec![
            s.add_real_card(P1, "Forest", Zone::Library, db),
            s.add_real_card(P1, "Divination", Zone::Library, db),
        ]
    });
    let [forest, divination] = staged[..] else {
        unreachable!()
    };
    activate_gix(&mut board, 1);
    assert_eq!(
        window(board.runner.state()),
        (vec![forest], CardPlayMode::Play)
    );
    choose(&mut board.runner, None);
    assert_eq!(board.runner.state().objects[&forest].zone, Zone::Exile);
    activate_gix(&mut board, 1);
    assert_eq!(
        window(board.runner.state()),
        (vec![divination], CardPlayMode::Play),
        "the earlier batch's land is not re-offered"
    );
}

/// CR 305.3: activated on the opponent's turn, Gix's window offers the spell
/// (reach guard) but never the land.
#[test]
fn gix_offers_no_land_on_an_opponents_turn() {
    let Some(db) = load_db() else { return };
    let (mut board, staged) = gix_board(1, |s| {
        vec![
            s.add_real_card(P1, "Forest", Zone::Library, db),
            s.add_real_card(P1, "Divination", Zone::Library, db),
        ]
    });
    let [forest, divination] = staged[..] else {
        unreachable!()
    };
    {
        let state = board.runner.state_mut();
        state.active_player = P1;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    activate_gix(&mut board, 2);
    assert_eq!(
        window(board.runner.state()),
        (vec![divination], CardPlayMode::Play),
        "reach-guard: the spell is offered; the land is not"
    );
    assert!(board
        .runner
        .act(GameAction::FreeCastWindowChoice {
            selection: Some(forest)
        })
        .is_err());
}

/// Magus of the Mind: "Until end of turn, you may play lands and cast spells
/// from among cards exiled this way without paying their mana costs."
/// CR 611.2a + CR 701.18b: the dated Play grant lets the exiled land be played
/// as a land later in the turn. Magus's own exile clause ("exile the top X
/// cards, where X is one plus the number of spells cast this turn") is still a
/// parse gap (`where_x_binding`), so the batch is staged as exiled with Magus —
/// the link and per-turn record the exile resolver writes — and the printed
/// grant clause is what resolves.
#[test]
fn magus_of_the_minds_dated_grant_lets_the_exiled_land_be_played_this_turn() {
    let Some(db) = load_db() else { return };
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let magus = s.add_real_card(P0, "Magus of the Mind", Zone::Battlefield, db);
    s.with_library_top(P0, &["Bottom Filler"]);
    let forest = s.add_real_card(P0, "Forest", Zone::Exile, db);
    let mut runner = s.build();
    {
        let state = runner.state_mut();
        state.objects.get_mut(&magus).unwrap().summoning_sick = false;
        state.exile_links.push(ExileLink {
            exiled_id: forest,
            source_id: magus,
            kind: ExileLinkKind::TrackedBySource,
        });
        state
            .cards_exiled_with_source_this_turn
            .entry(magus)
            .or_default()
            .push(forest);
        state.players[0]
            .mana_pool
            .add(ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]));
    }
    let card_id = runner.state().objects[&forest].card_id;
    assert!(
        !legal_actions(runner.state()).contains(&GameAction::PlayLand {
            object_id: forest,
            card_id
        }),
        "reach-guard: before the grant the exiled land is not playable"
    );
    runner.activate(magus, 0).resolve();
    let state = runner.state();
    assert_eq!(
        state.objects[&magus].zone,
        Zone::Graveyard,
        "Magus was sacrificed"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "a dated grant opens no window"
    );
    assert!(
        legal_actions(state).contains(&GameAction::PlayLand {
            object_id: forest,
            card_id
        }),
        "the exiled land is playable as a land"
    );
    runner
        .act(GameAction::PlayLand {
            object_id: forest,
            card_id,
        })
        .expect("playing the exiled land");
    assert_eq!(runner.state().objects[&forest].zone, Zone::Battlefield);
    assert_eq!(lands_played(runner.state(), P0), 1);
}

/// Kayla's Music Box: "{W}, {T}: Look at the top card of your library, then
/// exile it face down." / "{T}: Until end of turn, you may play cards you own
/// exiled with Kayla's Music Box." CR 701.18b: the Play grant's residual filter
/// ("cards you own") admits an exiled land as well as a spell, so the land
/// survives the grant's linked-exile prefilter and is playable as a land.
#[test]
fn kaylas_music_box_lets_an_exiled_land_be_played() {
    let Some(db) = load_db() else { return };
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let music_box = s.add_real_card(P0, "Kayla's Music Box", Zone::Battlefield, db);
    s.with_library_top(P0, &["Bottom Filler"]);
    let forest = s.add_real_card(P0, "Forest", Zone::Library, db);
    let opt = s.add_real_card(P0, "Opt", Zone::Library, db);
    let mut runner = s.build();
    let add_mana = |runner: &mut GameRunner, mana: ManaType| {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            mana,
            ObjectId(0),
            false,
            vec![],
        ));
    };
    let untap = |runner: &mut GameRunner| {
        runner
            .state_mut()
            .objects
            .get_mut(&music_box)
            .unwrap()
            .tapped = false;
    };
    for card in [forest, opt] {
        put_on_top(&mut runner, P0, card);
        add_mana(&mut runner, ManaType::White);
        untap(&mut runner);
        runner.activate(music_box, 0).resolve();
        assert_eq!(
            runner.state().objects[&card].zone,
            Zone::Exile,
            "reach-guard: the Music Box exiled the top card"
        );
    }
    untap(&mut runner);
    runner.activate(music_box, 1).resolve();
    add_mana(&mut runner, ManaType::Blue);
    let state = runner.state();
    let offered = legal_actions(state);
    assert!(
        offered.iter().any(
            |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == opt)
        ),
        "reach-guard: the exiled spell is castable"
    );
    let card_id = state.objects[&forest].card_id;
    assert!(
        offered.contains(&GameAction::PlayLand {
            object_id: forest,
            card_id
        }),
        "the exiled land is playable as a land"
    );
    runner
        .act(GameAction::PlayLand {
            object_id: forest,
            card_id,
        })
        .expect("playing the exiled land");
    assert_eq!(runner.state().objects[&forest].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&forest].controller, P0);
    assert_eq!(lands_played(runner.state(), P0), 1);
}
/// An exiled batch with nothing playable (a lone land, land drop spent) opens no
/// window; the ability finishes and the land stays exiled.
#[test]
fn gix_with_nothing_playable_opens_no_window() {
    let Some(db) = load_db() else { return };
    let (mut board, staged) = gix_board(1, |s| {
        vec![s.add_real_card(P1, "Forest", Zone::Library, db)]
    });
    let [forest] = staged[..] else { unreachable!() };
    {
        let state = board.runner.state_mut();
        state.lands_played_this_turn = 1;
        state.players[0].lands_played_this_turn = 1;
    }
    activate_gix(&mut board, 1);
    assert_eq!(
        board.runner.state().objects[&forest].zone,
        Zone::Exile,
        "reach-guard: the land was exiled"
    );
    assert_parent_finished(&board.runner, board.gix);
}

// ---------------------------------------------------------------------------
// Land//land modal cards (CR 712.12) and the remaining entry-pause producers
// ---------------------------------------------------------------------------

/// Gix exiles Branchloft Pathway // Boulderloft Pathway and Divination, opens
/// the Play window, and the Pathway is chosen. Returns the board at the
/// resolution-owned `ModalFaceChoice`, with the AI's face actions checked.
fn gix_at_pathway_face_choice(
    db: &'static engine::database::card_db::CardDatabase,
) -> (GixBoard, ObjectId, ObjectId) {
    let (mut board, staged) = gix_board(1, |s| {
        vec![
            s.add_real_card(P1, "Branchloft Pathway", Zone::Library, db),
            s.add_real_card(P1, "Divination", Zone::Library, db),
        ]
    });
    let [pathway, divination] = staged[..] else {
        unreachable!()
    };
    assert!(
        board.runner.state().objects[&pathway].back_face.is_some(),
        "reach-guard: the Pathway carries its modal land back face"
    );
    activate_gix(&mut board, 2);
    assert_eq!(
        window(board.runner.state()),
        (sorted(vec![pathway, divination]), CardPlayMode::Play)
    );
    choose(&mut board.runner, Some(pathway));
    assert!(
        matches!(
            board.runner.state().waiting_for,
            WaitingFor::ModalFaceChoice { player: P0, object_id, .. } if object_id == pathway
        ),
        "CR 712.12: both land faces are electable, got {:?}",
        board.runner.state().waiting_for
    );
    let offered = legal_actions(board.runner.state());
    for back_face in [false, true] {
        assert!(
            offered.contains(&GameAction::ChooseModalFace { back_face }),
            "AI legal actions offer face back_face={back_face}"
        );
    }
    assert!(offered.contains(&GameAction::CancelCast));
    (board, pathway, divination)
}

/// CR 712.12 + CR 305.2a: electing the Pathway's front face plays Branchloft
/// Pathway as a land; the window re-opens with Divination, which is cast.
#[test]
fn gix_window_plays_a_pathways_front_face() {
    let Some(db) = load_db() else { return };
    let (mut board, pathway, divination) = gix_at_pathway_face_choice(db);
    board
        .runner
        .act(GameAction::ChooseModalFace { back_face: false })
        .expect("electing the front face");
    let state = board.runner.state();
    assert_eq!(state.objects[&pathway].zone, Zone::Battlefield);
    assert_eq!(state.objects[&pathway].name, "Branchloft Pathway");
    assert_eq!(state.objects[&pathway].controller, P0);
    assert_eq!(lands_played(state, P0), 1);
    assert_eq!(window(state), (vec![divination], CardPlayMode::Play));
    choose(&mut board.runner, Some(divination));
    assert_eq!(board.runner.state().objects[&divination].zone, Zone::Stack);
    assert_parent_finished(&board.runner, board.gix);
}

/// CR 712.12 + CR 305.2a: electing the Pathway's back face plays Boulderloft
/// Pathway as a land; the window re-opens with Divination.
#[test]
fn gix_window_plays_a_pathways_back_face() {
    let Some(db) = load_db() else { return };
    let (mut board, pathway, divination) = gix_at_pathway_face_choice(db);
    board
        .runner
        .act(GameAction::ChooseModalFace { back_face: true })
        .expect("electing the back face");
    let state = board.runner.state();
    assert_eq!(state.objects[&pathway].zone, Zone::Battlefield);
    assert_eq!(state.objects[&pathway].name, "Boulderloft Pathway");
    assert_eq!(lands_played(state, P0), 1);
    assert_eq!(window(state), (vec![divination], CardPlayMode::Play));
}

/// Cancelling the Pathway's face election re-offers the whole window (nothing
/// was played or counted); declining the window then leaves both cards exiled.
#[test]
fn cancelling_a_pathway_election_reoffers_the_window() {
    let Some(db) = load_db() else { return };
    let (mut board, pathway, divination) = gix_at_pathway_face_choice(db);
    board
        .runner
        .act(GameAction::CancelCast)
        .expect("cancelling the election");
    let state = board.runner.state();
    assert_eq!(state.objects[&pathway].zone, Zone::Exile);
    assert_eq!(lands_played(state, P0), 0);
    assert_eq!(
        window(state),
        (sorted(vec![pathway, divination]), CardPlayMode::Play)
    );
    choose(&mut board.runner, None);
    let state = board.runner.state();
    assert_eq!(state.objects[&pathway].zone, Zone::Exile);
    assert_eq!(state.objects[&divination].zone, Zone::Exile);
    assert_parent_finished(&board.runner, board.gix);
}

/// CR 614.12a (as-enters instruction, the post-replacement producer): Valgavoth's
/// Lair enters tapped and pauses on its color choice for P0. A save/reload at
/// the prompt keeps the window parked; after green is chosen the land is
/// tapped under P0, one land drop is spent, only Divination is re-offered, and
/// casting it finishes the ability with no parked work left.
#[test]
fn gix_window_reopens_after_an_as_enters_color_choice_across_a_reload() {
    let Some(db) = load_db() else { return };
    let (mut board, staged) = gix_board(1, |s| {
        vec![
            s.add_real_card(P1, "Valgavoth's Lair", Zone::Library, db),
            s.add_real_card(P1, "Divination", Zone::Library, db),
        ]
    });
    let [lair, divination] = staged[..] else {
        unreachable!()
    };
    activate_gix(&mut board, 2);
    assert_eq!(
        window(board.runner.state()),
        (sorted(vec![lair, divination]), CardPlayMode::Play)
    );
    choose(&mut board.runner, Some(lair));
    let WaitingFor::NamedChoice {
        player, options, ..
    } = board.runner.state().waiting_for.clone()
    else {
        panic!(
            "reach-guard: the Lair's entry paused on its color choice, got {:?}",
            board.runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "the color is chosen by the land's controller");
    let green = options
        .iter()
        .find(|option| option.eq_ignore_ascii_case("green"))
        .expect("green is offered")
        .clone();

    let saved = serde_json::to_string(board.runner.state()).expect("serializes");
    let restored: GameState = serde_json::from_str(&saved).expect("deserializes");
    let mut runner = GameRunner::from_state(restored);
    runner
        .act(GameAction::ChooseOption { choice: green })
        .expect("choosing green after reload");
    let state = runner.state();
    assert_eq!(state.objects[&lair].zone, Zone::Battlefield);
    assert_eq!(state.objects[&lair].controller, P0);
    assert!(state.objects[&lair].tapped, "This land enters tapped.");
    assert_eq!(lands_played(state, P0), 1);
    assert_eq!(window(state), (vec![divination], CardPlayMode::Play));
    choose(&mut runner, Some(divination));
    assert_eq!(runner.state().objects[&divination].zone, Zone::Stack);
    assert_parent_finished(&runner, board.gix);
    assert!(
        runner.state().active_ability_continuation().is_none(),
        "no parked window or parent work remains"
    );
}

/// CR 614.12a + CR 614.1c + CR 616.1 (delivery-tail producer), with printed
/// cards: Deanna Troi, Psychic Counsel ("Other creatures you control enter with
/// an additional +1/+1 counter on them.") gives the land creature Dryad Arbor a
/// counter as Gix's window plays it, and Hardened Scales and Doubling
/// Season both modify that counter placement, so its entry pauses on the
/// ordering choice in the delivery tail. Once answered, Dryad Arbor is on the
/// battlefield with its counters and the window re-opens with Divination.
#[test]
fn gix_window_reopens_after_a_delivery_tail_counter_order_choice() {
    use engine::types::counter::CounterType;

    const DEANNA_TROI: &str =
        "Other creatures you control enter with an additional +1/+1 counter on them.";

    let Some(db) = load_db() else { return };
    let (mut board, staged) = gix_board(1, |s| {
        s.add_creature_from_oracle(P0, "Deanna Troi, Psychic Counsel", 2, 2, DEANNA_TROI);
        s.add_real_card(P0, "Hardened Scales", Zone::Battlefield, db);
        s.add_real_card(P0, "Doubling Season", Zone::Battlefield, db);
        vec![
            s.add_real_card(P1, "Dryad Arbor", Zone::Library, db),
            s.add_real_card(P1, "Divination", Zone::Library, db),
        ]
    });
    let [arbor, divination] = staged[..] else {
        unreachable!()
    };
    activate_gix(&mut board, 2);
    assert_eq!(
        window(board.runner.state()),
        (sorted(vec![arbor, divination]), CardPlayMode::Play),
        "reach-guard: the land creature is a land candidate (CR 305.9)"
    );
    choose(&mut board.runner, Some(arbor));
    assert!(
        matches!(
            board.runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "reach-guard: the entry paused on the counter-order choice, got {:?}",
        board.runner.state().waiting_for
    );
    board
        .runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("ordering the counter modifiers");
    let state = board.runner.state();
    assert_eq!(state.objects[&arbor].zone, Zone::Battlefield);
    assert_eq!(state.objects[&arbor].controller, P0);
    assert_eq!(lands_played(state, P0), 1);
    let counters = state.objects[&arbor]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0);
    assert!(
        counters == 3 || counters == 4,
        "Deanna Troi's one counter, doubled and plus one in the chosen order: got {counters}"
    );
    assert_eq!(window(state), (vec![divination], CardPlayMode::Play));
    choose(&mut board.runner, Some(divination));
    assert_eq!(board.runner.state().objects[&divination].zone, Zone::Stack);
    assert_parent_finished(&board.runner, board.gix);
}
/// CR 712.12 (the election's land-face-only branch): Gix has shroud, so Hagra
/// Mauling's spell face has no legal target and its land face is the only
/// legal face. Choosing it plays Hagra Broodpit with no face prompt, and the
/// window re-opens with Divination.
#[test]
fn gix_window_plays_a_modal_cards_only_legal_land_face_without_a_prompt() {
    use engine::types::keywords::Keyword;

    let Some(db) = load_db() else { return };
    let (mut board, staged) = gix_board(1, |s| {
        vec![
            s.add_real_card(P1, "Hagra Mauling", Zone::Library, db),
            s.add_real_card(P1, "Divination", Zone::Library, db),
        ]
    });
    let [mdfc, divination] = staged[..] else {
        unreachable!()
    };
    {
        let gix = board
            .runner
            .state_mut()
            .objects
            .get_mut(&board.gix)
            .expect("Gix");
        gix.keywords.push(Keyword::Shroud);
        gix.base_keywords.push(Keyword::Shroud);
    }
    board.runner.state_mut().layers_dirty.mark_full();
    activate_gix(&mut board, 2);
    assert_eq!(
        window(board.runner.state()).0,
        sorted(vec![mdfc, divination]),
        "reach-guard: the land face alone keeps the modal card a candidate"
    );
    choose(&mut board.runner, Some(mdfc));
    let state = board.runner.state();
    assert!(
        !matches!(state.waiting_for, WaitingFor::ModalFaceChoice { .. }),
        "one legal face is no player decision"
    );
    assert_eq!(state.objects[&mdfc].zone, Zone::Battlefield);
    assert_eq!(lands_played(state, P0), 1);
    assert_eq!(window(state), (vec![divination], CardPlayMode::Play));
}

/// "… from among cards exiled this way" (CR 608.2c) + CR 614.12a: an older,
/// declined Gix batch (Opt) stays out of a later window. The later window's
/// shock land pauses, survives a reload, and re-opens with only its own
/// Divination, which is cast to finish the ability.
#[test]
fn a_parked_second_window_offers_only_its_own_batch_after_reload() {
    let Some(db) = load_db() else { return };
    let (mut board, staged) = gix_board(2, |s| {
        vec![
            s.add_real_card(P1, "Opt", Zone::Library, db),
            s.add_real_card(P1, "Stomping Ground", Zone::Library, db),
            s.add_real_card(P1, "Divination", Zone::Library, db),
        ]
    });
    let [opt, shock, divination] = staged[..] else {
        unreachable!()
    };
    activate_gix(&mut board, 1);
    assert_eq!(
        window(board.runner.state()),
        (vec![opt], CardPlayMode::Play)
    );
    choose(&mut board.runner, None);
    assert_eq!(board.runner.state().objects[&opt].zone, Zone::Exile);

    activate_gix(&mut board, 2);
    assert_eq!(
        window(board.runner.state()),
        (sorted(vec![shock, divination]), CardPlayMode::Play),
        "the older batch's Opt is not offered"
    );
    choose(&mut board.runner, Some(shock));
    assert!(
        matches!(
            board.runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "reach-guard: the shock land paused"
    );
    let saved = serde_json::to_string(board.runner.state()).expect("serializes");
    let restored: GameState = serde_json::from_str(&saved).expect("deserializes");
    let mut runner = GameRunner::from_state(restored);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("answering the entry choice after reload");
    assert_eq!(runner.state().objects[&shock].zone, Zone::Battlefield);
    assert_eq!(
        window(runner.state()),
        (vec![divination], CardPlayMode::Play),
        "only this batch's spell is re-offered"
    );
    choose(&mut runner, Some(divination));
    assert_eq!(runner.state().objects[&divination].zone, Zone::Stack);
    assert_eq!(runner.state().objects[&opt].zone, Zone::Exile);
    assert_parent_finished(&runner, board.gix);
}
