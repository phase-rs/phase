//! Slice 1's synthetic Oracle producer, driven only through public game actions.
//! CR 115.10a + CR 608.2d: these are resolution choices, including shroud.
//! CR 705.1 + CR 705.2: every outcome assertion has a forced CoinFlipped guard.

use std::collections::HashSet;

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{ChoiceType, ContinuousModification};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, GameRng, GameState, RandomDraw, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use rand::Rng;

const P2: PlayerId = PlayerId(2);
const GAMBLE: &str = "Choose a creature you control and an opponent. Flip a coin. If you lose the flip, that opponent gains control of that creature.";
const KRARK: &str = "If you would flip a coin, instead flip two coins and ignore one.";

struct Gamble {
    runner: GameRunner,
    spell: ObjectId,
    plain: Option<ObjectId>,
    shroud: Option<ObjectId>,
    land: ObjectId,
    opposing: [ObjectId; 2],
    events: Vec<GameEvent>,
}

fn setup(with_creatures: bool, thumb_controller: Option<PlayerId>) -> Gamble {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let plain = with_creatures.then(|| scenario.add_creature(P1, "Bear A", 2, 2).id());
    let shroud = with_creatures.then(|| {
        scenario
            .add_creature(P1, "Shroud Bear", 2, 2)
            .with_keyword(Keyword::Shroud)
            .id()
    });
    let opposing = [
        scenario.add_creature(P0, "P0 Bear", 2, 2).id(),
        scenario.add_creature(P2, "P2 Bear", 2, 2).id(),
    ];
    let land = scenario.add_basic_land(P1, ManaColor::Green);
    if let Some(player) = thumb_controller {
        scenario.add_artifact_from_oracle(player, "Krark's Thumb", KRARK);
    }
    // The persistence row crosses into the next turn without a failed draw.
    for player in [P0, P1, P2] {
        scenario.with_library_top(player, &["Library card"]);
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Risky Gamble", false, GAMBLE)
        .id();
    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.active_player = P1;
    state.priority_player = P1;
    state.waiting_for = WaitingFor::Priority { player: P1 };
    Gamble {
        runner,
        spell,
        plain,
        shroud,
        land,
        opposing,
        events: Vec::new(),
    }
}

pub(super) fn seed_for_flip(won: bool) -> u64 {
    (0u64..10_000)
        .find(|&seed| {
            GameRng::seed_from_u64(seed)
                .draw(RandomDraw::Outcome)
                .random_bool(0.5)
                == won
        })
        .expect("a seed exists for either flip outcome")
}

pub(super) fn seed_for_mixed_pair() -> u64 {
    (0u64..10_000)
        .find(|&seed| {
            let mut rng = GameRng::seed_from_u64(seed);
            let first = rng.draw(RandomDraw::Outcome).random_bool(0.5);
            let second = rng.draw(RandomDraw::Outcome).random_bool(0.5);
            first != second
        })
        .expect("a seed exists for a mixed pair")
}

impl Gamble {
    fn act(&mut self, action: GameAction) {
        self.events.extend(
            self.runner
                .act(action)
                .expect("the public action must succeed")
                .events,
        );
    }

    fn cast_to_creature_choice(&mut self) {
        let card_id = self.runner.state().objects[&self.spell].card_id;
        self.act(GameAction::CastSpell {
            object_id: self.spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        });
        // S1-1: reverting Resolution timing asks for a target at announcement.
        assert!(matches!(
            self.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert_eq!(self.runner.state().stack.len(), 1, "the cast committed");
        for _ in 0..12 {
            match &self.runner.state().waiting_for {
                WaitingFor::ChooseFromZoneChoice { .. } | WaitingFor::NamedChoice { .. } => return,
                WaitingFor::Priority { .. } => {
                    assert!(
                        !self.runner.state().stack.is_empty(),
                        "spell must reach a choice"
                    );
                    self.act(GameAction::PassPriority);
                }
                other => panic!("unexpected prompt while resolving Risky Gamble: {other:?}"),
            }
        }
        panic!("Risky Gamble did not reach its resolution choice");
    }

    fn choose_shroud(&mut self) {
        self.choose(self.shroud.unwrap());
    }

    fn choose(&mut self, pick: ObjectId) {
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
                "required creature choice: {:?}",
                self.runner.state().waiting_for
            );
        };
        assert_eq!(*player, P1);
        assert_eq!(*source_id, self.spell);
        assert_eq!(*count, 1);
        assert!(!*up_to);
        let offered: HashSet<_> = cards.iter().copied().collect();
        assert_eq!(cards.len(), 2);
        assert_eq!(
            offered,
            HashSet::from([self.plain.unwrap(), self.shroud.unwrap()])
        );
        self.act(GameAction::SelectCards { cards: vec![pick] });
        self.assert_opponents();
    }

    fn assert_opponents(&self) {
        let WaitingFor::NamedChoice {
            player,
            choice_type,
            options,
            ..
        } = &self.runner.state().waiting_for
        else {
            panic!(
                "required opponent choice: {:?}",
                self.runner.state().waiting_for
            );
        };
        assert_eq!(*player, P1);
        assert_eq!(*choice_type, ChoiceType::opponent());
        assert_eq!(options.len(), 2);
        assert_eq!(
            options.iter().map(String::as_str).collect::<HashSet<_>>(),
            HashSet::from(["0", "2"])
        );
    }

    fn answer_opponent(&mut self, opponent: PlayerId, won: bool, own_thumb: bool) {
        self.assert_opponents();
        // Install the exact production Outcome stream immediately before the
        // opponent answer resumes FlipCoins, after all preceding actions.
        let seed = if own_thumb {
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
        if own_thumb {
            let WaitingFor::CoinFlipKeepChoice {
                player,
                results,
                keep_count,
            } = &self.runner.state().waiting_for
            else {
                panic!(
                    "required Krark keep choice: {:?}",
                    self.runner.state().waiting_for
                );
            };
            assert_eq!(*player, P1);
            assert_eq!(*keep_count, 1);
            assert_eq!(results.len(), 2);
            assert_ne!(results[0], results[1], "reach: the forced pair is mixed");
            let index = results.iter().position(|&result| result == won).unwrap();
            assert!(self.runner.state().active_coin_flip_frame().is_some());
            // CR 614.6: the ignored flip has not happened. The real mixed
            // CoinFlipKeepChoice above guards this negative assertion.
            assert!(self.flips().is_empty());
            self.act(GameAction::SelectCoinFlips {
                keep_indices: vec![index],
            });
        }
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
        // CR 705.2: exactly the caster/flipper has the kept outcome. This is
        // the positive reach guard for every unchanged-controller assertion.
        assert_eq!(self.flips(), vec![(P1, won)]);
        self.assert_complete();
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

    fn assert_complete(&self) {
        assert!(self.runner.state().stack.is_empty());
        assert!(matches!(
            self.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(self.runner.state().resolution_stack.is_empty());
        assert_eq!(
            self.runner.state().objects[&self.spell].zone,
            Zone::Graveyard
        );
        assert!(self.runner.state().players[1]
            .graveyard
            .contains(&self.spell));
    }

    fn assert_creatures(&self, recipient: PlayerId) {
        assert_eq!(
            self.runner.state().objects[&self.shroud.unwrap()].controller,
            recipient
        );
        assert_eq!(
            self.runner.state().objects[&self.plain.unwrap()].controller,
            P1
        );
        assert_eq!(
            self.runner.state().objects[&self.opposing[0]].controller,
            P0
        );
        assert_eq!(
            self.runner.state().objects[&self.opposing[1]].controller,
            P2
        );
        assert_eq!(self.runner.state().objects[&self.land].controller, P1);
    }
}

#[test]
fn s1_1_creature_choice_is_untargeted_and_happens_at_resolution() {
    // Discriminates C's Resolution timing and D's plain candidate scan:
    // reverting either loses the real prompt or excludes Shroud Bear.
    let mut game = setup(true, None);
    game.cast_to_creature_choice();
    game.choose_shroud();
    game.answer_opponent(P2, false, false);
    game.assert_creatures(P2);
}

#[test]
fn s1_2_opponent_choice_offers_exactly_the_two_other_players() {
    // Discriminates C's nested Choose(Opponent), rather than a targeted player
    // or a missing second instruction. The required prompt is its reach guard.
    let mut game = setup(true, None);
    game.cast_to_creature_choice();
    game.choose_shroud();
    game.assert_opponents();
    // The opponent prompt gates the rest of the spell: until it is answered the
    // coin has not been flipped and nothing has changed hands.
    assert!(game.flips().is_empty());
    game.assert_creatures(P1);
    game.answer_opponent(P2, false, false);
    game.assert_creatures(P2);
}

#[test]
fn s1_3_lost_flip_hands_over_the_chosen_creature_permanently() {
    // Discriminates C/E/F: chosen object and chosen player survive both
    // continuations and the branch reads that recipient, not the source owner.
    let mut game = setup(true, None);
    game.cast_to_creature_choice();
    game.choose_shroud();
    game.answer_opponent(P2, false, false);
    game.assert_creatures(P2);
    let turn = game.runner.state().turn_number;
    for _ in 0..120 {
        if game.runner.state().turn_number > turn {
            break;
        }
        match &game.runner.state().waiting_for {
            WaitingFor::Priority { .. } => game.act(GameAction::PassPriority),
            WaitingFor::DeclareAttackers { .. } => game.act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            }),
            other => panic!("unexpected prompt crossing cleanup: {other:?}"),
        }
    }
    assert!(
        game.runner.state().turn_number > turn,
        "reach: passed through cleanup"
    );
    assert_eq!(game.runner.state().active_player, P2);
    // CR 611.2a: no duration was specified, so cleanup does not end the effect.
    game.assert_creatures(P2);
    game.assert_complete();
}

#[test]
fn s1_4_the_other_opponent_is_a_different_recipient() {
    // Discriminates F's ChosenPlayer lookup against a fixed/default opponent.
    let mut game = setup(true, None);
    game.cast_to_creature_choice();
    game.choose_shroud();
    game.answer_opponent(P0, false, false);
    game.assert_creatures(P0);
}

#[test]
fn s1_5_winning_the_flip_keeps_both_creatures() {
    // Discriminates E's win/lose branch dispatch; forced win is the reach
    // guard. The same-test loss control also discriminates E's inherited
    // chosen-player context; an empty/broken branch cannot pass vacuously.
    let mut game = setup(true, None);
    game.cast_to_creature_choice();
    game.choose_shroud();
    game.answer_opponent(P2, true, false);
    game.assert_creatures(P1);
    let mut loss_control = setup(true, None);
    loss_control.cast_to_creature_choice();
    loss_control.choose_shroud();
    loss_control.answer_opponent(P2, false, false);
    loss_control.assert_creatures(P2);
}

#[test]
fn s1_6_krark_keep_lost_preserves_the_chosen_opponent() {
    // Discriminates E's PendingCoinFlip.branch_context.chosen_players: losing
    // it on suspension prevents the chosen P2 from receiving Shroud Bear.
    let mut game = setup(true, Some(P1));
    game.cast_to_creature_choice();
    game.choose_shroud();
    game.answer_opponent(P2, false, true);
    game.assert_creatures(P2);
}

#[test]
fn s1_7_krark_keep_won_does_not_run_the_lost_branch() {
    // Discriminates E's resumed kept-result dispatch: the mixed pair contains
    // a loss, but only the explicitly kept win may determine the branch. The
    // same-test keep-lost control kills loss of the parked chosen-player carrier.
    let mut game = setup(true, Some(P1));
    game.cast_to_creature_choice();
    game.choose_shroud();
    game.answer_opponent(P2, true, true);
    game.assert_creatures(P1);
    let mut loss_control = setup(true, Some(P1));
    loss_control.cast_to_creature_choice();
    loss_control.choose_shroud();
    loss_control.answer_opponent(P2, false, true);
    loss_control.assert_creatures(P2);
}

pub(super) fn change_controller_effect_count(state: &GameState) -> usize {
    state
        .transient_continuous_effects
        .iter()
        .filter(|effect| {
            effect.modifications.iter().any(|modification| {
                matches!(modification, ContinuousModification::ChangeController)
            })
        })
        .count()
}

fn zero_creatures_lost_flip(own_thumb: bool) {
    let mut game = setup(false, own_thumb.then_some(P1));
    let controllers: Vec<_> = game
        .runner
        .state()
        .objects
        .iter()
        .map(|(id, object)| (*id, object.controller))
        .collect();
    let effects_before = change_controller_effect_count(game.runner.state());
    game.cast_to_creature_choice();
    // CR 609.3: the completed-empty object choice does not stop independent
    // instructions. NamedChoice is required, so a creature prompt cannot hide.
    game.assert_opponents();
    game.answer_opponent(P2, false, own_thumb);
    assert_eq!(
        game.flips(),
        vec![(P1, false)],
        "reach: a lost flip really happened"
    );
    assert!(
        !game
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::ControllerChanged { .. })),
        "the resolving sorcery must not be given away"
    );
    assert_eq!(
        change_controller_effect_count(game.runner.state()),
        effects_before
    );
    assert_eq!(game.runner.state().objects[&game.spell].controller, P1);
    for (id, controller) in controllers {
        assert_eq!(
            game.runner.state().objects[&id].controller,
            controller,
            "controller unchanged for {id:?}"
        );
    }
    game.assert_complete();
}

#[test]
fn s1_8_zero_creatures_lost_flip_does_not_steal_the_sorcery() {
    // Discriminates D's Some([]) binding, the parked Choose continuation's
    // forwarding, and E's inline branch pruning. Without them ParentTarget
    // falls back to the resolving sorcery; event/TCE/controller checks catch it.
    zero_creatures_lost_flip(false);
}

#[test]
fn s1_9_zero_creatures_krark_keep_lost_does_not_steal_the_sorcery() {
    // Discriminates D's completed-empty binding through the Krark's Thumb
    // suspension. Defaulting the whole parked carrier also loses the chosen
    // player, so the handoff errors before installing a control effect and this
    // row can still pass; the marker-only loss is killed by the flip_coin unit
    // tests (`completed_empty_result_prunes_the_dependent_branch_node`, resumed
    // path, and the nested-flip rows).
    zero_creatures_lost_flip(true);
}

#[test]
fn s1_10_the_unchosen_creature_is_not_forwarded() {
    // Discriminates D's TargetOnly is_partition arm: reverting it forwards the
    // complement to the handoff instead of the selected creature. The plain
    // creature is picked here (s1_1 and s1_3 pick Shroud Bear), so the
    // complement is the shroud one; the transfer is the reach guard.
    let mut game = setup(true, None);
    game.cast_to_creature_choice();
    let plain = game.plain.unwrap();
    let shroud = game.shroud.unwrap();
    game.choose(plain);
    game.answer_opponent(P2, false, false);
    assert_eq!(game.runner.state().objects[&plain].controller, P2);
    assert_eq!(game.runner.state().objects[&shroud].controller, P1);
}

#[test]
fn s1_11_another_players_krark_does_not_double_this_flip() {
    // Discriminates controller-scoped replacement dispatch: P0's Thumb cannot
    // park P1's flip. A single lost CoinFlipped and transfer guard the absence.
    let mut game = setup(true, Some(P0));
    game.cast_to_creature_choice();
    game.choose_shroud();
    game.answer_opponent(P2, false, false);
    game.assert_creatures(P2);
}
