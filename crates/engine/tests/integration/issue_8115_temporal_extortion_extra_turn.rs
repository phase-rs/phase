//! Issue #8115 — Temporal Extortion must grant its caster an extra turn when no
//! player pays to counter it.
//!
//! > When you cast this spell, any player may pay half their life, rounded up.
//! > If a player does, counter Temporal Extortion.
//! > Take an extra turn after this one.
//!
//! Reported as "doesn't grant an extra turn even when uncountered", in both AI
//! and PvP games. The card has two independent halves, so the two rows below
//! separate them and each is the other's reach-guard:
//!
//! - nobody pays  → the spell resolves and its caster takes an extra turn
//! - a player pays → that player loses half their life and the spell is
//!   countered, so NO extra turn is created
//!
//! Without the paying row, the declining row would pass just as well on an
//! engine where the cast trigger never fires at all.
//!
//! The Dash Hopes rows at the bottom cover the rest of the class: the poll
//! continues past the first acceptance, and a player who cannot pay does not
//! consume the slot of one who can.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::WaitingFor;
use engine::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// `scenario` exports only P0/P1; the three-player rows below need the third.
const P2: PlayerId = PlayerId(2);

// Verbatim Oracle text (Scryfall, 2026-11-05).
const TEMPORAL_EXTORTION: &str = "When you cast this spell, any player may pay half their life, rounded up. If a player does, counter Temporal Extortion.\nTake an extra turn after this one.";

// Verbatim Oracle text (Scryfall, 2026-11-09).
const DASH_HOPES: &str = "When you cast this spell, any player may pay 5 life. If a player does, counter Dash Hopes.\nCounter target spell.";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}

fn was_countered(events: &[GameEvent], spell: ObjectId) -> bool {
    events.iter().any(
        |event| matches!(event, GameEvent::SpellCountered { object_id, .. } if *object_id == spell),
    )
}

/// Drive every prompt the cast raises through to settlement, answering each
/// "any player may" offer with `accepts`. Returns the players offered the
/// choice, in the order they were asked, plus every event the run produced.
///
/// Both failure exits panic rather than returning. A driver that gives up
/// quietly on an unexpected prompt — or on running out of iterations — lets the
/// life-total assertions downstream pass against a spell that never finished
/// resolving, which is precisely how this helper read before. Returning
/// normally therefore carries its own assertion: the game settled at a priority
/// window with an empty stack.
fn settle_cast(
    runner: &mut GameRunner,
    accepts: &dyn Fn(PlayerId) -> bool,
) -> (Vec<PlayerId>, Vec<GameEvent>) {
    let mut prompted = Vec::new();
    let mut events = Vec::new();
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OpponentMayChoice { player, .. } => {
                prompted.push(player);
                let result = runner
                    .act(GameAction::DecideOptionalEffect {
                        accept: accepts(player),
                    })
                    .expect("the any-player-may offer must accept a decision");
                events.extend(result.events);
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return (prompted, events);
                }
                let result = runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must be accepted");
                events.extend(result.events);
            }
            other => panic!(
                "unexpected prompt while settling the cast: {other:?}; prompted={prompted:?}"
            ),
        }
    }
    panic!(
        "the cast never settled: still {:?} after 60 actions; prompted={prompted:?}",
        runner.state().waiting_for
    );
}

/// Cast Temporal Extortion from P0, answering every "any player may pay" offer
/// with `accepts`. P1 starts on an ODD life total so the paying row below
/// distinguishes whose life was halved and proves the rounding direction.
fn cast_extortion(
    accepts: impl Fn(PlayerId) -> bool,
) -> (GameRunner, Vec<PlayerId>, Vec<GameEvent>, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 13);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Temporal Extortion", false, TEMPORAL_EXTORTION)
        .id();
    scenario.with_mana_pool(P0, floating_mana(4, ManaType::Black));
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&spell].card_id;
    let mut events = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("Temporal Extortion must be castable with {B}{B}{B}{B} available")
        .events;

    let (prompted, settle_events) = settle_cast(&mut runner, &accepts);
    events.extend(settle_events);
    (runner, prompted, events, spell)
}

/// CR 118.12 + CR 500.7: nobody pays, so the spell is not countered and its
/// controller takes an extra turn after this one.
#[test]
fn temporal_extortion_grants_an_extra_turn_when_no_one_pays() {
    let (runner, prompted, events, spell) = cast_extortion(|_| false);

    // Reach-guard: the offer must actually have been made, so the assertions
    // below cannot pass on an engine whose cast trigger never fired.
    assert!(
        prompted.contains(&P0) || prompted.contains(&P1),
        "at least one player must be offered the pay-half-life choice; prompted={prompted:?}"
    );
    assert_eq!(life(&runner, P0), 20, "nobody paid, so no life was lost");
    assert_eq!(life(&runner, P1), 13, "nobody paid, so no life was lost");
    assert!(
        !was_countered(&events, spell),
        "nobody paid, so the spell must not be countered"
    );

    assert!(
        runner.state().extra_turns.iter().any(|et| et.player == P0),
        "an uncountered Temporal Extortion must give its caster an extra turn, got {:?}",
        runner.state().extra_turns
    );
    assert!(
        !runner.state().extra_turns.iter().any(|et| et.player == P1),
        "the extra turn belongs to the caster, not the opponent, got {:?}",
        runner.state().extra_turns
    );
}

/// Paired reach-guard (CR 118.12 + CR 701.6a): a player pays half their life
/// rounded up, so the spell is countered and no extra turn is created. This is
/// what proves the row above is not passing because the offer never happens.
#[test]
fn temporal_extortion_is_countered_when_a_player_pays_half_their_life() {
    let (runner, prompted, events, spell) = cast_extortion(|player| player == P1);

    assert!(
        prompted.contains(&P1),
        "P1 must be offered the choice in order to accept it; prompted={prompted:?}"
    );
    // CR 119.4: half of 13 rounded up is 7, so an odd total both fixes the
    // rounding direction and identifies P1 as the player who paid.
    assert_eq!(
        life(&runner, P1),
        6,
        "the paying player loses half their life, rounded UP (13 - 7)"
    );
    assert_eq!(
        life(&runner, P0),
        20,
        "the caster pays nothing; the parsed `payer: Controller` names the offer, not the payer"
    );
    // CR 701.6a: a countered spell "doesn't resolve and none of its effects
    // occur" — assert the counter itself, not only the absence of its effect.
    assert!(
        was_countered(&events, spell),
        "a paid-for Temporal Extortion must be countered; events={events:?}"
    );
    assert!(
        runner.state().extra_turns.is_empty(),
        "a countered Temporal Extortion grants no extra turn, got {:?}",
        runner.state().extra_turns
    );
}

// ---------------------------------------------------------------------------
// Class sibling — Dash Hopes.
//
// Dash Hopes is the only other card printed with this shape, and it is what
// makes the fix a building block rather than a one-card patch: it parses to the
// identical structure (`PayCost { payer: Controller }` under
// `optional_for: AnyPlayer`, with a `Counter { SelfRef }` rider gated on
// `OptionalEffectPerformed`). Its amount is a literal 5 rather than a "half
// their life" anaphor, so it isolates the *payer* binding from the dangling
// `PlayerScope::Target` anaphor that Temporal Extortion also depends on — each
// card covers one half of the defect, and neither alone would prove both.
//
// Its printed rulings also pin the two poll properties the rows below cover:
//
// > As soon as any player performs the action, the spell is countered, but the
// > remaining players still get the option.
//
// > A player can't choose to pay 5 life unless they actually have 5 or more
// > life to pay.
// ---------------------------------------------------------------------------

/// Seat Dash Hopes in P0's hand with a spell on the stack for it to target, in a
/// game of `players` seats. Returns the runner, the Dash Hopes object, and the
/// opposing spell.
fn stage_dash_hopes(players: u8, lives: &[(PlayerId, i32)]) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(players, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for &(player, life) in lives {
        scenario.with_life(player, life);
    }
    let dash = scenario
        .add_spell_to_hand_from_oracle(P0, "Dash Hopes", true, DASH_HOPES)
        .id();
    scenario.with_mana_pool(P0, floating_mana(2, ManaType::Black));
    let mut runner = scenario.build();

    // A spell for Dash Hopes to target, so the cast is legal and its
    // cast-trigger actually fires. Same shape as `fold_into_aether_counter_put`.
    let opposing = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(701),
        P1,
        "Opposing Spell".to_string(),
        Zone::Stack,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&opposing)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Sorcery];
    runner.state_mut().stack.push_back(StackEntry {
        id: opposing,
        source_id: opposing,
        controller: P1,
        kind: StackEntryKind::Spell {
            card_id: CardId(701),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    (runner, dash, opposing)
}

fn cast_dash_hopes(
    runner: &mut GameRunner,
    dash: ObjectId,
    opposing: ObjectId,
    accepts: &dyn Fn(PlayerId) -> bool,
) -> (Vec<PlayerId>, Vec<GameEvent>) {
    let card_id = runner.state().objects[&dash].card_id;
    let mut events = runner
        .act(GameAction::CastSpell {
            object_id: dash,
            card_id,
            targets: vec![opposing],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("Dash Hopes must be castable with {B}{B} available and a spell to target")
        .events;
    let (prompted, settle_events) = settle_cast(runner, accepts);
    events.extend(settle_events);
    (prompted, events)
}

/// CR 118.12 + CR 119.4: "any player may pay 5 life. If a player does, counter
/// Dash Hopes." The accepting player pays, not the caster.
#[test]
fn dash_hopes_is_paid_by_the_accepting_player_not_the_caster() {
    // Distinct life totals, so a 5-life payment names exactly one payer.
    let (mut runner, dash, opposing) = stage_dash_hopes(2, &[(P0, 20), (P1, 13)]);
    let (prompted, events) = cast_dash_hopes(&mut runner, dash, opposing, &|player| player == P1);

    // Reach-guard: P1 must actually have been offered the choice, so the life
    // assertions below cannot pass on an engine that never fired the trigger.
    assert!(
        prompted.contains(&P1),
        "P1 must be offered the pay-5-life choice in order to accept it; prompted={prompted:?}"
    );
    // CR 118.12: the accepting player pays the cost.
    assert_eq!(life(&runner, P1), 8, "the accepting player pays the 5 life");
    // The caster is not the payer — this is the assertion that fails when the
    // `payer` rebind is reverted, because the parsed `payer` is `Controller`.
    assert_eq!(
        life(&runner, P0),
        20,
        "the caster pays nothing; the parsed `payer: Controller` names the offer, not the payer"
    );
    // CR 701.6a: the payment is what counters Dash Hopes, so assert the counter
    // of DASH HOPES specifically. Without this the life totals alone would also
    // be satisfied by a Dash Hopes that resolved and countered the other spell.
    assert!(
        was_countered(&events, dash),
        "a paid-for Dash Hopes must itself be countered; events={events:?}"
    );
    assert!(
        !was_countered(&events, opposing),
        "a countered Dash Hopes never resolves, so its target survives (CR 701.6a)"
    );
}

/// CR 118.12 + CR 101.4: an acceptance answers one responder, not the whole
/// poll. Dash Hopes: "As soon as any player performs the action, the spell is
/// countered, but the remaining players still get the option."
///
/// Three seats are the minimum that can show it: P0 declines, P1 pays, and P2
/// must STILL be asked. Reverting the re-offer in the accept branch of
/// `handle_opponent_may_choice_inner` drops P2 from `prompted`.
#[test]
fn an_acceptance_does_not_end_the_poll_for_the_remaining_players() {
    let (mut runner, dash, opposing) = stage_dash_hopes(3, &[(P0, 20), (P1, 13), (P2, 17)]);
    let (prompted, events) = cast_dash_hopes(&mut runner, dash, opposing, &|player| player == P1);

    // Reach-guard: the poll must have reached P1 at all, and P1 must have paid,
    // so the P2 assertion cannot pass on an engine that offered nobody anything.
    assert!(
        prompted.contains(&P1),
        "P1 must be offered the choice in order to accept it; prompted={prompted:?}"
    );
    assert_eq!(life(&runner, P1), 8, "P1 accepted, so P1 paid the 5 life");
    assert!(
        was_countered(&events, dash),
        "P1's payment must counter Dash Hopes; events={events:?}"
    );

    // The finding: P2's option survives P1's acceptance.
    assert!(
        prompted.contains(&P2),
        "the remaining players still get the option after one player pays; prompted={prompted:?}"
    );
    assert_eq!(
        life(&runner, P2),
        17,
        "P2 was offered the choice and declined, so P2 pays nothing"
    );
}

/// CR 118.3 + CR 608.2d: "A player can't pay a cost without having the
/// necessary resources to pay it fully", and cannot choose an impossible
/// option. Dash Hopes: "A player can't choose to pay 5 life unless they
/// actually have 5 or more life to pay."
///
/// P1 at 4 life tries to accept. That must not spend the poll: P2, who can pay,
/// still gets the offer. Reverting the affordability pre-flight lets P1 take the
/// slot, records a payment that never happened in `player_actions_this_way`, and
/// leaves P2 unasked.
#[test]
fn an_unpayable_acceptance_does_not_consume_a_payable_players_option() {
    let (mut runner, dash, opposing) = stage_dash_hopes(3, &[(P0, 20), (P1, 4), (P2, 17)]);
    // Everyone but the caster tries to pay; only P2 can afford it.
    let (prompted, events) = cast_dash_hopes(&mut runner, dash, opposing, &|player| player != P0);

    // Reach-guard: P1 must genuinely have been offered the choice and tried to
    // take it, or this proves nothing about the unaffordable path.
    assert!(
        prompted.contains(&P1),
        "P1 must be offered the choice in order to fail it; prompted={prompted:?}"
    );
    assert_eq!(
        life(&runner, P1),
        4,
        "P1 cannot pay 5 from 4 life, so no life is lost"
    );
    assert!(
        !runner
            .state()
            .player_actions_this_way
            .contains(&(P1, PlayerActionKind::AcceptedOptionalEffect)),
        "a player who cannot pay never performed the action, so the ledger \
         'did a player do this' riders read must not record them"
    );

    // The finding: the payable player still gets their option, and it works.
    assert!(
        prompted.contains(&P2),
        "a player who cannot pay must not consume a payable player's option; prompted={prompted:?}"
    );
    assert_eq!(life(&runner, P2), 12, "P2 can pay 5 from 17 life, and did");
    assert!(
        was_countered(&events, dash),
        "P2's payment must counter Dash Hopes; events={events:?}"
    );
    assert_eq!(
        life(&runner, P0),
        20,
        "the caster is never the payer for an accepted any-player offer"
    );
}
