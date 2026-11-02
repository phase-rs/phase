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
//! The finished game's resolution is retired at the action boundary
//! (`engine::settle_finished_game`), after every resolver has unwound and keyed on
//! the recorded terminal result (`GameState::game_end`), not on the wait. The work
//! the resolution parked outside the resolution stack goes first: its own paused
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
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::move_to_zone;
use engine::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, Effect, PlayerFilter,
    QuantityExpr, ReplacementDefinition, ReplacementMode, ReturnResultId, SacrificeCost,
    SubAbilityLink, TargetFilter,
};
use engine::types::actions::{CastChoice, GameAction, ResolutionOptionalPaymentChoice};
use engine::types::events::GameEvent;
use engine::types::game_state::{
    GameState, PayCostKind, PendingCostMoveResume, PendingSacrificeCostCompletion,
    PersistedGameState, PersistedRestoreFinalization, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::match_config::{MatchConfig, MatchPhase, MatchType};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::resolution::FrameKind;
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
/// Synthetic (no legal printed card has exactly this text): an enters trigger, so
/// the carrier is a triggered ability with a firing, whose "you may" parks an
/// `OptionalEffect` frame.
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
fn assert_round_trips(runner: &GameRunner) {
    let json = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("capture: the state serializes");
    serde_json::from_str::<PersistedGameState>(&json)
        .expect("decode: the persisted state decodes")
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("admit: the persisted state is admissible")
        .finalize_after_rehydration(|_| Ok(()))
        .expect("finalize: the restored state is publishable");
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
        "CR 104.1: a finished game keeps none of its resolution's parked work, found {left:?}"
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
    assert!(
        state.resolution_stack.is_empty(),
        "a finished game keeps no resolution frame, found {:?}",
        frame_kinds(runner)
    );
    assert!(
        state.resolving_stack_entry.is_none(),
        "a finished game keeps no resolution carrier"
    );
    assert!(
        state.owed_spell_final_part().is_none(),
        "CR 104.1: no CR 608.2n final part is owed to a finished game"
    );
    assert!(
        state.return_result_frames.is_empty() && state.active_return_result_occurrence.is_none(),
        "a finished game keeps no return-result occurrence"
    );
    assert_no_parked_resolution_work(state);
    assert_round_trips(runner);
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
            WaitingFor::OptionalEffectChoice { player, .. } if player == P0
        ),
        "reach guard: the search offer is asked, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(frame_kinds(&runner), vec![FrameKind::OptionalEffect]);
    assert!(runner.state().game_end.is_none());
    assert_eq!(runner.state().players[1].life, 16);
    runner
}

/// Cast the synthetic `ENTERS_MAY_DRAW` creature and pass until its enters
/// trigger asks the "you may".
fn enters_may_draw_at_its_offer(players: u8) -> GameRunner {
    let mut scenario = board(players);
    scenario.add_basic_land(P0, ManaColor::Blue);
    let creature = scenario
        .add_creature_to_hand_from_oracle(P0, "Test Seer", 1, 1, ENTERS_MAY_DRAW)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
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
fn optional_graveyard_exile_replacement() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Graveyard)
        .mode(ReplacementMode::Optional { decline: None })
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                owner_library: false,
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
/// on the Warden's CR 616.1 choice. The cursor is parked at index 1 outside the
/// resolution stack. `observer` adds a synthetic P0 "Whenever another creature
/// dies, you gain 1 life." creature. Returns the runner, both fodder and P0's life
/// before the cast.
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
            "Death Observer",
            1,
            1,
            "Whenever another creature dies, you gain 1 life.",
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

    // Reach guard: the second sacrifice is paused on a CR 616.1 choice, its cursor
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
    let mut scenario = board(2);
    add_swamps(&mut scenario, 5);
    scenario.add_creature(P0, "Host", 2, 2);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Shadow Slice", false, SHADOW_SLICE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 4,
        })
        .with_keyword(Keyword::Cipher)
        .id();
    let mut runner = scenario.build();

    runner.cast(spell).target_player(P1).commit();
    pass(&mut runner);
    pass(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CipherEncodeChoice { player, .. } if player == P0
        ),
        "reach guard: the cipher offer is asked, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(frame_kinds(&runner), vec![FrameKind::CipherEncode]);
    assert!(carrier_is_live(&runner));

    concede(&mut runner, P1);

    assert!(runner.state().players[1].is_eliminated);
    assert_finished_game_retired_its_resolution(&runner, Some(P0));
}

/// CR 104.3a: P1 concedes while an enters trigger's "you may" is pending; the
/// carrier is a triggered ability with a firing. Synthetic text (see
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

/// CR 104.3a + CR 616.1: the resolution's own optional sacrifice, paid
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
/// so P0's paused sacrifice and its CR 616.1 choice stay and resume: declining
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
/// and the active player then receives priority. Synthetic text (see
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

/// CR 104.3a + CR 616.1: Hymn to Tourach (verbatim; cost zero as in
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

/// CR 104.3a + CR 608.2g + CR 616.1: Violent Outburst's cascade casts Village
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
fn demonic_tutor_at_its_search(players: u8) -> (GameRunner, ObjectId) {
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
    assert!(runner.state().active_library_searches.get(&P0).is_some());
    assert!(runner
        .state()
        .active_search_decision_controls
        .get(&P0)
        .is_some());
    assert!(carrier_is_live(&runner));
    assert!(runner.state().game_end.is_none());
    let found = *cards.first().expect("P0's library offers a card");
    (runner, found)
}

/// CR 104.3a + CR 701.23a: P1 concedes while P0 searches P0's library for Demonic
/// Tutor. The search session and its decision control go with the finished game,
/// and no card is found (CR 104.1).
#[test]
fn conceding_during_a_library_search_retires_the_search_session() {
    let (mut runner, found) = demonic_tutor_at_its_search(2);
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
    let (mut runner, found) = demonic_tutor_at_its_search(3);

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
