//! Coordinated player subjects: "you and <player> each <body>" binds ONE body to
//! the player set `AnyOf[Controller, member]` (CR 608.2c + CR 608.2e), where the
//! member is the targeted, triggering, chosen or possessive-actor player the
//! second conjunct names, or the players an attack trigger's matching attackers
//! were declared attacking (CR 508.6). Each seat performs the body for itself,
//! and a player outside the set is untouched.
//!
//! Runtime rows drive the real cast / combat pipeline with verbatim Oracle text.

use engine::game::combat::AttackTarget;
use engine::game::quantity::resolve_quantity;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AttackSubject, CombatHistoryScope, ControllerRef, Effect, EffectKind, PlayerFilter,
    QuantityExpr, QuantityRef, StaticDefinition, TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const ANGEL_OF_DESTINY: &str = "Flying, double strike\nWhenever a creature you control deals combat damage to a player, you and that player each gain that much life.\nAt the beginning of your end step, if you have at least 15 life more than your starting life total, each player this creature attacked this turn loses the game.";
const MANA_CLASH: &str = "You and target opponent each flip a coin. Mana Clash deals 1 damage to each player whose coin comes up tails. Repeat this process until both players' coins come up heads on the same flip.";
const KARAZIKAR: &str = "Whenever you attack a player, tap target creature that player controls and goad it. (Until your next turn, that creature attacks each combat if able and attacks a player other than you if able.)\nWhenever an opponent attacks another one of your opponents, you and the attacking player each draw a card and lose 1 life.";
const SECRET_RENDEZVOUS: &str = "You and target opponent each draw three cards.";
const THRAN_SPIDER: &str = "Reach\nWhen this creature enters, you and target opponent each create a tapped Powerstone token.\n{7}: Look at the top four cards of your library. You may reveal an artifact card from among them and put it into your hand. Put the rest on the bottom of your library in a random order.";
const BLOODROOT_APOTHECARY: &str = "Toxic 2 (Players dealt combat damage by this creature also get two poison counters. A player with ten or more poison counters loses the game.)\nWhen this creature enters, you and target opponent each create a Treasure token.\nWhenever an opponent sacrifices a noncreature token, that player gets two poison counters.";
const SYLVAN_OFFERING: &str = "Choose an opponent. You and that player each create an X/X green Treefolk creature token.\nChoose an opponent. You and that player each create X 1/1 green Elf Warrior creature tokens.";
const INFERNAL_OFFERING: &str = "Choose an opponent. You and that player each sacrifice a creature. Each player who sacrificed a creature this way draws two cards.\nChoose an opponent. Return a creature card from your graveyard to the battlefield, then that player returns a creature card from their graveyard to the battlefield.";
const DIVINER_SPIRIT: &str = "Whenever this creature deals combat damage to a player, you and that player each draw that many cards.";
/// Building block: a lone `LoseLife` body, outside every census card.
const COORDINATED_LOSE: &str = "You and target opponent each lose 2 life.";
const SIGN_IN_BLOOD: &str = "Target player draws two cards and loses 2 life.";
/// Building block: the possessive-actor conjunct with a recipient-bearing body.
const POSSESSIVE_DRAW: &str = "You and target creature's controller each draw a card.";
/// Building block: a coordinated targeted clause after an earlier player target.
const EARLIER_PLAYER_THEN_COORDINATED: &str =
    "Target player mills two cards. You and target opponent each draw a card.";
/// Building block: the same two clauses in the other order.
const COORDINATED_THEN_LATER_PLAYER: &str =
    "You and target opponent each draw a card. Target player mills two cards.";
const ZURZOTH: &str = "Whenever an opponent draws their first card each turn, if it's not their turn, you create a 1/1 red Devil creature token with \"When this token dies, it deals 1 damage to any target.\"\nWhenever one or more Devils you control attack one or more players, you and those players each draw a card, then discard a card at random.";
/// Control head: CR 508.3e triggers once per attacked player.
const YOU_ATTACK_A_PLAYER: &str = "Whenever you attack a player, you gain 1 life.";

const FILLER: [&str; 8] = ["F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8"];

fn game(players: u8) -> GameScenario {
    let mut scenario = if players == 2 {
        GameScenario::new()
    } else {
        GameScenario::new_n_player(players, 7)
    };
    scenario.at_phase(Phase::PreCombatMain);
    for player in (0..players).map(PlayerId) {
        scenario.with_library_top(player, &FILLER);
        // A three-card hand, so a random discard always has a card to take.
        for card in 0..3 {
            scenario.add_card_to_hand(player, &format!("Hand {} {card}", player.0));
        }
    }
    scenario
}

fn zone_count(runner: &GameRunner, player: PlayerId, zone: Zone) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|object| object.owner == player && object.zone == zone)
        .count()
}

fn zone_counts(runner: &GameRunner, players: u8, zone: Zone) -> Vec<i64> {
    (0..players)
        .map(|p| zone_count(runner, PlayerId(p), zone) as i64)
        .collect()
}

fn deltas(after: &[i64], before: &[i64]) -> Vec<i64> {
    after.iter().zip(before).map(|(a, b)| a - b).collect()
}

/// Drive the game to an empty stack: pass priority, order triggers, name
/// `opponent` for an opponent choice, and pick the first offered cards of a zone
/// choice. Returns every emitted event and, per sacrifice prompt, the prompted
/// player with the number of permanents sacrificed before it.
fn settle(runner: &mut GameRunner, opponent: PlayerId) -> (Vec<GameEvent>, Vec<(PlayerId, usize)>) {
    let mut events = Vec::new();
    let mut sacrifice_prompts = Vec::new();
    for _ in 0..64 {
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            WaitingFor::NamedChoice { .. } => GameAction::ChooseOption {
                choice: opponent.0.to_string(),
            },
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                count,
                effect_kind,
                ..
            } => {
                if *effect_kind == EffectKind::Sacrifice {
                    let sacrificed = events
                        .iter()
                        .filter(|event| matches!(event, GameEvent::PermanentSacrificed { .. }))
                        .count();
                    sacrifice_prompts.push((*player, sacrificed));
                }
                GameAction::SelectCards {
                    cards: cards.iter().take(*count).copied().collect(),
                }
            }
            other => panic!("unexpected waiting state {other:?}"),
        };
        events.extend(runner.act(action).expect("drive action").events);
    }
    (events, sacrifice_prompts)
}

fn hexproof_for_controller(
    runner_state: &mut engine::types::game_state::GameState,
    grantor: ObjectId,
) {
    let grant = vec![
        StaticDefinition::new(StaticMode::Hexproof).affected(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You),
        )),
    ];
    let object = runner_state.objects.get_mut(&grantor).expect("grantor");
    object.base_static_definitions = std::sync::Arc::new(grant.clone());
    object.static_definitions = grant.into();
    runner_state.layers_dirty.mark_full();
    engine::game::layers::flush_layers(runner_state);
}

fn tokens(
    runner: &GameRunner,
    player: PlayerId,
    name: &str,
) -> Vec<(Option<i32>, Option<i32>, bool)> {
    runner
        .state()
        .objects
        .values()
        .filter(|object| {
            object.controller == player && object.zone == Zone::Battlefield && object.name == name
        })
        .map(|object| (object.power, object.toughness, object.tapped))
        .collect()
}

/// Row 1 (Angel of Destiny). CR 119.3: the Bears deal 2 combat damage to P1, so
/// "you and that player each gain that much life" gives P0 20 + 2 = 22 and P1
/// 18 + 2 = 20. In a 3-player game the player who was not damaged is untouched:
/// the member is the damaged player, not every opponent.
#[test]
fn angel_of_destiny_controller_and_damaged_player_each_gain_that_much_life() {
    for players in [2, 3] {
        let mut scenario = game(players);
        scenario.add_creature_from_oracle(P0, "Angel of Destiny", 2, 6, ANGEL_OF_DESTINY);
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let mut runner = scenario.build();
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(bears, AttackTarget::Player(P1))])
            .expect("attack");
        let outcome = runner.combat_damage();
        assert!(
            outcome.events().iter().any(|event| matches!(
                event,
                GameEvent::DamageDealt {
                    amount: 2,
                    is_combat: true,
                    ..
                }
            )),
            "reach: the Bears dealt combat damage"
        );
        settle(&mut runner, P1);
        assert_eq!(runner.life(P0), 22, "{players}p: the controller gains 2");
        assert_eq!(
            runner.life(P1),
            20,
            "{players}p: the damaged player gains 2"
        );
        if players == 3 {
            assert_eq!(runner.life(P2), 20, "the undamaged opponent is untouched");
        }
    }
}

/// Row 2 (Mana Clash, first sentence). CR 705.1: the caster and the targeted
/// opponent each flip once; in a 3-player game the other opponent does not flip
/// (`TargetPlayer`, not `Opponent`). The rest of the card stays unsupported.
#[test]
fn mana_clash_caster_and_target_opponent_each_flip_a_coin() {
    let mut scenario = game(3);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Mana Clash", false, MANA_CLASH)
        .id();
    let mut runner = scenario.build();
    let gap_names: Vec<String> =
        std::iter::successors(runner.state().objects[&spell].abilities.first(), |link| {
            link.sub_ability.as_deref()
        })
        .filter_map(|link| match &*link.effect {
            Effect::Unimplemented { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect();
    assert!(
        gap_names
            .iter()
            .any(|name| name == "per_object_coin_flip_outcome"),
        "reach: the coin-outcome sentence stays an honest gap, got {gap_names:?}"
    );
    let outcome = runner.cast(spell).target_player(P1).resolve();
    let flips: Vec<PlayerId> = outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::CoinFlipped { player_id, .. } => Some(*player_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        flips,
        vec![P0, P1],
        "exactly the caster and the target flip"
    );
}

/// Row 3 (building block: the seat leaf). CR 119.3: "You and target opponent each
/// lose 2 life" — each of the two loses 2; the other opponent is untouched.
/// Paired guard on the untouched single-subject path: Sign in Blood's
/// "Target player draws two cards and loses 2 life" still reads the declared
/// target for its undirected life loss.
#[test]
fn coordinated_lose_life_binds_each_seat_and_sign_in_blood_is_unchanged() {
    let mut scenario = game(3);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Coordinated Drain", false, COORDINATED_LOSE)
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_player(P1).resolve();
    assert_eq!(outcome.life_delta(P0), -2);
    assert_eq!(outcome.life_delta(P1), -2);
    assert_eq!(outcome.life_delta(P2), 0, "the other opponent is untouched");

    let mut scenario = game(2);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Sign in Blood", false, SIGN_IN_BLOOD)
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_player(P1).resolve();
    assert_eq!(outcome.life_delta(P0), 0);
    assert_eq!(outcome.life_delta(P1), -2);
    assert_eq!(outcome.hand_drawn(P1), 2);
}

/// Row 4 (Karazikar, the Eye Tyrant). P1 attacks P2: Karazikar's controller (P0)
/// and the attacking player (P1) each draw a card (CR 121.2) and lose 1 life
/// (CR 119.3). P2, the attacked player, is untouched.
#[test]
fn karazikar_controller_and_attacking_player_each_draw_and_lose_life() {
    let mut scenario = game(3);
    scenario.add_creature_from_oracle(P0, "Karazikar, the Eye Tyrant", 5, 5, KARAZIKAR);
    let attacker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.advance_to_combat();
    let hands = zone_counts(&runner, 3, Zone::Hand);
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P2))])
        .expect("attack");
    let (events, _) = settle(&mut runner, P1);
    assert!(
        events
            .iter()
            .filter(|event| matches!(event, GameEvent::EffectResolved { .. }))
            .count()
            >= 2,
        "reach: the trigger resolved"
    );
    assert_eq!(
        deltas(&zone_counts(&runner, 3, Zone::Hand), &hands),
        vec![1, 1, 0],
        "P0 and P1 draw, P2 does not"
    );
    assert_eq!(
        [runner.life(P0), runner.life(P1), runner.life(P2)],
        [19, 19, 20]
    );
}

/// Row 7 (building block: possessive actor). "You and target creature's
/// controller each draw a card" targeting P1's creature: P0 and P1 each draw one;
/// P2 draws nothing.
#[test]
fn possessive_actor_controller_and_caster_each_draw() {
    let mut scenario = game(3);
    let creature = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Coordinated Draw", false, POSSESSIVE_DRAW)
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_object(creature).resolve();
    assert_eq!(
        [
            outcome.hand_drawn(P0),
            outcome.hand_drawn(P1),
            outcome.hand_drawn(P2)
        ],
        [1, 1, 0]
    );
}

/// Row 8 + P-Secret (Secret Rendezvous). CR 121.2: with its target legal, P0 and
/// P1 each draw three. CR 608.2b: when the spell's only target is illegal as it
/// resolves, it does not resolve, so the caster does not draw either.
#[test]
fn secret_rendezvous_draws_for_both_and_fizzles_with_an_illegal_target() {
    let mut scenario = game(2);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Secret Rendezvous", false, SECRET_RENDEZVOUS)
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_player(P1).resolve();
    assert_eq!(outcome.hand_drawn(P0), 3, "the caster draws three");
    assert_eq!(outcome.hand_drawn(P1), 3, "the target draws three");

    let mut scenario = game(2);
    let grantor = scenario.add_creature(P1, "Hexproof Grantor", 1, 1).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Secret Rendezvous", false, SECRET_RENDEZVOUS)
        .id();
    let mut runner = scenario.build();
    let mut commit = runner.cast(spell).target_player(P1).commit();
    hexproof_for_controller(commit.state_mut(), grantor);
    assert!(
        engine::game::static_abilities::player_has_hexproof(commit.state(), P1),
        "reach: the target is illegal before resolution"
    );
    let outcome = commit.resolve();
    assert_eq!(outcome.hand_drawn(P0), 0, "the caster does not draw");
    assert_eq!(
        outcome.hand_drawn(P1),
        0,
        "the illegal target does not draw"
    );
}

/// Row 10 (targeted member beside a second player target). CR 115.1 + CR 608.2c:
/// the draw goes to the controller and the "target opponent" target, and the
/// other target mills two. The coordinated clause binds only when no earlier
/// player slot precedes it; after one it keeps the halves shape.
#[test]
fn targeted_member_beside_a_second_player_target() {
    for (text, targets, drew, milled) in [
        (
            EARLIER_PLAYER_THEN_COORDINATED,
            [P1, P2],
            [1, 0, 1],
            [0, 2, 0],
        ),
        (
            EARLIER_PLAYER_THEN_COORDINATED,
            [P2, P1],
            [1, 1, 0],
            [0, 0, 2],
        ),
        (
            COORDINATED_THEN_LATER_PLAYER,
            [P2, P1],
            [1, 0, 1],
            [0, 2, 0],
        ),
    ] {
        let mut scenario = game(3);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Coordinated Probe", false, text)
            .id();
        let mut runner = scenario.build();
        assert_coordinated_shape(&runner, spell, text == COORDINATED_THEN_LATER_PLAYER);
        let graveyards = zone_counts(&runner, 3, Zone::Graveyard);
        let outcome = runner.cast(spell).target_players(&targets).resolve();
        let label = format!("{text:?} targets {targets:?}");
        assert_eq!(
            [
                outcome.hand_drawn(P0),
                outcome.hand_drawn(P1),
                outcome.hand_drawn(P2)
            ],
            drew,
            "{label}: draws"
        );
        let after: Vec<i64> = (0..3u8)
            .map(|p| {
                outcome
                    .state()
                    .objects
                    .values()
                    .filter(|o| {
                        o.owner == PlayerId(p) && o.zone == Zone::Graveyard && o.id != spell
                    })
                    .count() as i64
            })
            .collect();
        assert_eq!(
            deltas(&after, &graveyards),
            milled.to_vec(),
            "{label}: mills"
        );
    }
}

/// The shape of the coordinated sentence in `spell`'s chain: the bound clause
/// (`TargetOnly { opponent }` → scoped `Draw { ScopedPlayer }`) or the halves
/// (`Draw { OriginalController }` → `Draw { opponent }`, unscoped).
fn assert_coordinated_shape(runner: &GameRunner, spell: ObjectId, bound: bool) {
    let opponent = TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent));
    let links: Vec<_> =
        std::iter::successors(runner.state().objects[&spell].abilities.first(), |link| {
            link.sub_ability.as_deref()
        })
        .collect();
    let scoped: Vec<_> = links
        .iter()
        .filter(|link| link.player_scope.is_some())
        .collect();
    if bound {
        assert!(
            links.iter().any(|link| *link.effect
                == Effect::TargetOnly {
                    target: opponent.clone()
                }),
            "the bound clause declares its opponent through a head: {links:#?}"
        );
        assert_eq!(scoped.len(), 1, "{links:#?}");
        assert!(matches!(
            &*scoped[0].effect,
            Effect::Draw {
                target: TargetFilter::ScopedPlayer,
                ..
            }
        ));
        assert_eq!(
            scoped[0].player_scope,
            Some(PlayerFilter::AnyOf {
                filters: vec![PlayerFilter::Controller, PlayerFilter::TargetPlayer],
            })
        );
    } else {
        assert!(scoped.is_empty(), "the halves carry no set: {links:#?}");
        let draws: Vec<&TargetFilter> = links
            .iter()
            .filter_map(|link| match &*link.effect {
                Effect::Draw { target, .. } => Some(target),
                _ => None,
            })
            .collect();
        assert_eq!(
            draws,
            vec![&TargetFilter::OriginalController, &opponent],
            "the halves fallback: {links:#?}"
        );
    }
}

/// Row 10h (CR 608.2b with two declared player targets, one made illegal). An
/// illegal target is not affected and no part of the effect reads it; the legal
/// parts still happen. (i) and (ii) take the halves path; (iii) binds.
#[test]
fn an_illegal_player_target_beside_another_is_not_affected() {
    for (text, targets, hexproof_on, drew, milled) in [
        (
            EARLIER_PLAYER_THEN_COORDINATED,
            [P1, P2],
            P2,
            [1, 0, 0],
            [0, 2, 0],
        ),
        (
            EARLIER_PLAYER_THEN_COORDINATED,
            [P1, P2],
            P1,
            [1, 0, 1],
            [0, 0, 0],
        ),
        (
            COORDINATED_THEN_LATER_PLAYER,
            [P2, P1],
            P2,
            [1, 0, 0],
            [0, 2, 0],
        ),
    ] {
        let mut scenario = game(3);
        let grantor = scenario
            .add_creature(hexproof_on, "Hexproof Grantor", 1, 1)
            .id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Coordinated Probe", false, text)
            .id();
        let mut runner = scenario.build();
        let graveyards = zone_counts(&runner, 3, Zone::Graveyard);
        let mut commit = runner.cast(spell).target_players(&targets).commit();
        hexproof_for_controller(commit.state_mut(), grantor);
        assert!(
            engine::game::static_abilities::player_has_hexproof(commit.state(), hexproof_on),
            "reach: {hexproof_on:?} is an illegal target before resolution"
        );
        let outcome = commit.resolve();
        let label = format!("{text:?} targets {targets:?}, {hexproof_on:?} illegal");
        assert_eq!(
            [
                outcome.hand_drawn(P0),
                outcome.hand_drawn(P1),
                outcome.hand_drawn(P2)
            ],
            drew,
            "{label}: draws"
        );
        let after: Vec<i64> = (0..3u8)
            .map(|p| {
                outcome
                    .state()
                    .objects
                    .values()
                    .filter(|o| {
                        o.owner == PlayerId(p) && o.zone == Zone::Graveyard && o.id != spell
                    })
                    .count() as i64
            })
            .collect();
        assert_eq!(
            deltas(&after, &graveyards),
            milled.to_vec(),
            "{label}: mills"
        );
    }
}

/// P-Thran / P-Bloodroot. CR 111.2: the player who creates a token owns and
/// controls it, so the caster and the targeted opponent each control one.
#[test]
fn caster_and_target_opponent_each_create_a_token() {
    for (name, oracle, power, toughness, token, tapped) in [
        ("Thran Spider", THRAN_SPIDER, 2, 4, "Powerstone", true),
        (
            "Bloodroot Apothecary",
            BLOODROOT_APOTHECARY,
            3,
            3,
            "Treasure",
            false,
        ),
    ] {
        let mut scenario = game(3);
        let creature = scenario
            .add_creature_to_hand_from_oracle(P0, name, power, toughness, oracle)
            .id();
        let mut runner = scenario.build();
        let _ = runner.cast(creature).target_player(P1).resolve();
        settle(&mut runner, P1);
        for (player, expected) in [(P0, 1), (P1, 1), (P2, 0)] {
            let made = tokens(&runner, player, token);
            assert_eq!(made.len(), expected, "{name}: {player:?} {token}s {made:?}");
            assert!(
                made.iter().all(|(_, _, is_tapped)| *is_tapped == tapped),
                "{name}: {player:?} {token} tapped state"
            );
        }
    }
}

/// P-Sylvan (X = 3). Each of the caster and the chosen opponent creates one 3/3
/// Treefolk and three 1/1 Elf Warriors.
#[test]
fn sylvan_offering_caster_and_chosen_opponent_each_create_tokens() {
    let mut scenario = game(2);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Sylvan Offering", false, SYLVAN_OFFERING)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Green],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    let _ = runner.cast(spell).x(3).resolve();
    settle(&mut runner, P1);
    for player in [P0, P1] {
        assert_eq!(
            tokens(&runner, player, "Treefolk"),
            vec![(Some(3), Some(3), false)],
            "{player:?}: one 3/3 Treefolk"
        );
        assert_eq!(
            tokens(&runner, player, "Elf Warrior").len(),
            3,
            "{player:?}: three Elf Warriors"
        );
    }
}

/// P-Infernal, sentence 1. CR 701.21a: the caster and the chosen opponent each
/// sacrifice a creature. CR 101.4: both choices are made, P0's then P1's, before
/// either permanent is sacrificed. (The draw sentence is a pre-existing defect and
/// is deliberately not asserted.)
#[test]
fn infernal_offering_both_choose_then_both_sacrifice() {
    let mut scenario = game(2);
    for player in [P0, P1] {
        scenario.add_creature(player, "Grizzly Bears", 2, 2);
        scenario.add_creature(player, "Hill Giant", 3, 3);
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Infernal Offering", false, INFERNAL_OFFERING)
        .id();
    let mut runner = scenario.build();
    let _ = runner.cast(spell).commit();
    let (events, prompts) = settle(&mut runner, P1);
    assert_eq!(
        prompts,
        vec![(P0, 0), (P1, 0)],
        "P0 then P1 choose before anything is sacrificed"
    );
    let sacrificed: Vec<PlayerId> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PermanentSacrificed { player_id, .. } => Some(*player_id),
            _ => None,
        })
        .collect();
    assert_eq!(sacrificed.len(), 2, "{sacrificed:?}");
    assert!(sacrificed.contains(&P0) && sacrificed.contains(&P1));
}

/// P-Diviner. Diviner Spirit deals 2 combat damage, so the controller and the
/// damaged player each draw two (CR 121.2).
#[test]
fn diviner_spirit_controller_and_damaged_player_each_draw_that_many() {
    let mut scenario = game(2);
    let spirit = scenario
        .add_creature_from_oracle(P0, "Diviner Spirit", 2, 2, DIVINER_SPIRIT)
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    let hands = zone_counts(&runner, 2, Zone::Hand);
    runner
        .declare_attackers(&[(spirit, AttackTarget::Player(P1))])
        .expect("attack");
    let _ = runner.combat_damage();
    settle(&mut runner, P1);
    assert_eq!(runner.life(P1), 18, "reach: 2 combat damage");
    assert_eq!(
        deltas(&zone_counts(&runner, 2, Zone::Hand), &hands),
        vec![2, 2]
    );
}

fn zurzoth(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, "Zurzoth, Chaos Rider", 2, 2, ZURZOTH)
        .with_subtypes(vec!["Devil"])
        .id()
}

fn devil(scenario: &mut GameScenario, name: &str) -> ObjectId {
    scenario
        .add_creature(P0, name, 1, 1)
        .with_subtypes(vec!["Devil"])
        .id()
}

fn walker(scenario: &mut GameScenario, controller: PlayerId) -> ObjectId {
    scenario
        .add_planeswalker_from_oracle(controller, "Walker", "Gideon", 5, "+1: You gain 1 life.")
        .id()
}

/// Declare `attacks`, order a simultaneous batch of triggers, and return the
/// source of every triggered ability on the stack before anything resolves.
fn declare_and_stack(
    runner: &mut GameRunner,
    attacks: &[(ObjectId, AttackTarget)],
) -> Vec<ObjectId> {
    runner.declare_attackers(attacks).expect("attack");
    if let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for {
        let order = (0..triggers.len()).collect();
        runner
            .act(GameAction::OrderTriggers { order })
            .expect("order triggers");
    }
    runner
        .state()
        .stack
        .iter()
        .filter_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility { source_id, .. } => Some(*source_id),
            _ => None,
        })
        .collect()
}

/// Resolve the stack. Row R-7: CR 701.9b — a random discard asks no choice. A
/// chosen discard parks on `WaitingFor::DiscardChoice`, which `settle` has no
/// arm for (it panics there), so reaching an empty stack is the assertion.
/// `settle` must not gain a `DiscardChoice` arm.
fn resolve_all(runner: &mut GameRunner) -> Vec<GameEvent> {
    let (events, _) = settle(runner, P1);
    assert!(
        runner.state().stack.is_empty(),
        "the stack resolved without a discard choice: {:?}",
        runner.state().waiting_for
    );
    events
}

/// Per-seat draws and discards in `events`. A draw followed by a discard leaves
/// the hand size unchanged, so the events are what count.
fn draws_and_discards(events: &[GameEvent], players: u8) -> (Vec<usize>, Vec<usize>) {
    let per_seat = |is_seat: &dyn Fn(&GameEvent, PlayerId) -> bool| -> Vec<usize> {
        (0..players)
            .map(|p| {
                events
                    .iter()
                    .filter(|event| is_seat(event, PlayerId(p)))
                    .count()
            })
            .collect()
    };
    (
        per_seat(
            &|event, seat| matches!(event, GameEvent::CardDrawn { player_id, .. } if *player_id == seat),
        ),
        per_seat(
            &|event, seat| matches!(event, GameEvent::Discarded { player_id, .. } if *player_id == seat),
        ),
    )
}

/// Row R-1 (Zurzoth, Chaos Rider). Two Devils attack P1 and P2. CR 603.2c: the
/// batched head fires once for the one declaration, while the CR 508.3e control
/// fires once per attacked player. CR 508.6 + CR 608.2e: the controller and both
/// attacked players each draw a card, then discard a card at random. Zurzoth's
/// first ability then makes one Devil token for each opponent who drew their
/// first card on P0's turn.
#[test]
fn zurzoth_controller_and_each_attacked_player_draw_then_discard_at_random() {
    let mut scenario = game(3);
    let zurzoth = zurzoth(&mut scenario);
    let devil_a = devil(&mut scenario, "Devil A");
    let devil_b = devil(&mut scenario, "Devil B");
    let control = scenario
        .add_creature_from_oracle(P0, "Attack Counter", 0, 4, YOU_ATTACK_A_PLAYER)
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    let stacked = declare_and_stack(
        &mut runner,
        &[
            (devil_a, AttackTarget::Player(P1)),
            (devil_b, AttackTarget::Player(P2)),
        ],
    );
    assert_eq!(
        stacked.iter().filter(|source| **source == zurzoth).count(),
        1,
        "one declaration fires the batched head once: {stacked:?}"
    );
    assert_eq!(
        stacked.iter().filter(|source| **source == control).count(),
        2,
        "reach: the per-player control fired for each attacked player"
    );
    let devils_before = tokens(&runner, P0, "Devil").len();
    let events = resolve_all(&mut runner);
    assert!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved { source_id, .. } if *source_id == zurzoth
            ))
            .count()
            >= 2,
        "reach: Zurzoth's trigger resolved"
    );
    assert_eq!(runner.life(P0), 22, "reach: the control resolved twice");
    let (draws, discards) = draws_and_discards(&events, 3);
    assert_eq!(draws, vec![1, 1, 1], "each set member draws one");
    assert_eq!(discards, vec![1, 1, 1], "each set member discards one");
    assert_eq!(
        tokens(&runner, P0, "Devil").len() - devils_before,
        2,
        "both opponents drew their first card on P0's turn"
    );
}

/// Rows R-2, R-3, R-5 (Zurzoth). CR 508.6: "those players" are the players a
/// Devil was declared attacking.
/// - R-2: a non-Devil's defender is not in the set.
/// - R-3: a Devil attacking a planeswalker attacks no player (CR 506.3), so its
///   controller is not in the set (Zurzoth ruling, 2020-06-23).
/// - R-5: a player two Devils attacked is in the set once.
#[test]
fn zurzoth_set_is_the_players_a_devil_attacked() {
    enum Second {
        BearsAtP2,
        DevilAtP2Walker,
        DevilAtP1,
    }
    for second in [
        Second::BearsAtP2,
        Second::DevilAtP2Walker,
        Second::DevilAtP1,
    ] {
        let mut scenario = game(3);
        let zurzoth = zurzoth(&mut scenario);
        let devil_a = devil(&mut scenario, "Devil A");
        let (label, other) = match second {
            Second::BearsAtP2 => (
                "R-2 Devil→P1, Bears→P2",
                (
                    scenario.add_creature(P0, "Grizzly Bears", 2, 2).id(),
                    AttackTarget::Player(P2),
                ),
            ),
            Second::DevilAtP2Walker => (
                "R-3 Devil→P1, Devil→P2's planeswalker",
                (
                    devil(&mut scenario, "Devil B"),
                    AttackTarget::Planeswalker(walker(&mut scenario, P2)),
                ),
            ),
            Second::DevilAtP1 => (
                "R-5 Devil→P1 ×2",
                (devil(&mut scenario, "Devil B"), AttackTarget::Player(P1)),
            ),
        };
        let mut runner = scenario.build();
        runner.advance_to_combat();
        let stacked = declare_and_stack(&mut runner, &[(devil_a, AttackTarget::Player(P1)), other]);
        assert_eq!(
            stacked.iter().filter(|source| **source == zurzoth).count(),
            1,
            "{label}: fires once"
        );
        let (draws, discards) = draws_and_discards(&resolve_all(&mut runner), 3);
        assert_eq!(draws, vec![1, 1, 0], "{label}: draws");
        assert_eq!(discards, vec![1, 1, 0], "{label}: discards");
    }
}

/// Row R-4 (Zurzoth). CR 508.3a: "attack one or more players" triggers only on a
/// creature declared attacking a player. Devils attacking only a planeswalker do
/// not trigger it; R-3 (a Devil at a player beside one at a planeswalker) is the
/// positive.
#[test]
fn zurzoth_does_not_trigger_when_devils_attack_only_a_planeswalker() {
    let mut scenario = game(3);
    let zurzoth = zurzoth(&mut scenario);
    let devil_a = devil(&mut scenario, "Devil A");
    let walker = walker(&mut scenario, P2);
    let mut runner = scenario.build();
    runner.advance_to_combat();
    let stacked = declare_and_stack(
        &mut runner,
        &[(devil_a, AttackTarget::Planeswalker(walker))],
    );
    assert!(
        !stacked.contains(&zurzoth),
        "a planeswalker-only declaration does not trigger: {stacked:?}"
    );
    let (draws, discards) = draws_and_discards(&resolve_all(&mut runner), 3);
    assert_eq!(draws, vec![0, 0, 0]);
    assert_eq!(discards, vec![0, 0, 0]);
}

/// Row R-6 (Zurzoth, two players). Zurzoth, itself a Devil, attacks P1 alone:
/// P0 and P1 each draw a card, then discard a card at random.
#[test]
fn zurzoth_two_player_controller_and_defender_draw_then_discard() {
    let mut scenario = game(2);
    let zurzoth = zurzoth(&mut scenario);
    let mut runner = scenario.build();
    runner.advance_to_combat();
    let stacked = declare_and_stack(&mut runner, &[(zurzoth, AttackTarget::Player(P1))]);
    assert_eq!(
        stacked,
        vec![zurzoth],
        "Zurzoth's own attack fires the head"
    );
    let (draws, discards) = draws_and_discards(&resolve_all(&mut runner), 2);
    assert_eq!(draws, vec![1, 1]);
    assert_eq!(discards, vec![1, 1]);
}

/// Row R-8 (building block: the `EventAttackers` read). CR 508.6: the set is the
/// players the trigger event's attackers were declared attacking; a planeswalker
/// attack attacks no player (CR 506.3), and with no event in scope the set is
/// empty. The read is the declaration as narrowed, not live combat membership:
/// with no combat in progress (attacker `a` is in no combat) it still answers
/// from the event, and the `PlayerCount` path agrees.
#[test]
fn event_attackers_reads_the_trigger_events_attacked_players() {
    let mut scenario = game(3);
    let a = scenario.add_creature(P0, "Attacker A", 1, 1).id();
    let b = scenario.add_creature(P0, "Attacker B", 1, 1).id();
    let walker = walker(&mut scenario, P2);
    let mut runner = scenario.build();
    let attacked = |state: &GameState, player| {
        state.opponent_attacked(
            AttackSubject::EventAttackers,
            CombatHistoryScope::ThisCombat,
            P0,
            a,
            player,
        )
    };
    let count = |state: &GameState| {
        resolve_quantity(
            state,
            &QuantityExpr::Ref {
                qty: QuantityRef::PlayerCount {
                    filter: PlayerFilter::OpponentAttacked {
                        subject: AttackSubject::EventAttackers,
                        scope: CombatHistoryScope::ThisCombat,
                    },
                },
            },
            P0,
            a,
        )
    };
    assert!(
        runner.state().combat.is_none(),
        "reach: no combat is in progress"
    );
    assert_eq!(
        (
            attacked(runner.state(), P1),
            attacked(runner.state(), P2),
            count(runner.state())
        ),
        (false, false, 0),
        "no attack event in scope: the set is empty"
    );

    runner.state_mut().current_trigger_event = Some(GameEvent::AttackersDeclared {
        attacker_ids: vec![a, b],
        defending_player: P1,
        attacks: vec![
            (a, AttackTarget::Player(P1)),
            (b, AttackTarget::Planeswalker(walker)),
        ],
        declaration_records: vec![],
    });
    assert!(attacked(runner.state(), P1), "a attacked P1");
    assert!(
        !attacked(runner.state(), P2),
        "b attacked P2's planeswalker, which attacks no player"
    );
    assert_eq!(
        count(runner.state()),
        1,
        "the count reads the event without a combat in progress"
    );
}
