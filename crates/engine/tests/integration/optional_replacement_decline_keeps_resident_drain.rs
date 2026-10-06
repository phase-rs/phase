//! CR 614.6 + CR 616.1f + CR 616.1g: declining an optional replacement whose
//! chosen branch has no post-effect must keep every post-replacement drain that
//! is already resident.
//!
//! The defect: `continue_replacement_impl`'s optional arm answered a declined
//! "may" with no post-effect (a declined printed or granted dredge, CR 702.52a's
//! "you may"; an unpaid shock-land payment) by abandoning the whole active drain
//! stack. Two things were lost:
//!
//! * **The same event's earlier rider.** A mandatory replacement already applied
//!   to this draw or zone change (Blood Scrivener's "you lose 1 life",
//!   Breathstealer's Crypt's reveal, Land Equilibrium's sacrifice) stashed its own
//!   actions. CR 614.6 makes the modified event, actions included, the event that
//!   happens; CR 616.1f only repeats the choice among effects that are still
//!   applicable. It never undoes one that already applied.
//! * **An outer paused continuation's event context.** When a continuation's
//!   own draw is replaced (Alms Collector's "you and that player each draw a
//!   card", CR 616.1g), the outer drain is `Paused` while the contained draw's
//!   prompt is answered. Abandoning it misroutes the rest of the continuation.
//!
//! The fix: a declined branch with no post-effect installs nothing and clears
//! nothing. That leaves the drain stack exactly as every other resumed choice
//! does, including a mandatory CR 616.1 ordering pick.
//!
//! R7 also pins CR 109.5 against the "that player" scope: Land Equilibrium's "you"
//! is its controller (P1), while the sacrifice belongs to the entering land's
//! controller (P0), so P1's land may never be in the pool.
//!
//! Oracle texts are verbatim from Scryfall.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{
    ActionResult, PersistedGameState, ReplacementCandidateSummary, ReplacementChoiceKind,
    WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const BLOOD_SCRIVENER: &str = "If you would draw a card while you have no cards in hand, instead you draw two cards and you lose 1 life.";

const BREATHSTEALERS_CRYPT: &str = "If a player would draw a card, instead they draw a card and reveal it. If it's a creature card, that player discards it unless they pay 3 life.";

const ALMS_COLLECTOR: &str =
    "Flash\nIf an opponent would draw two or more cards, instead you and that player each draw a card.";

const NECROBLOOM: &str = "Landfall — Whenever a land you control enters, create a 0/1 green Plant creature token. If you control seven or more lands with different names, create a 2/2 black Zombie creature token instead.\nLand cards in your graveyard have dredge 2. (You may return a land card from your graveyard to your hand and mill two cards instead of drawing a card.)";

const TEFERIS_AGELESS_INSIGHT: &str = "If you would draw a card except the first one you draw in each of your draw steps, draw two cards instead.";

const LAND_EQUILIBRIUM: &str = "If an opponent who controls at least as many lands as you do would put a land onto the battlefield, that player instead puts that land onto the battlefield then sacrifices a land of their choice.";

const STOMPING_GROUND: &str =
    "({T}: Add {R} or {G}.)\nAs this land enters, you may pay 2 life. If you don't, it enters tapped.";

/// Precombat main, with four typeless library cards for each player (enough
/// for Dredge 2's CR 702.52b threshold and for Blood Scrivener's two cards).
fn base_scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["P0 Lib A", "P0 Lib B", "P0 Lib C", "P0 Lib D"]);
    scenario.with_library_top(P1, &["P1 Lib A", "P1 Lib B", "P1 Lib C", "P1 Lib D"]);
    scenario
}

fn draw_cards(runner: &mut GameRunner, player: PlayerId, count: u32) -> ActionResult {
    runner.state_mut().debug_mode = true;
    runner
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: player,
            count,
        }))
        .expect("debug draw must succeed")
}

fn draw_one(runner: &mut GameRunner, player: PlayerId) -> ActionResult {
    draw_cards(runner, player, 1)
}

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn library_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].library.len()
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner
        .state()
        .objects
        .get(&id)
        .map(|o| o.zone)
        .expect("object must exist")
}

/// The parked `ReplacementChoice`, or `None` when no replacement prompt is open.
fn prompt(
    runner: &GameRunner,
) -> Option<(
    PlayerId,
    ReplacementChoiceKind,
    Vec<ReplacementCandidateSummary>,
)> {
    match runner.state().waiting_for.clone() {
        WaitingFor::ReplacementChoice {
            player,
            kind,
            candidates,
            ..
        } => Some((player, kind, candidates)),
        _ => None,
    }
}

/// Nothing left on the stack, on the resolution stack, or mid-resolution.
fn stacks_settled(runner: &GameRunner) -> bool {
    let state = runner.state();
    state.stack.is_empty()
        && state.resolution_stack.is_empty()
        && state.resolving_stack_entry.is_none()
}

/// A printed "Dredge 2" creature card in `player`'s graveyard, built through the
/// scenario harness's production synthesis so it carries the object-carried
/// Draw replacement.
fn add_printed_dredger(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_creature_to_graveyard(player, "Test Dredger", 1, 1)
        .from_oracle_text_with_keywords(&["Dredge"], "Dredge 2")
        .id()
}

/// Reach-guard, then answer a CR 616.1 ordering prompt by picking `pick`.
fn order_pick(runner: &mut GameRunner, player: PlayerId, pick: ObjectId, other: ObjectId) {
    let (chooser, kind, candidates) = prompt(runner).unwrap_or_else(|| {
        panic!(
            "expected a CR 616.1 ordering prompt, got {:?}",
            runner.state().waiting_for
        )
    });
    assert_eq!(
        kind,
        ReplacementChoiceKind::Order,
        "reach-guard: expected an ordering prompt, got {candidates:?}"
    );
    assert_eq!(
        chooser, player,
        "reach-guard: the affected player orders the effects, got {candidates:?}"
    );
    assert!(
        candidates.iter().any(|c| c.source_id == pick)
            && candidates.iter().any(|c| c.source_id == other),
        "reach-guard: the ordering prompt must hold both sources, got {candidates:?}"
    );
    let index = candidates
        .iter()
        .position(|c| c.source_id == pick)
        .expect("the picked source is present");
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("the ordering pick must be accepted");
}

/// Reach-guard, then answer `source`'s accept/decline prompt.
fn answer_optional(
    runner: &mut GameRunner,
    player: PlayerId,
    source: ObjectId,
    accept: bool,
) -> ActionResult {
    let (chooser, kind, candidates) = prompt(runner).unwrap_or_else(|| {
        panic!(
            "expected the accept/decline prompt, got {:?}",
            runner.state().waiting_for
        )
    });
    assert_eq!(
        kind,
        ReplacementChoiceKind::OptionalBranch,
        "reach-guard: expected an accept/decline prompt, got {candidates:?}"
    );
    assert_eq!(
        chooser, player,
        "reach-guard: wrong chooser for the accept/decline prompt, got {candidates:?}"
    );
    assert!(
        candidates.iter().all(|c| c.source_id == source),
        "reach-guard: every option must be attributed to the optional source, got {candidates:?}"
    );
    assert_eq!(
        candidates.get(1).map(|c| c.description.as_str()),
        Some("Decline"),
        "reach-guard: decline is index 1, got {candidates:?}"
    );
    let index = if accept { 0 } else { 1 };
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("the accept/decline answer must be accepted")
}

fn revealed_for(events: &[GameEvent], player: PlayerId) -> bool {
    events
        .iter()
        .any(|e| matches!(e, GameEvent::CardsRevealed { player: p, .. } if *p == player))
}

/// P0 casts "Draw two cards." in precombat main with ten library cards for each
/// player (a copy of `count_form_draw_replacement.rs`'s `cast_draw_spell_in`).
/// `resolve()` halts at any `ReplacementChoice` it is not told to answer.
fn cast_draw_two_with<T>(setup: impl FnOnce(&mut GameScenario) -> T) -> (GameRunner, T) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        for i in 0..10 {
            scenario.add_spell_to_library_top(player, &format!("Filler {i}"), true);
        }
    }
    let ids = setup(&mut scenario);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Draw Spell", false, "Draw two cards.")
        .id();
    let mana: Vec<ManaUnit> = (0..8)
        .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
        .collect();
    scenario.with_mana_pool(P0, mana);

    let mut runner = scenario.build();
    runner.cast(spell).resolve();
    (runner, ids)
}

// --- Same event, Draw (S1) ---

/// R1: Blood Scrivener applies first (count 1 → 2, rider "you lose 1 life"), then
/// the printed dredge is declined. The draw that happens is Scrivener's, so the
/// life loss happens too (CR 614.6 + CR 616.1f).
#[test]
fn scrivener_first_then_declined_printed_dredge_still_loses_one_life() {
    let mut scenario = base_scenario();
    let scrivener = scenario
        .add_creature_from_oracle(P0, "Blood Scrivener", 2, 1, BLOOD_SCRIVENER)
        .id();
    let dredger = add_printed_dredger(&mut scenario, P0);
    let mut runner = scenario.build();
    assert_eq!(
        hand_len(&runner, P0),
        0,
        "fixture: Blood Scrivener needs an empty hand"
    );
    let library_before = library_len(&runner, P0);

    draw_one(&mut runner, P0);
    order_pick(&mut runner, P0, scrivener, dredger);
    answer_optional(&mut runner, P0, dredger, false);
    answer_optional(&mut runner, P0, dredger, false);

    assert_eq!(hand_len(&runner, P0), 2, "Blood Scrivener: draw two cards");
    assert_eq!(
        life(&runner, P0),
        19,
        "CR 614.6 + CR 616.1f: declining the dredge must not delete Blood Scrivener's \
         \"you lose 1 life\""
    );
    assert_eq!(
        zone_of(&runner, dredger),
        Zone::Graveyard,
        "the declined dredger stays"
    );
    assert_eq!(library_len(&runner, P0), library_before - 2);
    assert!(prompt(&runner).is_none(), "no prompt may remain");
    assert!(stacks_settled(&runner), "every stack must be settled");
}

/// R2: the same board with Necrobloom's GRANTED dredge instead of a printed one.
#[test]
fn scrivener_first_then_declined_granted_dredge_still_loses_one_life() {
    let mut scenario = base_scenario();
    let scrivener = scenario
        .add_creature_from_oracle(P0, "Blood Scrivener", 2, 1, BLOOD_SCRIVENER)
        .id();
    scenario.add_creature_from_oracle(P0, "The Necrobloom", 2, 7, NECROBLOOM);
    let forest = scenario.add_land_to_graveyard(P0, "Forest").id();
    let mut runner = scenario.build();
    assert_eq!(
        hand_len(&runner, P0),
        0,
        "fixture: Blood Scrivener needs an empty hand"
    );
    let library_before = library_len(&runner, P0);

    draw_one(&mut runner, P0);
    order_pick(&mut runner, P0, scrivener, forest);
    answer_optional(&mut runner, P0, forest, false);
    answer_optional(&mut runner, P0, forest, false);

    assert_eq!(hand_len(&runner, P0), 2, "Blood Scrivener: draw two cards");
    assert_eq!(
        life(&runner, P0),
        19,
        "CR 614.6 + CR 616.1f: declining the granted dredge must not delete Blood \
         Scrivener's \"you lose 1 life\""
    );
    assert_eq!(
        zone_of(&runner, forest),
        Zone::Graveyard,
        "the declined land stays"
    );
    assert_eq!(library_len(&runner, P0), library_before - 2);
    assert!(prompt(&runner).is_none(), "no prompt may remain");
    assert!(stacks_settled(&runner), "every stack must be settled");
}

/// Accepted Dredge replaces one draw from Blood Scrivener's two-card event,
/// while its already-applied life-loss rider still resolves and the second draw
/// resumes (CR 614.6 + CR 616.1f + CR 702.52a).
#[test]
fn scrivener_first_then_accepted_printed_dredge_keeps_the_rider_and_second_draw() {
    let mut scenario = base_scenario();
    let scrivener = scenario
        .add_creature_from_oracle(P0, "Blood Scrivener", 2, 1, BLOOD_SCRIVENER)
        .id();
    let dredger = add_printed_dredger(&mut scenario, P0);
    let mut runner = scenario.build();
    let library_before = library_len(&runner, P0);

    draw_one(&mut runner, P0);
    order_pick(&mut runner, P0, scrivener, dredger);
    assert_eq!(
        life(&runner, P0),
        20,
        "the rider must wait while the replacement-created draws are pending"
    );
    answer_optional(&mut runner, P0, dredger, true);
    runner.advance_until_stack_empty();

    let outcome = (
        life(&runner, P0),
        zone_of(&runner, dredger),
        hand_len(&runner, P0),
        library_len(&runner, P0),
        prompt(&runner).is_none(),
        stacks_settled(&runner),
    );
    assert_eq!(
        outcome,
        (19, Zone::Hand, 2, library_before - 3, true, true),
        "expected (life, dredger zone, hand size, library size, no prompt, settled)"
    );
    assert_eq!(
        runner.state().last_effect_count,
        Some(1),
        "the returned Dredger is not a draw; only the child's second unit counts"
    );

    // The completed Scrivener rider and Dredge continuation must not be left
    // pending for the next ordinary Engine draw.
    draw_one(&mut runner, P0);
    assert_eq!(life(&runner, P0), 19, "the life-loss rider runs once");
    assert_eq!(
        zone_of(&runner, dredger),
        Zone::Hand,
        "Dredge does not repeat"
    );
    assert_eq!(hand_len(&runner, P0), 3);
    assert_eq!(library_len(&runner, P0), library_before - 4);
    assert!(prompt(&runner).is_none());
    assert!(stacks_settled(&runner));
}

/// The same accepted path with Necrobloom's granted Dredge.
#[test]
fn scrivener_first_then_accepted_granted_dredge_keeps_the_rider_and_second_draw() {
    let mut scenario = base_scenario();
    let scrivener = scenario
        .add_creature_from_oracle(P0, "Blood Scrivener", 2, 1, BLOOD_SCRIVENER)
        .id();
    scenario.add_creature_from_oracle(P0, "The Necrobloom", 2, 7, NECROBLOOM);
    let forest = scenario.add_land_to_graveyard(P0, "Forest").id();
    let mut runner = scenario.build();
    let library_before = library_len(&runner, P0);

    draw_one(&mut runner, P0);
    order_pick(&mut runner, P0, scrivener, forest);
    assert_eq!(
        life(&runner, P0),
        20,
        "the rider must wait while the replacement-created draws are pending"
    );
    answer_optional(&mut runner, P0, forest, true);
    runner.advance_until_stack_empty();

    let outcome = (
        life(&runner, P0),
        zone_of(&runner, forest),
        hand_len(&runner, P0),
        library_len(&runner, P0),
        prompt(&runner).is_none(),
        stacks_settled(&runner),
    );
    assert_eq!(
        outcome,
        (19, Zone::Hand, 2, library_before - 3, true, true),
        "expected (life, dredged land zone, hand size, library size, no prompt, settled)"
    );
    assert_eq!(
        runner.state().last_effect_count,
        Some(1),
        "the returned Forest is not a draw; only the child's second unit counts"
    );
}

#[test]
fn declined_dredge_does_not_leak_into_the_parent_draws_next_unit() {
    let mut scenario = base_scenario();
    let scrivener = scenario
        .add_creature_from_oracle(P0, "Blood Scrivener", 2, 1, BLOOD_SCRIVENER)
        .id();
    let dredger = add_printed_dredger(&mut scenario, P0);
    let mut runner = scenario.build();
    let library_before = library_len(&runner, P0);
    draw_cards(&mut runner, P0, 2);
    order_pick(&mut runner, P0, dredger, scrivener);
    let (chooser, kind, candidates) = prompt(&runner).expect("the first draw offers Dredge");
    assert_eq!(chooser, P0);
    assert_eq!(kind, ReplacementChoiceKind::OptionalBranch);
    assert!(candidates
        .iter()
        .any(|candidate| candidate.source_id == dredger));
    assert_eq!(life(&runner, P0), 20);

    answer_optional(&mut runner, P0, dredger, false);

    // The generated child Draw(2) and its life rider finish before the parent
    // instruction resumes its original second draw.
    let (chooser, kind, candidates) =
        prompt(&runner).expect("the parent's second draw offers Dredge again");
    assert_eq!(chooser, P0);
    assert_eq!(kind, ReplacementChoiceKind::OptionalBranch);
    assert!(candidates
        .iter()
        .any(|candidate| candidate.source_id == dredger));
    assert_eq!(hand_len(&runner, P0), 2);
    assert_eq!(life(&runner, P0), 19);

    answer_optional(&mut runner, P0, dredger, true);
    runner.advance_until_stack_empty();

    assert_eq!(life(&runner, P0), 19, "the rider resolves exactly once");
    assert_eq!(zone_of(&runner, dredger), Zone::Hand);
    assert_eq!(hand_len(&runner, P0), 3);
    assert_eq!(library_len(&runner, P0), library_before - 4);
    assert!(prompt(&runner).is_none());
    assert!(stacks_settled(&runner));
}

/// CR 121.6c: replacing the Crypt's draw with Dredge must not run its
/// card-specific "reveal it" action on a card that was never drawn.
#[test]
fn crypt_first_then_accepted_dredge_does_not_reveal_a_drawn_card() {
    let mut scenario = base_scenario();
    let crypt = scenario
        .add_enchantment_from_oracle(P1, "Breathstealer's Crypt", BREATHSTEALERS_CRYPT)
        .id();
    let dredger = add_printed_dredger(&mut scenario, P0);
    let mut runner = scenario.build();
    let library_before = library_len(&runner, P0);

    draw_one(&mut runner, P0);
    order_pick(&mut runner, P0, crypt, dredger);
    let accepted = answer_optional(&mut runner, P0, dredger, true);
    runner.advance_until_stack_empty();

    assert!(
        !revealed_for(&accepted.events, P0),
        "CR 121.6c: the Crypt must not reveal a card when Dredge replaces the draw"
    );
    assert_eq!(zone_of(&runner, dredger), Zone::Hand);
    assert_eq!(
        hand_len(&runner, P0),
        1,
        "only the dredged card reaches hand"
    );
    assert_eq!(library_len(&runner, P0), library_before - 2);
    assert_eq!(life(&runner, P0), 20);
    assert!(prompt(&runner).is_none());
    assert!(stacks_settled(&runner));
}

/// R3: the rider's controller (P1, Breathstealer's Crypt) is not the drawer
/// (P0). The kept rider must still reveal for P0, the player its drain's
/// `event_target` names. Only the reveal is asserted: the parsed rider reveals
/// the library top rather than the drawn card (accept-side follow-up recorded on
/// PR #9235, item 5), and the typeless revealed card raises no pay/discard prompt.
#[test]
fn crypt_first_then_declined_dredge_still_runs_the_crypt_rider() {
    let mut scenario = base_scenario();
    let crypt = scenario
        .add_enchantment_from_oracle(P1, "Breathstealer's Crypt", BREATHSTEALERS_CRYPT)
        .id();
    let dredger = add_printed_dredger(&mut scenario, P0);
    let mut runner = scenario.build();
    let hand_before = hand_len(&runner, P0);
    let library_before = library_len(&runner, P0);

    draw_one(&mut runner, P0);
    order_pick(&mut runner, P0, crypt, dredger);
    let decline = answer_optional(&mut runner, P0, dredger, false);

    assert!(
        revealed_for(&decline.events, P0),
        "CR 614.6 + CR 616.1f: the Crypt's rider must still reveal for the drawer after \
         the dredge is declined, got {:?}",
        decline.events
    );
    assert_eq!(hand_len(&runner, P0), hand_before + 1);
    assert_eq!(library_len(&runner, P0), library_before - 1);
    assert_eq!(life(&runner, P0), 20);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "got {:?}",
        runner.state().waiting_for
    );
    assert!(stacks_settled(&runner), "every stack must be settled");
}

/// R3c (control): the Crypt alone runs its rider. This shows the reveal check
/// can fire, and that a typeless revealed card raises no pay/discard prompt.
#[test]
fn crypt_alone_runs_its_rider_on_the_draw() {
    let mut scenario = base_scenario();
    scenario.add_enchantment_from_oracle(P1, "Breathstealer's Crypt", BREATHSTEALERS_CRYPT);
    let mut runner = scenario.build();
    let hand_before = hand_len(&runner, P0);

    let draw = draw_one(&mut runner, P0);

    assert!(
        revealed_for(&draw.events, P0),
        "the Crypt's rider must reveal for the drawer, got {:?}",
        draw.events
    );
    assert_eq!(hand_len(&runner, P0), hand_before + 1);
    assert_eq!(life(&runner, P0), 20);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "got {:?}",
        runner.state().waiting_for
    );
}

/// R4c (control): Blood Scrivener alone.
#[test]
fn scrivener_alone_draws_two_and_loses_one_life() {
    let mut scenario = base_scenario();
    scenario.add_creature_from_oracle(P0, "Blood Scrivener", 2, 1, BLOOD_SCRIVENER);
    let mut runner = scenario.build();

    draw_one(&mut runner, P0);

    assert!(
        prompt(&runner).is_none(),
        "a lone mandatory replacement never prompts"
    );
    assert_eq!(hand_len(&runner, P0), 2);
    assert_eq!(life(&runner, P0), 19);
}

/// R4o (control): the reverse order. The dredge is declined BEFORE Blood
/// Scrivener applies, so its rider is stashed after the decline.
#[test]
fn declined_dredge_first_then_scrivener_still_loses_one_life() {
    let mut scenario = base_scenario();
    let scrivener = scenario
        .add_creature_from_oracle(P0, "Blood Scrivener", 2, 1, BLOOD_SCRIVENER)
        .id();
    let dredger = add_printed_dredger(&mut scenario, P0);
    let mut runner = scenario.build();

    draw_one(&mut runner, P0);
    order_pick(&mut runner, P0, dredger, scrivener);
    answer_optional(&mut runner, P0, dredger, false);

    assert!(prompt(&runner).is_none(), "no prompt may remain");
    assert_eq!(hand_len(&runner, P0), 2);
    assert_eq!(life(&runner, P0), 19);
    assert_eq!(zone_of(&runner, dredger), Zone::Graveyard);
}

// --- Nested, CR 616.1g (S4, S5) ---

/// R5 (S4): Alms Collector's continuation ("you and that player each draw a
/// card") draws for its controller, and that contained draw offers P1 a granted
/// dredge. Declining it must keep the outer paused continuation, so the drawer
/// (P0) still gets its card.
#[test]
fn alms_collector_tail_reaches_the_drawer_after_a_declined_nested_granted_dredge() {
    let (mut runner, forest) = cast_draw_two_with(|scenario| {
        scenario.add_creature_from_oracle(P1, "Alms Collector", 3, 3, ALMS_COLLECTOR);
        scenario.add_creature_from_oracle(P1, "The Necrobloom", 2, 7, NECROBLOOM);
        scenario.add_land_to_graveyard(P1, "Forest").id()
    });

    answer_optional(&mut runner, P1, forest, false);
    runner.advance_until_stack_empty();

    assert_eq!(
        hand_len(&runner, P0),
        1,
        "CR 616.1g: the outer continuation must still give the drawer its card"
    );
    assert_eq!(
        hand_len(&runner, P1),
        1,
        "Alms Collector's controller draws one card"
    );
    assert_eq!(
        zone_of(&runner, forest),
        Zone::Graveyard,
        "the declined land stays"
    );
    assert_eq!(library_len(&runner, P0), 9);
    assert_eq!(library_len(&runner, P1), 9);
    assert!(prompt(&runner).is_none(), "no prompt may remain");
    assert!(stacks_settled(&runner), "every stack must be settled");
}

/// Nested P1 Scrivener child completion, then the outer P0 Dredge wait.
#[test]
fn nested_scrivener_rider_and_alms_tail_survive_an_accepted_granted_dredge() {
    let (mut runner, (scrivener, forest, p0_dredger)) = cast_draw_two_with(|scenario| {
        scenario.add_creature_from_oracle(P1, "Alms Collector", 3, 3, ALMS_COLLECTOR);
        scenario.add_creature_from_oracle(P1, "The Necrobloom", 2, 7, NECROBLOOM);
        let scrivener = scenario
            .add_creature_from_oracle(P1, "Blood Scrivener", 2, 1, BLOOD_SCRIVENER)
            .id();
        let forest = scenario.add_land_to_graveyard(P1, "Forest").id();
        let p0_dredger = add_printed_dredger(scenario, P0);
        (scrivener, forest, p0_dredger)
    });

    order_pick(&mut runner, P1, scrivener, forest);
    assert_eq!(life(&runner, P1), 20);
    let p0_library_before = library_len(&runner, P0);
    let accepted = answer_optional(&mut runner, P1, forest, true);
    let p1_draw = accepted
        .events
        .iter()
        .position(|event| matches!(event, GameEvent::CardDrawn { player_id: P1, .. }))
        .expect("the generated child draw completes before its rider");
    let rider = accepted
        .events
        .iter()
        .position(|event| {
            matches!(
                event,
                GameEvent::LifeChanged {
                    player_id: P1,
                    amount: -1,
                    ..
                }
            )
        })
        .expect("the child draw's life-loss rider resolves once");
    assert!(p1_draw < rider);
    assert_eq!(
        accepted
            .events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::LifeChanged {
                    player_id: P1,
                    amount: -1,
                    ..
                }
            ))
            .count(),
        1
    );
    assert!(
        !accepted
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::CardDrawn { player_id: P0, .. })),
        "the parent P0 draw is still waiting on its own Dredge choice"
    );
    assert!(has_printed_dredge_prompt(&runner, p0_dredger));

    let draw_ledger_count = |runner: &GameRunner, player: PlayerId| {
        runner
            .state()
            .player_actions_this_turn
            .iter()
            .filter(|entry| **entry == (player, PlayerActionKind::Draw))
            .count()
    };
    let completed_p1_draw_events = accepted
        .events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    player_id: P1,
                    action: PlayerActionKind::Draw,
                    ..
                }
            )
        })
        .count();
    let recorded_p1_draws_before_save = draw_ledger_count(&runner, P1);
    assert!(
        completed_p1_draw_events > 0,
        "the child Draw instruction completed"
    );
    assert_eq!(
        recorded_p1_draws_before_save, completed_p1_draw_events,
        "the completed child instruction is already recorded before the parent wait"
    );
    assert_eq!(
        draw_ledger_count(&runner, P0),
        0,
        "the parent instruction has not completed while its Dredge choice is pending"
    );

    let ledger_before_save = runner.state().player_actions_this_turn.clone();
    let mut restored = {
        let wire = serde_json::to_value(PersistedGameState::capture(runner.state().clone()))
            .expect("capture the parent Dredge wait after recording the child draw");
        let restored_state = serde_json::from_value::<PersistedGameState>(wire)
            .expect("decode the parent Dredge wait after recording the child draw")
            .into_game_state()
            .expect("canonical restore preserves the completed child's ledger entry");
        GameRunner::from_state(restored_state)
    };
    assert_eq!(
        restored.state().player_actions_this_turn,
        ledger_before_save
    );

    let resumed = answer_optional(&mut runner, P0, p0_dredger, true);
    assert_eq!(
        resumed
            .events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    player_id: P1,
                    action: PlayerActionKind::Draw,
                    ..
                }
            ))
            .count(),
        0,
        "resuming the parent must not replay the completed child instruction"
    );
    runner.advance_until_stack_empty();
    let restored_resumed = answer_optional(&mut restored, P0, p0_dredger, true);
    assert_eq!(
        restored_resumed
            .events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    player_id: P1,
                    action: PlayerActionKind::Draw,
                    ..
                }
            ))
            .count(),
        0,
        "restoring the parent wait must not replay the completed child instruction"
    );
    restored.advance_until_stack_empty();

    assert_eq!(
        draw_ledger_count(&runner, P1),
        recorded_p1_draws_before_save
    );
    assert_eq!(
        draw_ledger_count(&restored, P1),
        recorded_p1_draws_before_save
    );
    assert_eq!(
        runner.state().player_actions_this_turn,
        restored.state().player_actions_this_turn,
        "restored and uninterrupted parent resumes record the same new work"
    );
    let outcome = (
        hand_len(&runner, P1),
        life(&runner, P1),
        hand_len(&runner, P0),
        library_len(&runner, P1),
        library_len(&runner, P0),
        zone_of(&runner, forest),
        zone_of(&runner, p0_dredger),
        prompt(&runner).is_none(),
        runner.state().active_draw_sequence().is_none(),
        stacks_settled(&runner),
    );
    assert_eq!(
        outcome,
        (
            2,
            19,
            1,
            7,
            p0_library_before - 2,
            Zone::Hand,
            Zone::Hand,
            true,
            true,
            true,
        ),
        "both nested results resolve once and both Dredge returns are preserved"
    );
    assert_eq!(
        (
            hand_len(&restored, P1),
            life(&restored, P1),
            hand_len(&restored, P0),
            library_len(&restored, P1),
            library_len(&restored, P0),
            zone_of(&restored, forest),
            zone_of(&restored, p0_dredger),
            prompt(&restored).is_none(),
            restored.state().active_draw_sequence().is_none(),
            stacks_settled(&restored),
        ),
        outcome,
        "saving after the child result and resuming the parent matches uninterrupted resolution"
    );
}

/// Keep the Scrivener-free accepted-Dredge control for Alms Collector's tail.
#[test]
fn alms_collector_tail_reaches_the_drawer_after_an_accepted_nested_granted_dredge() {
    let (mut runner, forest) = cast_draw_two_with(|scenario| {
        scenario.add_creature_from_oracle(P1, "Alms Collector", 3, 3, ALMS_COLLECTOR);
        scenario.add_creature_from_oracle(P1, "The Necrobloom", 2, 7, NECROBLOOM);
        scenario.add_land_to_graveyard(P1, "Forest").id()
    });

    answer_optional(&mut runner, P1, forest, true);
    runner.advance_until_stack_empty();

    assert_eq!(hand_len(&runner, P0), 1, "the drawer still gets its card");
    assert_eq!(
        hand_len(&runner, P1),
        1,
        "Alms Collector's controller draws"
    );
    assert_eq!(zone_of(&runner, forest), Zone::Hand);
    assert_eq!(library_len(&runner, P0), 9);
    assert_eq!(library_len(&runner, P1), 8);
    assert!(prompt(&runner).is_none());
    assert!(stacks_settled(&runner));
}

/// Exercise a real CR 616.1 ordering prompt with Teferi chosen before Scrivener.
#[test]
fn teferi_first_then_scrivener_preserves_the_modified_draw_event() {
    let mut scenario = base_scenario();
    let scrivener = scenario
        .add_creature_from_oracle(P0, "Blood Scrivener", 2, 1, BLOOD_SCRIVENER)
        .id();
    let teferi = scenario
        .add_enchantment_from_oracle(P0, "Teferi's Ageless Insight", TEFERIS_AGELESS_INSIGHT)
        .as_legendary()
        .id();
    let mut runner = scenario.build();

    draw_one(&mut runner, P0);
    let (chooser, kind, candidates) = prompt(&runner).expect("ordering prompt must be shown");
    assert_eq!(chooser, P0);
    assert_eq!(kind, ReplacementChoiceKind::Order);
    let teferi_index = candidates
        .iter()
        .position(|candidate| candidate.source_id == teferi)
        .expect("Teferi must be offered");
    assert!(candidates
        .iter()
        .any(|candidate| candidate.source_id == scrivener));
    let ordered = runner
        .act(GameAction::ChooseReplacement {
            index: teferi_index,
        })
        .expect("choose Teferi before Scrivener");
    runner.advance_until_stack_empty();

    let card_drawn_events = ordered
        .events
        .iter()
        .filter(|event| matches!(event, GameEvent::CardDrawn { player_id: P0, .. }))
        .count();
    assert_eq!(
        (
            card_drawn_events,
            hand_len(&runner, P0),
            life(&runner, P0),
            library_len(&runner, P0),
            runner.state().last_effect_count,
        ),
        (3, 3, 19, 1, Some(3)),
        "Teferi's two individual draws let Scrivener replace only the first one"
    );
    assert!(prompt(&runner).is_none());
    assert!(stacks_settled(&runner));
}

/// CR 121.2 + CR 121.6b + CR 616.1g + CR 702.52a: a Dredge choice inside
/// Teferi's count-modified child draw must pause that child before its last unit.
fn teferi_first_then_printed_dredge_wait() -> (GameRunner, ObjectId, usize, Vec<GameEvent>) {
    let mut scenario = base_scenario();
    let teferi = scenario
        .add_enchantment_from_oracle(P0, "Teferi's Ageless Insight", TEFERIS_AGELESS_INSIGHT)
        .as_legendary()
        .id();
    let dredger = add_printed_dredger(&mut scenario, P0);
    let mut runner = scenario.build();
    let library_before = library_len(&runner, P0);
    let mut events = draw_one(&mut runner, P0).events;

    let (chooser, kind, candidates) = prompt(&runner).expect("ordering prompt must be shown");
    assert_eq!(chooser, P0);
    assert_eq!(kind, ReplacementChoiceKind::Order);
    let teferi_index = candidates
        .iter()
        .position(|candidate| candidate.source_id == teferi)
        .expect("Teferi must be offered");
    assert!(candidates
        .iter()
        .any(|candidate| candidate.source_id == dredger));
    let ordered = runner
        .act(GameAction::ChooseReplacement {
            index: teferi_index,
        })
        .expect("choose Teferi before Dredge");
    events.extend(ordered.events);
    (runner, dredger, library_before, events)
}

fn has_printed_dredge_prompt(runner: &GameRunner, dredger: ObjectId) -> bool {
    matches!(
        prompt(runner),
        Some((P0, ReplacementChoiceKind::OptionalBranch, candidates))
            if candidates.iter().any(|candidate| candidate.source_id == dredger)
    )
}

/// Captures whether the first prompt still owns the first of Teferi's two units.
fn first_teferi_child_pause_is_parked(runner: &mut GameRunner, dredger: ObjectId) -> bool {
    let waiting = runner.state().waiting_for.clone();
    let waiting_for_dredge = has_printed_dredge_prompt(runner, dredger);
    let pending = runner.state().pending_replacement.as_ref().map(|pending| {
        (
            pending.is_optional,
            pending.choice_player,
            pending.candidates.len(),
        )
    });
    let active = runner.state().active_draw_sequence().map(|frame| {
        (
            frame.frame_id,
            frame.remaining,
            frame.accumulated,
            frame.delivery_owner,
        )
    });
    let owner_result = active.and_then(|(_, _, _, owner)| owner).and_then(|owner| {
        runner
            .state_mut()
            .draw_sequence_frame_mut(owner)
            .map(|frame| frame.accumulated)
    });
    let parked_first_unit = active.is_some_and(|(_, remaining, accumulated, owner)| {
        remaining == 1 && accumulated == 0 && owner.is_some()
    });
    println!(
        "task32 first-pause waiting={waiting:?} pending={pending:?} active={active:?} owner_result={owner_result:?}"
    );
    waiting_for_dredge
        && pending == Some((true, Some(P0), 1))
        && parked_first_unit
        && owner_result == Some(0)
}

#[test]
fn teferi_child_accepts_first_printed_dredge_then_draws_its_last_unit() {
    let (mut runner, dredger, library_before, mut events) = teferi_first_then_printed_dredge_wait();
    let first_pause = first_teferi_child_pause_is_parked(&mut runner, dredger);
    let mut restored = if first_pause {
        let wire = serde_json::to_value(PersistedGameState::capture(runner.state().clone()))
            .expect("capture the nested Teferi child wait");
        let restored_state = serde_json::from_value::<PersistedGameState>(wire)
            .expect("decode the nested Teferi child wait")
            .into_game_state()
            .expect("canonical restore accepts the parent-child draw frames");
        Some(GameRunner::from_state(restored_state))
    } else {
        None
    };
    if has_printed_dredge_prompt(&runner, dredger) {
        events.extend(answer_optional(&mut runner, P0, dredger, true).events);
        runner.advance_until_stack_empty();
    }
    let restored_outcome = restored.as_mut().map(|restored| {
        let restored_events = answer_optional(restored, P0, dredger, true).events;
        restored.advance_until_stack_empty();
        (
            restored_events
                .iter()
                .filter(|event| matches!(event, GameEvent::CardDrawn { player_id: P0, .. }))
                .count(),
            hand_len(restored, P0),
            library_len(restored, P0),
            restored.state().last_effect_count,
            zone_of(restored, dredger),
            prompt(restored).is_none(),
            restored.state().active_draw_sequence().is_none(),
            stacks_settled(restored),
        )
    });

    let card_drawn = events
        .iter()
        .filter(|event| matches!(event, GameEvent::CardDrawn { player_id: P0, .. }))
        .count();
    let outcome = (
        card_drawn,
        hand_len(&runner, P0),
        library_len(&runner, P0),
        runner.state().last_effect_count,
        zone_of(&runner, dredger),
        prompt(&runner).is_none(),
        runner.state().active_draw_sequence().is_none(),
        stacks_settled(&runner),
    );
    println!("task32 accept-first pause={first_pause} outcome={outcome:?}");
    assert!(
        first_pause,
        "the first Dredge choice must park before unit two"
    );
    assert_eq!(
        outcome,
        (
            1,
            2,
            library_before - 3,
            Some(1),
            Zone::Hand,
            true,
            true,
            true
        ),
        "Dredge replaces unit one; only unit two is an actual draw"
    );
    assert_eq!(
        restored_outcome,
        Some(outcome),
        "same-version restore resumes the same nested choice and parent result"
    );
}

#[test]
fn teferi_child_decline_first_dredge_then_offer_second_independently() {
    let (mut runner, dredger, library_before, mut events) = teferi_first_then_printed_dredge_wait();
    let first_pause = first_teferi_child_pause_is_parked(&mut runner, dredger);
    let declined_first = has_printed_dredge_prompt(&runner, dredger);
    if declined_first {
        events.extend(answer_optional(&mut runner, P0, dredger, false).events);
    }

    let second_prompt = has_printed_dredge_prompt(&runner, dredger);
    let second_frame = runner
        .state()
        .active_draw_sequence()
        .map(|frame| (frame.remaining, frame.accumulated, frame.delivery_owner));
    let owner_result = second_frame
        .and_then(|(_, _, owner)| owner)
        .and_then(|owner| {
            runner
                .state_mut()
                .draw_sequence_frame_mut(owner)
                .map(|frame| frame.accumulated)
        });
    let second_pause = second_prompt
        && hand_len(&runner, P0) == 1
        && library_len(&runner, P0) == library_before - 1
        && second_frame.is_some_and(|(remaining, accumulated, owner)| {
            remaining == 0 && accumulated == 1 && owner.is_some()
        })
        && owner_result == Some(0);
    println!(
        "task32 after-decline second-prompt={second_prompt} frame={second_frame:?} owner_result={owner_result:?} hand={} library={}",
        hand_len(&runner, P0),
        library_len(&runner, P0)
    );
    assert!(
        declined_first && second_pause,
        "declining unit one must advance to a distinct unit-two choice"
    );
    events.extend(answer_optional(&mut runner, P0, dredger, true).events);
    runner.advance_until_stack_empty();

    let card_drawn = events
        .iter()
        .filter(|event| matches!(event, GameEvent::CardDrawn { player_id: P0, .. }))
        .count();
    let outcome = (
        card_drawn,
        hand_len(&runner, P0),
        library_len(&runner, P0),
        runner.state().last_effect_count,
        zone_of(&runner, dredger),
        prompt(&runner).is_none(),
        runner.state().active_draw_sequence().is_none(),
        stacks_settled(&runner),
    );
    println!(
        "task32 decline-first pause={first_pause} second-pause={second_pause} outcome={outcome:?}"
    );
    assert!(
        first_pause,
        "the first Dredge choice must park before unit two"
    );
    assert_eq!(
        outcome,
        (
            1,
            2,
            library_before - 3,
            Some(1),
            Zone::Hand,
            true,
            true,
            true
        ),
        "unit one draws normally; Dredge replaces unit two"
    );
}

/// R6 (S5): the contained draw also carries an inner Ready rider. P1 orders
/// Blood Scrivener before a printed dredge and declines the dredge: the inner
/// rider (P1 loses 1 life, its drain's `controller`) and the outer continuation
/// (P0 draws, its drain's `event_target`) must both survive.
#[test]
fn nested_scrivener_rider_and_alms_tail_survive_a_declined_nested_printed_dredge() {
    let (mut runner, (scrivener, dredger)) = cast_draw_two_with(|scenario| {
        scenario.add_creature_from_oracle(P1, "Alms Collector", 3, 3, ALMS_COLLECTOR);
        let scrivener = scenario
            .add_creature_from_oracle(P1, "Blood Scrivener", 2, 1, BLOOD_SCRIVENER)
            .id();
        let dredger = add_printed_dredger(scenario, P1);
        (scrivener, dredger)
    });
    assert_eq!(
        hand_len(&runner, P1),
        0,
        "fixture: Blood Scrivener needs P1's hand empty when the contained draw is proposed"
    );

    order_pick(&mut runner, P1, scrivener, dredger);
    answer_optional(&mut runner, P1, dredger, false);
    answer_optional(&mut runner, P1, dredger, false);
    runner.advance_until_stack_empty();

    assert_eq!(
        hand_len(&runner, P1),
        2,
        "Blood Scrivener: P1 draws two cards"
    );
    assert_eq!(
        life(&runner, P1),
        19,
        "CR 614.6 + CR 616.1f: the inner rider must survive the declined dredge"
    );
    assert_eq!(
        hand_len(&runner, P0),
        1,
        "CR 616.1g: the outer continuation must still give the drawer its card"
    );
    assert_eq!(
        life(&runner, P0),
        20,
        "the rider's \"you\" is P1, never the drawer"
    );
    assert_eq!(
        zone_of(&runner, dredger),
        Zone::Graveyard,
        "the declined dredger stays"
    );
    assert!(stacks_settled(&runner), "every stack must be settled");
}

// --- Same event, zone change (S1 on Moved) ---

/// The Land Equilibrium board: P1 controls Land Equilibrium and one basic; P0
/// controls two basics and plays Stomping Ground. Returns
/// `(runner, land_equilibrium, stomping_ground, p0_basic, p1_basic)`.
fn play_stomping_ground_into_land_equilibrium(
) -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let equilibrium = scenario
        .add_enchantment_from_oracle(P1, "Land Equilibrium", LAND_EQUILIBRIUM)
        .id();
    let p1_basic = scenario.add_basic_land(P1, ManaColor::White);
    let p0_basic = scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Red);
    let mut builder = scenario.add_land_to_hand(P0, "Stomping Ground");
    builder.from_oracle_text(STOMPING_GROUND);
    let stomping_ground = builder.id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&stomping_ground].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: stomping_ground,
            card_id,
        })
        .expect("P0 may play a land");
    (runner, equilibrium, stomping_ground, p0_basic, p1_basic)
}

/// The forced sacrifice: prompted to P0 with a pool of only P0's lands, then
/// answered with `p0_basic`, leaving the end state both orderings must reach.
fn sacrifice_p0_basic_and_assert_end_state(
    runner: &mut GameRunner,
    stomping_ground: ObjectId,
    p0_basic: ObjectId,
    p1_basic: ObjectId,
) {
    let WaitingFor::EffectZoneChoice { player, cards, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "Land Equilibrium: \"that player ... sacrifices a land of their choice\" — \
             expected the sacrifice prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        player, P0,
        "CR 109.5: the entering land's controller sacrifices, not Land Equilibrium's"
    );
    assert!(
        cards
            .iter()
            .all(|id| runner.state().objects[id].controller == P0),
        "the pool may hold only P0's lands, got {cards:?}"
    );
    assert!(
        !cards.contains(&p1_basic),
        "Land Equilibrium's controller's land is never eligible"
    );
    assert!(cards.contains(&p0_basic), "P0's basic must be eligible");
    runner
        .act(GameAction::SelectCards {
            cards: vec![p0_basic],
        })
        .expect("P0 sacrifices a land of their choice");

    assert_eq!(
        zone_of(runner, p0_basic),
        Zone::Graveyard,
        "the sacrificed land"
    );
    assert_eq!(zone_of(runner, stomping_ground), Zone::Battlefield);
    assert!(
        runner.state().objects[&stomping_ground].tapped,
        "the unpaid shock land enters tapped"
    );
    assert_eq!(life(runner, P0), 20, "the payment was declined");
    assert_eq!(
        zone_of(runner, p1_basic),
        Zone::Battlefield,
        "P1's land is untouched"
    );
    assert!(prompt(runner).is_none(), "no prompt may remain");
    assert!(stacks_settled(runner), "every stack must be settled");
}

/// R7 (S1 on Moved): Land Equilibrium applies first, then the shock payment is
/// declined. The land still enters and "that player" still sacrifices a land.
#[test]
fn land_equilibrium_first_then_declined_shock_payment_still_forces_the_sacrifice() {
    let (mut runner, equilibrium, stomping_ground, p0_basic, p1_basic) =
        play_stomping_ground_into_land_equilibrium();

    order_pick(&mut runner, P0, equilibrium, stomping_ground);
    answer_optional(&mut runner, P0, stomping_ground, false);

    sacrifice_p0_basic_and_assert_end_state(&mut runner, stomping_ground, p0_basic, p1_basic);
}

/// R7c (control): the reverse order. The payment is declined first, then Land
/// Equilibrium applies in the same resume.
#[test]
fn declined_shock_payment_first_then_land_equilibrium_forces_the_sacrifice() {
    let (mut runner, equilibrium, stomping_ground, p0_basic, p1_basic) =
        play_stomping_ground_into_land_equilibrium();

    order_pick(&mut runner, P0, stomping_ground, equilibrium);
    answer_optional(&mut runner, P0, stomping_ground, false);

    sacrifice_p0_basic_and_assert_end_state(&mut runner, stomping_ground, p0_basic, p1_basic);
}
