//! Synthetic composition tests for the chosen-player coin-branch context.
//! These are grammar witnesses, not claims that the synthetic spells are printed cards.

use std::collections::HashSet;

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    ChoiceType, CoinFlipRun, Effect, ForwardedResultContext, QuantityExpr,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, GameRng, RandomDraw, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolution::{FlipBranchContext, PendingCoinFlipKind, ResolutionStateWire};
use engine::types::zones::Zone;
use rand::Rng;

const P2: PlayerId = PlayerId(2);
const KRARK: &str = "If you would flip a coin, instead flip two coins and ignore one.";
const TWO_CHOICES: &str = "Choose a creature you control and an opponent. Choose a player. Flip a coin. If you lose the flip, that player gains control of that creature.";
// Per-head grammar from Ral Zarek; per-win grammar from Mirror March. The
// chosen-player/object handoff is deliberately synthetic, as in TWO_CHOICES.
const FLIP_N: &str = "Choose a creature you control and an opponent. Flip two coins. That opponent gains control of that creature for each coin that comes up heads.";
const UNTIL_LOSE: &str = "Choose a creature you control and an opponent. Flip a coin until you lose a flip. For each flip you won, that opponent gains control of that creature.";

struct Game {
    runner: GameRunner,
    spell: ObjectId,
    picked: ObjectId,
    unpicked: ObjectId,
    events: Vec<GameEvent>,
}

fn seed_for(outcomes: &[bool]) -> u64 {
    (0..100_000)
        .find(|&seed| {
            let mut rng = GameRng::seed_from_u64(seed);
            outcomes
                .iter()
                .all(|&won| rng.draw(RandomDraw::Outcome).random_bool(0.5) == won)
        })
        .expect("the bounded seed search must find the required outcome sequence")
}

impl Game {
    fn new(oracle: &str, thumb: bool) -> Self {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let picked = scenario.add_creature(P1, "Picked Bear", 2, 2).id();
        let unpicked = scenario.add_creature(P1, "Unpicked Bear", 3, 3).id();
        if thumb {
            scenario.add_artifact_from_oracle(P1, "Krark's Thumb", KRARK);
        }
        let spell = scenario
            .add_spell_to_hand_from_oracle(P1, "Context Gamble", false, oracle)
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
        Self {
            runner,
            spell,
            picked,
            unpicked,
            events: Vec::new(),
        }
    }

    fn act(&mut self, action: GameAction) {
        self.events.extend(
            self.runner
                .act(action)
                .expect("public game action succeeds")
                .events,
        );
    }

    fn choose_creature(&mut self) {
        let card_id = self.runner.state().objects[&self.spell].card_id;
        self.act(GameAction::CastSpell {
            object_id: self.spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        });
        // CR 115.10a + CR 608.2d: the choice is made during resolution.
        assert!(matches!(
            self.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert_eq!(self.runner.state().stack.len(), 1);
        for _ in 0..12 {
            if !matches!(self.runner.state().waiting_for, WaitingFor::Priority { .. }) {
                break;
            }
            assert!(
                !self.runner.state().stack.is_empty(),
                "reach the object choice"
            );
            self.act(GameAction::PassPriority);
        }
        let WaitingFor::ChooseFromZoneChoice {
            player,
            cards,
            count,
            up_to,
            source_id,
            ..
        } = &self.runner.state().waiting_for
        else {
            panic!(
                "required object choice: {:?}",
                self.runner.state().waiting_for
            );
        };
        assert_eq!(*player, P1);
        assert_eq!(*source_id, self.spell);
        assert_eq!(*count, 1);
        assert!(!*up_to);
        assert_eq!(cards.len(), 2);
        assert_eq!(
            cards.iter().copied().collect::<HashSet<_>>(),
            HashSet::from([self.picked, self.unpicked])
        );
        self.act(GameAction::SelectCards {
            cards: vec![self.picked],
        });
    }

    fn choose_player(&mut self, kind: ChoiceType, options_expected: &[&str], answer: PlayerId) {
        let WaitingFor::NamedChoice {
            player,
            choice_type,
            options,
            ..
        } = &self.runner.state().waiting_for
        else {
            panic!(
                "required player choice: {:?}",
                self.runner.state().waiting_for
            );
        };
        assert_eq!(*player, P1);
        assert_eq!(*choice_type, kind);
        assert_eq!(options.len(), options_expected.len());
        assert_eq!(
            options.iter().map(String::as_str).collect::<HashSet<_>>(),
            options_expected.iter().copied().collect()
        );
        self.act(GameAction::ChooseOption {
            choice: answer.0.to_string(),
        });
    }

    fn pin(&mut self, outcomes: &[bool]) {
        let seed = seed_for(outcomes);
        let state = self.runner.state_mut();
        state.rng_seed = seed;
        state.rng_word_pos = 0;
        state.rng = GameRng::seed_from_u64(seed);
    }

    fn keep(&mut self, won: bool, kind: PendingCoinFlipKind, chosen: &[PlayerId]) {
        let WaitingFor::CoinFlipKeepChoice {
            player,
            results,
            keep_count,
        } = &self.runner.state().waiting_for
        else {
            panic!(
                "required Krark prompt: {:?}",
                self.runner.state().waiting_for
            );
        };
        assert_eq!(*player, P1);
        assert_eq!(*keep_count, 1);
        assert_eq!(results.len(), 2);
        assert_ne!(results[0], results[1], "reach: the pinned pair is mixed");
        let index = results.iter().position(|&result| result == won).unwrap();
        let frame = self
            .runner
            .state()
            .active_coin_flip_frame()
            .expect("parked frame owns the prompt");
        assert_eq!(frame.kind, kind);
        assert_eq!(frame.source_id, self.spell);
        assert_eq!(frame.branch_context.chosen_players, chosen);
        self.act(GameAction::SelectCoinFlips {
            keep_indices: vec![index],
        });
    }

    fn finish(&mut self, outcomes: &[bool], recipient: PlayerId) {
        for _ in 0..12 {
            if self.runner.state().stack.is_empty() {
                break;
            }
            assert!(matches!(
                self.runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            self.act(GameAction::PassPriority);
        }
        // CR 705.2: the actual kept outcomes guard every unchanged-object check.
        let flips: Vec<_> = self
            .events
            .iter()
            .filter_map(|event| match event {
                GameEvent::CoinFlipped { player_id, won } => Some((*player_id, *won)),
                _ => None,
            })
            .collect();
        assert_eq!(
            flips,
            outcomes.iter().map(|&won| (P1, won)).collect::<Vec<_>>()
        );
        // CR 608.2c: the branch refers to the chosen object and chosen player.
        assert_eq!(
            self.runner.state().objects[&self.picked].controller,
            recipient
        );
        assert_eq!(self.runner.state().objects[&self.unpicked].controller, P1);
        assert_eq!(self.runner.state().objects[&self.spell].controller, P1);
        assert!(self.events.iter().any(|event| matches!(event,
            GameEvent::ControllerChanged { object_id, old_controller, new_controller, .. }
                if *object_id == self.picked && *old_controller == P1 && *new_controller == recipient)),
            "reach: the selected creature was actually handed off");
        assert!(self.runner.state().stack.is_empty());
        assert!(self.runner.state().resolution_stack.is_empty());
        assert!(matches!(
            self.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert_eq!(
            self.runner.state().objects[&self.spell].zone,
            Zone::Graveyard
        );
    }
}

fn two_choices(thumb: bool) -> Game {
    // SHAPE: the public parse must match compound_choice_c9, including index 1.
    let parsed = parse_oracle_text(
        TWO_CHOICES,
        "Context Gamble",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    assert_eq!(parsed.abilities.len(), 1);
    let root = &parsed.abilities[0];
    assert!(
        matches!(root.effect.as_ref(), Effect::TargetOnly { .. }),
        "ROOT: {:#?}",
        root
    );
    let first = root.sub_ability.as_deref().expect("first choice");
    assert!(
        matches!(first.effect.as_ref(), Effect::Choose { choice_type, .. } if *choice_type == ChoiceType::opponent())
    );
    let second = first.sub_ability.as_deref().expect("second choice");
    assert!(
        matches!(second.effect.as_ref(), Effect::Choose { choice_type, .. } if *choice_type == ChoiceType::player())
    );
    let flip = second.sub_ability.as_deref().expect("single flip");
    let Effect::FlipCoins {
        lose_effect: Some(branch),
        ..
    } = flip.effect.as_ref()
    else {
        panic!("required lost-flip branch: {flip:?}");
    };
    let Effect::GiveControl { recipient, .. } = branch.effect.as_ref() else {
        panic!("required handoff: {branch:?}");
    };
    assert_eq!(recipient.chosen_player_index(), Some(1));
    let mut game = Game::new(TWO_CHOICES, thumb);
    game.choose_creature();
    game.choose_player(ChoiceType::opponent(), &["0", "2"], P0);
    game.pin(if thumb { &[true, false] } else { &[false] });
    game.choose_player(ChoiceType::player(), &["0", "1", "2"], P2);
    game
}

fn round_trip_isolated_context(empty_result_only: bool) {
    let mut game = two_choices(true);
    let original = if empty_result_only {
        // Isolate the other half of the serde predicate on a real parked frame.
        // This is a wire fixture, not a claim that changing the live choice is legal.
        FlipBranchContext {
            chosen_players: vec![],
            forwarded_result_context: Some(Box::new(ForwardedResultContext::from_object_ids(
                game.runner.state(),
                &[],
            ))),
        }
    } else {
        game.runner
            .state()
            .active_coin_flip_frame()
            .expect("real parked frame")
            .branch_context
            .clone()
    };
    assert_eq!(original.chosen_players.is_empty(), empty_result_only);
    assert_eq!(
        original.forwarded_result_context.is_some(),
        empty_result_only
    );
    game.runner
        .state_mut()
        .active_coin_flip_frame_mut()
        .unwrap()
        .branch_context = original.clone();
    game.runner.state_mut().capture_rng_word_pos();
    let json = serde_json::to_value(ResolutionStateWire::from_game_state(
        game.runner.state().clone(),
    ))
    .expect("serialize parked frame");
    let data = json["resolution_frames"]["frames"]
        .as_array()
        .unwrap()
        .iter()
        .find(|frame| frame["type"] == "CoinFlip")
        .expect("reach: serialized coin frame");
    assert!(
        data["data"].get("branch_context").is_some(),
        "either nonempty component must write the key"
    );
    let restored = serde_json::from_value::<ResolutionStateWire>(json)
        .expect("deserialize wire")
        .into_game_state();
    assert_eq!(
        restored
            .active_coin_flip_frame()
            .map(|frame| &frame.branch_context),
        Some(&original)
    );
    assert!(matches!(
        restored.waiting_for,
        WaitingFor::CoinFlipKeepChoice { .. }
    ));
}

#[test]
fn chosen_players_only_coin_context_round_trips() {
    // REVERT: resolution.rs FlipBranchContext::is_empty (`&&` -> `||`) skips this key.
    round_trip_isolated_context(false);
}

#[test]
fn empty_result_only_coin_context_round_trips() {
    // REVERT: resolution.rs FlipBranchContext::is_empty (`&&` -> `||`) skips this key.
    round_trip_isolated_context(true);
}

#[test]
fn second_chosen_player_receives_the_creature_inline() {
    // REVERT: oracle_effect/mod.rs coin-branch with_player_scope or flip_coin.rs
    // run_flip_branch chosen-player propagation selects index 0 / loses the answer.
    two_choices(false).finish(&[false], P2);
}

#[test]
fn second_chosen_player_receives_the_creature_after_krark() {
    // REVERT: FlipRun::park frame capture / resume_after_keep context forwarding,
    // or the parser's enclosing scope, loses or misindexes the second answer.
    let mut game = two_choices(true);
    game.keep(
        false,
        PendingCoinFlipKind::Exactly { remaining: 0 },
        &[P0, P2],
    );
    game.finish(&[false], P2);
}

fn multi_flip(until_lose: bool, thumb: bool) {
    let oracle = if until_lose { UNTIL_LOSE } else { FLIP_N };
    // SHAPE: production parsing must put the chosen-player reader INSIDE the
    // multi-flip branch, rather than a sibling or an extra single-coin stub.
    let parsed = parse_oracle_text(oracle, "Context Gamble", &[], &["Sorcery".to_string()], &[]);
    assert_eq!(parsed.abilities.len(), 1);
    let root = &parsed.abilities[0];
    assert!(
        matches!(root.effect.as_ref(), Effect::TargetOnly { .. }),
        "ROOT: {:#?}",
        root
    );
    let choose = root.sub_ability.as_deref().expect("opponent choice");
    assert!(
        matches!(choose.effect.as_ref(), Effect::Choose { choice_type, .. } if *choice_type == ChoiceType::opponent())
    );
    let flip = choose
        .sub_ability
        .as_deref()
        .expect("multi-flip instruction");
    let branch = match flip.effect.as_ref() {
        Effect::FlipCoins {
            flips:
                CoinFlipRun::Exactly {
                    count: QuantityExpr::Fixed { value: 2 },
                },
            win_effect: Some(branch),
            lose_effect: None,
            ..
        } if !until_lose => branch,
        Effect::FlipCoins {
            flips: CoinFlipRun::UntilLoss,
            win_effect: Some(win_effect),
            lose_effect: None,
            ..
        } if until_lose => win_effect,
        other => panic!("required production multi-flip shape: {other:?}"),
    };
    let Effect::GiveControl { recipient, .. } = branch.effect.as_ref() else {
        panic!("required multi-flip handoff: {branch:?}");
    };
    assert_eq!(recipient.chosen_player_index(), Some(0));
    assert!(flip.sub_ability.is_none());
    assert!(branch.sub_ability.is_none());
    let mut game = Game::new(oracle, thumb);
    game.choose_creature();
    // For an Exactly run the FIRST loss must leave the context available for the
    // SECOND flip's win, including the resume path's re-pushed frame.
    let outcomes: &[bool] = if until_lose {
        &[true, true, false]
    } else {
        &[false, true]
    };
    let draws: Vec<bool> = if thumb {
        outcomes.iter().flat_map(|_| [true, false]).collect()
    } else {
        outcomes.to_vec()
    };
    game.pin(&draws);
    game.choose_player(ChoiceType::opponent(), &["0", "2"], P2);
    if thumb {
        for (index, &won) in outcomes.iter().enumerate() {
            let kind = if until_lose {
                PendingCoinFlipKind::UntilLoss {
                    wins_so_far: index as u32,
                }
            } else {
                PendingCoinFlipKind::Exactly {
                    remaining: (outcomes.len() - index - 1) as u32,
                }
            };
            game.keep(won, kind, &[P2]);
        }
    }
    game.finish(outcomes, P2);
}

#[test]
fn n_flip_branch_reads_chosen_player_inline() {
    // REVERT: flip_coin.rs resolve_flip_coins -> run_flip_branch context parameter.
    multi_flip(false, false);
}

#[test]
fn n_flip_branch_reads_chosen_player_after_second_krark_prompt() {
    // REVERT: flip_coin.rs FlipRun::park capture of an Exactly run,
    // resume_after_keep or re-park context.
    multi_flip(false, true);
}

#[test]
fn until_lose_branch_reads_chosen_player_inline() {
    // REVERT: flip_coin.rs resolve -> complete_run reward branch context.
    multi_flip(true, false);
}

#[test]
fn until_lose_branch_reads_chosen_player_after_repeated_krark_prompts() {
    // REVERT: flip_coin.rs UntilLoss run/frame, resume or finish context threading.
    multi_flip(true, true);
}

const BRANCH_CHOICE: &str =
    "Choose a player. Flip a coin. If you lose the flip, choose a player to draw a card.";

/// CR 608.2c: a player chosen INSIDE the lost branch is appended after the player
/// chosen before the flip, so its `ChosenPlayer` index continues the count. The
/// two answers differ (P0, then P2) and the card goes to the SECOND answer, inline
/// and after a Krark's Thumb keep choice.
/// REVERT: oracle_effect/mod.rs `parse_effect_chain_ir_after_player_choices` call
/// in the coin-branch fold (the branch's draw reads index 0 and draws for P0).
fn branch_local_player_choice(thumb: bool) {
    use engine::game::zones::create_object;
    use engine::types::identifiers::CardId;

    let mut game = Game::new(BRANCH_CHOICE, thumb);
    for (offset, player) in [P0, P1, P2].into_iter().enumerate() {
        create_object(
            game.runner.state_mut(),
            CardId(900 + offset as u64),
            player,
            "Library card".to_string(),
            Zone::Library,
        );
    }
    let hand =
        |game: &Game, player: PlayerId| game.runner.state().players[player.0 as usize].hand.len();
    let (p0_before, p2_before) = (hand(&game, P0), hand(&game, P2));

    let card_id = game.runner.state().objects[&game.spell].card_id;
    game.act(GameAction::CastSpell {
        object_id: game.spell,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    });
    for _ in 0..12 {
        if !matches!(game.runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        assert!(
            !game.runner.state().stack.is_empty(),
            "reach the first choice"
        );
        game.act(GameAction::PassPriority);
    }

    // The flip happens when the first answer resumes it: pin the Outcome stream
    // for a LOST flip first (a mixed pair under Krark's Thumb).
    game.pin(if thumb { &[false, true] } else { &[false] });
    game.choose_player(ChoiceType::player(), &["0", "1", "2"], P0);
    if thumb {
        game.keep(false, PendingCoinFlipKind::Exactly { remaining: 0 }, &[P0]);
    }
    game.choose_player(ChoiceType::player(), &["0", "1", "2"], P2);
    for _ in 0..12 {
        if game.runner.state().stack.is_empty() {
            break;
        }
        game.act(GameAction::PassPriority);
    }

    assert!(game
        .events
        .iter()
        .any(|event| matches!(event, GameEvent::CoinFlipped { won: false, .. })));
    assert_eq!(hand(&game, P2), p2_before + 1, "the second answer draws");
    assert_eq!(hand(&game, P0), p0_before, "the first answer does not");
    assert!(game.runner.state().stack.is_empty());
    assert!(game.runner.state().resolution_stack.is_empty());
}

#[test]
fn branch_local_player_choice_follows_the_enclosing_choice_inline() {
    branch_local_player_choice(false);
}

#[test]
fn branch_local_player_choice_follows_the_enclosing_choice_after_krark() {
    branch_local_player_choice(true);
}
