//! CR 104.1: a game that ends while a resolution is paused on a player's choice
//! retires that resolution; nothing of it is owed to a finished game.
//!
//! A resolving spell or ability parks a prompt (a "you may" offer, a cipher
//! offer, a land choice inside a repeat, a CR 616.1 replacement choice, a choice
//! inside a spell it instructs a player to cast, ...) and in the same action the
//! game ends immediately: a player concedes (CR 104.3a), an effect says a player
//! wins (CR 104.2b, here through Laboratory Maniac's CR 614.6 replacement of a
//! draw), or a loop of mandatory actions draws the game (CR 104.4b). Before the
//! fix the resolution's frames stayed on top of the resolution stack under
//! `WaitingFor::GameOver` (or the best-of-three sideboarding prompt), and a debug
//! build panicked at the action boundary with "canonical resolution frames must
//! validate: top frame OptionalEffect does not match waiting prompt GameOver".
//! Release builds skipped that check and kept the stranded frame and its live
//! carrier in the terminal state.
//!
//! The recorded game result (`GameState::game_end`) authorizes retirement at the
//! action boundary (`engine::settle_finished_game`), after every resolver has
//! unwound. An admitted trusted match forfeit (`MatchPhase::Completed` together
//! with `GameState::match_forfeit_result`) authorizes retirement at
//! `match_flow::apply_trusted_match_forfeit`'s own synchronous terminal boundary.
//! Those trusted rows are application-policy match completion without a
//! `game_end`, not a CR 104.3a game loss. Neither authority is the wait alone.
//! The work the resolution parked outside the resolution stack goes first: its own paused
//! cost cursors, the CR 616.1 replacement choices its moves raised, its legacy
//! suspended-instruction batches, a CR 608.2g instructed cast's parked cost,
//! payment prompt and reversal record, and the sessions it opened (a library
//! search, CR 701.23a; a private look, CR 701.20e; a reveal, CR 701.20a). Then its
//! frames, then its carrier. Every
//! terminal row asserts the fields themselves (no frame, no carrier, no owed final
//! part, no parked work) and that the terminal state round-trips through
//! persistence, so each one discriminates even with the debug invariant compiled
//! out. The multiplayer rows check that a game that continues keeps the
//! survivors' resolution.

use engine::game::engine::apply;
use engine::game::match_flow::apply_trusted_match_forfeit;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::move_to_zone;
use engine::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, Effect, PlayerFilter,
    QuantityExpr, ReplacementDefinition, ReplacementMode, ReturnResultId, SacrificeCost,
    SubAbilityLink, TargetFilter, TargetRef,
};
use engine::types::actions::{CastChoice, GameAction, ResolutionOptionalPaymentChoice};
use engine::types::events::GameEvent;
use engine::types::format::{ChallengeDeck, FormatConfig};
use engine::types::game_state::{
    ActiveSearchDecisionAuthority, GameState, PayCostKind, PendingCostMoveResume,
    PendingSacrificeCostCompletion, PersistedGameState, PersistedRestoreFinalization,
    StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::match_config::{
    MatchConfig, MatchForfeitCause, MatchForfeitResult, MatchPhase, MatchScore, MatchType,
};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::resolution::FrameKind;
use engine::types::un_actions::{UnAction, UnTell};
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const P2: PlayerId = PlayerId(2);

const VRASKAS_SCORN: &str = "Target opponent loses 4 life. You may search your library and/or graveyard for a card named Vraska, Scheming Gorgon, reveal it, and put it into your hand. If you search your library this way, shuffle.";
const SHADOW_SLICE: &str = "Target opponent loses 3 life.\nCipher (Then you may exile this spell card encoded on a creature you control. Whenever that creature deals combat damage to a player, its controller may cast a copy of the encoded card without paying its mana cost.)";
const COLOSSUS: &str = "Trample\nCultivator Colossus's power and toughness are each equal to the number of lands you control.\nWhen this creature enters, you may put a land card from your hand onto the battlefield tapped. If you do, draw a card and repeat this process.";
const LABORATORY_MANIAC: &str =
    "If you would draw a card while your library has no cards in it, you win the game instead.";
const TAINTED_PACT: &str = "Exile the top card of your library. You may put that card into your hand unless it has the same name as another card exiled this way. Repeat this process until you put a card into your hand or you exile two cards with the same name, whichever comes first.";
const LEYLINE_OF_THE_VOID: &str = "If this card is in your opening hand, you may begin the game with it on the battlefield.\nIf a card would be put into an opponent's graveyard from anywhere, exile it instead.";
const REST_IN_PEACE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";
const HYMN_TO_TOURACH: &str = "Target player discards two cards at random.";
const VIOLENT_OUTBURST: &str = "Cascade (When you cast this spell, exile cards from the top of your library until you exile a nonland card that costs less. You may cast it without paying its mana cost. Put the exiled cards on the bottom in a random order.)\nCreatures you control get +1/+0 until end of turn.";
const VILLAGE_RITES: &str =
    "As an additional cost to cast this spell, sacrifice a creature.\nDraw two cards.";
const SHOCK: &str = "Shock deals 2 damage to any target.";
const DEMONIC_TUTOR: &str =
    "Search your library for a card, put that card into your hand, then shuffle.";
const SATYR_WAYFINDER: &str = "When this creature enters, reveal the top four cards of your library. You may put a land card from among them into your hand. Put the rest into your graveyard.";
const BIRTHING_RITUAL: &str = "At the beginning of your end step, if you control a creature, look at the top seven cards of your library. Then you may sacrifice a creature. If you do, you may put a creature card with mana value X or less from among those cards onto the battlefield, where X is 1 plus the sacrificed creature's mana value. Put the rest on the bottom of your library in a random order.";
/// Mer Man's verbatim Oracle text. Its optional ETB trigger supplies a triggered
/// ability carrier with a firing and an `OptionalEffect` frame.
const ENTERS_MAY_DRAW: &str = "When this creature enters, you may draw a card.";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn frame_kinds(runner: &GameRunner) -> Vec<FrameKind> {
    runner
        .state()
        .resolution_stack
        .iter()
        .map(|frame| frame.kind())
        .collect()
}

/// Whether the serialized state carries no value under `key`. Used for the
/// `pub(crate)` fields an integration test cannot read directly; both are
/// skipped when `None`.
fn json_key_absent(state: &GameState, key: &str) -> bool {
    serde_json::to_value(state)
        .expect("the state serializes")
        .get(key)
        .is_none()
}

/// The terminal state survives a save and restore: capture, decode (which runs
/// every decode validator and, in debug builds, the runtime resolution
/// invariants), admit, and finalize.
fn assert_round_trips(runner: &GameRunner) -> GameState {
    let json = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("capture: the state serializes");
    let restored = serde_json::from_str::<PersistedGameState>(&json)
        .expect("decode: the persisted state decodes")
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("admit: the persisted state is admissible")
        .finalize_after_rehydration(|_| Ok(()))
        .expect("finalize: the restored state is publishable");
    assert_eq!(
        restored.un_actions.pending_offers,
        runner.state().un_actions.pending_offers,
        "persistence preserves the exact queued Gotcha offers, including an empty terminal queue"
    );
    assert_eq!(
        restored.un_actions.gotcha_offers,
        runner.state().un_actions.gotcha_offers,
        "persistence preserves the exact open Gotcha offers, including an empty terminal window"
    );
    assert_eq!(
        restored.pending_single_discard,
        runner.state().pending_single_discard
    );
    assert_eq!(
        restored.pending_damage_post_loss,
        runner.state().pending_damage_post_loss
    );
    restored
}

/// CR 104.1: none of the work a resolution parked outside the resolution stack,
/// and none of the sessions it opened (a library search, CR 701.23a; a private
/// look, CR 701.20e; a reveal, CR 701.20a), is left in a finished game.
fn assert_no_parked_resolution_work(state: &GameState) {
    let parked = [
        ("pending_cast", state.pending_cast.is_some()),
        (
            "pending_cost_move_resume",
            state.pending_cost_move_resume.is_some(),
        ),
        (
            "pending_deferred_life_cost_resume",
            state.pending_deferred_life_cost_resume.is_some(),
        ),
        (
            "pending_discard_for_cost",
            state.pending_discard_for_cost.is_some(),
        ),
        (
            "pending_single_discard",
            state.pending_single_discard.is_some(),
        ),
        (
            "pending_damage_post_loss",
            state.pending_damage_post_loss.is_some(),
        ),
        ("pending_replacement", state.pending_replacement.is_some()),
        (
            "replacement_may_cost_paused",
            state.replacement_may_cost_paused,
        ),
        (
            "post_replacement_token_choice_applied",
            state.post_replacement_token_choice_applied.is_some(),
        ),
        (
            "post_replacement_token_substitution_count",
            state.post_replacement_token_substitution_count.is_some(),
        ),
        (
            "pending_liminal_entry_resume",
            state.pending_liminal_entry_resume.is_some(),
        ),
        ("liminal_entries", !state.liminal_entries.is_empty()),
        ("deferred_triggers", !state.deferred_triggers.is_empty()),
        (
            "pending_trigger_order",
            state.pending_trigger_order.is_some(),
        ),
        ("payment_transaction", state.payment_transaction.is_some()),
        (
            "pending_player_scope_unless_payment",
            state.pending_player_scope_unless_payment.is_some(),
        ),
        (
            "pending_player_scope_sacrifice_choice",
            state.pending_player_scope_sacrifice_choice.is_some(),
        ),
        (
            "pending_mass_library_order_choice",
            state.pending_mass_library_order_choice.is_some(),
        ),
        (
            "pending_discard_batch",
            state.pending_discard_batch.is_some(),
        ),
        (
            "pending_exile_from_top_until",
            state.pending_exile_from_top_until.is_some(),
        ),
        (
            "pending_scoped_library_search",
            state.pending_scoped_library_search.is_some(),
        ),
        (
            "pending_library_search_delivery",
            state.pending_library_search_delivery.is_some(),
        ),
        (
            "pending_search_found_batch",
            state.pending_search_found_batch.is_some(),
        ),
        (
            "pending_die_roll_instruction",
            state.pending_die_roll_instruction.is_some(),
        ),
        (
            "pending_resolution_completion",
            state.pending_resolution_completion.is_some(),
        ),
        (
            "entering_aura_authority",
            state.entering_aura_authority.is_some(),
        ),
        (
            "active_library_searches",
            !state.active_library_searches.is_empty(),
        ),
        (
            "active_search_decision_controls",
            !state.active_search_decision_controls.is_empty(),
        ),
        (
            "private_look",
            state.private_look_player.is_some() || !state.private_look_ids.is_empty(),
        ),
        ("revealed_cards", !state.revealed_cards.is_empty()),
        (
            "spell_cast_reversal",
            !json_key_absent(state, "spell_cast_reversal"),
        ),
        (
            "pending_triggered_mana_resume",
            !json_key_absent(state, "pending_triggered_mana_resume"),
        ),
    ];
    let left: Vec<&str> = parked
        .iter()
        .filter(|(_, present)| *present)
        .map(|(name, _)| *name)
        .collect();
    assert!(
        left.is_empty(),
        "a terminal state keeps none of its resolution's parked work, found {left:?}"
    );
}

/// CR 104.1: the finished game's terminal record names `winner`, and nothing of
/// the resolution it was in the middle of is left: no frame, no carrier, no owed
/// final part, no return-result occurrence, no parked work. The terminal state
/// round-trips through persistence.
fn assert_resolution_retired(runner: &GameRunner, winner: Option<PlayerId>) {
    let state = runner.state();
    assert_eq!(
        state.game_end.as_ref().map(|end| end.winner),
        Some(winner),
        "the game ended with the expected result"
    );
    assert_resolution_state_retired(state);
    assert_round_trips(runner);
}

/// Common assertions after terminal retirement or normal resolution settlement.
/// Terminal retirement does not run an abandoned spell's CR 608.2n final part.
fn assert_resolution_state_retired(state: &GameState) {
    assert!(
        state.resolution_stack.is_empty(),
        "a terminal state keeps no resolution frame, found {:?}",
        state
            .resolution_stack
            .iter()
            .map(|frame| frame.kind())
            .collect::<Vec<_>>()
    );
    assert!(
        state.resolving_stack_entry.is_none(),
        "a terminal state keeps no resolution carrier"
    );
    assert!(
        state.owed_spell_final_part().is_none(),
        "a terminal state owes no spell final part"
    );
    assert!(
        json_key_absent(state, "resolving_stack_object"),
        "a terminal state keeps no resolving object pin"
    );
    assert!(
        state.resolution_source_relatch.is_none(),
        "a terminal state keeps no resolution source relatch"
    );
    assert!(
        state.return_result_frames.is_empty() && state.active_return_result_occurrence.is_none(),
        "a finished game keeps no return-result occurrence"
    );
    assert!(
        state.pending_miracle_offers.is_empty(),
        "a finished game keeps no queued miracle offer"
    );
    assert!(
        state.pending_paradigm_remaining_offers.is_none(),
        "a finished game keeps no remaining paradigm offer"
    );
    assert!(
        state.un_actions.pending_offers.is_empty(),
        "a finished game keeps no queued Gotcha offer"
    );
    assert!(
        state.un_actions.gotcha_offers.is_empty(),
        "a finished game keeps no open Gotcha offer"
    );
    assert_no_parked_resolution_work(state);
}

/// The trusted boundary freezes the existing match policy, retires the abandoned
/// resolution and publishes the same durable result after real persistence.
fn assert_trusted_match_retired(
    runner: &GameRunner,
    result: MatchForfeitResult,
    score: MatchScore,
    game_number: u8,
) {
    assert_resolution_state_retired(runner.state());
    let restored = assert_round_trips(runner);
    for state in [runner.state(), &restored] {
        assert_eq!(state.match_phase, MatchPhase::Completed);
        assert_eq!(state.match_forfeit_result, Some(result));
        assert_eq!(state.match_score, score);
        assert_eq!(state.game_number, game_number);
        assert_eq!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(result.winner)
            }
        );
        assert!(state.sideboard_submitted.is_empty());
        assert_eq!(state.next_game_chooser, None);
        assert_resolution_state_retired(state);
    }
    // `game_end` is serde-skipped; the separate match result is durable.
    assert!(restored.game_end.is_none());
}

fn assert_match_forfeit_refused(runner: &mut GameRunner, player: PlayerId, expected_error: &str) {
    let before = runner.state().clone();
    let persisted_before = serde_json::to_value(PersistedGameState::capture(before.clone()))
        .expect("the pending state can be captured");

    let error =
        apply_trusted_match_forfeit(runner.state_mut(), player, MatchForfeitCause::HostKick)
            .expect_err("the unadmitted request is refused");

    assert_eq!(error, expected_error);
    assert_eq!(runner.state(), &before);
    // GameState equality deliberately omits the transient game result.
    assert_eq!(runner.state().game_end, before.game_end);
    assert_eq!(runner.state().state_revision, before.state_revision);
    assert_eq!(runner.state().resolution_stack, before.resolution_stack);
    assert_eq!(
        runner.state().resolving_stack_entry,
        before.resolving_stack_entry
    );
    assert_eq!(
        runner.state().active_library_searches,
        before.active_library_searches
    );
    assert_eq!(
        runner.state().active_search_decision_controls,
        before.active_search_decision_controls
    );
    assert_eq!(
        serde_json::to_value(PersistedGameState::capture(runner.state().clone()))
            .expect("the refused state can still be captured"),
        persisted_before
    );
}

/// The terminal wait is `GameOver { winner }` and the resolution is retired.
fn assert_finished_game_retired_its_resolution(runner: &GameRunner, winner: Option<PlayerId>) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::GameOver { winner: w } if w == winner
        ),
        "expected GameOver {{ winner: {winner:?} }}, got {:?}",
        runner.state().waiting_for
    );
    assert_resolution_retired(runner, winner);
}

fn board(players: u8) -> GameScenario {
    let mut scenario = if players == 2 {
        GameScenario::new()
    } else {
        GameScenario::new_n_player(players, 42)
    };
    scenario.at_phase(Phase::PreCombatMain);
    for seat in 0..players {
        scenario.with_library_top(PlayerId(seat), &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }
    scenario
}

fn add_swamps(scenario: &mut GameScenario, count: usize) {
    for _ in 0..count {
        scenario.add_basic_land(P0, ManaColor::Black);
    }
}

fn pass(runner: &mut GameRunner) -> Vec<GameEvent> {
    runner
        .act(GameAction::PassPriority)
        .expect("the pass is accepted")
        .events
}

/// Pass priority until the wait is something other than `Priority`.
fn pass_until_prompt(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..8 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        events.extend(pass(runner));
    }
    events
}

fn concede(runner: &mut GameRunner, player: PlayerId) {
    runner
        .act(GameAction::Concede { player_id: player })
        .expect("a concession is accepted");
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn carrier_is_live(runner: &GameRunner) -> bool {
    runner.state().resolving_stack_entry.is_some()
}

fn assert_live_spell_carrier(runner: &GameRunner, spell: ObjectId) {
    let carrier = runner
        .state()
        .resolving_stack_entry
        .as_ref()
        .expect("reach guard: the spell's resolution carrier is live");
    assert_eq!(
        (carrier.id, carrier.source_id, carrier.controller),
        (spell, spell, P0)
    );
    assert!(matches!(
        &carrier.kind,
        StackEntryKind::Spell {
            ability: Some(_),
            ..
        }
    ));
    assert!(
        !json_key_absent(runner.state(), "resolving_stack_object"),
        "reach guard: the carrier pins the resolving spell object"
    );
}

fn assert_face_text(name: &str, text: &str) {
    let db = shared_card_db().expect("production fixtures must load");
    assert_eq!(
        db.get_face_by_name(name)
            .unwrap_or_else(|| panic!("{name} is in the card fixture"))
            .oracle_text
            .as_deref(),
        Some(text),
        "{name} is loaded with its verbatim Oracle text"
    );
}

/// Whether `id` is on the stack as a CR 601.2a announcement that has not yet
/// been finalized: a spell entry with no resolved ability.
fn announced_spell_on_stack(state: &GameState, id: ObjectId) -> bool {
    state.stack.iter().any(|entry| {
        entry.id == id && matches!(&entry.kind, StackEntryKind::Spell { ability: None, .. })
    })
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Cast Vraska's Scorn at P1 (20 life) and pass until its search offer is asked.
fn vraskas_scorn_at_its_offer(match_config: Option<MatchConfig>) -> GameRunner {
    assert_face_text("Vraska's Scorn", VRASKAS_SCORN);
    let mut scenario = board(2);
    add_swamps(&mut scenario, 4);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Vraska's Scorn", false, VRASKAS_SCORN)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black; 2],
            generic: 2,
        })
        .id();
    let mut runner = scenario.build();
    if let Some(config) = match_config {
        runner.state_mut().set_match_config(config);
    }
    runner.cast(spell).target_player(P1).commit();
    pass(&mut runner);
    pass(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player, source_id, .. }
                if player == P0 && source_id == spell
        ),
        "reach guard: the search offer is asked, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(frame_kinds(&runner), vec![FrameKind::OptionalEffect]);
    assert_live_spell_carrier(&runner, spell);
    assert!(runner.state().owed_spell_final_part().is_some());
    assert!(runner.state().game_end.is_none());
    assert!(runner.state().match_forfeit_result.is_none());
    // CR 608.2c + CR 608.2d: the first instruction happens before the may choice.
    assert_eq!(runner.state().players[1].life, 16);
    runner
}

/// CR 702.99a: Shadow Slice's last instruction offers either of two distinct
/// creatures P0 controls. The Oracle line break and Cipher keyword are retained.
fn shadow_slice_at_its_offer(
    match_config: Option<MatchConfig>,
) -> (GameRunner, ObjectId, [ObjectId; 2]) {
    assert_face_text("Shadow Slice", SHADOW_SLICE);
    let mut scenario = board(2);
    add_swamps(&mut scenario, 5);
    let hosts = [
        scenario.add_creature(P0, "First Host", 2, 2).id(),
        scenario.add_creature(P0, "Second Host", 3, 3).id(),
    ];
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Shadow Slice", false, SHADOW_SLICE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 4,
        })
        .with_keyword(Keyword::Cipher)
        .id();
    let mut runner = scenario.build();
    if let Some(config) = match_config {
        runner.state_mut().set_match_config(config);
    }
    runner.cast(spell).target_player(P1).commit();
    pass(&mut runner);
    pass(&mut runner);
    let WaitingFor::CipherEncodeChoice {
        player,
        card_id,
        creatures,
    } = &runner.state().waiting_for
    else {
        panic!(
            "reach guard: the cipher offer is asked, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((*player, *card_id), (P0, spell));
    assert_eq!(creatures.len(), 2);
    assert!(hosts.iter().all(|host| creatures.contains(host)));
    assert_eq!(frame_kinds(&runner), vec![FrameKind::CipherEncode]);
    assert_live_spell_carrier(&runner, spell);
    assert!(runner.state().owed_spell_final_part().is_none());
    assert!(runner.state().game_end.is_none());
    assert!(runner.state().match_forfeit_result.is_none());
    // CR 608.2c: the loss precedes the optional encoding instruction.
    assert_eq!(runner.state().players[1].life, 17);
    (runner, spell, hosts)
}

/// Cast Mer Man and pass until its enters trigger asks the "you may".
fn enters_may_draw_at_its_offer(players: u8) -> GameRunner {
    let mut scenario = board(players);
    for _ in 0..5 {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }
    let creature = scenario
        .add_creature_to_hand_from_oracle(P0, "Mer Man", 3, 3, ENTERS_MAY_DRAW)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 4,
        })
        .id();
    let mut runner = scenario.build();
    runner.cast(creature).commit();
    pass_until_prompt(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player, .. } if player == P0
        ),
        "reach guard: the draw offer is asked, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(frame_kinds(&runner), vec![FrameKind::OptionalEffect]);
    assert!(
        runner
            .state()
            .resolving_stack_entry
            .as_ref()
            .is_some_and(|entry| matches!(
                &entry.kind,
                StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == creature
            )),
        "reach guard: the carrier is the creature's enters trigger"
    );
    assert!(runner.state().game_end.is_none());
    runner
}

/// Copied from `resolution_optional_payments.rs`.
fn sacrifice(count: u32) -> AbilityCost {
    AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::Any, count))
}

/// Copied from `resolution_optional_payments.rs` (synthetic): an optional
/// `PayCost` of "sacrifice `count`" or {99}, followed by a per-opponent
/// "loses 1 life" and a "gain 3 life" payoff that needs the payment. The
/// `return_result` parameter is added here for
/// `a_parked_sacrifice_cursor_does_not_outlive_its_return_result_occurrence`.
/// Birthing Ritual (full Oracle text in `BIRTHING_RITUAL`) supplies the nearest
/// printed optional sacrifice during resolution: "Then you may sacrifice a
/// creature. If you do, you may put a creature card with mana value X or less
/// from among those cards onto the battlefield, where X is 1 plus the sacrificed
/// creature's mana value." No card in the checked AtomicCards corpus combines a
/// sacrifice with a {99} alternative. This reduced fixture needs two sacrifices
/// to park a partly paid cursor, an unaffordable alternative to expose its branch,
/// and a conditional payoff to exercise the cursor's return-result occurrence.
/// It tests terminal retirement of that composed state, not a printed card's
/// complete ability or parser support.
fn optional_sacrifice_spell(
    payer: TargetFilter,
    count: u32,
    return_result: Option<ReturnResultId>,
) -> AbilityDefinition {
    let mut root = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PayCost {
            cost: AbilityCost::OneOf {
                costs: vec![
                    sacrifice(count),
                    AbilityCost::Mana {
                        cost: ManaCost::generic(99),
                    },
                ],
            },
            scale: None,
            payer,
        },
    );
    root.optional = true;
    let mut payoff = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 3 },
            player: TargetFilter::Controller,
        },
    );
    payoff.condition = Some(AbilityCondition::effect_performed());
    payoff.declares_return_result = return_result;
    let mut continuation = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 1 },
            target: Some(TargetFilter::ScopedPlayer),
        },
    );
    continuation.player_scope = Some(PlayerFilter::Opponent);
    continuation.sub_link = SubAbilityLink::SequentialSibling;
    continuation.sub_ability = Some(Box::new(payoff));
    root.sub_ability = Some(Box::new(continuation));
    root
}

/// Copied from `resolution_optional_payments.rs` ("Graveyard Warden", synthetic):
/// an optional "exile it instead" replacement on any move to a graveyard.
/// The nearest global printed replacement is Rest in Peace: "If a card or token
/// would be put into a graveyard from anywhere, exile it instead." No printed
/// card in the checked AtomicCards corpus makes that global replacement optional.
/// Forger's Foundry offers optional exile only for a qualifying resolving spell,
/// not a sacrificed creature. Optionality here lets the first sacrifice finish
/// before the second parks, exposing a partly paid cursor for terminal cleanup.
fn optional_graveyard_exile_replacement() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Graveyard)
        .mode(ReplacementMode::Optional { decline: None })
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                actor: engine::types::ability::LibraryInstructionActor::Controller,
                origin: None,
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                owner_library: false,
                library_shuffle: Default::default(),
                enter_transformed: false,
                enters_under: None,
                enter_tapped: engine::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                enters_modified_if: None,
                face_down_profile: None,
            },
        ))
}

fn decline_index(runner: &GameRunner) -> (PlayerId, usize) {
    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "a replacement choice is asked, got {:?}",
            runner.state().waiting_for
        );
    };
    let decline = candidates
        .iter()
        .position(|candidate| candidate.description == "Decline")
        .expect("the optional replacement offers Decline");
    (*player, decline)
}

/// A replacement-paused optional sacrifice, as in `resolution_optional_payments.rs`:
/// P0 casts the synthetic optional "sacrifice two" spell and pays it itself; P0's
/// Graveyard Warden (moved to exile for the cast and back after the resolution
/// starts, as the source fixtures do) offers to exile the first fodder instead; P0
/// declines, the first fodder dies (CR 701.21a), and the second sacrifice is paused
/// on the Warden's CR 614.1a choice. The cursor is parked at index 1 outside the
/// resolution stack. `observer` adds Deathgreeter with its verbatim Oracle text:
/// "Whenever another creature dies, you may gain 1 life." Its trigger must not
/// reach the stack after the game ends. Returns the runner, both fodder and P0's
/// life before the cast.
fn optional_sacrifice_paused_on_its_second_move(
    players: u8,
    observer: bool,
    return_result: Option<ReturnResultId>,
) -> (GameRunner, ObjectId, ObjectId, i32) {
    let mut scenario = if players == 2 {
        GameScenario::new()
    } else {
        GameScenario::new_n_player(players, 42)
    };
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Optional Sacrifice", false)
        .with_mana_cost(ManaCost::generic(0))
        .with_ability_definition(optional_sacrifice_spell(
            TargetFilter::Controller,
            2,
            return_result,
        ))
        .id();
    let warden = scenario
        .add_creature(P0, "Graveyard Warden", 1, 1)
        .with_replacement_definition(optional_graveyard_exile_replacement())
        .id();
    let first = scenario.add_creature(P0, "First Fodder", 1, 1).id();
    let second = scenario.add_creature(P0, "Second Fodder", 1, 1).id();
    if observer {
        scenario.add_creature_from_oracle(
            P0,
            "Deathgreeter",
            1,
            1,
            "Whenever another creature dies, you may gain 1 life.",
        );
    }
    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;
    move_to_zone(runner.state_mut(), warden, Zone::Exile, &mut Vec::new());
    runner.cast(spell).commit();
    runner.resolve_top();
    assert!(
        carrier_is_live(&runner),
        "fixture must pause a real resolving spell carrier"
    );
    if let WaitingFor::OptionalEffectChoice { player, .. } = &runner.state().waiting_for {
        let player = *player;
        apply(
            runner.state_mut(),
            player,
            GameAction::DecideOptionalEffect { accept: true },
        )
        .expect("the optional payment is accepted");
    }
    move_to_zone(
        runner.state_mut(),
        warden,
        Zone::Battlefield,
        &mut Vec::new(),
    );
    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
        },
    )
    .expect("the sacrifice branch is chosen");
    apply(
        runner.state_mut(),
        P0,
        GameAction::SelectCards {
            cards: vec![first, second],
        },
    )
    .expect("both fodder are selected");
    let (chooser, decline) = decline_index(&runner);
    apply(
        runner.state_mut(),
        chooser,
        GameAction::ChooseReplacement { index: decline },
    )
    .expect("the Warden is declined on the first fodder");

    // Reach guard: the second sacrifice is paused on a CR 614.1a choice, its cursor
    // parked outside the resolution stack under a live carrier.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player, .. } if player == P0
        ),
        "reach guard: the second sacrifice waits on P0's replacement choice, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(
            runner.state().pending_cost_move_resume,
            Some(PendingCostMoveResume::SacrificeForCost {
                pending: None,
                paused_at_index: 1,
                completion: PendingSacrificeCostCompletion::ResolutionOptionalPayment { .. },
                ..
            })
        ),
        "reach guard: the resolution's sacrifice cursor is parked at its second move"
    );
    assert!(runner.state().pending_replacement.is_some());
    assert!(carrier_is_live(&runner));
    assert!(runner.state().game_end.is_none());
    assert_eq!(runner.state().objects[&first].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&second].zone, Zone::Battlefield);
    (runner, first, second, life_before)
}

/// Real cards from the committed fixture, as `cascade_spell_ability_spells.rs`
/// builds its boards: P0 casts Violent Outburst with `hit` as its only library
/// card, passes until cascade offers the cast (CR 702.85a), and casts it. The
/// cascaded spell is then being cast inside the cascade trigger's resolution
/// (CR 608.2g). Two vanilla P0 fodder creatures; P1 has a 2/2 and, with
/// `warden`, the synthetic Graveyard Warden. Returns the runner, Outburst, the
/// hit and the fodder.
fn cascade_window(hit: &str, warden: bool) -> (GameRunner, ObjectId, ObjectId, Vec<ObjectId>) {
    assert_face_text("Violent Outburst", VIOLENT_OUTBURST);
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let outburst = scenario.add_real_card(P0, "Violent Outburst", Zone::Hand, db);
    let hit = scenario.add_real_card(P0, hit, Zone::Library, db);
    let fodder = vec![
        scenario.add_creature(P0, "Fodder A", 1, 1).id(),
        scenario.add_creature(P0, "Fodder B", 1, 1).id(),
    ];
    if warden {
        scenario
            .add_creature(P1, "Graveyard Warden", 1, 1)
            .with_replacement_definition(optional_graveyard_exile_replacement());
    }
    scenario.add_creature(P1, "Bear", 2, 2);
    scenario.with_mana_pool(
        P0,
        [ManaType::Red, ManaType::Green, ManaType::Colorless]
            .into_iter()
            .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|player| player.id == P0)
        .expect("P0 exists")
        .library = vec![hit].into();

    runner.cast(outburst).commit();
    for _ in 0..8 {
        if matches!(runner.state().waiting_for, WaitingFor::CastOffer { .. }) {
            break;
        }
        pass(&mut runner);
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::CastOffer { player, .. } if player == P0),
        "reach guard: cascade offers the cast, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::CascadeChoice {
            choice: CastChoice::Cast,
        })
        .expect("the cascaded cast starts");
    (runner, outburst, hit, fodder)
}

// ---------------------------------------------------------------------------
// Frames and carrier
// ---------------------------------------------------------------------------

/// CR 104.3a + CR 104.2a: the opponent concedes while P0's offer is pending; P0
/// wins and the offer is retired with the game.
#[test]
fn the_other_player_conceding_under_a_may_prompt_ends_the_game() {
    let mut runner = vraskas_scorn_at_its_offer(None);

    concede(&mut runner, P1);

    assert!(runner.state().players[1].is_eliminated);
    assert_finished_game_retired_its_resolution(&runner, Some(P0));
}

/// CR 104.3a + CR 104.2a: the player who owns the pending offer concedes; P1
/// wins and the leaver's offer is retired with the game.
#[test]
fn the_prompt_owner_conceding_ends_the_game() {
    let mut runner = vraskas_scorn_at_its_offer(None);

    concede(&mut runner, P0);

    assert!(runner.state().players[0].is_eliminated);
    assert_finished_game_retired_its_resolution(&runner, Some(P1));
}

/// CR 104.3a: P1 concedes while Shadow Slice (verbatim, cipher as its keyword)
/// waits on its cipher offer. A different direct-choice frame (`CipherEncode`) on
/// a spell carrier that owes no final part.
#[test]
fn conceding_under_a_cipher_offer_ends_the_game() {
    let (mut runner, _, _) = shadow_slice_at_its_offer(None);

    concede(&mut runner, P1);

    assert!(runner.state().players[1].is_eliminated);
    assert_finished_game_retired_its_resolution(&runner, Some(P0));
}

/// CR 104.3a: P1 concedes while an enters trigger's "you may" is pending; the
/// carrier is a triggered ability with a firing. Mer Man's verbatim Oracle text (see
/// `ENTERS_MAY_DRAW`).
#[test]
fn conceding_under_an_enters_triggers_may_prompt_ends_the_game() {
    let mut runner = enters_may_draw_at_its_offer(2);
    let hand_before = hand_size(&runner, P0);

    concede(&mut runner, P1);

    assert!(runner.state().players[1].is_eliminated);
    assert_finished_game_retired_its_resolution(&runner, Some(P0));
    // CR 104.1: the "you may draw" is never answered.
    assert_eq!(hand_size(&runner, P0), hand_before);
}

/// CR 104.3a + CR 100.4: in a best-of-three match the finished game's wait
/// becomes the sideboarding prompt, and the resolution is retired all the same
/// (`game_end` is the authority, not the wait).
#[test]
fn a_best_of_three_concession_under_a_may_prompt() {
    let mut runner = vraskas_scorn_at_its_offer(Some(MatchConfig {
        match_type: MatchType::Bo3,
        ..Default::default()
    }));

    concede(&mut runner, P1);

    assert_eq!(runner.state().match_score.p0_wins, 1);
    // CR 100.4: the match moves to sideboarding between games.
    assert_eq!(runner.state().match_phase, MatchPhase::BetweenGames);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::BetweenGamesSideboard { .. }
        ),
        "got {:?}",
        runner.state().waiting_for
    );
    assert_resolution_retired(&runner, Some(P0));
}

/// Application policy: every trusted cause and either forfeiting seat completes
/// the match without a current-game loss or another instruction of Vraska's Scorn.
#[test]
fn trusted_match_forfeit_retires_an_optional_spell_for_every_seat_and_cause() {
    for forfeiting_player in [P0, P1] {
        for cause in [
            MatchForfeitCause::MatchConcede,
            MatchForfeitCause::HostKick,
            MatchForfeitCause::DisconnectedForfeit,
        ] {
            let mut runner = vraskas_scorn_at_its_offer(Some(MatchConfig {
                match_type: MatchType::Bo3,
                ..Default::default()
            }));
            let spell = runner.state().resolving_stack_entry.as_ref().unwrap().id;
            let zone_before = runner.state().objects[&spell].zone;
            let hand_before = hand_size(&runner, P0);
            assert_eq!(runner.state().players.len(), 2);
            assert_eq!(runner.state().match_phase, MatchPhase::InGame);
            assert!(runner
                .state()
                .players
                .iter()
                .all(|player| !player.is_eliminated));
            runner.state_mut().match_score = MatchScore {
                p0_wins: 1,
                p1_wins: 1,
                draws: 1,
            };
            runner.state_mut().game_number = 4;
            runner.state_mut().sideboard_submitted = vec![P0, P1];
            runner.state_mut().next_game_chooser = Some(P1);
            let revision_before = runner.state().state_revision;
            let winner = if forfeiting_player == P0 { P1 } else { P0 };

            let events = apply_trusted_match_forfeit(runner.state_mut(), forfeiting_player, cause)
                .expect("the authenticated two-seat Bo3 forfeit is admitted");

            assert_eq!(
                events,
                vec![GameEvent::GameOver {
                    winner: Some(winner)
                }]
            );
            assert_eq!(
                runner.state().state_revision,
                revision_before.wrapping_add(1)
            );
            assert!(runner.state().game_end.is_none());
            assert!(runner
                .state()
                .players
                .iter()
                .all(|player| !player.is_eliminated));
            assert_eq!(
                (
                    runner.state().players[0].life,
                    runner.state().players[1].life
                ),
                (20, 16)
            );
            assert_eq!(runner.state().objects[&spell].zone, zone_before);
            assert_eq!(hand_size(&runner, P0), hand_before);
            let result = MatchForfeitResult {
                winner,
                forfeiting_player,
                cause,
            };
            let score = MatchScore {
                p0_wins: if winner == P0 { 2 } else { 1 },
                p1_wins: if winner == P1 { 2 } else { 1 },
                draws: 1,
            };
            assert_trusted_match_retired(&runner, result, score, 4);

            // This is the actual completed result above, not an assigned phase.
            assert_eq!(runner.state().match_config.match_type, MatchType::Bo3);
            assert_eq!(runner.state().players.len(), 2);
            assert_eq!(runner.state().match_phase, MatchPhase::Completed);
            assert_match_forfeit_refused(&mut runner, winner, "Match is already complete");
            assert_eq!(runner.state().match_forfeit_result, Some(result));
            assert_eq!(runner.state().match_score, score);
        }
    }
}

/// Application policy: P1's trusted match forfeit abandons P0's distinct cipher
/// frame without accepting or declining its offer, moving the spell or encoding it.
#[test]
fn trusted_match_forfeit_retires_a_cipher_offer_without_encoding_or_moving_cards() {
    let (mut runner, spell, hosts) = shadow_slice_at_its_offer(Some(MatchConfig {
        match_type: MatchType::Bo3,
        ..Default::default()
    }));
    let zones_before = [spell, hosts[0], hosts[1]].map(|id| runner.state().objects[&id].zone);
    let links_before = runner.state().exile_links.clone();
    assert!(hosts
        .iter()
        .all(
            |host| engine::game::cipher::encoded_cards_on_creature(runner.state(), *host)
                .is_empty()
        ));
    let revision_before = runner.state().state_revision;

    let events = apply_trusted_match_forfeit(runner.state_mut(), P1, MatchForfeitCause::HostKick)
        .expect("the trusted match forfeit is admitted at the cipher offer");

    assert_eq!(events, vec![GameEvent::GameOver { winner: Some(P0) }]);
    assert_eq!(
        runner.state().state_revision,
        revision_before.wrapping_add(1)
    );
    assert!(runner.state().game_end.is_none());
    assert!(runner
        .state()
        .players
        .iter()
        .all(|player| !player.is_eliminated));
    assert_eq!(runner.state().players[1].life, 17);
    assert_eq!(
        [spell, hosts[0], hosts[1]].map(|id| runner.state().objects[&id].zone),
        zones_before
    );
    assert_eq!(runner.state().exile_links, links_before);
    assert_trusted_match_retired(
        &runner,
        MatchForfeitResult {
            winner: P0,
            forfeiting_player: P1,
            cause: MatchForfeitCause::HostKick,
        },
        MatchScore {
            p0_wins: 2,
            p1_wins: 0,
            draws: 0,
        },
        1,
    );
}

/// Application policy: a trusted match forfeit closes the positively live search
/// session and its decision control without delivering Demonic Tutor's found card.
#[test]
fn trusted_match_forfeit_retires_a_library_search_without_finding_a_card() {
    let (mut runner, found) = demonic_tutor_at_its_search(
        2,
        Some(MatchConfig {
            match_type: MatchType::Bo3,
            ..Default::default()
        }),
    );
    let tutor = runner.state().resolving_stack_entry.as_ref().unwrap().id;
    let zone_before = runner.state().objects[&tutor].zone;
    let hand_before = hand_size(&runner, P0);
    let revision_before = runner.state().state_revision;

    let events = apply_trusted_match_forfeit(
        runner.state_mut(),
        P1,
        MatchForfeitCause::DisconnectedForfeit,
    )
    .expect("the trusted match forfeit is admitted during the search");

    assert_eq!(events, vec![GameEvent::GameOver { winner: Some(P0) }]);
    assert_eq!(
        runner.state().state_revision,
        revision_before.wrapping_add(1)
    );
    assert!(runner.state().game_end.is_none());
    assert!(runner
        .state()
        .players
        .iter()
        .all(|player| !player.is_eliminated));
    assert_eq!(hand_size(&runner, P0), hand_before);
    assert_eq!(runner.state().objects[&found].zone, Zone::Library);
    assert_eq!(runner.state().objects[&tutor].zone, zone_before);
    assert_trusted_match_retired(
        &runner,
        MatchForfeitResult {
            winner: P0,
            forfeiting_player: P1,
            cause: MatchForfeitCause::DisconnectedForfeit,
        },
        MatchScore {
            p0_wins: 2,
            p1_wins: 0,
            draws: 0,
        },
        1,
    );
}

#[test]
fn a_refused_non_bo3_forfeit_preserves_the_optional_spell_until_declined() {
    let mut runner = vraskas_scorn_at_its_offer(None);
    let spell = runner.state().resolving_stack_entry.as_ref().unwrap().id;
    assert_eq!(runner.state().match_config.match_type, MatchType::Bo1);
    assert_eq!(runner.state().players.len(), 2);
    assert_eq!(runner.state().match_phase, MatchPhase::InGame);

    assert_match_forfeit_refused(
        &mut runner,
        P1,
        "Match forfeits require a best-of-three match",
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("the unretired Vraska offer can still be declined");

    // CR 608.2c + CR 608.2d: declining performs no search; the earlier loss stays.
    assert_eq!(runner.state().players[1].life, 16);
    // CR 608.2n + CR 117.3b: the final part finishes and P0 receives priority.
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0));
    assert!(runner.state().stack.is_empty());
    assert_resolution_state_retired(runner.state());
    assert!(runner.state().game_end.is_none());
    assert!(runner.state().match_forfeit_result.is_none());
    assert_eq!(runner.state().match_phase, MatchPhase::InGame);
    assert_eq!(runner.state().match_score, MatchScore::default());
}

/// CR 702.99a + CR 702.99b + CR 608.2n: the refused match request leaves both
/// public cipher responses usable, including a non-first-host encoding.
#[test]
fn a_refused_wrong_seat_forfeit_preserves_both_cipher_responses() {
    for host_index in [Some(1), None] {
        let (mut runner, spell, hosts) = shadow_slice_at_its_offer(Some(MatchConfig {
            match_type: MatchType::Bo3,
            ..Default::default()
        }));
        assert_eq!(runner.state().match_config.match_type, MatchType::Bo3);
        assert_eq!(runner.state().players.len(), 2);
        assert_eq!(runner.state().match_phase, MatchPhase::InGame);
        assert_match_forfeit_refused(&mut runner, P2, "Forfeiting player is not a match seat");
        assert_eq!(frame_kinds(&runner), vec![FrameKind::CipherEncode]);
        assert_live_spell_carrier(&runner, spell);
        assert!(runner.state().game_end.is_none());
        assert!(runner.state().match_forfeit_result.is_none());
        let creature = host_index.map(|index| hosts[index]);

        runner
            .act(GameAction::CipherEncode { creature })
            .expect("the unretired cipher offer accepts the public response");

        if creature.is_some() {
            assert_eq!(runner.state().objects[&spell].zone, Zone::Exile);
            assert_eq!(
                engine::game::cipher::encoded_cards_on_creature(runner.state(), hosts[1]),
                vec![spell]
            );
        } else {
            assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
            assert!(
                engine::game::cipher::encoded_cards_on_creature(runner.state(), hosts[1])
                    .is_empty()
            );
        }
        assert!(
            engine::game::cipher::encoded_cards_on_creature(runner.state(), hosts[0]).is_empty()
        );
        // Both candidate hosts remain on the battlefield in this fixture.
        assert!(hosts
            .iter()
            .all(|host| runner.state().objects[host].zone == Zone::Battlefield));
        // CR 117.3b: either final response returns priority to the active player.
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0)
        );
        assert!(runner.state().stack.is_empty());
        assert!(runner.state().resolution_stack.is_empty());
        assert_eq!(runner.state().players[1].life, 17);
        assert!(runner.state().game_end.is_none());
        assert!(runner.state().match_forfeit_result.is_none());
        assert!(runner
            .state()
            .players
            .iter()
            .all(|player| !player.is_eliminated));
        assert_eq!(runner.state().match_phase, MatchPhase::InGame);
        assert_eq!(runner.state().match_score, MatchScore::default());
        assert_eq!(runner.state().game_number, 1);

        // CR 117.3d: P0's pass gives P1 priority; settling the completed
        // resolution at this action boundary must not reset that succession.
        runner
            .act(GameAction::PassPriority)
            .expect("the completed cipher response leaves the next action usable");
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1)
        );
        assert_resolution_state_retired(runner.state());
    }
}

#[test]
fn a_refused_three_player_match_forfeit_preserves_the_search_until_selected() {
    let (mut runner, found) = demonic_tutor_at_its_search(
        3,
        Some(MatchConfig {
            match_type: MatchType::Bo3,
            ..Default::default()
        }),
    );
    let tutor = runner.state().resolving_stack_entry.as_ref().unwrap().id;
    assert_eq!(runner.state().match_config.match_type, MatchType::Bo3);
    assert_eq!(runner.state().players.len(), 3);
    assert_eq!(runner.state().match_phase, MatchPhase::InGame);
    assert_match_forfeit_refused(
        &mut runner,
        P1,
        "Match forfeits require exactly two players",
    );

    runner
        .act(GameAction::SelectCards { cards: vec![found] })
        .expect("the unretired search accepts its offered card");

    // CR 701.23a + CR 608.2c: Demonic Tutor puts the selected card into P0's hand.
    assert_eq!(runner.state().objects[&found].zone, Zone::Hand);
    // CR 608.2n + CR 117.3b: the spell finishes and P0 receives priority.
    assert_eq!(runner.state().objects[&tutor].zone, Zone::Graveyard);
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0));
    assert!(runner.state().stack.is_empty());
    assert_resolution_state_retired(runner.state());
    assert!(runner.state().game_end.is_none());
    assert!(runner.state().match_forfeit_result.is_none());
    assert!(runner
        .state()
        .players
        .iter()
        .all(|player| !player.is_eliminated));
    assert_eq!(runner.state().match_phase, MatchPhase::InGame);
    assert_eq!(runner.state().match_score, MatchScore::default());
}

#[test]
fn a_trusted_match_forfeit_preserves_the_different_earned_game_result() {
    let mut runner = vraskas_scorn_at_its_offer(Some(MatchConfig {
        match_type: MatchType::Bo3,
        ..Default::default()
    }));
    // CR 104.3a + CR 104.2a: P1's ordinary game concession earns P0 the game.
    concede(&mut runner, P1);
    assert_resolution_retired(&runner, Some(P0));
    // CR 100.4 permits sideboarding; the score and game number are match policy.
    assert_eq!(
        runner.state().match_score,
        MatchScore {
            p0_wins: 1,
            p1_wins: 0,
            draws: 0
        }
    );
    assert_eq!(runner.state().game_number, 2);
    assert_eq!(runner.state().match_phase, MatchPhase::BetweenGames);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::BetweenGamesSideboard { .. }
    ));
    let game_end_before = runner.state().game_end;
    assert!(!runner.state().players[0].is_eliminated);
    assert!(runner.state().players[1].is_eliminated);
    let revision_before = runner.state().state_revision;

    let events =
        apply_trusted_match_forfeit(runner.state_mut(), P0, MatchForfeitCause::MatchConcede)
            .expect("P0 can forfeit the unfinished match after winning its first game");

    assert_eq!(events, vec![GameEvent::GameOver { winner: Some(P1) }]);
    assert_eq!(
        runner.state().state_revision,
        revision_before.wrapping_add(1)
    );
    assert_eq!(runner.state().game_end, game_end_before);
    assert_eq!(
        runner.state().game_end.as_ref().map(|end| end.winner),
        Some(Some(P0))
    );
    assert!(!runner.state().players[0].is_eliminated);
    assert!(runner.state().players[1].is_eliminated);
    assert_eq!(runner.state().players[1].life, 16);
    assert_trusted_match_retired(
        &runner,
        MatchForfeitResult {
            winner: P1,
            forfeiting_player: P0,
            cause: MatchForfeitCause::MatchConcede,
        },
        MatchScore {
            p0_wins: 1,
            p1_wins: 2,
            draws: 0,
        },
        2,
    );
}

/// CR 104.2b + CR 614.6: an effect wins the game while a repeat and its
/// continuation are genuinely live. P0 controls Laboratory Maniac and has an
/// empty library; Cultivator Colossus's enters trigger puts a land onto the
/// battlefield, and its "draw a card" is replaced by "you win the game instead".
/// The win is recorded inside the repeat, which still owns its frames and its
/// triggered-ability carrier. Real cards from the committed fixture.
#[test]
fn laboratory_maniac_wins_inside_cultivator_colossus_repeat() {
    assert_face_text("Cultivator Colossus", COLOSSUS);
    assert_face_text("Laboratory Maniac", LABORATORY_MANIAC);
    let db = shared_card_db().expect("production fixtures must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Laboratory Maniac", Zone::Battlefield, db);
    let source = scenario.add_real_card(P0, "Cultivator Colossus", Zone::Hand, db);
    for _ in 0..2 {
        scenario.add_real_card(P0, "Forest", Zone::Hand, db);
    }
    for _ in 0..3 {
        scenario.add_real_card(P1, "Forest", Zone::Library, db);
    }
    scenario.with_mana_pool(
        P0,
        (0..40)
            .flat_map(|_| [ManaType::Green, ManaType::Blue])
            .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    assert!(
        runner.state().players[P0.0 as usize].library.is_empty(),
        "precondition: Laboratory Maniac's replacement needs an empty library"
    );

    runner.cast(source).commit();
    pass_until_prompt(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player, .. } if player == P0
        ),
        "reach guard: the land offer is asked, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("the land offer is accepted");
    let WaitingFor::EffectZoneChoice { player, cards, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "reach guard: the land choice is asked, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    assert_eq!(
        frame_kinds(&runner),
        vec![FrameKind::RepeatUntil, FrameKind::AbilityContinuation]
    );
    assert!(carrier_is_live(&runner));
    assert!(runner.state().game_end.is_none());

    let events = runner
        .act(GameAction::SelectCards {
            cards: vec![cards[0]],
        })
        .expect("the land choice is accepted")
        .events;

    // CR 614.6 + CR 104.2b: the replaced draw wins the game for P0.
    assert!(events
        .iter()
        .any(|event| matches!(event, GameEvent::GameOver { winner: Some(w) } if *w == P0)));
    assert!(events.iter().any(
        |event| matches!(event, GameEvent::PlayerEliminated { player_id } if *player_id == P1)
    ));
    assert_finished_game_retired_its_resolution(&runner, Some(P0));
}

/// CR 104.3a + CR 104.2a: P1 concedes while Cultivator Colossus's repeating
/// enters trigger waits on its land choice. Exactly two frames are parked, the
/// repeat (`RepeatUntil`) and the continuation on top of it
/// (`AbilityContinuation`, which is not a direct-choice frame), and both go with
/// the finished game along with the triggered-ability carrier.
#[test]
fn a_game_conceded_under_a_repeating_choice_drops_every_frame() {
    assert_face_text("Cultivator Colossus", COLOSSUS);
    let db = shared_card_db().expect("production fixtures must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let source = scenario.add_real_card(P0, "Cultivator Colossus", Zone::Hand, db);
    for _ in 0..2 {
        scenario.add_real_card(P0, "Forest", Zone::Hand, db);
    }
    for name in ["Forest", "Forest", "Shock"] {
        scenario.add_real_card(P0, name, Zone::Library, db);
    }
    scenario.with_mana_pool(
        P0,
        (0..40)
            .flat_map(|_| [ManaType::Green, ManaType::Blue])
            .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    runner.cast(source).commit();
    pass(&mut runner);
    pass(&mut runner);
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    runner.resolve_top();
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("the first land offer is accepted");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { player, .. } if player == P0
        ),
        "the land choice is asked, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        frame_kinds(&runner),
        vec![FrameKind::RepeatUntil, FrameKind::AbilityContinuation]
    );
    assert!(carrier_is_live(&runner));
    assert!(runner.state().game_end.is_none());

    concede(&mut runner, P1);

    assert!(runner.state().players[1].is_eliminated);
    assert_finished_game_retired_its_resolution(&runner, Some(P0));
}

// ---------------------------------------------------------------------------
// Work parked outside the resolution stack
// ---------------------------------------------------------------------------

/// CR 104.4b + CR 616.1: Tainted Pact on an empty library is a loop of mandatory
/// actions, so the game is a draw. The draw is recorded inside the resolution, and
/// the spell's own CR 608.2n move to the graveyard then meets P1's Leyline of the
/// Void and Rest in Peace, which parks an order choice. Nobody leaves the game,
/// so only the finished-game teardown can retire that choice. All three cards
/// verbatim.
#[test]
fn a_mandatory_loop_draw_retires_the_final_parts_replacement_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pact = scenario
        .add_spell_to_hand_from_oracle(P0, "Tainted Pact", true, TAINTED_PACT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    scenario.add_enchantment_from_oracle(P1, "Leyline of the Void", LEYLINE_OF_THE_VOID);
    scenario.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
    let mut runner = scenario.build();
    assert!(
        runner.state().players[P0.0 as usize].library.is_empty(),
        "precondition: Tainted Pact's repeat can never stop"
    );

    let mut direct = runner.cast(pact).commit().state().clone();
    assert_eq!(direct.stack.len(), 1);
    engine::game::stack::resolve_top(&mut direct, &mut Vec::new());
    // Reach guard: the same state, resolved without an action boundary, records
    // the draw and then parks the CR 616.1 order choice on Tainted Pact's
    // graveyard move.
    assert_eq!(
        direct.game_end.as_ref().map(|end| end.winner),
        Some(None),
        "reach guard: the resolution records the CR 104.4b draw"
    );
    assert!(
        matches!(direct.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "reach guard: a CR 616.1 order choice owns the wait after the draw, got {:?}",
        direct.waiting_for
    );
    assert!(direct.pending_replacement.is_some());

    let mut events = pass(&mut runner);
    events.extend(pass(&mut runner));

    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, GameEvent::GameOver { winner: None }))
            .count(),
        1,
        "CR 104.4b: the draw is announced exactly once"
    );
    // The action reached the same unfinished CR 608.2n move.
    assert_eq!(runner.state().objects[&pact].zone, Zone::Stack);
    assert!(!runner.state().stack.iter().any(|entry| entry.id == pact));
    assert_finished_game_retired_its_resolution(&runner, None);
}

/// CR 104.3a + CR 614.1a: the resolution's own optional sacrifice, paid
/// by its controller, is paused on its second move by P0's Graveyard Warden; P1
/// concedes. The parked cursor and its replacement choice go with the finished
/// game and nothing moves (CR 104.1): no payoff, no further sacrifice, and an
/// empty stack.
#[test]
fn conceding_under_a_replacement_paused_optional_sacrifice_retires_its_cursor() {
    let (mut runner, _first, second, life_before) =
        optional_sacrifice_paused_on_its_second_move(2, true, None);

    apply(
        runner.state_mut(),
        P1,
        GameAction::Concede { player_id: P1 },
    )
    .expect("a concession is accepted");

    assert_finished_game_retired_its_resolution(&runner, Some(P0));
    // CR 104.1: no payoff, no further sacrifice.
    assert_eq!(runner.state().players[P0.0 as usize].life, life_before);
    assert!(runner.state().deferred_triggers.is_empty());
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&second].zone, Zone::Battlefield);
}

/// CR 104.3a: the persistence side of the row above. The paused optional payment's
/// frame names a return-result occurrence that the carrier's retirement clears,
/// so the cursor holding that frame must go in the same teardown or the finished
/// game no longer decodes. `declares_return_result` is set on the synthetic
/// payoff by hand (it is the parser's metadata for a payoff whose result a later
/// instruction reads) so the parked frame names an occurrence.
#[test]
fn a_parked_sacrifice_cursor_does_not_outlive_its_return_result_occurrence() {
    let (mut runner, _, _, _) =
        optional_sacrifice_paused_on_its_second_move(2, false, Some(ReturnResultId(7)));
    assert!(
        !runner.state().return_result_frames.is_empty(),
        "reach guard: the paused payment's occurrence is live"
    );
    assert_round_trips(&runner);

    apply(
        runner.state_mut(),
        P1,
        GameAction::Concede { player_id: P1 },
    )
    .expect("a concession is accepted");

    assert_finished_game_retired_its_resolution(&runner, Some(P0));
}

/// CR 104.2a: with P0 and P1 still in the game, P2's concession does not end it,
/// so P0's paused sacrifice and its CR 614.1a choice stay and resume: declining
/// the Warden lets the second fodder die (CR 701.21a), and the spell's own
/// CR 608.2n move then meets the Warden again.
#[test]
fn a_continuing_game_keeps_a_replacement_paused_sacrifice() {
    let (mut runner, _, second, _) = optional_sacrifice_paused_on_its_second_move(3, false, None);

    apply(
        runner.state_mut(),
        P2,
        GameAction::Concede { player_id: P2 },
    )
    .expect("a concession is accepted");

    assert!(runner.state().players[P2.0 as usize].is_eliminated);
    assert!(runner.state().game_end.is_none());
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player, .. } if player == P0
        ),
        "the survivor's replacement choice stays, got {:?}",
        runner.state().waiting_for
    );
    assert!(matches!(
        runner.state().pending_cost_move_resume,
        Some(PendingCostMoveResume::SacrificeForCost {
            paused_at_index: 1,
            ..
        })
    ));
    assert!(runner.state().pending_replacement.is_some());

    let (chooser, decline) = decline_index(&runner);
    apply(
        runner.state_mut(),
        chooser,
        GameAction::ChooseReplacement { index: decline },
    )
    .expect("the Warden is declined on the second fodder");

    assert_eq!(runner.state().objects[&second].zone, Zone::Graveyard);
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(runner.state().game_end.is_none());
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the spell's own graveyard move meets the Warden, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 104.2a + CR 117.3b: with a third player still in the game, P1's
/// concession does not end it, so P0's "you may" stays and is answered normally,
/// and the active player then receives priority. Mer Man's verbatim Oracle text (see
/// `ENTERS_MAY_DRAW`).
#[test]
fn a_three_player_game_keeps_the_survivors_may_prompt() {
    let mut runner = enters_may_draw_at_its_offer(3);
    let hand_before = hand_size(&runner, P0);

    concede(&mut runner, P1);

    assert!(runner.state().players[1].is_eliminated);
    assert!(runner.state().game_end.is_none());
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player, .. } if player == P0
        ),
        "the survivor's offer stays, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(frame_kinds(&runner), vec![FrameKind::OptionalEffect]);

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("the offer is accepted");

    // CR 117.3b: the active player receives priority after the trigger resolves.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().resolution_stack.is_empty());
    assert!(runner.state().resolving_stack_entry.is_none());
    assert_eq!(hand_size(&runner, P0), hand_before + 1);
    assert!(runner.state().game_end.is_none());
}

/// CR 104.3a + CR 614.1a: Hymn to Tourach (verbatim; cost zero as in
/// `random_discard_cost_replacement_resume.rs`) is paused on the replacement
/// choice for its first discard: P1's choice about P1's Graveyard Warden (narrowed
/// to cards other than Hymn, as that source fixture does). Hymn's controller
/// concedes. The parked discard batch and its replacement choice go with the
/// finished game and no card is discarded (CR 104.1).
#[test]
fn conceding_under_a_paused_random_discard_retires_the_discard_batch() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hymn = scenario
        .add_spell_to_hand_from_oracle(P0, "Hymn to Tourach", false, HYMN_TO_TOURACH)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario
        .add_creature(P1, "Graveyard Warden", 1, 1)
        .with_replacement_definition(optional_graveyard_exile_replacement().valid_card(
            TargetFilter::Not {
                filter: Box::new(TargetFilter::SpecificObject { id: hymn }),
            },
        ));
    for i in 0..4 {
        scenario.add_card_to_hand(P1, &format!("Victim Card {i}"));
    }
    let mut runner = scenario.build();

    runner.cast(hymn).target_player(P1).commit();
    pass_until_prompt(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player, .. } if player == P1
        ),
        "reach guard: the first discard waits on P1's replacement choice, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().pending_discard_batch.is_some());
    assert!(runner.state().pending_replacement.is_some());
    assert!(carrier_is_live(&runner));
    assert_eq!(hand_size(&runner, P1), 4);

    apply(
        runner.state_mut(),
        P0,
        GameAction::Concede { player_id: P0 },
    )
    .expect("a concession is accepted");

    assert_finished_game_retired_its_resolution(&runner, Some(P1));
    // CR 104.1: neither discard happens.
    assert_eq!(hand_size(&runner, P1), 4);
}

/// CR 104.3a + CR 608.2g + CR 614.1a: Violent Outburst's cascade casts Village
/// Rites inside the cascade trigger's resolution, and Village Rites' sacrifice
/// cost is paused on the choice about P1's Graveyard Warden. P1 concedes. The
/// instructed cast's parked cost cursor, its replacement choice and its reversal
/// record go with the finished game; nothing is settled, published or reversed,
/// and nothing moves (CR 104.1): Village Rites' CR 601.2a announcement stays on
/// the stack with the card still in exile, and the fodder stays on the
/// battlefield. Real Violent Outburst and Village Rites from the committed
/// fixture.
#[test]
fn conceding_inside_a_cascaded_casts_paused_sacrifice_retires_the_cast() {
    assert_face_text("Village Rites", VILLAGE_RITES);
    let (mut runner, outburst, rites, fodder) = cascade_window("Village Rites", true);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost { player, kind: PayCostKind::Sacrifice, .. } if player == P0
        ),
        "reach guard: the cascaded cast asks for its sacrifice, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().waiting_for.has_pending_cast());

    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder[0]],
        })
        .expect("the fodder is selected");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player, .. } if player == P0
        ),
        "reach guard: the sacrifice waits on a replacement choice, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(
            runner.state().pending_cost_move_resume,
            Some(PendingCostMoveResume::SacrificeForCost {
                pending: Some(_),
                ..
            })
        ),
        "reach guard: the instructed cast's sacrifice cursor is parked"
    );
    assert!(runner.state().pending_replacement.is_some());
    assert!(
        !json_key_absent(runner.state(), "spell_cast_reversal"),
        "reach guard: the instructed cast's reversal record is open"
    );
    assert!(carrier_is_live(&runner));
    assert!(runner
        .state()
        .stack
        .iter()
        .any(|entry| entry.id == outburst));
    assert!(announced_spell_on_stack(runner.state(), rites));
    assert_eq!(runner.state().objects[&rites].zone, Zone::Exile);
    for id in &fodder {
        assert_eq!(runner.state().objects[id].zone, Zone::Battlefield);
    }

    concede(&mut runner, P1);

    assert_finished_game_retired_its_resolution(&runner, Some(P0));
    // CR 104.1: nothing moves.
    assert_eq!(runner.state().stack.len(), 2);
    assert_eq!(runner.state().objects[&outburst].zone, Zone::Stack);
    assert!(announced_spell_on_stack(runner.state(), rites));
    assert_eq!(runner.state().objects[&rites].zone, Zone::Exile);
    for id in &fodder {
        assert_eq!(runner.state().objects[id].zone, Zone::Battlefield);
    }
    assert!(runner.state().deferred_triggers.is_empty());
}

/// CR 104.3a + CR 608.2g: Violent Outburst's cascade casts Shock inside the
/// cascade trigger's resolution, and P1 concedes while Shock waits on its target.
/// The prompt that held the cast is replaced by the terminal wait, so only the
/// instructed cast's reversal record is left to retire. Shock's CR 601.2a
/// announcement stays on the stack with the card still in exile (CR 104.1). Real
/// Violent Outburst and Shock from the committed fixture.
#[test]
fn conceding_inside_a_cascaded_casts_target_choice_retires_its_reversal() {
    assert_face_text("Shock", SHOCK);
    let (mut runner, _, shock, _) = cascade_window("Shock", false);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { player, .. } if player == P0
        ),
        "reach guard: the cascaded cast asks for its target, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().waiting_for.has_pending_cast());
    assert!(
        !json_key_absent(runner.state(), "spell_cast_reversal"),
        "reach guard: the instructed cast's reversal record is open"
    );
    assert!(carrier_is_live(&runner));
    assert!(announced_spell_on_stack(runner.state(), shock));
    assert_eq!(runner.state().objects[&shock].zone, Zone::Exile);

    concede(&mut runner, P1);

    assert_finished_game_retired_its_resolution(&runner, Some(P0));
    // CR 104.1: nothing moves.
    assert!(announced_spell_on_stack(runner.state(), shock));
    assert_eq!(runner.state().objects[&shock].zone, Zone::Exile);
}

// ---------------------------------------------------------------------------
// Sessions the resolution opened
// ---------------------------------------------------------------------------

/// Cast Demonic Tutor (verbatim, from the committed fixture) and pass until its
/// search asks P0 to choose from P0's library. Reach guard: the search session
/// and its decision control are open under the live carrier. Returns the runner
/// and the first offered card.
fn demonic_tutor_at_its_search(
    players: u8,
    match_config: Option<MatchConfig>,
) -> (GameRunner, ObjectId) {
    assert_face_text("Demonic Tutor", DEMONIC_TUTOR);
    let mut scenario = board(players);
    add_swamps(&mut scenario, 2);
    let tutor = scenario
        .add_spell_to_hand_from_oracle(P0, "Demonic Tutor", false, DEMONIC_TUTOR)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .id();
    let mut runner = scenario.build();
    if let Some(config) = match_config {
        runner.state_mut().set_match_config(config);
    }
    runner.cast(tutor).commit();
    pass_until_prompt(&mut runner);
    let WaitingFor::SearchChoice {
        player,
        library_owner,
        cards,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "reach guard: the search asks for its card, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((player, library_owner), (P0, Some(P0)));
    let search = runner
        .state()
        .active_library_searches
        .get(&P0)
        .expect("reach guard: the search session is live");
    assert_eq!(
        (
            search.searcher(),
            search.searched_zone_owner(),
            search.effective_library_owner()
        ),
        (P0, P0, Some(P0))
    );
    let decision = runner
        .state()
        .active_search_decision_controls
        .get(&P0)
        .expect("reach guard: the search's decision controller is live");
    assert_eq!((decision.searcher, decision.searched_zone_owner), (P0, P0));
    assert_eq!(
        decision.authority,
        ActiveSearchDecisionAuthority::LatchedController { controller: P0 }
    );
    assert_live_spell_carrier(&runner, tutor);
    assert!(runner.state().owed_spell_final_part().is_some());
    assert!(runner.state().game_end.is_none());
    assert!(runner.state().match_forfeit_result.is_none());
    let found = *cards.first().expect("P0's library offers a card");
    assert_eq!(runner.state().objects[&found].zone, Zone::Library);
    (runner, found)
}

/// CR 104.3a + CR 701.23a: P1 concedes while P0 searches P0's library for Demonic
/// Tutor. The search session and its decision control go with the finished game,
/// and no card is found (CR 104.1).
#[test]
fn conceding_during_a_library_search_retires_the_search_session() {
    let (mut runner, found) = demonic_tutor_at_its_search(2, None);
    let hand_before = hand_size(&runner, P0);

    concede(&mut runner, P1);

    assert_finished_game_retired_its_resolution(&runner, Some(P0));
    // CR 104.1: nothing is found.
    assert_eq!(hand_size(&runner, P0), hand_before);
    assert_eq!(runner.state().objects[&found].zone, Zone::Library);
}

/// CR 104.2a + CR 800.4a: with a third player still in the game, P1's concession
/// does not end it, so P0's search of P0's own library stays open and is answered
/// normally: the card goes to P0's hand and the search session closes.
#[test]
fn a_three_player_game_keeps_the_survivors_library_search() {
    let (mut runner, found) = demonic_tutor_at_its_search(3, None);

    concede(&mut runner, P1);

    assert!(runner.state().players[1].is_eliminated);
    assert!(runner.state().game_end.is_none());
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::SearchChoice { player, .. } if player == P0
        ),
        "the survivor's search stays, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().active_library_searches.get(&P0).is_some());
    assert!(runner
        .state()
        .active_search_decision_controls
        .get(&P0)
        .is_some());

    runner
        .act(GameAction::SelectCards { cards: vec![found] })
        .expect("the search choice is accepted");

    assert_eq!(runner.state().objects[&found].zone, Zone::Hand);
    assert!(runner.state().active_library_searches.is_empty());
    assert!(runner.state().active_search_decision_controls.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(runner.state().resolving_stack_entry.is_none());
    assert!(runner.state().game_end.is_none());
    assert!(runner.state().stack.is_empty());
    // CR 117.3b: with the search completed, the active player receives priority.
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0));
}

/// CR 104.3a + CR 701.20e: P1 concedes while Birthing Ritual's end-step trigger
/// (verbatim, from the committed fixture) asks "you may sacrifice a creature",
/// with P0 still privately looking at the top of P0's library for the "from among
/// those cards" that follows. The look ends with the finished game.
#[test]
fn conceding_during_a_private_look_ends_the_look() {
    assert_face_text("Birthing Ritual", BIRTHING_RITUAL);
    let mut scenario = board(2);
    scenario.add_enchantment_from_oracle(P0, "Birthing Ritual", BIRTHING_RITUAL);
    scenario
        .add_creature(P0, "Ritual Fodder", 2, 2)
        .with_mana_cost(ManaCost::generic(2));
    let mut runner = scenario.build();
    runner.advance_to_end_step();
    for _ in 0..32 {
        match runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => break,
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("no attack is declared");
            }
            _ => {
                pass(&mut runner);
            }
        }
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player, .. } if player == P0
        ),
        "reach guard: the sacrifice offer is asked, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().private_look_player, Some(P0));
    assert_eq!(runner.state().private_look_ids.len(), 4);
    assert!(carrier_is_live(&runner));

    concede(&mut runner, P1);

    assert_finished_game_retired_its_resolution(&runner, Some(P0));
}

/// CR 104.3a + CR 701.20a: P1 concedes while Satyr Wayfinder's enters trigger
/// (verbatim, from the committed fixture) has revealed the top four cards of P0's
/// library and waits on P0's land choice. The reveal ends with the finished game,
/// and nothing moves (CR 104.1).
#[test]
fn conceding_during_a_reveal_ends_the_reveal() {
    assert_face_text("Satyr Wayfinder", SATYR_WAYFINDER);
    let mut scenario = board(2);
    let land = scenario.add_land_to_library_top(P0, "Revealed Land").id();
    for _ in 0..2 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let satyr = scenario
        .add_creature_to_hand_from_oracle(P0, "Satyr Wayfinder", 1, 1, SATYR_WAYFINDER)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();
    let mut runner = scenario.build();
    runner.cast(satyr).commit();
    pass_until_prompt(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DigChoice { player, .. } if player == P0
        ),
        "reach guard: the land choice is asked, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().revealed_cards.len(), 4);
    assert!(runner.state().revealed_cards.contains(&land));
    assert!(carrier_is_live(&runner));

    concede(&mut runner, P1);

    assert_finished_game_retired_its_resolution(&runner, Some(P0));
    // CR 104.1: nothing moves.
    assert_eq!(runner.state().objects[&land].zone, Zone::Library);
}

const WAR_DOCTOR: &str = "Whenever one or more other permanents phase out and whenever one or more other cards are put into exile from anywhere, put a time counter on The War Doctor.\nWhenever The War Doctor attacks, it deals damage equal to the number of time counters on it to any target. If a creature dealt damage this way would die this turn, exile it instead.";
const PAINTERS_SERVANT: &str = "As this creature enters, choose a color.\nAll cards that aren't on the battlefield, spells, and permanents are the chosen color in addition to their other colors.";
const RITE_OF_REPLICATION: &str = "Kicker {5} (You may pay an additional {5} as you cast this spell.)\nCreate a token that's a copy of target creature. If this spell was kicked, create five of those tokens instead.";

fn pact_with_a_deferred_exile_trigger() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let doctor = scenario
        .add_creature_from_oracle(P0, "The War Doctor", 3, 5, WAR_DOCTOR)
        .id();
    let only_card = scenario
        .add_spell_to_library_top(P0, "Only Card", true)
        .id();
    let pact = scenario
        .add_spell_to_hand_from_oracle(P0, "Tainted Pact", true, TAINTED_PACT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let mut runner = scenario.build();
    runner.cast(pact).commit();
    pass_until_prompt(&mut runner);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ));
    assert_eq!(runner.state().objects[&only_card].zone, Zone::Exile);
    assert!(runner.state().players[0].library.is_empty());
    assert!(carrier_is_live(&runner));
    assert!(
        !runner.state().deferred_triggers.is_empty(),
        "reach guard: The War Doctor's exile trigger is queued inside Tainted Pact's resolution"
    );
    (runner, doctor, only_card)
}

/// CR 104.4b + CR 603.3: declining the only exiled card leaves a mandatory
/// empty-library loop. No player is eliminated; The War Doctor's already queued
/// trigger must retire even if Tainted Pact has finished its own final part.
#[test]
fn a_loop_draw_retires_a_survivors_deferred_trigger() {
    let (mut runner, doctor, _) = pact_with_a_deferred_exile_trigger();
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .unwrap();
    assert!(runner
        .state()
        .players
        .iter()
        .all(|player| !player.is_eliminated));
    assert!(runner.state().objects[&doctor].counters.is_empty());
    assert_finished_game_retired_its_resolution(&runner, None);
}

/// CR 603.3: accepting the card ends the repeat normally, so the same queued
/// trigger remains available to resolve instead of being discarded.
#[test]
fn a_nonterminal_pact_keeps_its_deferred_trigger() {
    let (mut runner, doctor, card) = pact_with_a_deferred_exile_trigger();
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .unwrap();
    assert!(runner.state().game_end.is_none());
    assert_eq!(runner.state().objects[&card].zone, Zone::Hand);
    assert!(
        !runner.state().stack.is_empty(),
        "the deferred trigger is published"
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&doctor]
            .counters
            .get(&engine::types::counter::CounterType::Time),
        Some(&1)
    );
}

fn copying_painter_at_its_entry_choice(players: u8) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = board(players);
    let painter = scenario
        .add_creature_from_oracle(P0, "Painter's Servant", 1, 3, PAINTERS_SERVANT)
        .id();
    let rite = scenario
        .add_spell_to_hand_from_oracle(P0, "Rite of Replication", false, RITE_OF_REPLICATION)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 2,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    runner.cast(rite).target_object(painter).commit();
    pass_until_prompt(&mut runner);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::NamedChoice { .. }),
        "got {:?}",
        runner.state().waiting_for
    );
    assert!(carrier_is_live(&runner));
    assert!(runner.state().pending_liminal_entry_resume.is_some());
    assert_eq!(runner.state().liminal_entries.len(), 1);
    let entrant = *runner.state().liminal_entries.keys().next().unwrap();
    assert!(
        !runner.state().objects.contains_key(&entrant),
        "the token is still a detached projection"
    );
    (runner, painter, entrant)
}

/// CR 104.1 + CR 614.12a: the game ends before Painter's Servant's token has
/// chosen its color, so its detached projection and batch never enter.
#[test]
fn conceding_during_a_token_entry_retires_its_detached_projection() {
    let (mut runner, painter, entrant) = copying_painter_at_its_entry_choice(2);
    concede(&mut runner, P1);
    assert!(!runner.state().objects.contains_key(&entrant));
    assert_eq!(runner.state().objects[&painter].zone, Zone::Battlefield);
    assert_finished_game_retired_its_resolution(&runner, Some(P0));
}

/// CR 800.4: a third player's departure does not discard the surviving
/// controller's pending entry; choosing blue still creates exactly that token.
#[test]
fn a_nonterminal_concession_preserves_a_token_entry() {
    let (mut runner, painter, entrant) = copying_painter_at_its_entry_choice(3);
    concede(&mut runner, P2);
    assert!(runner.state().game_end.is_none());
    assert!(runner.state().liminal_entries.contains_key(&entrant));
    let WaitingFor::NamedChoice { options, .. } = &runner.state().waiting_for else {
        panic!("the color choice survives")
    };
    let blue = options
        .iter()
        .find(|option| option.eq_ignore_ascii_case("blue"))
        .unwrap()
        .clone();
    runner
        .act(GameAction::ChooseOption { choice: blue })
        .unwrap();
    assert!(runner.state().liminal_entries.is_empty());
    assert!(runner.state().pending_liminal_entry_resume.is_none());
    assert_eq!(runner.state().objects[&entrant].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&entrant].chosen_color(),
        Some(ManaColor::Blue)
    );
    assert_eq!(runner.state().objects[&painter].zone, Zone::Battlefield);
    assert!(runner.state().game_end.is_none());
    assert_round_trips(&runner);
}

const BRAINSTORM: &str =
    "Draw three cards, then put two cards from your hand on top of your library in any order.";
const TEMPORAL_MASTERY: &str = "Take an extra turn after this one. Exile Temporal Mastery.\nMiracle {1}{U} (You may cast this card for its miracle cost when you draw it if it's the first card you drew this turn.)";

fn add_miracle_to_library(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_library_top(P0, "Temporal Mastery", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue; 2],
            generic: 5,
        })
        .from_oracle_text(TEMPORAL_MASTERY)
        .id()
}

fn brainstorm_with_a_queued_miracle(players: u8) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = board(players);
    let miracle = add_miracle_to_library(&mut scenario);
    scenario.add_basic_land(P0, ManaColor::Blue);
    let brainstorm = scenario
        .add_spell_to_hand_from_oracle(P0, "Brainstorm", true, BRAINSTORM)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    runner.cast(brainstorm).commit();
    let events = pass_until_prompt(&mut runner);
    assert!(
        events.iter().any(|event| matches!(event,
            GameEvent::CardDrawn { object_id, nth_in_turn: 1, .. } if *object_id == miracle
        )),
        "reach guard: Temporal Mastery was the first actual draw"
    );
    let WaitingFor::EffectZoneChoice {
        cards,
        count: 2,
        zone: Zone::Hand,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "reach guard: Brainstorm must pause for its two-card placement: {:?}",
            runner.state().waiting_for
        );
    };
    let put_back: Vec<_> = cards.iter().copied().filter(|id| *id != miracle).collect();
    assert_eq!(put_back.len(), 2);
    assert!(carrier_is_live(&runner));
    assert_eq!(runner.state().pending_miracle_offers.len(), 1);
    assert_eq!(runner.state().pending_miracle_offers[0].object_id, miracle);
    assert_eq!(runner.state().objects[&miracle].zone, Zone::Hand);
    (runner, miracle, put_back)
}

/// CR 104.1 + CR 104.3a: a concession ends the game while Brainstorm has
/// queued a first-draw miracle but still waits for its placement instruction.
#[test]
fn terminal_concession_retires_a_queued_miracle_offer() {
    let (mut runner, miracle, _) = brainstorm_with_a_queued_miracle(2);
    concede(&mut runner, P1);
    assert_eq!(runner.state().objects[&miracle].zone, Zone::Hand);
    assert_finished_game_retired_its_resolution(&runner, Some(P0));
}

#[test]
fn sideboarding_retires_a_queued_miracle_offer() {
    let (mut runner, _, _) = brainstorm_with_a_queued_miracle(2);
    runner.state_mut().set_match_config(MatchConfig {
        match_type: MatchType::Bo3,
        ..Default::default()
    });
    concede(&mut runner, P1);
    assert_eq!(runner.state().match_phase, MatchPhase::BetweenGames);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::BetweenGamesSideboard { .. }
    ));
    assert_resolution_retired(&runner, Some(P0));
}

/// CR 702.94a: a nonterminal concession preserves the pending offer, which
/// becomes the reveal prompt after Brainstorm finishes its remaining choice.
#[test]
fn nonterminal_concession_preserves_a_queued_miracle_offer() {
    let (mut runner, miracle, put_back) = brainstorm_with_a_queued_miracle(3);
    concede(&mut runner, P2);
    assert!(runner.state().game_end.is_none());
    assert_eq!(runner.state().pending_miracle_offers.len(), 1);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::EffectZoneChoice { .. }
    ));
    runner
        .act(GameAction::SelectCards { cards: put_back })
        .unwrap();
    assert!(
        matches!(runner.state().waiting_for,
            WaitingFor::MiracleReveal { player: P0, object_id, .. } if object_id == miracle
        ),
        "the continuing game presents the miracle reveal: {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().pending_miracle_offers.is_empty());
    assert_eq!(runner.state().objects[&miracle].zone, Zone::Hand);
}

/// CR 104.1: Divination draws a miracle, then Laboratory Maniac replaces its
/// empty-library second draw with a win. Its queued offer must retire even
/// when the spell's carrier has already completed.
#[test]
fn an_effect_win_retires_a_miracle_offer_after_the_spell_finishes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let miracle = add_miracle_to_library(&mut scenario);
    scenario.add_creature_from_oracle(P0, "Laboratory Maniac", 2, 2, LABORATORY_MANIAC);
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }
    let divination = scenario
        .add_spell_to_hand_from_oracle(P0, "Divination", false, "Draw two cards.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        })
        .id();
    let mut runner = scenario.build();
    runner.cast(divination).commit();
    let events = pass_until_prompt(&mut runner);
    assert!(events.iter().any(|event| matches!(event,
        GameEvent::CardDrawn { object_id, nth_in_turn: 1, .. } if *object_id == miracle
    )));
    assert_eq!(runner.state().objects[&miracle].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&divination].zone, Zone::Graveyard);
    assert!(!carrier_is_live(&runner), "the drawing spell has completed");
    assert_finished_game_retired_its_resolution(&runner, Some(P0));
}

const DECORUM_DISSERTATION: &str = "Target player draws two cards and loses 2 life.\nParadigm (Then exile this spell. After you first resolve a spell with this name, you may cast a copy of it from exile without paying its mana cost at the beginning of each of your first main phases.)";
const ECHOCASTING_SYMPOSIUM: &str = "Target player creates a token that's a copy of target creature you control.\nParadigm (Then exile this spell. After you first resolve a spell with this name, you may cast a copy of it from exile without paying its mana cost at the beginning of each of your first main phases.)";
const ARCHMAGE_EMERITUS: &str =
    "Magecraft — Whenever you cast or copy an instant or sorcery spell, draw a card.";
const STORM_KILN_ARTIST: &str = "This creature gets +1/+0 for each artifact you control.\nMagecraft — Whenever you cast or copy an instant or sorcery spell, create a Treasure token. (It's an artifact with \"{T}, Sacrifice this token: Add one mana of any color.\")";

/// Stage the established exiled sources, as in the existing multiple-offer
/// regression. Accepting/targeting the copy and collecting its Magecraft
/// observers use public actions; no pending-offer or trigger queue is injected.
fn paradigm_copy_at_its_observer_order(players: u8) -> (GameRunner, ObjectId) {
    use engine::game::effects::paradigm::{arm_paradigm, enqueue_offer_if_any};

    let mut scenario = board(players);
    let source = scenario
        .add_spell_to_exile(P0, "Decorum Dissertation", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black; 2],
            generic: 3,
        })
        .with_subtypes(vec!["Lesson"])
        .from_oracle_text(DECORUM_DISSERTATION)
        .id();
    let remaining = scenario
        .add_spell_to_exile(P0, "Echocasting Symposium", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue; 2],
            generic: 4,
        })
        .with_subtypes(vec!["Lesson"])
        .from_oracle_text(ECHOCASTING_SYMPOSIUM)
        .id();
    scenario
        .add_creature_from_oracle(P0, "Archmage Emeritus", 2, 2, ARCHMAGE_EMERITUS)
        .with_subtypes(vec!["Human", "Wizard"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue; 2],
            generic: 2,
        });
    scenario
        .add_creature_from_oracle(P0, "Storm-Kiln Artist", 2, 2, STORM_KILN_ARTIST)
        .with_subtypes(vec!["Dwarf", "Shaman"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 3,
        });
    let mut runner = scenario.build();
    assert!(arm_paradigm(
        runner.state_mut(),
        source,
        P0,
        "Decorum Dissertation"
    ));
    assert!(arm_paradigm(
        runner.state_mut(),
        remaining,
        P0,
        "Echocasting Symposium"
    ));
    assert!(enqueue_offer_if_any(runner.state_mut(), P0));
    runner.act(GameAction::CastParadigmCopy { source }).unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::CopyRetarget { .. }
    ));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(engine::types::ability::TargetRef::Player(P1)),
        })
        .unwrap();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }),
        "reach guard: Magecraft observers must pause at ordering: {:?}",
        runner.state().waiting_for
    );
    let pending = runner
        .state()
        .pending_paradigm_remaining_offers
        .as_ref()
        .expect("the other source is parked while observers are ordered");
    assert_eq!(pending.player, P0);
    assert_eq!(pending.offers, vec![remaining]);
    assert!(
        !carrier_is_live(&runner),
        "the copy is announced, not resolving"
    );
    (runner, remaining)
}

/// CR 104.1 + CR 104.3a: no resumed offer is owed after terminal concession.
#[test]
fn terminal_concession_retires_remaining_paradigm_offers() {
    let (mut runner, remaining) = paradigm_copy_at_its_observer_order(2);
    concede(&mut runner, P1);
    assert_eq!(runner.state().objects[&remaining].zone, Zone::Exile);
    assert_finished_game_retired_its_resolution(&runner, Some(P0));
}

#[test]
fn sideboarding_retires_remaining_paradigm_offers() {
    let (mut runner, _) = paradigm_copy_at_its_observer_order(2);
    runner.state_mut().set_match_config(MatchConfig {
        match_type: MatchType::Bo3,
        ..Default::default()
    });
    concede(&mut runner, P1);
    assert_eq!(runner.state().match_phase, MatchPhase::BetweenGames);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::BetweenGamesSideboard { .. }
    ));
    assert_resolution_retired(&runner, Some(P0));
}

/// CR 603.3b: ordering the observers in a continuing game resumes the same
/// remaining source offer; another player's departure must not discard it.
#[test]
fn nonterminal_concession_preserves_remaining_paradigm_offers() {
    let (mut runner, remaining) = paradigm_copy_at_its_observer_order(3);
    concede(&mut runner, P2);
    assert!(runner.state().game_end.is_none());
    assert_eq!(
        runner
            .state()
            .pending_paradigm_remaining_offers
            .as_ref()
            .unwrap()
            .offers,
        vec![remaining]
    );
    let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for else {
        panic!("the trigger order prompt must survive");
    };
    let count = triggers.len();
    assert!(
        count >= 2,
        "both different Magecraft observers require ordering"
    );
    runner
        .act(GameAction::OrderTriggers {
            order: (0..count).collect(),
        })
        .unwrap();
    assert!(
        matches!(&runner.state().waiting_for,
            WaitingFor::CastOffer { player: P0, kind: engine::types::game_state::CastOfferKind::Paradigm { offers } }
            if offers == &vec![remaining]
        ),
        "remaining source must be offered: {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().pending_paradigm_remaining_offers.is_none());
}

const NAME_DROPPING: &str = "Gotcha — If an opponent says a word that's in the name of a card in your graveyard, you may say \"Gotcha!\" When you do, return that card to your hand.";
const ROD_OF_SPANKING: &str = "{2}, {T}: This artifact deals 1 damage to target player. Then untap this artifact unless that player says \"Thank you, sir. May I have another?\"";
const YOU_ARE_ALREADY_DEAD: &str =
    "Destroy target creature that was dealt damage this turn.\nDraw a card.";
const INFERNIUS_SPAWNINGTON: &str = "Flying, first strike, trample, haste\nThis spell costs {3} less to cast for each card you've revealed this turn.\nWhen this creature enters, you may say \"I'm here.\" If you do, it deals 3 damage to target player.";
const IM_GOING_ON_AN_ADVENTURE: &str = "Exile target creature. It's on an adventure. When it enters from being on an adventure, it explores. (Its owner may cast it from exile. To have a creature you control explore, reveal the top card of your library. Put that card into your hand if it's a land. Otherwise, put a +1/+1 counter on the creature, then put the card back or put it into your graveyard.)\nFlashback {4}{W}";

fn enable_spoken_actions(runner: &mut GameRunner, players: &[PlayerId]) {
    for &player in players {
        apply(
            runner.state_mut(),
            player,
            GameAction::UnAction {
                action: UnAction::SetEnabled { enabled: true },
            },
        )
        .expect("the player opts in to spoken actions");
    }
}

/// Both printed permanents enter through fixture setup. Public activation,
/// targeting and priority passes reach the Rod's spoken payment; P1's public
/// tell then matches a real graveyard card while that payment choice is pending.
fn rod_with_a_queued_gotcha(players: u8) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = board(players);
    let rod = scenario
        .add_artifact_from_oracle(P0, "Rod of Spanking", ROD_OF_SPANKING)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 1,
        })
        .id();
    let source = scenario
        .add_enchantment_from_oracle(P0, "Name Dropping", NAME_DROPPING)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();
    let returns = scenario
        .add_spell_to_graveyard(P0, "You Are Already Dead", true)
        .from_oracle_text(YOU_ARE_ALREADY_DEAD)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    enable_spoken_actions(&mut runner, &[P0, P1]);
    let ability_index = runner.state().objects[&rod]
        .abilities
        .iter()
        .position(|ability| ability.cost.is_some())
        .expect("the printed Rod has its activated ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: rod,
            ability_index,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(engine::types::ability::TargetRef::Player(P1)),
        })
        .unwrap();
    pass_until_prompt(&mut runner);
    assert!(matches!(
        &runner.state().waiting_for,
        WaitingFor::UnlessPayment {
            player: P1,
            cost: AbilityCost::SayPhrase { .. },
            ..
        }
    ));
    assert_eq!(runner.life(P1), 19, "the Rod really dealt its damage");
    apply(
        runner.state_mut(),
        P1,
        GameAction::UnAction {
            action: UnAction::PerformTell {
                tell: UnTell::Said { text: "you".into() },
            },
        },
    )
    .unwrap();
    assert!(runner.state().un_actions.gotcha_offers.is_empty());
    let pending = &runner.state().un_actions.pending_offers;
    assert_eq!(
        pending.len(),
        1,
        "reach guard: an actual tell queued Gotcha"
    );
    assert_eq!(pending[0].player, P0);
    assert_eq!(pending[0].source.reference.object_id, source);
    assert_eq!(pending[0].returns.reference.object_id, returns);
    assert!(
        !carrier_is_live(&runner),
        "the Rod's pending spoken payment outlives its resolution carrier"
    );
    assert_round_trips(&runner);
    (runner, source, returns)
}

/// CR 104.1 + CR 104.3a: a tell made under an unresolved printed ability
/// cannot leave a Gotcha offer waiting for priority after terminal concession.
#[test]
fn terminal_concession_retires_a_queued_gotcha_offer() {
    let (mut runner, _, returns) = rod_with_a_queued_gotcha(2);
    concede(&mut runner, P1);
    assert_eq!(runner.state().objects[&returns].zone, Zone::Graveyard);
    assert_finished_game_retired_its_resolution(&runner, Some(P0));
}

#[test]
fn sideboarding_retires_a_queued_gotcha_offer() {
    let (mut runner, _, _) = rod_with_a_queued_gotcha(2);
    runner.state_mut().set_match_config(MatchConfig {
        match_type: MatchType::Bo3,
        ..Default::default()
    });
    concede(&mut runner, P1);
    assert_eq!(runner.state().match_phase, MatchPhase::BetweenGames);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::BetweenGamesSideboard { .. }
    ));
    assert_resolution_retired(&runner, Some(P0));
}

/// CR 104.3a: leaving two survivors preserves the paused ability and exact
/// queued offer, which can subsequently be announced and actually return the card.
#[test]
fn nonterminal_concession_preserves_a_queued_gotcha_offer() {
    let (mut runner, source, returns) = rod_with_a_queued_gotcha(3);
    let pending = runner.state().un_actions.pending_offers.clone();
    concede(&mut runner, P2);
    assert!(runner.state().game_end.is_none());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::UnlessPayment { player: P1, .. }
    ));
    assert_eq!(runner.state().un_actions.pending_offers, pending);
    assert_round_trips(&runner);
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .unwrap();
    assert!(runner.state().un_actions.pending_offers.is_empty());
    assert_eq!(runner.state().un_actions.gotcha_offers.len(), 1);
    assert_eq!(
        runner.state().un_actions.gotcha_offers[0]
            .returns
            .reference
            .object_id,
        returns
    );
    apply(
        runner.state_mut(),
        P0,
        GameAction::UnAction {
            action: UnAction::SayGotcha {
                source_id: source,
                returns,
            },
        },
    )
    .unwrap();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&returns].zone, Zone::Hand);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// CR 104.1: the optional-effect handler restores Priority before Infernius's
/// printed payment, so Name Dropping opens a live offer rather than queueing it.
/// Its damage then ends the game in the same action; that open offer retires too.
#[test]
fn a_terminal_spoken_payment_retires_its_open_gotcha_offer() {
    let mut scenario = board(2);
    scenario.with_life(P1, 3);
    scenario.add_enchantment_from_oracle(P1, "Name Dropping", NAME_DROPPING);
    scenario
        .add_spell_to_graveyard(P1, "I'm Going On An Adventure!", false)
        .from_oracle_text(IM_GOING_ON_AN_ADVENTURE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        });
    let infernius = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Infernius Spawnington III, Esq.",
            9,
            9,
            INFERNIUS_SPAWNINGTON,
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 10,
        })
        .id();
    let mut mana = vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])];
    mana.extend((0..10).map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![])));
    scenario.with_mana_pool(P0, mana);
    let mut runner = scenario.build();
    enable_spoken_actions(&mut runner, &[P0, P1]);
    runner.cast(infernius).commit();
    runner.advance_until_stack_empty();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(engine::types::ability::TargetRef::Player(P1)),
        })
        .unwrap();
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        && !runner.state().stack.is_empty()
    {
        runner.resolve_top();
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player: P0, .. }
    ));
    assert_eq!(
        engine::game::un_actions::open_gotcha_offers(
            runner.state(),
            P0,
            &UnTell::Said {
                text: "I'm here.".into()
            },
        )
        .len(),
        1,
        "reach guard: the printed phrase matches a live opposing Gotcha source"
    );
    let events = runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .unwrap()
        .events;
    assert!(events.iter().any(|event| matches!(event,
        GameEvent::PhraseSaid { player: P0, phrase } if phrase == "I'm here."
    )));
    assert_finished_game_retired_its_resolution(&runner, Some(P0));
}

/// CR 701.50a + CR 701.9a: with an empty hand, Grifter's real activation
/// draws one Island and automatically selects it for the connive discard.
/// Library of Leng's optional replacement pauses the single-card move.
fn connive_with_a_paused_single_discard(players: u8) -> (GameRunner, ObjectId) {
    let db = shared_card_db().expect("card database");
    let mut scenario = if players == 2 {
        GameScenario::new()
    } else {
        GameScenario::new_n_player(players, 42)
    };
    scenario.at_phase(Phase::PreCombatMain);
    for seat in 1..players {
        scenario.add_real_card(PlayerId(seat), "Island", Zone::Library, db);
    }
    let drawn = scenario.add_real_card(P0, "Island", Zone::Library, db);
    let grifter = scenario.add_real_card(P0, "Hypnotic Grifter", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Library of Leng", Zone::Battlefield, db);
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    assert!(runner.state().players[P0.0 as usize].hand.is_empty());
    let ability_index = runner.state().objects[&grifter]
        .abilities
        .iter()
        .position(|ability| matches!(ability.effect.as_ref(), Effect::Connive { .. }))
        .expect("the printed Grifter has its connive activation");
    runner.activate(grifter, ability_index).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    let pending = runner
        .state()
        .pending_single_discard
        .as_ref()
        .expect("reach guard: the real replacement paused a frameless single discard");
    assert_eq!(pending.player, P0);
    assert_eq!(pending.source_id, None);
    assert_eq!(pending.paused_card.object_id, drawn);
    assert_eq!(
        pending.paused_card.incarnation,
        runner.state().objects[&drawn].incarnation
    );
    assert!(runner.state().pending_discard_batch.is_none());
    assert_eq!(runner.state().objects[&drawn].zone, Zone::Hand);
    assert!(carrier_is_live(&runner));
    assert_round_trips(&runner);
    (runner, drawn)
}

/// CR 104.1 + CR 104.3a: the departing opponent is not the replacement chooser;
/// the surviving player's single-discard cursor retires with the finished game.
#[test]
fn terminal_concession_retires_a_paused_single_discard() {
    let (mut runner, drawn) = connive_with_a_paused_single_discard(2);
    concede(&mut runner, P1);
    assert_eq!(
        runner.state().objects[&drawn].zone,
        Zone::Hand,
        "retiring a discard must not commit the paused move"
    );
    assert_finished_game_retired_its_resolution(&runner, Some(P0));
}

/// CR 104.3a: a different opponent leaving a continuing game preserves the
/// replacement and its cursor. Declining Leng must still perform that discard.
#[test]
fn nonterminal_concession_preserves_a_paused_single_discard() {
    let (mut runner, drawn) = connive_with_a_paused_single_discard(3);
    concede(&mut runner, P2);
    assert!(runner.state().game_end.is_none());
    assert!(runner.state().pending_single_discard.is_some());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    assert_round_trips(&runner);
    let (chooser, decline) = decline_index(&runner);
    apply(
        runner.state_mut(),
        chooser,
        GameAction::ChooseReplacement { index: decline },
    )
    .expect("declining Leng resumes the single discard");
    assert!(runner.state().pending_single_discard.is_none());
    assert_eq!(runner.state().objects[&drawn].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().cards_discarded_this_turn_by_player.get(&P0),
        Some(&1)
    );
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// Prodigal Sorcerer's printed ping targets its controller, the Horde. The
/// variant's damage-to-mill rule reaches a CR 616.1 ordering choice between
/// Rest in Peace and the opponent's Leyline of the Void. The source owner and
/// replacement chooser both survive the opponent's concession.
fn horde_with_paused_damage() -> (GameRunner, ObjectId, ObjectId) {
    let db = shared_card_db().expect("card database");
    let mut scenario = GameScenario::new_with_format(
        FormatConfig::horde(ChallengeDeck::CybermanHorde.default_ruleset()),
        2,
        42,
    );
    scenario.at_phase(Phase::PreCombatMain);
    let tim = scenario.add_real_card(P0, "Prodigal Sorcerer", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Rest in Peace", Zone::Battlefield, db);
    scenario.add_real_card(P1, "Leyline of the Void", Zone::Battlefield, db);
    let milled = scenario.add_real_card(P0, "Island", Zone::Library, db);
    for _ in 0..4 {
        scenario.add_real_card(P0, "Island", Zone::Library, db);
    }
    scenario.add_real_card(P1, "Island", Zone::Library, db);
    let mut runner = scenario.build();
    // Horde starts with a survivor's turn. Pass through that priority window
    // so the Horde can activate its own permanent at instant speed.
    for _ in 0..2 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ) {
            break;
        }
        pass(&mut runner);
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    runner
        .act(GameAction::ActivateAbility {
            source_id: tim,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("the printed ping activation is legal");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P0)),
        })
        .expect("any target includes the source's controller");
    pass_until_prompt(&mut runner);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    let pending = runner
        .state()
        .pending_damage_post_loss
        .as_ref()
        .expect("reach guard: the real damage paused after its redirected life loss");
    assert_eq!(pending.source_id, tim);
    assert_eq!(pending.controller, P0);
    assert_eq!(pending.target, P0);
    assert_eq!(pending.actual_amount, 1);
    assert!(!pending.is_combat);
    assert_eq!(runner.state().objects[&milled].zone, Zone::Library);
    assert!(carrier_is_live(&runner));
    assert_round_trips(&runner);
    (runner, tim, milled)
}

/// CR 104.1 + CR 104.3a: no post-loss damage work is owed to a finished game,
/// including a Horde damage-to-mill instruction paused on CR 616.1 ordering.
#[test]
fn terminal_concession_retires_paused_damage_post_loss() {
    let (mut runner, _, milled) = horde_with_paused_damage();
    concede(&mut runner, P1);
    assert_eq!(runner.state().objects[&milled].zone, Zone::Library);
    assert_finished_game_retired_its_resolution(&runner, Some(P0));
}

/// CR 120.1 + CR 616.1: an ongoing game preserves the damage continuation.
/// Answering the original replacement choice still records the damage once.
#[test]
fn a_continuing_game_finishes_paused_damage_post_loss() {
    let (mut runner, tim, milled) = horde_with_paused_damage();
    assert!(runner.state().game_end.is_none());
    assert!(runner.state().pending_damage_post_loss.is_some());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    assert_round_trips(&runner);
    for _ in 0..4 {
        if !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            break;
        }
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("the original mill replacement resumes");
    }
    runner.advance_until_stack_empty();
    assert!(runner.state().pending_damage_post_loss.is_none());
    assert_eq!(runner.state().objects[&milled].zone, Zone::Exile);
    let records: Vec<_> = runner
        .state()
        .damage_dealt_this_turn
        .iter()
        .filter(|record| record.source_id == tim)
        .collect();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].target, TargetRef::Player(P0));
    assert_eq!(records[0].amount, 1);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}
