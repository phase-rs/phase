//! Uba Mask — "If a player would draw a card, that player exiles that card face
//! up instead. / Each player may play lands and cast spells from among cards
//! they exiled with this artifact this turn." (Oracle text verified vs Scryfall.)
//!
//! Covers the two building blocks the card composes:
//!   * CR 121.1 + CR 614.6: a draw replacement whose head is "exiles that card"
//!     exiles the top card of the *drawing* player's library.
//!   * CR 406.6 + CR 607.2b: an "each player may … cards they exiled with ~
//!     this turn" exile-play grant (`ExileCastGrantee::EachPlayerOwnExiles`,
//!     per-turn pool) lets each player use only the source-linked cards that
//!     player exiled (`GameObject::exiled_by`) — whoever owns them — and only
//!     during the turn they were exiled.

use engine::ai_support::legal_actions;
use engine::game::casting::spell_objects_available_to_cast;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, LibraryPosition, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{ExileLink, ExileLinkKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{ExileCardPool, ExileCastGrantee, StaticMode};
use engine::types::zones::Zone;

const UBA_MASK: &str = "If a player would draw a card, that player exiles that card face up instead.\nEach player may play lands and cast spells from among cards they exiled with this artifact this turn.";

const DRAW_A_CARD: &str = "Target player draws a card.";

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn in_hand(runner: &GameRunner, player: PlayerId, id: ObjectId) -> bool {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .is_some_and(|p| p.hand.contains(&id))
}

fn can_play_land(runner: &GameRunner, id: ObjectId) -> bool {
    legal_actions(runner.state())
        .iter()
        .any(|action| matches!(action, GameAction::PlayLand { object_id, .. } if *object_id == id))
}

/// Stage `card` as exiled with `source` this turn by `exiler`, whoever owns it —
/// the link and per-turn record the exile resolver writes, plus the recorded
/// exiling player (CR 406.6 + CR 607.2b). Uba Mask itself only ever exiles a
/// card from its drawer's own library, so owner and exiling player always
/// match there; staging is how the permission predicate is tested with the two
/// apart. The production writer is covered by this file's unstaged tests.
fn stage_exiled_with_source(
    runner: &mut GameRunner,
    card: ObjectId,
    source: ObjectId,
    exiler: PlayerId,
) {
    let state = runner.state_mut();
    state.exile_links.push(ExileLink {
        exiled_id: card,
        source_id: source,
        kind: ExileLinkKind::TrackedBySource,
    });
    state
        .cards_exiled_with_source_this_turn
        .entry(source)
        .or_default()
        .push(card);
    state.objects.get_mut(&card).unwrap().exiled_by = Some(exiler);
}

/// Make a staged card a land card (no rules text needed).
fn make_land(runner: &mut GameRunner, id: ObjectId) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Land);
    obj.base_card_types = obj.card_types.clone();
}

#[test]
fn uba_mask_parses_to_drawer_exile_top_and_each_player_grant() {
    let parsed = parse_oracle_text(UBA_MASK, "Uba Mask", &[], &["Artifact".to_string()], &[]);

    let replacement = parsed
        .replacements
        .first()
        .expect("Uba Mask must parse a draw replacement");
    let execute = replacement.execute.as_deref().expect("replacement execute");
    assert!(
        matches!(
            &*execute.effect,
            Effect::ExileTop {
                player: TargetFilter::PostReplacementDamageTarget,
                position: LibraryPosition::Top,
                face_down: false,
                ..
            }
        ),
        "\"that player exiles that card\" must exile the drawing player's top card, got {:?}",
        execute.effect
    );

    let grant = parsed
        .statics
        .iter()
        .find_map(|s| match &s.mode {
            StaticMode::ExileCastPermission { grantee, pool, .. } => Some((*grantee, *pool)),
            _ => None,
        })
        .expect("Uba Mask must parse an ExileCastPermission static");
    assert_eq!(
        grant,
        (
            ExileCastGrantee::EachPlayerOwnExiles,
            ExileCardPool::ThisTurn
        )
    );
    assert!(
        parsed.abilities.is_empty(),
        "no Unimplemented fallback may remain: {:?}",
        parsed.abilities
    );
}

/// CR 121.1 + CR 614.6 + CR 305.1: The controller's own draw is replaced by
/// exiling that card face up, and the controller may play it as a land this
/// turn — the opponent may not.
#[test]
fn controller_draw_is_exiled_and_playable_as_land_this_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_artifact_from_oracle(P0, "Uba Mask", UBA_MASK);
    let land = scenario.add_card_to_library_top(P0, "Masked Forest");
    let draw = scenario
        .add_spell_to_hand_from_oracle(P0, "Peek", true, DRAW_A_CARD)
        .id();
    let mut runner = scenario.build();
    make_land(&mut runner, land);

    runner.cast(draw).target_player(P0).resolve();

    assert_eq!(
        zone(&runner, land),
        Zone::Exile,
        "the draw must exile the card"
    );
    assert!(
        !in_hand(&runner, P0, land),
        "the replaced draw must not happen"
    );
    assert!(
        !runner.state().objects[&land].face_down,
        "CR 406.3: Uba Mask exiles the card face up"
    );

    assert!(
        can_play_land(&runner, land),
        "the exiling player may play the exiled land this turn"
    );
    let card_id = runner.state().objects[&land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect("playing the exiled land must succeed");
    assert_eq!(zone(&runner, land), Zone::Battlefield);
}

/// CR 406.6 + CR 607.2b: "Each player may … cards *they* exiled" — an
/// opponent's exiled draw was exiled by that opponent ("that player exiles"),
/// so it is castable by them, never by Uba Mask's controller.
#[test]
fn opponent_draw_is_castable_only_by_that_opponent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_artifact_from_oracle(P0, "Uba Mask", UBA_MASK);
    let spell = scenario
        .add_spell_to_library_top(P1, "Opponent Instant", true)
        .id();
    let draw = scenario
        .add_spell_to_hand_from_oracle(P0, "Peek", true, DRAW_A_CARD)
        .id();
    let mut runner = scenario.build();

    runner.cast(draw).target_player(P1).resolve();

    assert_eq!(zone(&runner, spell), Zone::Exile);
    assert!(!in_hand(&runner, P1, spell));
    assert_eq!(
        runner.state().objects[&spell].exiled_by,
        Some(P1),
        "CR 608.2c: \"that player exiles\" — the drawing player performed the exile"
    );
    assert!(
        spell_objects_available_to_cast(runner.state(), P1).contains(&spell),
        "the player who exiled the card may cast it"
    );
    assert!(
        !spell_objects_available_to_cast(runner.state(), P0).contains(&spell),
        "Uba Mask's controller may not cast a card another player exiled"
    );
}

/// CR 121.1 + CR 614.6 + CR 608.2c + CR 406.6: end to end on the opponent's
/// own turn, with no staged state. P0 controls Uba Mask; on P1's turn, P1's
/// draw-step draw and a later spell-driven draw are both replaced, P1 is
/// recorded as the exiling player, and P1 then plays the land and casts the
/// spell from exile through the real land-play and casting pipelines. P0 may
/// use neither.
#[test]
fn opponent_plays_and_casts_own_exiled_draws_on_their_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_artifact_from_oracle(P0, "Uba Mask", UBA_MASK);
    scenario.with_library_top(P1, &["Filler A", "Filler B"]);
    let spell = scenario
        .add_spell_to_library_top(P1, "Opponent Instant", true)
        .id();
    let land = scenario.add_card_to_library_top(P1, "Masked Plains");
    let peek = scenario
        .add_spell_to_hand_from_oracle(P1, "Peek", true, DRAW_A_CARD)
        .id();
    let mut runner = scenario.build();
    make_land(&mut runner, land);

    // CR 504.1: P1's draw step draw is replaced — P1 exiles the land instead.
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P1, "reach-guard: P1's turn");
    assert_eq!(zone(&runner, land), Zone::Exile);
    assert!(!in_hand(&runner, P1, land));
    assert_eq!(
        runner.state().objects[&land].exiled_by,
        Some(P1),
        "CR 608.2c: \"that player exiles\" — the drawing player exiled it"
    );

    // A spell-driven draw is replaced the same way.
    runner.cast(peek).target_player(P1).resolve();
    assert_eq!(zone(&runner, spell), Zone::Exile);
    assert_eq!(runner.state().objects[&spell].exiled_by, Some(P1));
    assert!(
        !spell_objects_available_to_cast(runner.state(), P0).contains(&spell),
        "Uba Mask's controller did not exile it, so may not cast it"
    );

    // CR 305.1: P1 plays the exiled land through the land-play pipeline.
    assert!(can_play_land(&runner, land));
    let card_id = runner.state().objects[&land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect("P1 may play the land P1 exiled with Uba Mask");
    assert_eq!(zone(&runner, land), Zone::Battlefield);
    assert_eq!(runner.state().objects[&land].controller, P1);

    // CR 601.2: P1 casts the exiled spell through the casting pipeline.
    runner.cast(spell).resolve();
    assert_eq!(
        zone(&runner, spell),
        Zone::Graveyard,
        "the spell was cast from exile and resolved"
    );
}

/// CR 406.6: "…exiled with this artifact *this turn*" — a card exiled on an
/// earlier turn stays in exile and is no longer playable.
#[test]
fn exiled_card_is_not_playable_on_a_later_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_artifact_from_oracle(P0, "Uba Mask", UBA_MASK);
    let spell = scenario
        .add_spell_to_library_top(P0, "Old Instant", true)
        .id();
    let draw = scenario
        .add_spell_to_hand_from_oracle(P0, "Peek", true, DRAW_A_CARD)
        .id();
    let mut runner = scenario.build();

    runner.cast(draw).target_player(P0).resolve();
    assert!(spell_objects_available_to_cast(runner.state(), P0).contains(&spell));

    let mut events = Vec::new();
    engine::game::turns::start_next_turn(runner.state_mut(), &mut events);

    assert_eq!(zone(&runner, spell), Zone::Exile, "the card stays exiled");
    for player in [P0, P1] {
        assert!(
            !spell_objects_available_to_cast(runner.state(), player).contains(&spell),
            "a card exiled on a previous turn must not be castable"
        );
    }
}

/// CR 406.6 + CR 607.2b: the grant follows the exiling player, not ownership.
/// P1 exiled a P0-owned card and P0 exiled a P1-owned card, both with Uba Mask:
/// P1 may cast the P0-owned card through the cast pipeline, and neither owner
/// may cast their own card that the other player exiled.
#[test]
fn exiling_player_not_owner_may_cast_through_the_cast_pipeline() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let uba = scenario
        .add_artifact_from_oracle(P0, "Uba Mask", UBA_MASK)
        .id();
    let p0_owned = scenario.add_spell_to_exile(P0, "P0 Instant", true).id();
    let p1_owned = scenario.add_spell_to_exile(P1, "P1 Instant", true).id();
    let mut runner = scenario.build();
    stage_exiled_with_source(&mut runner, p0_owned, uba, P1);
    stage_exiled_with_source(&mut runner, p1_owned, uba, P0);

    let p1_castable = spell_objects_available_to_cast(runner.state(), P1);
    let p0_castable = spell_objects_available_to_cast(runner.state(), P0);
    assert!(
        p1_castable.contains(&p0_owned),
        "P1 exiled it, so P1 may cast it"
    );
    assert!(
        !p1_castable.contains(&p1_owned),
        "P1 owns it but P0 exiled it, so P1 may not cast it"
    );
    assert!(
        p0_castable.contains(&p1_owned),
        "P0 exiled it, so P0 may cast it"
    );
    assert!(
        !p0_castable.contains(&p0_owned),
        "P0 owns it but P1 exiled it, so P0 may not cast it"
    );

    // CR 117.3c: hand P1 priority and cast the P0-owned card through the full
    // casting pipeline (CR 601.2).
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    runner.cast(p0_owned).resolve();

    assert_eq!(
        zone(&runner, p0_owned),
        Zone::Graveyard,
        "the cast spell resolved and went to its owner's graveyard"
    );
    assert_eq!(
        zone(&runner, p1_owned),
        Zone::Exile,
        "the other card was never cast"
    );
}

/// CR 305.1 + CR 406.6 + CR 607.2b: land plays follow the exiling player too.
/// On P0's turn, P0 may play the P1-owned land P0 exiled with Uba Mask, but not
/// the P0-owned land P1 exiled.
#[test]
fn exiling_player_not_owner_may_play_the_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let uba = scenario
        .add_artifact_from_oracle(P0, "Uba Mask", UBA_MASK)
        .id();
    let p0_owned = scenario.add_land_to_exile(P0, "P0 Land").id();
    let p1_owned = scenario.add_land_to_exile(P1, "P1 Land").id();
    let mut runner = scenario.build();
    stage_exiled_with_source(&mut runner, p0_owned, uba, P1);
    stage_exiled_with_source(&mut runner, p1_owned, uba, P0);

    assert!(
        !can_play_land(&runner, p0_owned),
        "P0 owns it but P1 exiled it, so P0 may not play it"
    );
    assert!(
        can_play_land(&runner, p1_owned),
        "P0 exiled it, so P0 may play it"
    );

    let card_id = runner.state().objects[&p1_owned].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: p1_owned,
            card_id,
        })
        .expect("playing the land P0 exiled must succeed");
    assert_eq!(zone(&runner, p1_owned), Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&p1_owned].controller,
        P0,
        "CR 305.1: the player who plays a land controls it"
    );
    assert_eq!(zone(&runner, p0_owned), Zone::Exile);
}
