//! Risky Move, driven only through public game actions: the gain-side
//! `ChangesController` trigger ("When you gain control of this enchantment from
//! another player").
//!
//! Risky Move {3}{R}{R}{R}, Enchantment (Oracle text, verbatim):
//!
//! > At the beginning of each player's upkeep, that player gains control of this
//! > enchantment.
//! > When you gain control of this enchantment from another player, choose a
//! > creature you control and an opponent. Flip a coin. If you lose the flip,
//! > that opponent gains control of that creature.
//!
//! Rulings: "The change of control of creatures does not end at end of turn or
//! even when this card leaves the battlefield." and "This card does not target
//! any creatures or players."
//!
//! CR 603.2 + CR 603.10: the gain side is an ordinary trigger on the
//! `ControllerChanged` event. CR 603.3a + CR 109.5: it is controlled by the
//! player who gained control, so "you" in its body is the gainer. CR 603.2c:
//! each gain triggers separately. CR 110.2: entering the battlefield is not a
//! gain of control. CR 608.2d: the body's choices are made on resolution (the
//! ability is untargeted). CR 705.2: only the player who flips wins or loses the
//! flip. CR 611.2a: a lost-flip control change lasts indefinitely. CR 609.3: an
//! impossible instruction does as much as possible. CR 113.7a: a triggered
//! ability on the stack exists independently of its source. CR 514.2: "until
//! end of turn" effects end in cleanup.

use std::collections::HashSet;

use engine::game::layers::flush_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::types::ability::{
    ChoiceType, ContinuousModification, Duration, EffectKind, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, GameRng, GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::choose_creature_and_opponent_flip::{
    change_controller_effect_count, seed_for_flip, seed_for_mixed_pair,
};

const P2: PlayerId = PlayerId(2);

const RISKY_MOVE: &str = "At the beginning of each player's upkeep, that player gains control of this enchantment.\nWhen you gain control of this enchantment from another player, choose a creature you control and an opponent. Flip a coin. If you lose the flip, that opponent gains control of that creature.";
const KRARK: &str = "If you would flip a coin, instead flip two coins and ignore one.";
const DISENCHANT: &str = "Destroy target artifact or enchantment.";
const BRAND: &str = "Gain control of all permanents you own. (This effect lasts indefinitely.)\nCycling {2} ({2}, Discard this card: Draw a card.)";
const WORD_OF_SEIZING: &str = "Split second (As long as this spell is on the stack, players can't cast spells or activate abilities that aren't mana abilities.)\nUntap target permanent and gain control of it until end of turn. It gains haste until end of turn.";

const LIBRARY: [&str; 10] = [
    "Lib 1", "Lib 2", "Lib 3", "Lib 4", "Lib 5", "Lib 6", "Lib 7", "Lib 8", "Lib 9", "Lib 10",
];

/// Where Risky Move starts: in P0's hand (cast through the public pipeline) or
/// on the battlefield under the given player.
#[derive(Clone, Copy)]
enum RiskyStart {
    CastByP0,
    Battlefield(PlayerId),
}

/// An instant in a player's hand, built from its verbatim Oracle text.
struct HandCard {
    owner: PlayerId,
    name: &'static str,
    text: &'static str,
    keywords: &'static [&'static str],
}

struct Opts {
    risky: RiskyStart,
    /// Creatures per player (0 or 2), named `P<n> Bear A` / `P<n> Bear B`.
    creatures: [usize; 3],
    thumb: Option<PlayerId>,
    hand: Vec<HandCard>,
}

impl Opts {
    fn on_battlefield(controller: PlayerId) -> Self {
        Opts {
            risky: RiskyStart::Battlefield(controller),
            creatures: [2, 2, 2],
            thumb: None,
            hand: Vec::new(),
        }
    }

    fn cast_by_p0() -> Self {
        Opts {
            risky: RiskyStart::CastByP0,
            ..Opts::on_battlefield(P0)
        }
    }

    fn creatures(mut self, creatures: [usize; 3]) -> Self {
        self.creatures = creatures;
        self
    }

    fn thumb(mut self, controller: PlayerId) -> Self {
        self.thumb = Some(controller);
        self
    }

    fn hand(mut self, card: HandCard) -> Self {
        self.hand.push(card);
        self
    }
}

struct Board {
    runner: GameRunner,
    risky: ObjectId,
    creatures: [Vec<ObjectId>; 3],
    hand: Vec<(&'static str, ObjectId)>,
    events: Vec<GameEvent>,
}

fn opponents_of(player: PlayerId) -> [PlayerId; 2] {
    let mut others = [P0, P1, P2].into_iter().filter(|p| *p != player);
    [others.next().unwrap(), others.next().unwrap()]
}

impl Board {
    fn new(opts: Opts) -> Self {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let mut creatures: [Vec<ObjectId>; 3] = Default::default();
        for (index, count) in opts.creatures.iter().enumerate() {
            for label in ["A", "B"].into_iter().take(*count) {
                let name = format!("P{index} Bear {label}");
                let id = scenario
                    .add_creature(PlayerId(index as u8), &name, 2, 2)
                    .id();
                creatures[index].push(id);
            }
        }
        let risky = match opts.risky {
            RiskyStart::CastByP0 => scenario
                .add_spell_to_hand_from_oracle(P0, "Risky Move", false, RISKY_MOVE)
                .as_enchantment()
                .id(),
            RiskyStart::Battlefield(controller) => scenario
                .add_enchantment_from_oracle(controller, "Risky Move", RISKY_MOVE)
                .id(),
        };
        if let Some(player) = opts.thumb {
            scenario.add_artifact_from_oracle(player, "Krark's Thumb", KRARK);
        }
        let hand = opts
            .hand
            .iter()
            .map(|card| {
                let id = scenario
                    .add_spell_to_hand(card.owner, card.name, true)
                    .from_oracle_text_with_keywords(card.keywords, card.text)
                    .id();
                (card.name, id)
            })
            .collect();
        // Libraries for every player so draw steps never deck anyone out.
        for player in [P0, P1, P2] {
            scenario.with_library_top(player, &LIBRARY);
        }
        let runner = scenario.build();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ));
        Board {
            runner,
            risky,
            creatures,
            hand,
            events: Vec::new(),
        }
    }

    fn state(&self) -> &GameState {
        self.runner.state()
    }

    fn act(&mut self, action: GameAction) {
        self.events.extend(
            self.runner
                .act(action)
                .expect("the public action must succeed")
                .events,
        );
    }

    fn controller_of(&self, id: ObjectId) -> PlayerId {
        self.state().objects[&id].controller
    }

    fn hand_card(&self, name: &str) -> ObjectId {
        self.hand
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, id)| *id)
            .expect("hand card staged")
    }

    /// One default action for a non-choice prompt; any other prompt is returned
    /// to the caller as `false` (it is a named prompt the test must answer).
    fn default_step(&mut self) -> bool {
        match &self.state().waiting_for {
            WaitingFor::Priority { .. } => self.act(GameAction::PassPriority),
            WaitingFor::DeclareAttackers { .. } => self.act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            }),
            WaitingFor::DeclareBlockers { .. } => self.act(GameAction::DeclareBlockers {
                assignments: vec![],
            }),
            _ => return false,
        }
        true
    }

    /// Passes priority / declares no attackers or blockers until `stop` holds.
    /// A prompt that is neither a default step nor `stop` is a test failure, so
    /// an unexpected `OrderTriggers` or target prompt cannot hide.
    fn drive_until(&mut self, what: &str, stop: impl Fn(&GameState) -> bool) {
        for _ in 0..600 {
            if stop(self.state()) {
                return;
            }
            assert!(
                self.default_step(),
                "unexpected prompt while driving to {what}: {:?}",
                self.state().waiting_for
            );
        }
        panic!("never reached {what}");
    }

    /// Drives to Risky Move's next resolution choice: the creature choice
    /// (`ChooseFromZoneChoice`) or, when no creature can be chosen, the opponent
    /// choice (`NamedChoice`).
    fn drive_to_prompt(&mut self) {
        self.drive_until("a Risky Move choice", |s| {
            matches!(
                s.waiting_for,
                WaitingFor::ChooseFromZoneChoice { .. } | WaitingFor::NamedChoice { .. }
            )
        });
    }

    /// The player making the pending creature choice.
    fn chooser(&self) -> PlayerId {
        match &self.state().waiting_for {
            WaitingFor::ChooseFromZoneChoice { player, .. }
            | WaitingFor::NamedChoice { player, .. } => *player,
            other => panic!("not at a Risky Move choice: {other:?}"),
        }
    }

    /// CR 608.2d: the untargeted creature choice offers exactly the chooser's
    /// own creatures, one of them, not optional.
    fn expect_creature_choice(&self, chooser: PlayerId) {
        let WaitingFor::ChooseFromZoneChoice {
            player,
            cards,
            count,
            up_to,
            source_id,
            ..
        } = &self.state().waiting_for
        else {
            panic!("required creature choice: {:?}", self.state().waiting_for);
        };
        assert_eq!(*player, chooser);
        assert_eq!(*source_id, self.risky);
        assert_eq!(*count, 1);
        assert!(!*up_to);
        assert_eq!(
            cards.iter().copied().collect::<HashSet<_>>(),
            self.creatures[chooser.0 as usize]
                .iter()
                .copied()
                .collect::<HashSet<_>>(),
            "the chooser is offered exactly their own creatures"
        );
        assert_eq!(cards.len(), 2);
    }

    /// CR 806.1: in a free-for-all the opponent choice offers the chooser's two
    /// opponents (every other player).
    fn expect_opponent_choice(&self, chooser: PlayerId) {
        let WaitingFor::NamedChoice {
            player,
            choice_type,
            options,
            ..
        } = &self.state().waiting_for
        else {
            panic!("required opponent choice: {:?}", self.state().waiting_for);
        };
        assert_eq!(*player, chooser);
        assert_eq!(*choice_type, ChoiceType::opponent());
        let [a, b] = opponents_of(chooser);
        assert_eq!(
            options.iter().map(String::as_str).collect::<HashSet<_>>(),
            HashSet::from([a.0.to_string().as_str(), b.0.to_string().as_str()])
        );
    }

    fn pick_creature(&mut self, chooser: PlayerId, pick: ObjectId) {
        self.expect_creature_choice(chooser);
        self.act(GameAction::SelectCards { cards: vec![pick] });
        self.expect_opponent_choice(chooser);
    }

    fn flips(&self) -> Vec<(PlayerId, bool)> {
        self.events
            .iter()
            .filter_map(|event| match event {
                GameEvent::CoinFlipped { player_id, won } => Some((*player_id, *won)),
                _ => None,
            })
            .collect()
    }

    fn controller_changes(&self) -> Vec<(ObjectId, PlayerId, PlayerId)> {
        self.events
            .iter()
            .filter_map(|event| match event {
                GameEvent::ControllerChanged {
                    object_id,
                    old_controller,
                    new_controller,
                } => Some((*object_id, *old_controller, *new_controller)),
                _ => None,
            })
            .collect()
    }

    /// Answers the pending opponent choice with a forced flip outcome.
    /// `keep_prompt` is true when the flipper's Krark's Thumb parks the flip on a
    /// `CoinFlipKeepChoice` (CR 614.1a), whose kept result is `won`.
    fn answer_opponent(
        &mut self,
        chooser: PlayerId,
        opponent: PlayerId,
        won: bool,
        keep_prompt: bool,
    ) {
        self.expect_opponent_choice(chooser);
        let before = self.flips().len();
        // Install the exact production Outcome stream immediately before the
        // opponent answer resumes FlipCoins.
        let seed = if keep_prompt {
            seed_for_mixed_pair()
        } else {
            seed_for_flip(won)
        };
        let state = self.runner.state_mut();
        state.rng_seed = seed;
        state.rng_word_pos = 0;
        state.rng = GameRng::seed_from_u64(seed);
        self.act(GameAction::ChooseOption {
            choice: opponent.0.to_string(),
        });
        if keep_prompt {
            let WaitingFor::CoinFlipKeepChoice {
                player,
                results,
                keep_count,
            } = &self.state().waiting_for
            else {
                panic!("required Krark keep choice: {:?}", self.state().waiting_for);
            };
            assert_eq!(
                *player, chooser,
                "the flipper keeps, not the old controller"
            );
            assert_eq!(*keep_count, 1);
            assert_eq!(results.len(), 2);
            assert_ne!(results[0], results[1], "reach: the forced pair is mixed");
            let index = results.iter().position(|&result| result == won).unwrap();
            // CR 614.6: the ignored flip has not happened yet.
            assert_eq!(self.flips().len(), before);
            self.act(GameAction::SelectCoinFlips {
                keep_indices: vec![index],
            });
        } else {
            assert!(
                !matches!(
                    self.state().waiting_for,
                    WaitingFor::CoinFlipKeepChoice { .. }
                ),
                "no Krark's Thumb applies to this flip"
            );
        }
        // CR 705.2: exactly the gainer flipped, once, with the forced outcome.
        let flips = self.flips();
        assert_eq!(flips.len(), before + 1, "exactly one flip: {flips:?}");
        assert_eq!(flips[before], (chooser, won));
    }

    /// Completion boundary: empty stack, `Priority`, empty `resolution_stack`.
    fn assert_complete(&self) {
        assert!(self.state().stack.is_empty(), "{:?}", self.state().stack);
        assert!(matches!(
            self.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(self.state().resolution_stack.is_empty());
    }

    /// Lets everything pending resolve, answering any further Risky Move choice
    /// with a creature pick and a forced win. Used only to reach the completion
    /// boundary after the claim under test has been asserted.
    fn settle_with_wins(&mut self) {
        for _ in 0..200 {
            match &self.state().waiting_for {
                WaitingFor::Priority { .. } if self.state().stack.is_empty() => return,
                WaitingFor::ChooseFromZoneChoice { player, cards, .. } => {
                    let (chooser, pick) = (*player, cards[0]);
                    self.act(GameAction::SelectCards { cards: vec![pick] });
                    let [opponent, _] = opponents_of(chooser);
                    self.answer_opponent(chooser, opponent, true, false);
                }
                _ => assert!(
                    self.default_step(),
                    "unexpected prompt while settling: {:?}",
                    self.state().waiting_for
                ),
            }
        }
        panic!("never settled");
    }

    /// Casts a zero-cost hand instant, selecting `target` if it is prompted.
    fn cast_instant(&mut self, spell: ObjectId, target: Option<ObjectId>) {
        let card_id = self.state().objects[&spell].card_id;
        self.act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        });
        // A lone legal target is chosen automatically; otherwise the spell
        // prompts for it. Either way the caller's reach guard proves the target.
        if let WaitingFor::TargetSelection { .. } = self.state().waiting_for {
            let target = target.expect("the spell prompts for a target");
            self.act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            });
        }
        assert!(
            matches!(self.state().waiting_for, WaitingFor::Priority { .. }),
            "cast completed: {:?}",
            self.state().waiting_for
        );
        assert!(
            self.state()
                .stack
                .iter()
                .any(|entry| entry.source_id == spell),
            "the spell is on the stack"
        );
    }

    /// True when Risky Move's gain trigger (the only stack object) is pending and
    /// `gainer` controls Risky Move.
    fn gain_trigger_pending(state: &GameState, risky: ObjectId, gainer: PlayerId) -> bool {
        state.stack.len() == 1
            && state.stack[0].source_id == risky
            && state.stack[0].controller == gainer
            && state.objects[&risky].controller == gainer
            && matches!(state.waiting_for, WaitingFor::Priority { .. })
    }
}

// ---------------------------------------------------------------------------
// R1 - R10, R12
// ---------------------------------------------------------------------------

/// R1: casting Risky Move is not a gain of control (CR 110.2): nothing triggers
/// from the cast or the enter. Paired positive: the first upkeep gain does.
#[test]
fn r1_casting_does_not_trigger_but_the_first_upkeep_gain_does() {
    let mut board = Board::new(Opts::cast_by_p0());
    let risky = board.risky;
    let card_id = board.state().objects[&risky].card_id;
    board.act(GameAction::CastSpell {
        object_id: risky,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    });
    for _ in 0..8 {
        if board.state().stack.is_empty() {
            break;
        }
        board.act(GameAction::PassPriority);
    }
    assert_eq!(board.state().objects[&risky].zone, Zone::Battlefield);
    assert_eq!(board.controller_of(risky), P0);
    assert!(board.controller_changes().is_empty(), "casting is no gain");
    assert!(
        matches!(board.state().waiting_for, WaitingFor::Priority { .. }),
        "no creature/opponent choice from casting: {:?}",
        board.state().waiting_for
    );
    board.assert_complete();

    // Paired positive (same test): the first upkeep gain prompts P1.
    board.drive_to_prompt();
    assert_eq!(board.chooser(), P1);
    board.expect_creature_choice(P1);
    assert_eq!(
        board.controller_changes(),
        vec![(risky, P0, P1)],
        "the upkeep gain is the only control change"
    );
}

/// R2: the upkeep gain triggers for the gainer, who chooses among their own
/// creatures (CR 603.3a, CR 109.5, CR 503.1a); the choice is untargeted.
#[test]
fn r2_upkeep_gain_prompts_the_gainer_with_their_own_creatures() {
    let mut board = Board::new(Opts::on_battlefield(P0));
    board.drive_to_prompt();
    // Risky Move is already P1's before the prompt (reach), P0's creatures exist
    // and are not offered.
    assert_eq!(board.controller_of(board.risky), P1);
    assert_eq!(board.chooser(), P1);
    board.expect_creature_choice(P1);
    assert_eq!(board.creatures[0].len(), 2);
    board.pick_creature(P1, board.creatures[1][0]);
}

/// R3: the chosen opponent (P2, not the previous controller P0) receives the
/// chosen creature on a lost flip, permanently (CR 705.2, CR 611.2a);
/// the paired win keeps it.
#[test]
fn r3_lost_flip_hands_the_chosen_creature_to_the_chosen_opponent_permanently() {
    let mut board = Board::new(Opts::on_battlefield(P0));
    board.drive_to_prompt();
    let chosen = board.creatures[1][0];
    board.pick_creature(P1, chosen);
    board.answer_opponent(P1, P2, false, false);
    assert_eq!(board.flips(), vec![(P1, false)]);
    let assert_owners = |board: &Board| {
        assert_eq!(board.controller_of(chosen), P2, "chosen creature moved");
        assert_eq!(board.controller_of(board.creatures[1][1]), P1);
        assert_eq!(board.controller_of(board.creatures[0][0]), P0);
        assert_eq!(board.controller_of(board.creatures[0][1]), P0);
        assert_eq!(board.controller_of(board.creatures[2][0]), P2);
        assert_eq!(board.controller_of(board.creatures[2][1]), P2);
    };
    assert_owners(&board);
    board.assert_complete();

    // Persists through cleanup into P2's turn.
    let turn = board.state().turn_number;
    board.drive_until("the next turn", |s| s.turn_number > turn);
    assert_eq!(board.state().active_player, P2);
    assert_owners(&board);

    // Paired control: a forced win keeps the creature.
    let mut won = Board::new(Opts::on_battlefield(P0));
    won.drive_to_prompt();
    let kept = won.creatures[1][0];
    won.pick_creature(P1, kept);
    won.answer_opponent(P1, P2, true, false);
    assert_eq!(
        won.flips(),
        vec![(P1, true)],
        "reach: the win really happened"
    );
    assert_eq!(won.controller_of(kept), P1);
}

/// R4: each gain triggers separately (CR 603.2c): across a full rotation every
/// upkeep player gets exactly one prompt, offered only their own creatures.
#[test]
fn r4_each_gain_triggers_separately_around_the_table() {
    let mut board = Board::new(Opts::on_battlefield(P0));
    let mut actors = Vec::new();
    let mut turns = Vec::new();
    for _ in 0..3 {
        board.drive_to_prompt();
        let chooser = board.chooser();
        actors.push(chooser);
        turns.push(board.state().turn_number);
        assert_eq!(board.controller_of(board.risky), chooser);
        board.expect_creature_choice(chooser);
        let pick = board.creatures[chooser.0 as usize][0];
        board.pick_creature(chooser, pick);
        let [opponent, _] = opponents_of(chooser);
        board.answer_opponent(chooser, opponent, true, false);
        board.assert_complete();
    }
    assert_eq!(actors, vec![P1, P2, P0]);
    assert!(
        turns.windows(2).all(|pair| pair[0] < pair[1]),
        "one prompt per upkeep, never two in the same turn: {turns:?}"
    );
    assert_eq!(
        board.flips(),
        vec![(P1, true), (P2, true), (P0, true)],
        "each gainer flips once"
    );
    assert_eq!(board.controller_of(board.risky), P0);
}

/// R5: no trigger when the upkeep player already controls Risky Move.
#[test]
fn r5_no_gain_trigger_when_the_upkeep_player_already_controls_it() {
    let mut board = Board::new(Opts::on_battlefield(P1));
    let risky = board.risky;
    board.drive_until("P1's upkeep trigger", |s| {
        s.active_player == P1 && s.phase == Phase::Upkeep && !s.stack.is_empty()
    });
    assert_eq!(board.state().stack.len(), 1, "only the upkeep trigger");
    // Resolve it: every pass must be a plain priority pass (no creature choice).
    while !board.state().stack.is_empty() {
        assert!(
            matches!(board.state().waiting_for, WaitingFor::Priority { .. }),
            "no choice before P1's draw: {:?}",
            board.state().waiting_for
        );
        board.act(GameAction::PassPriority);
    }
    // Reach: the upkeep ability really resolved a GiveControl from Risky Move.
    assert!(board.events.iter().any(|event| matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::GiveControl,
            source_id,
            ..
        } if *source_id == risky
    )));
    assert!(
        board.controller_changes().is_empty(),
        "P1 already controlled it"
    );
    assert_eq!(board.controller_of(risky), P1);

    // Paired positive: P2's upkeep is a real gain.
    board.drive_to_prompt();
    assert_eq!(board.chooser(), P2);
    board.expect_creature_choice(P2);
    assert_eq!(board.controller_changes(), vec![(risky, P1, P2)]);
}

/// R6: the gainer's Krark's Thumb applies (CR 614.1a, CR 705.2): the keep
/// choice is the gainer's; the kept result decides the flip.
#[test]
fn r6_the_gainers_krarks_thumb_applies() {
    for (keep_win, expected_owner) in [(false, P2), (true, P1)] {
        let mut board = Board::new(Opts::on_battlefield(P0).thumb(P1));
        board.drive_to_prompt();
        let chosen = board.creatures[1][0];
        board.pick_creature(P1, chosen);
        board.answer_opponent(P1, P2, keep_win, true);
        assert_eq!(board.flips(), vec![(P1, keep_win)]);
        assert_eq!(
            board.controller_of(chosen),
            expected_owner,
            "kept result {keep_win} decides the flip"
        );
        board.assert_complete();
    }
}

/// R7: the previous controller's Krark's Thumb does not apply to the gainer's
/// flip: no keep choice, one flip.
#[test]
fn r7_the_previous_controllers_krarks_thumb_does_not_apply() {
    let mut board = Board::new(Opts::on_battlefield(P0).thumb(P0));
    // Reach guard: the previous controller's Krark's Thumb is on the battlefield with its
    // replacement, so the missing keep choice below is owed to who controls it, not to a
    // Thumb that was never installed.
    let thumb = board
        .state()
        .objects
        .values()
        .find(|object| object.name == "Krark's Thumb")
        .expect("Krark's Thumb is in the game");
    assert_eq!(thumb.zone, Zone::Battlefield);
    assert_eq!(thumb.controller, P0);
    assert!(
        !thumb.base_replacement_definitions.is_empty(),
        "Krark's Thumb carries its flip replacement"
    );
    board.drive_to_prompt();
    let chosen = board.creatures[1][0];
    board.pick_creature(P1, chosen);
    board.answer_opponent(P1, P2, false, false);
    assert_eq!(board.flips(), vec![(P1, false)]);
    assert_eq!(board.controller_of(chosen), P2);
    board.assert_complete();
}

/// R8: the gain trigger exists independently of its source (CR 113.7a): P1
/// destroys Risky Move with the trigger pending, and the trigger still resolves
/// for P1 (CR 603.3a); the lost-flip control change persists (ruling 1).
#[test]
fn r8_trigger_resolves_after_risky_move_leaves_the_battlefield() {
    let mut board = Board::new(Opts::on_battlefield(P0).hand(HandCard {
        owner: P1,
        name: "Disenchant",
        text: DISENCHANT,
        keywords: &[],
    }));
    let risky = board.risky;
    board.drive_until("P1's pending gain trigger", |s| {
        Board::gain_trigger_pending(s, risky, P1) && s.active_player == P1
    });
    assert_eq!(
        board.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    );
    let disenchant = board.hand_card("Disenchant");
    board.cast_instant(disenchant, Some(risky));
    while board.state().stack.len() > 1 {
        board.act(GameAction::PassPriority);
    }
    // Reach: Risky Move is gone (owner P0's graveyard) and only the trigger remains.
    assert_eq!(board.state().objects[&risky].zone, Zone::Graveyard);
    assert!(board.state().players[0].graveyard.contains(&risky));
    assert!(matches!(
        board.state().stack[0].kind,
        StackEntryKind::TriggeredAbility { .. }
    ));

    board.drive_to_prompt();
    assert_eq!(board.chooser(), P1);
    board.expect_creature_choice(P1);
    let chosen = board.creatures[1][0];
    board.pick_creature(P1, chosen);
    board.answer_opponent(P1, P2, false, false);
    assert_eq!(board.controller_of(chosen), P2);
    board.assert_complete();

    let turn = board.state().turn_number;
    board.drive_until("the next turn", |s| s.turn_number > turn);
    assert_eq!(
        board.controller_of(chosen),
        P2,
        "the change does not end when Risky Move leaves the battlefield"
    );
}

/// R9: the trigger's controller is latched when it triggers (CR 603.3a), and a
/// second gain with the first trigger pending triggers again for the new gainer
/// (CR 603.2c): P0's Brand retakes Risky Move while P1's trigger is pending.
#[test]
fn r9_a_second_gain_with_the_first_trigger_pending_triggers_for_the_new_gainer() {
    let mut board = Board::new(Opts::on_battlefield(P0).hand(HandCard {
        owner: P0,
        name: "Brand",
        text: BRAND,
        keywords: &["Cycling"],
    }));
    let risky = board.risky;
    board.drive_until("P1's pending gain trigger", |s| {
        Board::gain_trigger_pending(s, risky, P1) && s.active_player == P1
    });
    // P1 and P2 pass; P0 holds priority.
    board.act(GameAction::PassPriority);
    board.act(GameAction::PassPriority);
    assert_eq!(
        board.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
    let brand = board.hand_card("Brand");
    board.cast_instant(brand, None);

    // Brand resolves: Risky Move is P0's again, and P0's gain trigger resolves
    // first (it is on top).
    board.drive_to_prompt();
    assert_eq!(board.controller_of(risky), P0, "reach: Brand took it back");
    assert_eq!(board.chooser(), P0);
    board.expect_creature_choice(P0);
    let p0_pick = board.creatures[0][0];
    board.pick_creature(P0, p0_pick);
    board.answer_opponent(P0, P1, true, false);

    // Then P1's earlier trigger, still P1's although P0 controls Risky Move.
    board.drive_to_prompt();
    assert_eq!(board.controller_of(risky), P0);
    assert_eq!(board.chooser(), P1);
    board.expect_creature_choice(P1);
    let p1_pick = board.creatures[1][0];
    board.pick_creature(P1, p1_pick);
    board.answer_opponent(P1, P2, true, false);
    assert_eq!(board.flips(), vec![(P0, true), (P1, true)]);
    board.assert_complete();
}

/// R10: zero creatures on the trigger route: nothing changes hands (CR 609.3,
/// CR 608.2c). The empty creature choice must not fall back to Risky Move
/// itself. Variant: the gainer's Krark's Thumb keeps the lost flip.
#[test]
fn r10_zero_creatures_on_the_trigger_route_changes_nothing() {
    for thumb in [false, true] {
        let opts = Opts::on_battlefield(P0).creatures([2, 0, 2]);
        let opts = if thumb { opts.thumb(P1) } else { opts };
        let mut board = Board::new(opts);
        let risky = board.risky;
        board.drive_to_prompt();
        // Reach: the opponent choice is the first prompt; no creature prompt can
        // hide.
        board.expect_opponent_choice(P1);
        assert_eq!(board.controller_of(risky), P1);
        assert_eq!(board.controller_changes(), vec![(risky, P0, P1)]);
        let effects_after_upkeep = change_controller_effect_count(board.state());

        board.answer_opponent(P1, P2, false, thumb);
        assert_eq!(board.flips(), vec![(P1, false)], "a lost flip happened");
        assert_eq!(
            board.controller_changes(),
            vec![(risky, P0, P1)],
            "no further control change after the upkeep gain"
        );
        assert_eq!(
            board.controller_of(risky),
            P1,
            "Risky Move is not given away"
        );
        assert_eq!(
            change_controller_effect_count(board.state()),
            effects_after_upkeep
        );
        assert_eq!(board.controller_of(board.creatures[0][0]), P0);
        assert_eq!(board.controller_of(board.creatures[2][0]), P2);
        board.assert_complete();
    }
}

/// R12 (attribution only): the cleanup reversion of Word of Seizing's "until end
/// of turn" control is a gain from another player, and its trigger is the
/// gainer's (CR 514.2, CR 603.3a). No claim is made about the cleanup step's
/// own trigger timing (CR 514.3a).
#[test]
fn r12_cleanup_reversion_gain_trigger_belongs_to_the_gainer() {
    let mut board = Board::new(Opts::on_battlefield(P0).hand(HandCard {
        owner: P2,
        name: "Word of Seizing",
        text: WORD_OF_SEIZING,
        keywords: &["Split second"],
    }));
    let risky = board.risky;

    // P1's upkeep gain, resolved with a forced win.
    board.drive_to_prompt();
    assert_eq!(board.chooser(), P1);
    let pick = board.creatures[1][0];
    board.pick_creature(P1, pick);
    board.answer_opponent(P1, P0, true, false);
    board.assert_complete();

    // P1 passes with an empty stack; P2 takes Risky Move until end of turn.
    board.act(GameAction::PassPriority);
    assert_eq!(
        board.state().waiting_for,
        WaitingFor::Priority { player: P2 }
    );
    let word = board.hand_card("Word of Seizing");
    board.cast_instant(word, Some(risky));
    board.drive_to_prompt();
    assert_eq!(
        board.controller_of(risky),
        P2,
        "reach: Word took Risky Move"
    );
    assert_eq!(board.chooser(), P2);
    board.expect_creature_choice(P2);
    let p2_pick = board.creatures[2][0];
    board.pick_creature(P2, p2_pick);
    board.answer_opponent(P2, P0, true, false);

    // Keep going: the until-end-of-turn effect ends and Risky Move reverts to P1.
    let changes_before_reversion = board.controller_changes().len();
    board.drive_to_prompt();
    let reversion = board.controller_changes()[changes_before_reversion..].to_vec();
    assert!(
        reversion.contains(&(risky, P2, P1)),
        "reach: the reversion P2 -> P1 was published before the prompt: {reversion:?}"
    );
    assert_eq!(board.controller_of(risky), P1);
    assert_eq!(board.chooser(), P1, "the gainer, not the previous holder");
    board.expect_creature_choice(P1);
    let reverted_pick = board.creatures[1][1];
    board.pick_creature(P1, reverted_pick);
    board.answer_opponent(P1, P0, true, false);
    board.settle_with_wins();
    board.assert_complete();
}

// ---------------------------------------------------------------------------
// C1 - attribution reads the event's endpoints
// ---------------------------------------------------------------------------

/// C1 (synthetic, building-block row): attribution reads each event's own
/// endpoints, not the post-flush live controller, and simultaneous triggers go on
/// the stack in APNAP order (CR 603.2c, CR 603.3b). Two Layer-2 effects move
/// Risky Move P0 -> P1 -> P2 and flush; `process_triggers` then sees the two
/// events. Production emitters read `object.controller` before any flush, so a
/// single resolution would publish `(P0 -> P1), (P0 -> P2)`; this row pins the
/// attribution contract, not an emitter pair.
#[test]
fn c1_attribution_reads_event_endpoints_in_apnap_order() {
    let mut board = Board::new(Opts::on_battlefield(P0).creatures([0, 0, 0]));
    let risky = board.risky;
    let state = board.runner.state_mut();
    for recipient in [P1, P2] {
        state
            .add_transient_continuous_effect(
                risky,
                recipient,
                Duration::Permanent,
                TargetFilter::SpecificObject { id: risky },
                vec![ContinuousModification::ChangeController],
                None,
            )
            .expect("permanent control effect installs");
    }
    flush_layers(state);
    assert_eq!(
        state.objects[&risky].controller, P2,
        "reach: the live controller is the last gainer, not the first"
    );
    assert_eq!(state.active_player, P0);
    process_triggers(
        state,
        &[
            GameEvent::ControllerChanged {
                object_id: risky,
                old_controller: P0,
                new_controller: P1,
            },
            GameEvent::ControllerChanged {
                object_id: risky,
                old_controller: P1,
                new_controller: P2,
            },
        ],
    );
    let controllers: Vec<_> = state
        .stack
        .iter()
        .filter(|entry| entry.source_id == risky)
        .map(|entry| entry.controller)
        .collect();
    assert_eq!(
        controllers,
        vec![P1, P2],
        "bottom-to-top: P1's trigger then P2's (APNAP from P0)"
    );
}
