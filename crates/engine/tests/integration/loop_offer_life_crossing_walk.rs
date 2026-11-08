//! CR 704.5a + CR 732.2a: offering a loop shortcut, and taking it as suggested, reads a number of
//! life charges bounded by the declarations' switch points, not by any life total, so its work
//! does not grow with the players' life totals (a declaration whose target rotates in a round
//! robin of two or more is still read one repetition at a time; see
//! `PeriodicDelta::declared_seat_life_crossing`). A drain loop's offer asks at which repetition
//! each player would reach 0 life (CR 704.5a: "If a player has 0 or less life, that player loses
//! the game."). Each time the offer or a take asks, a player's life charge is read once for each
//! stretch the switch points cut the repetitions into, up to the stretch in which that player
//! crosses, so a player who crosses in an earlier stretch is read fewer times than one who crosses
//! later. On an untargeted drain with P0 at 4,000,000 or ten short of `i32::MAX` life or P1 at
//! 4,000,000, and on a targeted drain with the loop's controller at 4,000,000 or 1,000,000,000
//! life, the offer and a `Fixed(3)` take read exactly as many life charges as they read at an
//! ordinary life total.

use engine::analysis::decision_template::IterationCount;
use engine::analysis::loop_check::ShortcutResponse;
use engine::analysis::resource::{PeriodSignature, PeriodicDelta};
use engine::database::card_db::CardDatabase;
use engine::game::engine::apply;
use engine::game::perf_counters;
use engine::game::scenario::{GameRunner, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CastPaymentMode, GameState, StackEntryKind, WaitingFor, YieldTarget,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaType;

use crate::loop_shortcut_drain_boards::{
    drive_to_live_declarable_offer, lethal_lifegain_loss_board,
};
use crate::play_trace;

/// The certified period of the offer standing at `state`, through either signature arm.
fn offered_period(state: &GameState) -> &PeriodicDelta {
    let WaitingFor::LoopShortcut { certificate, .. } = &state.waiting_for else {
        panic!("REACH-GUARD: an offer stands, got {:?}", state.waiting_for);
    };
    match certificate
        .per_cycle
        .as_ref()
        .expect("a drain offer publishes its per-period signature")
    {
        PeriodSignature::EveryRepetition(period) | PeriodSignature::OneRepetition(period) => period,
    }
}

/// The name of the object a decision slot's `source` names on `state`.
fn source_name<'a>(state: &'a GameState, source: &YieldTarget) -> &'a str {
    let object = match source {
        YieldTarget::ThisObject { source_id, .. } => state.objects.get(source_id),
        YieldTarget::AllCopies { card_id, .. } => state
            .objects
            .values()
            .find(|object| object.card_id == *card_id),
    };
    object.map_or("<no object>", |object| object.name.as_str())
}

/// [`play_trace::altar_board`] with Zulaport Cutthroat ("Whenever this creature or another
/// creature you control dies, each opponent loses 1 life and you gain 1 life.") as its payoff:
/// Phyrexian Altar ("Sacrifice a creature: Add one mana of any color."), Gravecrawler in the
/// graveyard ("This creature can't block." / "You may cast this card from your graveyard as long
/// as you control a Zombie."), Walking Corpse (a Zombie with no rules text) and a Swamp, with the
/// two players' life set on the built state. Each cycle casts Gravecrawler with the Altar's {B}
/// and sacrifices it to the Altar.
struct AltarBoard {
    runner: GameRunner,
    altar: ObjectId,
    gravecrawler: ObjectId,
}

impl AltarBoard {
    fn new(db: &'static CardDatabase, lives: [i32; 2]) -> Self {
        let (mut runner, altar, gravecrawler) =
            play_trace::altar_board(Some("Zulaport Cutthroat"), db);
        for (player, life) in runner.state_mut().players.iter_mut().zip(lives) {
            player.life = life;
        }
        AltarBoard {
            runner,
            altar,
            gravecrawler,
        }
    }

    fn settle(&mut self) {
        let gravecrawler = [self.gravecrawler];
        let score = |action: &GameAction| {
            2 * play_trace::names(&gravecrawler)(action)
                + play_trace::chooses_color(ManaType::Black)(action)
        };
        play_trace::settle(&mut self.runner, &score);
    }

    /// Casts Gravecrawler and sacrifices it: the cycle that offers the shortcut.
    fn cycle_to_the_offer(&mut self) {
        play_trace::cast(
            &mut self.runner,
            self.gravecrawler,
            vec![],
            CastPaymentMode::Auto,
        );
        self.settle();
        let index = play_trace::ability(self.runner.state(), self.altar, true);
        play_trace::activate(&mut self.runner, self.altar, index);
        self.settle();
        assert!(
            play_trace::is_offer(self.runner.state()),
            "REACH-GUARD: one cycle offers the loop, got {:?}",
            self.runner.state().waiting_for
        );
    }

    /// Declares `Fixed(cycles)` and P1 accepts.
    fn take(&mut self, cycles: u32) {
        self.runner
            .act(GameAction::DeclareShortcut {
                count: IterationCount::Fixed(cycles),
                template: None,
            })
            .expect("the shortcut declaration is accepted");
        assert!(
            matches!(
                self.runner.state().waiting_for,
                WaitingFor::RespondToShortcut { .. }
            ),
            "REACH-GUARD: the declared count is within the offer's capacity, got {:?}",
            self.runner.state().waiting_for
        );
        self.runner
            .act(GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            })
            .expect("accepting the shortcut is accepted");
    }
}

/// CR 704.5a + CR 732.2a: an untargeted drain loop's offer and a `Fixed(3)` take of it read the
/// same number of life charges with P0 at 40, at 4,000,000 and ten short of `i32::MAX`, and with
/// P1 at 4,000,000. Each take ends where the shortcut ends (CR 732.2a: "The ending point of this
/// sequence must be a place where a player has priority"), P0's priority on an empty stack, with
/// P0 up 4 and P1 down 4 (the cycle that offers the shortcut and the three taken).
///
/// The loop is [`AltarBoard`]'s Zulaport Cutthroat cycle. Its offer comes by the recorded road
/// (`replay_bounded_offer`), whose certified period charges no target slot whatever the payoff
/// targets, so the walk reads every repetition alike.
///
/// REVERT-PROBE: step through the repetitions one at a time up to the sum of the life totals
/// (`PeriodicDelta::first_life_crossing` in `PeriodicDelta::seat_crossing`) ⇒ the reads with P0
/// at 4,000,000 are millions more than at 40.
#[test]
fn a_drain_shortcut_reads_as_many_life_charges_at_a_large_life_total() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    let reads_at = |lives: [i32; 2]| {
        let mut board = AltarBoard::new(db, lives);
        perf_counters::reset_declared_life_charge_counters();
        board.cycle_to_the_offer();
        assert!(
            offered_period(board.runner.state()).victim_slot.is_empty(),
            "REACH-GUARD, lives {lives:?}: the offer comes by the recorded road, whose certified \
             period charges no slot"
        );
        board.take(3);
        let state = board.runner.state();
        assert_eq!(
            state.waiting_for,
            WaitingFor::Priority { player: P0 },
            "lives {lives:?}: the take ends at P0's priority"
        );
        assert!(
            state.stack.is_empty(),
            "lives {lives:?}: the take ends on an empty stack, got {:?}",
            state.stack
        );
        assert_eq!(
            [board.runner.life(P0), board.runner.life(P1)],
            [lives[0] + 4, lives[1] - 4],
            "lives {lives:?}: four cycles each gain P0 1 and cost P1 1"
        );
        perf_counters::declared_life_charge_snapshot().reads
    };
    let ordinary = reads_at([40, 20]);
    assert!(ordinary > 0, "REACH-GUARD: the offer reads life charges");
    for lives in [[4_000_000, 20], [i32::MAX - 10, 20], [40, 4_000_000]] {
        assert_eq!(
            reads_at(lives),
            ordinary,
            "the reads with P0 at {} and P1 at {}",
            lives[0],
            lives[1]
        );
    }
}

/// CR 704.5a + CR 732.2a: a targeted drain loop's offer and a `Fixed(3)` take of it read the same
/// number of life charges with the loop's controller at the saved game's own life total, at
/// 4,000,000 and at 1,000,000,000.
///
/// The board is the saved four-player game `lethal_lifegain_loss_4p.json.gz`, driven by
/// `loop_shortcut_drain_boards::drive_to_live_declarable_offer` to its live offer. P0 controls
/// Exquisite Blood ("Whenever an opponent loses life, you gain that much life.") and Enduring
/// Tenacity ("Whenever you gain life, target opponent loses that much life." / "When Enduring
/// Tenacity dies, if it was a creature, return it to the battlefield under its owner's control.
/// It's an enchantment. (It's not a creature.)"). Enduring Tenacity's trigger chooses its target
/// as it is put on the stack (CR 603.3d + CR 601.2c) among three opponents. The offer comes by
/// the ring road, whose certified period charges the target slot an announcement in its window
/// names, so the walk gathers the declarations' change points and splits off the leading
/// repetition. Each repetition moves "that much life", the 4
/// the chain carries, from the targeted P1 to P0. Each take ends where the shortcut ends
/// (CR 732.2a), at P0's priority with Enduring Tenacity's next trigger on the stack, P0 up 12 and
/// P1 down 12.
///
/// REVERT-PROBE: as above ⇒ the reads with P0 at 4,000,000 are millions more than at the saved
/// total.
#[test]
fn a_targeted_drain_shortcut_reads_as_many_life_charges_at_a_large_life_total() {
    /// The life each repetition moves: the amount the saved game's chain carries.
    const HIT: i32 = 4;
    let reads_at = |life: Option<i32>| {
        let mut state = lethal_lifegain_loss_board();
        if let Some(life) = life {
            state.players[0].life = life;
        }
        perf_counters::reset_declared_life_charge_counters();
        let live = drive_to_live_declarable_offer(&mut state);
        assert_eq!(
            (live.proposer, live.aimed_at),
            (P0, P1),
            "REACH-GUARD, P0 at {life:?}: P0 offers, the drive aimed Enduring Tenacity at P1"
        );
        let charged: Vec<(&str, i64)> = offered_period(&state)
            .victim_slot
            .iter()
            .map(|(slot, charge)| (source_name(&state, &slot.source), *charge))
            .collect();
        assert_eq!(
            charged,
            [("Enduring Tenacity", i64::from(HIT))],
            "REACH-GUARD, P0 at {life:?}: the certified period charges exactly one target slot, \
             Enduring Tenacity's, {HIT} a repetition"
        );
        let before: Vec<i32> = state.players.iter().map(|p| p.life).collect();
        apply(
            &mut state,
            P0,
            GameAction::DeclareShortcut {
                count: IterationCount::Fixed(3),
                template: None,
            },
        )
        .expect("the shortcut declaration is accepted");
        let mut responders = 0;
        while let WaitingFor::RespondToShortcut { player, .. } = state.waiting_for {
            responders += 1;
            apply(
                &mut state,
                player,
                GameAction::RespondToShortcut {
                    response: ShortcutResponse::Accept,
                },
            )
            .expect("accepting the shortcut is accepted");
        }
        assert_eq!(
            responders, 3,
            "REACH-GUARD, P0 at {life:?}: each of the three opponents answers the declaration"
        );
        assert_eq!(
            state.waiting_for,
            WaitingFor::Priority { player: P0 },
            "P0 at {life:?}: the take ends at P0's priority"
        );
        let in_flight: Vec<&str> = state
            .stack
            .iter()
            .map(|entry| match &entry.kind {
                StackEntryKind::TriggeredAbility { source_id, .. } => {
                    state.objects[source_id].name.as_str()
                }
                other => panic!("P0 at {life:?}: a trigger is in flight, got {other:?}"),
            })
            .collect();
        assert_eq!(
            in_flight,
            ["Enduring Tenacity"],
            "P0 at {life:?}: the take ends with Enduring Tenacity's next trigger on the stack"
        );
        let after: Vec<i32> = state.players.iter().map(|p| p.life).collect();
        assert_eq!(
            after,
            [
                before[0] + 3 * HIT,
                before[1] - 3 * HIT,
                before[2],
                before[3]
            ],
            "P0 at {life:?}: three repetitions each move {HIT} from P1 to P0"
        );
        perf_counters::declared_life_charge_snapshot().reads
    };
    let ordinary = reads_at(None);
    assert!(ordinary > 0, "REACH-GUARD: the offer reads life charges");
    for life in [4_000_000, 1_000_000_000] {
        assert_eq!(
            reads_at(Some(life)),
            ordinary,
            "the reads with P0 at {life}"
        );
    }
}
