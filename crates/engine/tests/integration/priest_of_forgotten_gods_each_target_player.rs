//! Issue #3669: Priest of Forgotten Gods — "Any number of target players each lose 2 life and
//! sacrifice a creature of their choice. You add {B}{B} and draw a card." Each targeted player
//! is the subject of both instructions, so each of them sacrifices a creature they control
//! (CR 701.21a), while "you" stays the Priest's controller (CR 109.5). The instructions resolve
//! one at a time across all targeted players (CR 608.2e): everyone loses life, then the
//! sacrifice choices are made in APNAP order (CR 101.4) before any creature moves.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const PRIEST: &str = "{T}, Sacrifice two other creatures: Any number of target players each lose 2 life and sacrifice a creature of their choice. You add {B}{B} and draw a card. (Activate only as an instant.)";

struct PriestGame {
    runner: GameRunner,
    priest: ObjectId,
    cost: Vec<ObjectId>,
    keeper: ObjectId,
    opponents: Vec<ObjectId>,
    draw: ObjectId,
    gilded_light: ObjectId,
}

impl PriestGame {
    fn new(opponent_creatures: usize) -> Self {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let priest = scenario
            .add_creature_from_oracle(P0, "Priest of Forgotten Gods", 1, 2, PRIEST)
            .id();
        let cost = (0..2)
            .map(|i| scenario.add_creature(P0, &format!("Cost {i}"), 1, 1).id())
            .collect();
        let keeper = scenario.add_creature(P0, "Keeper", 1, 1).id();
        let opponents = (0..opponent_creatures)
            .map(|i| {
                scenario
                    .add_creature(P1, &format!("Opponent {i}"), 1, 1)
                    .id()
            })
            .collect();
        scenario.add_card_to_library_top(P0, "Second card");
        let draw = scenario.add_card_to_library_top(P0, "Priest's draw");
        let gilded_light = scenario
            .add_spell_to_hand_from_oracle(
                P1, "Gilded Light", true,
                "You gain shroud until end of turn. (You can't be the target of spells or abilities.)\nCycling {2} ({2}, Discard this card: Draw a card.)",
            )
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 1,
            })
            .id();
        scenario.with_mana_pool(
            P1,
            vec![
                ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
                ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ],
        );
        Self {
            runner: scenario.build(),
            priest,
            cost,
            keeper,
            opponents,
            draw,
            gilded_light,
        }
    }

    fn announce(&mut self, players: &[PlayerId]) {
        self.runner
            .act(GameAction::ActivateAbility {
                source_id: self.priest,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("activate Priest");
        for _ in 0..2 {
            match &self.runner.state().waiting_for {
                WaitingFor::TargetSelection { .. } => {
                    self.runner
                        .act(GameAction::SelectTargets {
                            targets: players.iter().copied().map(TargetRef::Player).collect(),
                        })
                        .expect("announce target players");
                }
                WaitingFor::PayCost {
                    kind: PayCostKind::Sacrifice,
                    ..
                } => {
                    self.runner
                        .act(GameAction::SelectCards {
                            cards: self.cost.clone(),
                        })
                        .expect("pay the sacrifice cost");
                }
                other => panic!("expected target selection or sacrifice cost, got {other:?}"),
            }
        }
        assert_eq!(self.runner.state().stack.len(), 1);
    }

    fn resolve(&mut self) {
        for _ in 0..2 {
            self.runner
                .act(GameAction::PassPriority)
                .expect("pass priority");
        }
    }

    fn expect_choice(&self, chooser: PlayerId, expected: &[ObjectId]) {
        let WaitingFor::EffectZoneChoice {
            player,
            cards,
            count: 1,
            effect_kind: EffectKind::Sacrifice,
            is_cost_payment: false,
            ..
        } = &self.runner.state().waiting_for
        else {
            panic!(
                "expected required sacrifice, got {:?}",
                self.runner.state().waiting_for
            );
        };
        assert_eq!(*player, chooser);
        assert_eq!(cards.len(), expected.len());
        assert!(
            expected.iter().all(|card| cards.contains(card)),
            "{cards:?}"
        );
    }

    fn choose(&mut self, creature: ObjectId) {
        self.runner
            .act(GameAction::SelectCards {
                cards: vec![creature],
            })
            .expect("choose sacrifice");
    }

    fn zone(&self, creature: ObjectId) -> Zone {
        self.runner.state().objects[&creature].zone
    }

    fn assert_life_delta(&self, player: PlayerId, delta: i32) {
        let life = self
            .runner
            .state()
            .players
            .iter()
            .find(|p| p.id == player)
            .expect("player present")
            .life;
        assert_eq!(life, 20 + delta, "life total for {player:?}");
    }

    fn assert_completed(&self, rider_resolved: bool) {
        assert!(self.runner.state().stack.is_empty());
        assert!(matches!(
            self.runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ));
        let caster = &self.runner.state().players[0];
        let mana = if rider_resolved { 2 } else { 0 };
        assert_eq!(caster.mana_pool.total(), mana);
        assert_eq!(caster.mana_pool.count_color(ManaType::Black), mana);
        assert_eq!(caster.hand.len(), usize::from(rider_resolved));
        assert_eq!(
            self.zone(self.draw),
            if rider_resolved {
                Zone::Hand
            } else {
                Zone::Library
            }
        );
    }
}

#[test]
fn priest_of_forgotten_gods_targeted_player_chooses() {
    let mut game = PriestGame::new(2);
    game.announce(&[P1]);
    game.resolve();
    game.expect_choice(P1, &game.opponents);
    game.assert_life_delta(P1, -2);
    game.choose(game.opponents[1]);
    assert_eq!(game.zone(game.opponents[1]), Zone::Graveyard);
    assert_eq!(game.zone(game.opponents[0]), Zone::Battlefield);
    assert_eq!(game.zone(game.keeper), Zone::Battlefield);
    game.assert_completed(true);
}

#[test]
fn priest_of_forgotten_gods_actions_finish_in_apnap_order() {
    let mut game = PriestGame::new(2);
    game.announce(&[P1, P0]);
    game.resolve();
    game.expect_choice(P0, &[game.priest, game.keeper]);
    game.assert_life_delta(P0, -2);
    game.assert_life_delta(P1, -2);
    game.choose(game.keeper);
    game.expect_choice(P1, &game.opponents);
    assert_eq!(game.zone(game.keeper), Zone::Battlefield);
    game.choose(game.opponents[1]);
    assert_eq!(game.zone(game.keeper), Zone::Graveyard);
    assert_eq!(game.zone(game.opponents[1]), Zone::Graveyard);
    game.assert_completed(true);
}

#[test]
fn priest_of_forgotten_gods_zero_targets_or_no_creatures() {
    for targets in [&[][..], &[P1][..]] {
        let mut game = PriestGame::new(0);
        game.announce(targets);
        game.resolve();
        game.assert_life_delta(P1, if targets.is_empty() { 0 } else { -2 });
        assert_eq!(game.zone(game.keeper), Zone::Battlefield);
        game.assert_completed(true);
    }
}

#[test]
fn priest_of_forgotten_gods_skips_illegal_targets() {
    for targets in [&[P1][..], &[P1, P0][..]] {
        let mut game = PriestGame::new(2);
        game.announce(targets);
        game.runner
            .act(GameAction::PassPriority)
            .expect("pass to P1");
        let light = game.gilded_light;
        let card_id = game.runner.state().objects[&light].card_id;
        game.runner
            .act(GameAction::CastSpell {
                object_id: light,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("cast Gilded Light in response");
        game.resolve();
        assert_eq!(game.runner.state().stack.len(), 1);
        assert!(engine::game::static_abilities::player_has_shroud(
            game.runner.state(),
            P1
        ));
        game.resolve();
        if targets.contains(&P0) {
            game.expect_choice(P0, &[game.priest, game.keeper]);
            game.choose(game.keeper);
            game.assert_life_delta(P0, -2);
        } else {
            game.assert_life_delta(P0, 0);
            assert_eq!(game.zone(game.keeper), Zone::Battlefield);
        }
        game.assert_life_delta(P1, 0);
        assert!(game
            .opponents
            .iter()
            .all(|&id| game.zone(id) == Zone::Battlefield));
        game.assert_completed(targets.contains(&P0));
    }
}
