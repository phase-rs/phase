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
//! consume the slot of one who can. Two further rows cover what the continuing
//! poll owes a responder who LEAVES the game while the accepted action is still
//! resolving (CR 800.4f) — one where a later responder is still owed its offer,
//! one where the departure exhausts the poll — and a printed Sin Prodder row is
//! the opposite control: an acceptance that consumes the thing being offered
//! ends the trigger instead of continuing it.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::WaitingFor;
use engine::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};

/// `scenario` exports only P0/P1; the three-player rows below need the third.
const P2: PlayerId = PlayerId(2);
/// The departed-responder row needs a fourth seat: one to leave the game and one
/// to still be owed its offer afterwards.
const P3: PlayerId = PlayerId(3);

// Verbatim Oracle text (Scryfall, 2026-11-05).
const TEMPORAL_EXTORTION: &str = "When you cast this spell, any player may pay half their life, rounded up. If a player does, counter Temporal Extortion.\nTake an extra turn after this one.";

// Verbatim Oracle text (Scryfall, 2026-11-16). 3/2 Devil, {2}{R}.
const SIN_PRODDER: &str = "Menace\nAt the beginning of your upkeep, reveal the top card of your library. Any opponent may have you put that card into your graveyard. If a player does, this creature deals damage to that player equal to that card's mana value. Otherwise, put that card into your hand.";

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
struct Settled {
    prompted: Vec<PlayerId>,
    events: Vec<GameEvent>,
    /// CR 616.1 ordering choices the run had to answer. Reported rather than
    /// swallowed so a row whose premise is "the acceptance suspends on a
    /// replacement choice" can prove that choice actually happened; a count of
    /// zero would otherwise be indistinguishable from an action that resolved
    /// straight through.
    replacement_choices: usize,
}

fn settle_cast(runner: &mut GameRunner, accepts: &dyn Fn(PlayerId) -> bool) -> Settled {
    settle_cast_with(runner, accepts, &|_| {})
}

/// `settle_cast` plus a hook that runs at each CR 616.1 ordering choice, BEFORE
/// it is answered — i.e. while the accepted action is suspended and the poll
/// cursor is parked beneath it. That is the only window in which a responder can
/// leave the game between the park and the arm, so it is the window the
/// departed-responder rows need.
fn settle_cast_with(
    runner: &mut GameRunner,
    accepts: &dyn Fn(PlayerId) -> bool,
    while_suspended: &dyn Fn(&mut GameRunner),
) -> Settled {
    let mut prompted = Vec::new();
    let mut events = Vec::new();
    let mut replacement_choices = 0usize;
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
            // CR 616.1: the affected player orders simultaneously applicable
            // replacements. Answering it here (rather than panicking as an
            // unexpected prompt) is what lets a row observe what happens AFTER
            // a mid-resolution suspension.
            WaitingFor::ReplacementChoice { .. } => {
                replacement_choices += 1;
                while_suspended(runner);
                let result = runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("the CR 616.1 ordering choice must accept a decision");
                events.extend(result.events);
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return Settled {
                        prompted,
                        events,
                        replacement_choices,
                    };
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

    let settled = settle_cast(&mut runner, &accepts);
    events.extend(settled.events);
    (runner, settled.prompted, events, spell)
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
) -> (Vec<PlayerId>, Vec<GameEvent>, usize) {
    cast_dash_hopes_with(runner, dash, opposing, accepts, &|_| {})
}

fn cast_dash_hopes_with(
    runner: &mut GameRunner,
    dash: ObjectId,
    opposing: ObjectId,
    accepts: &dyn Fn(PlayerId) -> bool,
    while_suspended: &dyn Fn(&mut GameRunner),
) -> (Vec<PlayerId>, Vec<GameEvent>, usize) {
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
    let settled = settle_cast_with(runner, accepts, while_suspended);
    events.extend(settled.events);
    (settled.prompted, events, settled.replacement_choices)
}

/// CR 118.12 + CR 119.4: "any player may pay 5 life. If a player does, counter
/// Dash Hopes." The accepting player pays, not the caster.
#[test]
fn dash_hopes_is_paid_by_the_accepting_player_not_the_caster() {
    // Distinct life totals, so a 5-life payment names exactly one payer.
    let (mut runner, dash, opposing) = stage_dash_hopes(2, &[(P0, 20), (P1, 13)]);
    let (prompted, events, _) =
        cast_dash_hopes(&mut runner, dash, opposing, &|player| player == P1);

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
    let (prompted, events, _) =
        cast_dash_hopes(&mut runner, dash, opposing, &|player| player == P1);

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
/// So a seat that cannot pay is not offered the payment at all, and — the
/// finding — it does not spend the poll either: the seat behind it, who can pay,
/// still gets its offer.
///
/// The two legs differ by ONE life point and nothing else, which is what makes
/// this discriminating: at 6 life P1 is offered the choice and takes it, at 4
/// life P1 is skipped. A run that offered every seat regardless would prompt P1
/// in both legs; one that dropped the rest of the poll would leave P2 unasked in
/// the 4-life leg.
///
/// The judgment lives in `effects::offered_seat_cannot_accept`, the single
/// per-seat feasibility authority, so it is asked when a seat is offered AND
/// again when it accepts. Before the upstream merge this branch asked it only at
/// acceptance time, from the handler; re-homing it onto that authority is what
/// also makes a seat skipped rather than prompted-then-refused.
#[test]
fn a_seat_that_cannot_pay_is_skipped_without_spending_the_poll() {
    for (p1_life, p1_is_offered) in [(6, true), (4, false)] {
        let (mut runner, dash, opposing) =
            stage_dash_hopes(3, &[(P0, 20), (P1, p1_life), (P2, 17)]);
        // Everyone but the caster tries to pay.
        let (prompted, events, _) =
            cast_dash_hopes(&mut runner, dash, opposing, &|player| player != P0);

        // Reach-guard: the poll ran at all, and reached the caster first.
        assert!(
            prompted.contains(&P0),
            "the poll must open on the caster; p1_life={p1_life} prompted={prompted:?}"
        );

        assert_eq!(
            prompted.contains(&P1),
            p1_is_offered,
            "a seat is offered the payment exactly when it can pay it (CR 118.3); \
             p1_life={p1_life} prompted={prompted:?}"
        );
        if !p1_is_offered {
            assert!(
                !runner
                    .state()
                    .player_actions_this_way
                    .contains(&(P1, PlayerActionKind::AcceptedOptionalEffect)),
                "a seat that was never offered the payment cannot have performed \
                 it — the ledger the \"did a player do this\" riders read must \
                 not record them; p1_life={p1_life}"
            );
        }

        if p1_is_offered {
            // P1 could pay and accepted, so P1 paid and the spell is countered.
            assert_eq!(life(&runner, P1), p1_life - 5, "P1 paid the 5 life");
        } else {
            assert_eq!(
                life(&runner, P1),
                p1_life,
                "P1 could not pay, so no life is lost"
            );
            // The finding: the unpayable seat did not consume P2's option.
            assert!(
                prompted.contains(&P2),
                "a seat that cannot pay must not spend a payable seat's option; \
                 prompted={prompted:?}"
            );
            assert_eq!(life(&runner, P2), 12, "P2 can pay 5 from 17 life, and did");
        }

        assert!(
            was_countered(&events, dash),
            "the accepted payment must counter Dash Hopes; p1_life={p1_life} \
             events={events:?}"
        );
        assert_eq!(
            life(&runner, P0),
            20,
            "the caster is never the payer for an accepted any-player offer"
        );
    }
}

/// Rest in Peace / Leyline of the Void class: "If a card would be put into a
/// graveyard from anywhere, exile it instead." A `Moved` replacement keyed on
/// `destination_zone(Graveyard)` whose execute exiles the moving card.
/// `valid_card` is left unset, so it watches every card.
fn graveyard_exile_redirect(description: &str) -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Graveyard)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                actor: engine::types::ability::LibraryInstructionActor::Controller,
                library_shuffle: Default::default(),
                destination: Zone::Exile,
                origin: None,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        ))
        .description(description.to_string())
}

/// Two simultaneously applicable `Moved` redirects, so a countered spell's
/// stack → graveyard move has two replacements competing for it and
/// `effects/counter.rs` raises a CR 616.1 ordering choice mid-resolution. That
/// is what makes an acceptance suspend on a prompt of its own.
///
/// The owner matters: CR 800.4a takes a departing player's objects with them, so
/// a row whose responder concedes must not host its redirects on that responder,
/// or the concession would dismantle the very suspension the row depends on.
fn install_graveyard_redirects(runner: &mut GameRunner, hosts: &[(PlayerId, &str)]) {
    for &(owner, description) in hosts {
        let host = engine::game::zones::create_object(
            runner.state_mut(),
            CardId(702),
            owner,
            description.to_string(),
            Zone::Battlefield,
        );
        runner
            .state_mut()
            .objects
            .get_mut(&host)
            .expect("the redirect host was just created")
            .replacement_definitions = vec![graveyard_exile_redirect(description)].into();
    }
}

/// Concede `player` once, while the accepted action is suspended. Idempotent, so
/// it is safe if the suspension raises more than one ordering choice.
fn concede_once(runner: &mut GameRunner, player: PlayerId) {
    if engine::game::players::is_alive(runner.state(), player) {
        runner
            .act(GameAction::Concede { player_id: player })
            .expect("CR 104.3a: a player can concede at any time");
    }
}

/// CR 101.4 + CR 616.1: the poll survives a replacement choice raised by the
/// accepted action itself.
///
/// Two Rest in Peace effects are on the battlefield, so countering Dash Hopes
/// moves it stack → graveyard with two applicable `Moved` redirects, and
/// `effects/counter.rs` surfaces a CR 616.1 ordering choice mid-resolution.
/// P1's acceptance therefore does NOT return straight to priority: the poll
/// cursor has to survive that child prompt for P2 to be offered the option that
/// Dash Hopes' ruling promises ("the remaining players still get the option").
///
/// This is the cast-path row for blocker 1 of the #10741 round-3 review. On the
/// engine before this change the re-offer was gated on an already-settled
/// priority, so P2 was dropped in exactly this case while the ordinary
/// no-replacement row above still passed.
#[test]
fn an_acceptance_that_suspends_on_a_replacement_choice_still_offers_the_rest() {
    let (mut runner, dash, opposing) = stage_dash_hopes(3, &[(P0, 20), (P1, 13), (P2, 17)]);
    install_graveyard_redirects(
        &mut runner,
        &[(P1, "Rest in Peace"), (P2, "Leyline of the Void")],
    );

    let (prompted, events, replacement_choices) =
        cast_dash_hopes(&mut runner, dash, opposing, &|player| player == P1);

    // Reach-guards. The poll must have reached P1, P1 must have paid, and the
    // payment's counter must genuinely have suspended on a CR 616.1 choice —
    // without that last one this row would be a duplicate of the plain
    // three-seat row and would pass without exercising the parked cursor at all.
    assert!(
        prompted.contains(&P1),
        "P1 must be offered the choice in order to accept it; prompted={prompted:?}"
    );
    assert_eq!(life(&runner, P1), 8, "P1 accepted, so P1 paid the 5 life");
    assert!(
        was_countered(&events, dash),
        "P1's payment must counter Dash Hopes; events={events:?}"
    );
    assert!(
        replacement_choices >= 1,
        "the countered spell's move must raise a CR 616.1 ordering choice for \
         this row to exercise the parked-cursor path; answered {replacement_choices}"
    );

    // The finding: P2's option survives both P1's acceptance AND the child
    // prompt that acceptance raised.
    assert!(
        prompted.contains(&P2),
        "the remaining players still get the option after an acceptance that \
         suspended on its own replacement choice; prompted={prompted:?}"
    );
    assert_eq!(
        life(&runner, P2),
        17,
        "P2 was offered the choice and declined, so P2 pays nothing"
    );
}

/// CR 800.4f + CR 102.1: a responder who leaves the game while the accepted
/// action resolves is skipped when the parked poll resumes, and the responders
/// after them are still owed their offers.
///
/// Four seats. P0 casts Dash Hopes and declines its own offer; P1 accepts and
/// pays, which counters the spell and suspends on a CR 616.1 ordering choice for
/// the two competing graveyard redirects; P2 concedes inside that window, i.e.
/// after the poll cursor parked naming P2 as next and before it arms. CR 800.4f:
/// "If an object requires a player who has left the game to pay a cost or choose
/// whether to pay a cost, that cost is not paid."
///
/// This is F1 of the #10741 round-4 review. Before the fix,
/// `arm_parked_opponent_may_poll` re-armed `WaitingFor::OpponentMayChoice` with
/// the seat recorded at park time, and the departure reconciliation only checks
/// the player currently on the clock — so a responder buried in the parked
/// cursor survived their own departure and the poll stalled waiting on them.
#[test]
fn a_responder_who_left_the_game_is_skipped_when_the_parked_poll_resumes() {
    let (mut runner, dash, opposing) =
        stage_dash_hopes(4, &[(P0, 20), (P1, 13), (P2, 17), (P3, 11)]);
    // Both hosted by P0, so P2's concession cannot remove a redirect and
    // dismantle the suspension this row depends on (CR 800.4a).
    install_graveyard_redirects(
        &mut runner,
        &[(P0, "Rest in Peace"), (P0, "Leyline of the Void")],
    );

    let (prompted, events, replacement_choices) = cast_dash_hopes_with(
        &mut runner,
        dash,
        opposing,
        &|player| player == P1,
        &|runner| concede_once(runner, P2),
    );

    // Reach-guards, in the order the run depends on them. Each one failing would
    // make the finding assertions below pass for the wrong reason.
    assert!(
        prompted.contains(&P1),
        "P1 must be offered the choice in order to accept it; prompted={prompted:?}"
    );
    assert_eq!(life(&runner, P1), 8, "P1 accepted, so P1 paid the 5 life");
    assert!(
        was_countered(&events, dash),
        "P1's payment must counter Dash Hopes; events={events:?}"
    );
    assert!(
        replacement_choices >= 1,
        "the countered spell's move must raise a CR 616.1 ordering choice, or the \
         cursor never parks and this row degenerates into the plain poll row; \
         answered {replacement_choices}"
    );
    assert!(
        !engine::game::players::is_alive(runner.state(), P2),
        "the concession must actually have removed P2 from the game, or the \
         skip below is not being tested at all"
    );

    // The finding: P2 is not asked, and P3 — queued behind the departed seat —
    // still is.
    assert!(
        !prompted.contains(&P2),
        "a responder who left the game must not be offered the payment \
         (CR 800.4f); prompted={prompted:?}"
    );
    assert!(
        prompted.contains(&P3),
        "the responder queued behind the departed seat is still owed its offer \
         (CR 101.4); prompted={prompted:?}"
    );
    assert_eq!(
        life(&runner, P3),
        11,
        "P3 was offered the choice and declined, so P3 pays nothing"
    );
}

/// CR 800.4f + CR 608.2d: when the departed seat was the LAST responder owed an
/// offer, the poll has nobody left to ask and closes instead of arming.
///
/// Same shape as the row above with three seats, so P2's concession exhausts the
/// queue. This is the sibling branch: the row above proves the cursor advances
/// PAST a departed seat, this one proves it terminates when advancing finds
/// nothing — the frame is taken and priority settles rather than the poll
/// hanging on a prompt for a player who is gone.
#[test]
fn a_parked_poll_whose_last_responder_left_the_game_settles() {
    let (mut runner, dash, opposing) = stage_dash_hopes(3, &[(P0, 20), (P1, 13), (P2, 17)]);
    install_graveyard_redirects(
        &mut runner,
        &[(P0, "Rest in Peace"), (P0, "Leyline of the Void")],
    );

    let (prompted, events, replacement_choices) = cast_dash_hopes_with(
        &mut runner,
        dash,
        opposing,
        &|player| player == P1,
        &|runner| concede_once(runner, P2),
    );

    // Reach-guards: the acceptance happened, it suspended, and P2 really left.
    assert_eq!(life(&runner, P1), 8, "P1 accepted, so P1 paid the 5 life");
    assert!(
        was_countered(&events, dash),
        "P1's payment must counter Dash Hopes; events={events:?}"
    );
    assert!(
        replacement_choices >= 1,
        "the acceptance must suspend on a CR 616.1 ordering choice for the \
         cursor to park; answered {replacement_choices}"
    );
    assert!(
        !engine::game::players::is_alive(runner.state(), P2),
        "the concession must actually have removed P2 from the game"
    );
    assert!(
        !prompted.contains(&P2),
        "the departed last responder must not be offered the payment \
         (CR 800.4f); prompted={prompted:?}"
    );

    // The finding: the exhausted poll closes. `settle_cast_with` returning at
    // all already proves it did not hang, and these pin the end state: the
    // optional-effect frame is gone rather than left parked on the stack.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "an exhausted poll returns to priority; waiting_for={:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().active_optional_effect_frame().is_none(),
        "the exhausted poll's frame must be taken, not left parked"
    );
}

/// CR 608.2d + the printed Sin Prodder ruling: an acceptance whose action
/// CONSUMES the thing being offered ends the trigger, so the opponents after the
/// accepting one get no offer.
///
/// > Each opponent in turn order, starting with the one after you in turn order,
/// > may choose to have you put that card into your graveyard. Once a player
/// > does so, Sin Prodder deals damage equal to that card's mana value to that
/// > player immediately and Sin Prodder's trigger has no further action.
///
/// This is the printed-card control for `acceptance_leaves_the_option_open`, and
/// the reason the Dash Hopes re-offer above is not unconditional. The two are
/// each other's discriminator: an engine that re-offers after EVERY acceptance
/// passes the Dash Hopes rows and fails this one, and an engine that ends the
/// poll at the first acceptance does the reverse.
///
/// F2 of the #10741 round-4 review, which asked for the printed trigger — the
/// card move and the damage — rather than a synthetic any-player offer. The
/// generic shape is still covered at the unit level by
/// `a_one_shot_acceptance_ends_the_poll_for_the_remaining_responders`.
/// Drive to `player`'s upkeep and on to the any-opponent offer its upkeep
/// trigger makes.
///
/// Two established harness idioms composed, because a single
/// `advance_to_upkeep()` satisfies neither on its own:
///
/// - `GameScenario::advance_to_phase` passes priority *two* seats per iteration
///   (`scenario.rs`), so in a three-seat game the upkeep it stops on is a parity
///   accident rather than the next seat's. Walking turn by turn and checking
///   `active_player` is the idiom in `advance_to_upkeep_of`
///   (`kashi_tribe_damage_tap_freeze.rs`, `issue_4271_birthing_ritual_cmc_filter.rs`).
/// - CR 603.3: reaching the step is not reaching the trigger. The ability is put
///   on the stack and resolves only once priority is passed, so the offer
///   surfaces some passes later — the idiom in `advance_to_upkeep_unless_prompt`
///   (`counter_indexed_unless_and_chroma_runtime.rs`).
///
/// Panics with the walk it took, so a harness miss reports where it ended up
/// instead of only that it missed.
fn advance_to_upkeep_offer_of(runner: &mut GameRunner, player: PlayerId) {
    fn snapshot(runner: &GameRunner) -> String {
        format!(
            "{:?}/{:?} {:?}",
            runner.state().active_player,
            runner.state().phase,
            runner.state().waiting_for
        )
    }
    fn offer_is_open(runner: &GameRunner) -> bool {
        matches!(
            runner.state().waiting_for,
            WaitingFor::OpponentMayChoice { .. }
        )
    }
    let at_upkeep_of = |runner: &GameRunner| {
        runner.state().phase == Phase::Upkeep && runner.state().active_player == player
    };

    let mut walk = vec![snapshot(runner)];
    for _ in 0..8 {
        if offer_is_open(runner) || at_upkeep_of(runner) {
            break;
        }
        runner.advance_to_upkeep();
        walk.push(snapshot(runner));
        if offer_is_open(runner) || at_upkeep_of(runner) {
            break;
        }
        if runner.waiting_for_kind() == "DeclareAttackers" {
            runner.declare_attackers(&[]).expect("declare no attackers");
        } else {
            runner.advance_to_end_step();
        }
        walk.push(snapshot(runner));
    }
    for _ in 0..20 {
        if offer_is_open(runner) {
            return;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
        walk.push(snapshot(runner));
    }
    panic!(
        "never reached {player:?}'s any-opponent upkeep offer; walk:\n  {}",
        walk.join("\n  ")
    );
}

#[test]
fn an_accepted_sin_prodder_trigger_makes_no_further_offer() {
    // Sin Prodder is P1's, so `advance_to_upkeep_offer_of` walks to P1's upkeep
    // — the "your upkeep" the printed trigger names — and its opponents are then
    // offered in turn order starting with the one after P1 (the ruling above):
    // P2 first, then P0.
    const REVEALED_MANA_VALUE: i32 = 3;
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for &player in &[P0, P1, P2] {
        scenario.with_life(player, 20);
    }
    let prodder = {
        let mut builder = scenario.add_creature(P1, "Sin Prodder", 3, 2);
        builder.from_oracle_text_with_keywords(&["Menace"], SIN_PRODDER);
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 2,
        });
        builder.id()
    };
    // Mana value 3, so the damage dealt is distinguishable from a default of 0
    // or 1 — an engine that failed to read the revealed card's mana value would
    // not land on 3 by accident.
    let revealed = {
        let mut builder = scenario.add_spell_to_library_top(P1, "Revealed Card", false);
        builder.with_mana_cost(ManaCost::generic(REVEALED_MANA_VALUE as u32));
        builder.id()
    };
    let mut runner = scenario.build();

    // Reach-guard: the printed upkeep trigger must actually fire and reach its
    // poll — the helper panics with its walk if it never does. Without this,
    // every assertion below would hold vacuously on an engine where Sin Prodder
    // does nothing at all.
    advance_to_upkeep_offer_of(&mut runner, P1);
    assert_eq!(
        runner.state().objects[&prodder].controller,
        P1,
        "reach-guard: the trigger that fired is the one on P1's Sin Prodder"
    );

    let settled = settle_cast(&mut runner, &|_| true);
    let prompted = settled.prompted;

    // Reach-guard: the first opponent after the controller is the one offered,
    // and it is the one that accepted.
    assert_eq!(
        prompted.first().copied(),
        Some(P2),
        "the offer opens on the opponent after the controller in turn order; \
         prompted={prompted:?}"
    );

    // The printed consequences of the acceptance: the revealed card is put into
    // its owner's graveyard, and Sin Prodder deals damage equal to that card's
    // mana value to the accepting opponent.
    assert_eq!(
        runner.state().objects[&revealed].zone,
        Zone::Graveyard,
        "the accepted action puts the revealed card into the graveyard"
    );
    assert_eq!(
        life(&runner, P2),
        20 - REVEALED_MANA_VALUE,
        "Sin Prodder deals damage equal to the revealed card's mana value to \
         the opponent who accepted"
    );

    // The finding: the trigger "has no further action", so P0 is never asked.
    assert!(
        !prompted.contains(&P0),
        "the accepted one-shot action consumes the revealed card, so no further \
         opponent is offered (CR 608.2d); prompted={prompted:?}"
    );
    assert_eq!(
        life(&runner, P0),
        20,
        "P0 was never offered the choice, so P0 takes no damage"
    );
}
