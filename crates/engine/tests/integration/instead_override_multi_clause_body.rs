//! CR 608.2c + CR 614.6: an "If <cost paid>, instead …" body of several
//! instructions replaces the earlier instruction once, and "search that
//! player's library" searches the player the spell already declared.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::card_type::Supertype;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{AlternativeCastKeyword, CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const SADISTIC_SACRAMENT: &str =
    "Kicker {7} (You may pay an additional {7} as you cast this spell.)\n\
Search target player's library for up to three cards, exile them, then that player shuffles. \
If this spell was kicked, instead search that player's library for up to fifteen cards, exile \
them, then that player shuffles.";

const GROW_FROM_THE_ASHES: &str =
    "Kicker {2} (You may pay an additional {2} as you cast this spell.)\n\
Search your library for a basic land card, put it onto the battlefield, then shuffle. If this \
spell was kicked, instead search your library for two basic land cards, put them onto the \
battlefield, then shuffle.";

const SPIRIT_WATER_REVIVAL: &str =
    "As an additional cost to cast this spell, you may waterbend {6}. (While paying a waterbend \
cost, you can tap your artifacts and creatures to help. Each one pays for {1}.)\n\
Draw two cards. If this spell's additional cost was paid, instead shuffle your graveyard into \
your library, draw seven cards, and you have no maximum hand size for the rest of the game.\n\
Exile Spirit Water Revival.";

const RIX_MAADI_REVELER: &str =
    "Spectacle {2}{B}{R} (You may cast this spell for its spectacle cost rather than its mana \
cost if an opponent lost life this turn.)\n\
When this creature enters, discard a card, then draw a card. If this creature's spectacle cost \
was paid, instead discard your hand, then draw three cards.";

const SHOCK: &str = "Shock deals 2 damage to any target.";

const HAUNTING_ECHOES: &str =
    "Exile all cards from target player's graveyard other than basic land cards. For each card \
exiled this way, search that player's library for all cards with the same name as that card \
and exile them. Then that player shuffles.";

const SUFFER_THE_PAST: &str = "Exile X target cards from target player's graveyard. For each \
card exiled this way, that player loses 1 life and you gain 1 life.";

/// One resolution-time prompt and what the drive answered.
#[derive(Debug)]
enum Prompt {
    Search {
        player: PlayerId,
        library_owner: Option<PlayerId>,
        cards: Vec<ObjectId>,
        count: usize,
        chosen: Vec<ObjectId>,
    },
    Discard {
        count: usize,
    },
}

struct Run {
    prompts: Vec<Prompt>,
    events: Vec<GameEvent>,
}

impl Run {
    fn searches(&self) -> Vec<&Prompt> {
        self.prompts
            .iter()
            .filter(|prompt| matches!(prompt, Prompt::Search { .. }))
            .collect()
    }

    fn shuffled(&self) -> Vec<PlayerId> {
        self.events
            .iter()
            .filter_map(|event| match event {
                GameEvent::PlayerPerformedAction {
                    player_id,
                    action: PlayerActionKind::ShuffledLibrary,
                    ..
                } => Some(*player_id),
                _ => None,
            })
            .collect()
    }
}

/// Answers every prompt until the stack is empty at priority: each search with
/// `pick(state, offered, count)`, each discard with the first `count` cards.
fn drive(
    runner: &mut GameRunner,
    mut events: Vec<GameEvent>,
    pick: impl Fn(&GameState, &[ObjectId], usize) -> Vec<ObjectId>,
) -> Run {
    let mut prompts = Vec::new();
    for _ in 0..64 {
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::SearchChoice {
                player,
                library_owner,
                cards,
                count,
                ..
            } => {
                let chosen = pick(runner.state(), &cards, count);
                prompts.push(Prompt::Search {
                    player,
                    library_owner,
                    cards,
                    count,
                    chosen: chosen.clone(),
                });
                GameAction::SelectCards { cards: chosen }
            }
            WaitingFor::DiscardChoice { count, cards, .. } => {
                prompts.push(Prompt::Discard { count });
                GameAction::SelectCards {
                    cards: cards.into_iter().take(count).collect(),
                }
            }
            WaitingFor::ManaPayment { .. } => GameAction::PassPriority,
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                return Run { prompts, events };
            }
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt while resolving: {other:?}"),
        };
        events.extend(runner.act(action).expect("the resolution advances").events);
    }
    panic!("resolution never settled: {:?}", runner.state().waiting_for);
}

fn first_n(_: &GameState, offered: &[ObjectId], count: usize) -> Vec<ObjectId> {
    offered.iter().take(count).copied().collect()
}

fn none(_: &GameState, _: &[ObjectId], _: usize) -> Vec<ObjectId> {
    Vec::new()
}

fn colorless(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn library_len(state: &GameState, player: PlayerId) -> usize {
    state.players[usize::from(player.0)].library.len()
}

fn exiled_owned_by(state: &GameState, player: PlayerId) -> usize {
    state
        .exile
        .iter()
        .filter(|id| state.objects[*id].owner == player)
        .count()
}

/// P0 casts Sadistic Sacrament at P1, kicked when `kicked`; both libraries hold 20 cards.
fn cast_sacrament(
    kicked: bool,
    pick: impl Fn(&GameState, &[ObjectId], usize) -> Vec<ObjectId>,
) -> (GameRunner, Run) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Sadistic Sacrament", false)
        .from_oracle_text_with_keywords(&["Kicker"], SADISTIC_SACRAMENT)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    for index in 0..20 {
        scenario.add_card_to_library_top(P0, &format!("Caster Card {index}"));
        scenario.add_card_to_library_top(P1, &format!("Target Card {index}"));
    }
    scenario.with_mana_pool(P0, colorless(7));
    let mut runner = scenario.build();
    let cast = runner.cast(spell).target_player(P1);
    let cast = if kicked {
        cast.accept_optional()
    } else {
        cast.decline_optional()
    };
    let events = cast.resolve().events().to_vec();
    let run = drive(&mut runner, events, pick);
    (runner, run)
}

#[test]
fn kicked_sadistic_sacrament_searches_only_the_target_players_library_once() {
    let (runner, run) = cast_sacrament(true, first_n);
    let searches = run.searches();
    assert_eq!(searches.len(), 1, "one search: {:?}", run.prompts);
    let Prompt::Search {
        library_owner,
        count,
        ..
    } = searches[0]
    else {
        unreachable!()
    };
    assert_eq!(*library_owner, Some(P1));
    assert_eq!(*count, 15);
    let state = runner.state();
    assert_eq!(exiled_owned_by(state, P1), 15);
    assert_eq!(library_len(state, P1), 5);
    assert_eq!(library_len(state, P0), 20);
    assert_eq!(exiled_owned_by(state, P0), 0);
    assert_eq!(run.shuffled(), vec![P1]);
}

#[test]
fn kicked_sadistic_sacrament_finding_nothing_still_shuffles_the_target_player() {
    let (runner, run) = cast_sacrament(true, none);
    let searches = run.searches();
    assert_eq!(searches.len(), 1, "one search: {:?}", run.prompts);
    assert!(matches!(
        searches[0],
        Prompt::Search {
            library_owner: Some(P1),
            count: 15,
            ..
        }
    ));
    assert!(runner.state().exile.is_empty());
    assert_eq!(run.shuffled(), vec![P1]);
}

#[test]
fn unkicked_sadistic_sacrament_exiles_up_to_three_from_the_target_player() {
    let (runner, run) = cast_sacrament(false, first_n);
    let searches = run.searches();
    assert_eq!(searches.len(), 1, "one search: {:?}", run.prompts);
    assert!(matches!(
        searches[0],
        Prompt::Search {
            library_owner: Some(P1),
            count: 3,
            ..
        }
    ));
    let state = runner.state();
    assert_eq!(exiled_owned_by(state, P1), 3);
    assert_eq!(library_len(state, P1), 17);
    assert_eq!(library_len(state, P0), 20);
    assert_eq!(run.shuffled(), vec![P1]);
}

/// P0 casts Grow from the Ashes with a library of five basic Forests.
fn cast_grow(kicked: bool) -> (GameRunner, Run, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Grow from the Ashes", false)
        .from_oracle_text_with_keywords(&["Kicker"], GROW_FROM_THE_ASHES)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let forests: Vec<ObjectId> = (0..5)
        .map(|_| {
            scenario
                .add_land_to_library_top(P0, "Forest")
                .with_subtypes(vec!["Forest"])
                .id()
        })
        .collect();
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    for forest in &forests {
        let object = runner.state_mut().objects.get_mut(forest).unwrap();
        object.card_types.supertypes.push(Supertype::Basic);
        object.base_card_types = object.card_types.clone();
    }
    let cast = runner.cast(spell);
    let cast = if kicked {
        cast.accept_optional()
    } else {
        cast.decline_optional()
    };
    let events = cast.resolve().events().to_vec();
    let run = drive(&mut runner, events, first_n);
    (runner, run, forests)
}

#[test]
fn kicked_grow_from_the_ashes_puts_two_basic_lands_with_one_search() {
    let (runner, run, forests) = cast_grow(true);
    let searches = run.searches();
    assert_eq!(searches.len(), 1, "one search: {:?}", run.prompts);
    assert!(matches!(searches[0], Prompt::Search { count: 2, .. }));
    let state = runner.state();
    let entered = forests
        .iter()
        .filter(|id| state.objects[*id].zone == Zone::Battlefield)
        .count();
    assert_eq!(entered, 2);
    assert_eq!(library_len(state, P0), 3);
    assert_eq!(run.shuffled(), vec![P0]);
}

#[test]
fn unkicked_grow_from_the_ashes_puts_one_basic_land() {
    let (runner, run, forests) = cast_grow(false);
    let searches = run.searches();
    assert_eq!(searches.len(), 1, "one search: {:?}", run.prompts);
    assert!(matches!(searches[0], Prompt::Search { count: 1, .. }));
    let state = runner.state();
    let entered = forests
        .iter()
        .filter(|id| state.objects[*id].zone == Zone::Battlefield)
        .count();
    assert_eq!(entered, 1);
    assert_eq!(library_len(state, P0), 4);
    assert_eq!(run.shuffled(), vec![P0]);
}

/// Passes through the rest of P0's turn, counting cleanup discard prompts (CR 514.1).
fn cleanup_discard_prompts(runner: &mut GameRunner) -> usize {
    let start_turn = runner.state().turn_number;
    let mut prompts = 0;
    for _ in 0..256 {
        if runner.state().turn_number != start_turn {
            return prompts;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DiscardToHandSize { count, cards, .. } => {
                prompts += 1;
                GameAction::SelectCards {
                    cards: cards.iter().take(*count).copied().collect(),
                }
            }
            _ => GameAction::PassPriority,
        };
        runner.act(action).expect("the turn advances");
    }
    panic!("turn never ended: {:?}", runner.state().waiting_for);
}

/// P0 casts Spirit Water Revival with six other cards in hand, three in the
/// graveyard and twenty in the library.
fn cast_revival(waterbend: bool) -> (GameRunner, Run) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Spirit Water Revival", false, SPIRIT_WATER_REVIVAL)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    for index in 0..6 {
        scenario.add_card_to_hand(P0, &format!("Hand Card {index}"));
    }
    for index in 0..3 {
        scenario.add_creature_to_graveyard(P0, &format!("Graveyard Card {index}"), 1, 1);
    }
    for index in 0..20 {
        scenario.add_card_to_library_top(P0, &format!("Library Card {index}"));
    }
    scenario.with_mana_pool(P0, colorless(6));
    let mut runner = scenario.build();
    let cast = runner.cast(spell);
    let cast = if waterbend {
        cast.accept_optional()
    } else {
        cast.decline_optional()
    };
    let events = cast.resolve().events().to_vec();
    let run = drive(&mut runner, events, first_n);
    (runner, run)
}

#[test]
fn paid_spirit_water_revival_shuffles_draws_seven_and_lifts_the_hand_limit() {
    let (mut runner, run) = cast_revival(true);
    let hand = |runner: &GameRunner| runner.state().players[0].hand.len();
    assert_eq!(hand(&runner), 13);
    assert!(runner.state().players[0].graveyard.is_empty());
    assert_eq!(run.shuffled(), vec![P0]);
    assert_eq!(cleanup_discard_prompts(&mut runner), 0);
    assert_eq!(hand(&runner), 13);
}

#[test]
fn unpaid_spirit_water_revival_draws_two_and_keeps_the_hand_limit() {
    let (mut runner, run) = cast_revival(false);
    assert_eq!(runner.state().players[0].hand.len(), 8);
    assert!(run.shuffled().is_empty());
    assert_eq!(cleanup_discard_prompts(&mut runner), 1);
}

/// After a Shock at P1, P0 casts Rix Maadi Reveler choosing `decision` at the
/// spectacle prompt and resolves its enters trigger. Returns P0's graveyard,
/// hand and library sizes measured once the creature spell is cast, and the run.
fn cast_rix(decision: AlternativeCastDecision) -> (GameRunner, Run, [usize; 3]) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for color in [
        ManaColor::Red,
        ManaColor::Red,
        ManaColor::Black,
        ManaColor::Black,
    ] {
        scenario.add_basic_land(P0, color);
    }
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK)
        .with_mana_cost(ManaCost::default())
        .id();
    let rix = scenario
        .add_creature_to_hand(P0, "Rix Maadi Reveler", 2, 2)
        .from_oracle_text_with_keywords(&["Spectacle"], RIX_MAADI_REVELER)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        })
        .id();
    for index in 0..4 {
        scenario.add_card_to_hand(P0, &format!("Hand Card {index}"));
    }
    for index in 0..10 {
        scenario.add_card_to_library_top(P0, &format!("Library Card {index}"));
    }
    let mut runner = scenario.build();
    runner.cast(shock).target_player(P1).resolve();
    assert!(runner.state().players[1].life_lost_this_turn > 0);

    let card_id = runner.state().objects[&rix].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: rix,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Rix Maadi Reveler is castable");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::AlternativeCastChoice {
                keyword: AlternativeCastKeyword::Spectacle,
                ..
            }
        ),
        "spectacle is offered: {:?}",
        runner.state().waiting_for
    );
    let mut events = runner
        .act(GameAction::ChooseAlternativeCast { choice: decision })
        .expect("the spectacle choice is accepted")
        .events;
    while let WaitingFor::ManaPayment { .. } = runner.state().waiting_for {
        events.extend(runner.act(GameAction::PassPriority).expect("pay").events);
    }
    assert_eq!(runner.state().objects[&rix].zone, Zone::Stack);
    let player = &runner.state().players[0];
    let cast_sizes = [
        player.graveyard.len(),
        player.hand.len(),
        player.library.len(),
    ];
    let run = drive(&mut runner, events, first_n);
    assert_eq!(runner.state().objects[&rix].zone, Zone::Battlefield);
    (runner, run, cast_sizes)
}

#[test]
fn spectacle_rix_maadi_reveler_discards_its_hand_then_draws_three() {
    let (runner, run, [graveyard, hand, library]) = cast_rix(AlternativeCastDecision::Alternative);
    assert_eq!(hand, 4);
    assert!(
        run.prompts.is_empty(),
        "no single-card discard: {:?}",
        run.prompts
    );
    let player = &runner.state().players[0];
    assert_eq!(player.graveyard.len(), graveyard + 4);
    assert_eq!(player.hand.len(), 3);
    assert_eq!(player.library.len(), library - 3);
}

#[test]
fn normally_cast_rix_maadi_reveler_discards_one_then_draws_one() {
    let (runner, run, [graveyard, hand, library]) = cast_rix(AlternativeCastDecision::Normal);
    assert_eq!(hand, 4);
    assert!(
        matches!(run.prompts.as_slice(), [Prompt::Discard { count: 1 }]),
        "{:?}",
        run.prompts
    );
    let player = &runner.state().players[0];
    assert_eq!(player.graveyard.len(), graveyard + 1);
    assert_eq!(player.hand.len(), 4);
    assert_eq!(player.library.len(), library - 1);
}

#[test]
fn haunting_echoes_searches_and_shuffles_the_target_players_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Haunting Echoes", false, HAUNTING_ECHOES)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.add_creature_to_graveyard(P1, "Grizzly Bears", 2, 2);
    let mut caster_bears = Vec::new();
    for player in [P0, P1] {
        for index in 0..3 {
            scenario.add_card_to_library_top(player, &format!("Filler {index}"));
        }
        for _ in 0..2 {
            let bear = scenario.add_card_to_library_top(player, "Grizzly Bears");
            if player == P0 {
                caster_bears.push(bear);
            }
        }
    }
    let mut runner = scenario.build();
    let events = runner
        .cast(spell)
        .target_player(P1)
        .resolve()
        .events()
        .to_vec();
    let run = drive(&mut runner, events, |state, offered, count| {
        offered
            .iter()
            .filter(|id| state.objects[*id].name == "Grizzly Bears")
            .take(count)
            .copied()
            .collect()
    });

    let searches = run.searches();
    assert!(!searches.is_empty(), "Haunting Echoes searches");
    let state = runner.state();
    for prompt in &searches {
        let Prompt::Search {
            player,
            library_owner,
            cards,
            chosen,
            ..
        } = prompt
        else {
            unreachable!()
        };
        assert_eq!(*player, P0, "the caster searches");
        assert_eq!(*library_owner, Some(P1));
        assert!(cards.iter().all(|id| state.objects[id].owner == P1));
        assert!(chosen
            .iter()
            .all(|id| state.objects[id].zone == Zone::Exile));
    }
    assert_eq!(library_len(state, P0), 5);
    assert!(caster_bears
        .iter()
        .all(|id| state.objects[id].zone == Zone::Library));
    assert_eq!(exiled_owned_by(state, P0), 0);
    assert_eq!(run.shuffled(), vec![P1]);
}

#[test]
fn suffer_the_past_drains_the_target_player_per_card_exiled() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Suffer the Past", true, SUFFER_THE_PAST)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Black],
            generic: 0,
        })
        .id();
    let first = scenario
        .add_creature_to_graveyard(P1, "Graveyard Card A", 1, 1)
        .id();
    let second = scenario
        .add_creature_to_graveyard(P1, "Graveyard Card B", 1, 1)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .x(2)
        .target_player(P1)
        .target_objects(&[first, second])
        .resolve();
    outcome.assert_zone(&[first, second], Zone::Exile);
    assert_eq!(outcome.state().players[1].life, 18);
    assert_eq!(outcome.state().players[0].life, 22);
}
