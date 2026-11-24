//! Integration coverage for Gift a card using the shared draw-sequence path.

use engine::database::card_db::CardDatabase;
use engine::game::merge::MergeSide;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{AdditionalCostOrigin, EffectKind, StaticDefinition, TargetRef};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, CastingVariant, GameState, PersistedGameState, WaitingFor,
};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{ProhibitionScope, StaticMode};
use engine::types::zones::Zone;
use engine::types::ObjectId;

use crate::support::shared_card_db as load_db;

const COILING_REBIRTH: &str =
    "Gift a card (You may promise an opponent a gift as you cast this spell. \
If you do, they draw a card before its other effects.)\n\
Return target creature card from your graveyard to the battlefield. Then if the gift was promised \
and that creature isn't legendary, create a token that's a copy of that creature, except it's 1/1.";

const STINKWEED_IMP_ORACLE: &str = "Flying\n\
Whenever this creature deals combat damage to a creature, destroy that creature.\n\
Dredge 5 (If you would draw a card, you may mill five cards instead. If you do, return this card from your graveyard to your hand.)";

#[derive(Debug, PartialEq, Eq)]
enum GiftResolution {
    ReplacementChoice,
    Resolved,
}

fn cast_promised_coiling(
    runner: &mut GameRunner,
    spell: ObjectId,
    target: ObjectId,
    events: &mut Vec<GameEvent>,
) -> GiftResolution {
    let card_id = runner.state().objects[&spell].card_id;
    events.extend(
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("cast Coiling Rebirth")
            .events,
    );

    for _ in 0..200 {
        let (action, why) = match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => (GameAction::PassPriority, "complete mana payment"),
            WaitingFor::OptionalCostChoice { .. } => (
                GameAction::DecideOptionalCost { index: 0 },
                "promise Gift a card",
            ),
            WaitingFor::ChooseGiftRecipient { candidates, .. } => (
                GameAction::ChooseGiftRecipient {
                    opponent: candidates[0],
                },
                "choose gift recipient",
            ),
            WaitingFor::TargetSelection { .. } => (
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(target)),
                },
                "choose Coiling Rebirth target",
            ),
            WaitingFor::OrderTriggers { .. } | WaitingFor::Priority { .. }
                if !runner.state().stack.is_empty() =>
            {
                (GameAction::PassPriority, "advance promised Gift spell")
            }
            WaitingFor::ReplacementChoice { .. } => return GiftResolution::ReplacementChoice,
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                return GiftResolution::Resolved;
            }
            other => panic!("unexpected Gift resolution prompt: {other:?}"),
        };
        events.extend(runner.act(action).expect(why).events);
    }

    panic!("promised Gift spell did not resolve within 200 actions");
}

fn finish_resolving_stack(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    for _ in 0..200 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } | WaitingFor::OrderTriggers { .. } => {
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("finish resolving Gift spell")
                        .events,
                );
            }
            other => panic!("unexpected prompt while finishing Gift spell: {other:?}"),
        }
    }

    panic!("Gift spell did not finish within 200 actions");
}

fn gift_resolved(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|e| {
            matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::GiftDelivery,
                    ..
                }
            )
        })
        .count()
}

fn position(events: &[GameEvent], what: &str, pred: impl Fn(&GameEvent) -> bool) -> usize {
    events
        .iter()
        .position(pred)
        .unwrap_or_else(|| panic!("no {what} event among {events:?}"))
}

fn hand_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|entry| entry.id == player)
        .expect("player exists")
        .hand
        .len()
}

fn library_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|entry| entry.id == player)
        .expect("player exists")
        .library
        .len()
}

fn object_zone(runner: &GameRunner, object_id: ObjectId) -> Zone {
    runner.state().objects[&object_id].zone
}

/// CR 117.2e: no player has priority while the spell resolves, so once the
/// last choice of its resolution is answered the spell finishes inside that
/// answer: the stack and every parked resolution frame are empty and a player
/// has priority.
fn assert_settled_by_the_answer(runner: &GameRunner) {
    let state = runner.state();
    assert!(
        state.stack.is_empty(),
        "the spell finished resolving inside the answer, stack {:?}",
        state.stack
    );
    assert!(
        state.resolution_stack.is_empty(),
        "no resolution work is left parked after the answer: {:?}",
        state.resolution_stack
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "a player has priority after the answer, got {:?}",
        state.waiting_for
    );
}

/// Coiling Rebirth promised to P1 and paused on P1's Dredge prompt.
struct DredgeBoard {
    runner: GameRunner,
    events: Vec<GameEvent>,
    /// Coiling Rebirth's target in P0's graveyard.
    target: ObjectId,
    /// The top card of P1's six-card library.
    drawn_card: ObjectId,
    /// Stinkweed Imp in P1's graveyard.
    imp: ObjectId,
}

/// P0 casts Coiling Rebirth with the gift promised to P1, who has six Plains
/// in library and Stinkweed Imp in graveyard. The gift's draw pauses on P1's
/// Dredge 5 prompt, and the gift has not completed while it waits.
fn coiling_rebirth_at_dredge_prompt(db: &CardDatabase) -> DredgeBoard {
    let (mut scenario, spell, target) = promised_gift_scenario();
    let drawn_card = scenario.add_real_card(P1, "Plains", Zone::Library, db);
    for _ in 0..5 {
        scenario.add_real_card(P1, "Plains", Zone::Library, db);
    }
    let imp = scenario
        .add_creature_to_graveyard(P1, "Stinkweed Imp", 1, 1)
        .from_oracle_text(STINKWEED_IMP_ORACLE)
        .id();
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let mut events = Vec::new();
    assert_eq!(
        cast_promised_coiling(&mut runner, spell, target, &mut events),
        GiftResolution::ReplacementChoice,
        "Dredge must pause the Gift draw for its optional replacement choice"
    );
    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = &runner.state().waiting_for
    else {
        unreachable!("cast_promised_coiling stopped on a replacement prompt");
    };
    assert_eq!(*player, P1, "CR 616.1: the drawing player chooses");
    assert!(
        candidates.iter().any(|c| c.source_id == imp),
        "Stinkweed Imp's Dredge is offered: {candidates:?}"
    );
    assert_eq!(
        gift_resolved(&events),
        0,
        "the gift has not completed while its draw waits for the Dredge choice"
    );
    DredgeBoard {
        runner,
        events,
        target,
        drawn_card,
        imp,
    }
}

/// Index of the `Decline` answer of the optional Dredge prompt.
fn decline_index(runner: &GameRunner) -> usize {
    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        panic!("expected the Dredge prompt");
    };
    candidates
        .iter()
        .position(|c| c.description == "Decline")
        .unwrap_or_else(|| panic!("no Decline among {candidates:?}"))
}

fn promised_gift_scenario() -> (GameScenario, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario
        .add_creature_to_graveyard(P0, "Coiling Target", 3, 3)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Coiling Rebirth", false, COILING_REBIRTH)
        .id();
    (scenario, spell, target)
}

/// CR 702.52a + CR 121.6b: A Dredge replacement on the Gift recipient's draw
/// pauses the Gift resolution; declining it resumes and performs the normal draw.
/// CR 702.174j: the gift completes once the draw has settled, before the
/// spell's own return-to-battlefield effect.
#[test]
fn gift_card_draw_pauses_on_dredge_and_resumes() {
    let Some(db) = load_db() else {
        return;
    };
    let mut board = coiling_rebirth_at_dredge_prompt(db);
    let (runner, events) = (&mut board.runner, &mut board.events);
    let (target, drawn_card) = (board.target, board.drawn_card);
    assert_eq!(
        decline_index(runner),
        1,
        "the optional prompt declines at index 1"
    );
    events.extend(
        runner
            .act(GameAction::ChooseReplacement { index: 1 })
            .expect("decline Dredge")
            .events,
    );
    assert_settled_by_the_answer(runner);
    // A no-op once the answer settled the spell.
    finish_resolving_stack(runner, events);

    assert_eq!(gift_resolved(events), 1, "the gift completes exactly once");
    let gift = position(events, "gift completion", |e| {
        matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::GiftDelivery,
                ..
            }
        )
    });
    let drawn = position(
        events,
        "the gift draw",
        |e| matches!(e, GameEvent::ZoneChanged { object_id, to: Zone::Hand, .. } if *object_id == drawn_card),
    );
    let returned = position(
        events,
        "the return to the battlefield",
        |e| matches!(e, GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. } if *object_id == target),
    );
    assert!(drawn < gift, "the gift completes after its card is drawn");
    assert!(
        gift < returned,
        "CR 702.174j: the gift completes before the spell's other effects"
    );

    assert_eq!(object_zone(runner, drawn_card), Zone::Hand);
    assert_eq!(
        hand_count(runner, P1),
        1,
        "declined Dredge must draw the card"
    );
}

/// R23 — CR 702.52a + CR 701.17a: accepting Dredge mills five cards instead
/// of the gift's draw and returns Stinkweed Imp to P1's hand. The gift
/// completes once that settles, inside the answering action, and before the
/// spell's own return-to-battlefield effect (CR 702.174j).
#[test]
fn gift_card_draw_accepting_dredge_completes_the_gift_before_the_return() {
    let Some(db) = load_db() else {
        return;
    };
    let mut board = coiling_rebirth_at_dredge_prompt(db);
    let decline = decline_index(&board.runner);
    let accept = usize::from(decline == 0);
    board.events.extend(
        board
            .runner
            .act(GameAction::ChooseReplacement { index: accept })
            .expect("accept Dredge")
            .events,
    );
    assert_settled_by_the_answer(&board.runner);

    let p1 = board
        .runner
        .state()
        .players
        .iter()
        .find(|entry| entry.id == P1)
        .expect("player exists");
    assert_eq!(p1.graveyard.len(), 5, "CR 701.17a: five cards were milled");
    assert!(
        p1.graveyard.contains(&board.drawn_card),
        "CR 701.17a: the library top is among the milled cards"
    );
    assert!(
        p1.graveyard
            .iter()
            .all(|id| board.runner.state().objects[id].name == "Plains"),
        "the five milled cards are the Plains"
    );
    assert_eq!(p1.library.len(), 1, "one of the six Plains is left");
    assert_eq!(
        p1.hand.iter().copied().collect::<Vec<_>>(),
        vec![board.imp],
        "CR 614.6: no card was drawn; Dredge returned Stinkweed Imp"
    );

    let events = &board.events;
    assert_eq!(gift_resolved(events), 1, "the gift completes exactly once");
    let gift = position(events, "gift completion", |e| {
        matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::GiftDelivery,
                ..
            }
        )
    });
    let imp_returned = position(
        events,
        "Stinkweed Imp's return to hand",
        |e| matches!(e, GameEvent::ZoneChanged { object_id, to: Zone::Hand, .. } if *object_id == board.imp),
    );
    let returned = position(
        events,
        "the return to the battlefield",
        |e| matches!(e, GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. } if *object_id == board.target),
    );
    assert!(
        imp_returned < gift,
        "the gift completes after Dredge returned the Imp"
    );
    assert!(
        gift < returned,
        "CR 702.174j: the gift completes before the spell's other effects"
    );
}

/// CR 121.1: Pins the `start_draw_sequence` migration bug fix. Gift used to
/// call `select_cards_to_draw` directly, skipping `allowed_draw_count` and
/// incorrectly drawing through a `CantDraw` static.
#[test]
fn gift_card_draw_respects_cant_draw_static() {
    let (mut scenario, spell, target) = promised_gift_scenario();
    let gift_card = scenario.add_card_to_library_top(P1, "Gift Recipient Card");
    let cant_draw_source = scenario.add_creature(P1, "Cant Draw Source", 1, 1).id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&cant_draw_source)
        .unwrap()
        .static_definitions
        .push(StaticDefinition::new(StaticMode::CantDraw {
            who: ProhibitionScope::AllPlayers,
        }));

    let hand_before = hand_count(&runner, P1);
    let library_before = library_count(&runner, P1);
    let mut events = Vec::new();
    assert_eq!(
        cast_promised_coiling(&mut runner, spell, target, &mut events),
        GiftResolution::Resolved
    );
    assert_eq!(
        gift_resolved(&events),
        1,
        "with no pause the gift completes inline, once"
    );

    assert_eq!(hand_count(&runner, P1), hand_before);
    assert_eq!(library_count(&runner, P1), library_before);
    assert_eq!(object_zone(&runner, gift_card), Zone::Library);
}

const STARFALL: &str = "Gift a card (You may promise an opponent a gift as you cast this spell. \
If you do, they draw a card before its other effects.)\n\
Destroy all creatures. If the gift was promised, return a creature card put into your graveyard \
this way to the battlefield under your control.";

const LEYLINE: &str =
    "If this card is in your opening hand, you may begin the game with it on the \
battlefield.\nIf a card would be put into an opponent's graveyard from anywhere, exile it instead.";
const DAUTHI: &str = "Shadow (This creature can block or be blocked by only creatures with \
shadow.)\nIf a card would be put into an opponent's graveyard from anywhere, instead exile it \
with a void counter on it.\n{T}, Sacrifice this creature: Choose an exiled card an opponent owns \
with a void counter on it. You may play it this turn without paying its mana cost.";

fn starfall(scenario: &mut GameScenario) -> ObjectId {
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_spell_to_hand_from_oracle(P0, "Starfall Invocation", false, STARFALL)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::White, ManaCostShard::White],
        })
        .id()
}

fn fund(scenario: &mut GameScenario, mana: &[(ManaType, usize)]) {
    scenario.with_mana_pool(
        P0,
        mana.iter()
            .flat_map(|&(kind, count)| {
                (0..count).map(move |_| ManaUnit::new(kind, ObjectId(0), false, vec![]))
            })
            .collect(),
    );
}

fn assert_starfall_choice(state: &GameState, spell: ObjectId, expected: &[ObjectId]) {
    let WaitingFor::EffectZoneChoice {
        player,
        cards,
        count,
        min_count,
        up_to,
        source_id,
        effect_kind,
        zone,
        destination,
        enters_under_player,
        ..
    } = &state.waiting_for
    else {
        panic!("expected Starfall's choice, got {:?}", state.waiting_for);
    };
    // CR 608.2d + CR 110.2a: the spell controller chooses exactly one of the
    // eligible graveyard cards and the card enters under that player's control.
    assert_eq!(
        (*player, *source_id, *effect_kind),
        (P0, spell, EffectKind::ChangeZone)
    );
    assert_eq!(
        (*zone, *destination),
        (Zone::Graveyard, Some(Zone::Battlefield))
    );
    assert_eq!(
        (*min_count, *count, *up_to, *enters_under_player),
        (1, 1, false, Some(P0))
    );
    let mut actual = cards.clone();
    actual.sort_unstable_by_key(|id| id.0);
    let mut expected = expected.to_vec();
    expected.sort_unstable_by_key(|id| id.0);
    assert_eq!(actual, expected);
}

fn select_return(runner: &mut GameRunner, id: ObjectId) {
    runner
        .act(GameAction::SelectCards { cards: vec![id] })
        .expect("return the selected card");
    assert_eq!(object_zone(runner, id), Zone::Battlefield);
    assert_eq!(runner.state().objects[&id].controller, P0);
}

#[test]
fn starfall_offers_only_own_creature_cards_from_its_destroy_population() {
    const FACTORY: &str = "{T}: Add {C}.\n{1}: This land becomes a 2/2 Assembly-Worker artifact \
creature until end of turn. It's still a land.\n{T}: Target Assembly-Worker creature gets +1/+1 \
until end of turn.";
    let mut scenario = GameScenario::new();
    let spell = starfall(&mut scenario);
    let own = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let stolen = scenario
        .add_creature(P0, "Runeclaw Bear", 2, 2)
        .controlled_by(P1)
        .id();
    let borrowed = scenario
        .add_creature(P1, "Forest Bear", 2, 2)
        .controlled_by(P0)
        .id();
    let preexisting = scenario
        .add_creature_to_graveyard(P0, "Alpine Grizzly", 4, 2)
        .id();
    let indestructible = scenario
        .add_creature(P0, "Darksteel Myr", 0, 1)
        .from_oracle_text_with_keywords(
            &["Indestructible"],
            "Indestructible (Damage and effects that say \"destroy\" don't destroy this creature. If its toughness is 0 or less, it still dies.)",
        )
        .id();
    let factory = scenario
        .add_land_from_oracle(P0, "Mishra's Factory", FACTORY)
        .id();
    let raise = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Raise the Alarm",
            true,
            "Create two 1/1 white Soldier creature tokens.",
        )
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::White],
        })
        .id();
    let gift = scenario.add_card_to_library_top(P1, "Plains");
    fund(&mut scenario, &[(ManaType::White, 8)]);
    let mut runner = scenario.build();
    runner.activate(factory, 1).resolve();
    assert!(runner.state().objects[&factory]
        .card_types
        .core_types
        .contains(&engine::types::card_type::CoreType::Creature));
    runner.cast(raise).resolve();
    let tokens: Vec<_> = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| runner.state().objects[id].is_token)
        .collect();
    assert_eq!(tokens.len(), 2);

    let outcome = runner.cast(spell).accept_optional().resolve();
    assert_eq!(object_zone(&runner, gift), Zone::Hand);
    assert_starfall_choice(runner.state(), spell, &[own, stolen]);
    // CR 702.174j + CR 701.8a: Gift draws before the simultaneous wipe.
    let draw = position(outcome.events(), "gift card entering hand", |event| {
        matches!(
            event, GameEvent::ZoneChanged { object_id, to: Zone::Hand, .. } if *object_id == gift
        )
    });
    let deaths: Vec<_> = outcome
        .events()
        .iter()
        .enumerate()
        .filter_map(|(index, event)| match event {
            GameEvent::ZoneChanged {
                object_id,
                to: Zone::Graveyard,
                record,
                ..
            } if [own, stolen, factory, borrowed].contains(object_id)
                || tokens.contains(object_id) =>
            {
                Some((index, record))
            }
            _ => None,
        })
        .collect();
    assert_eq!(deaths.len(), 6);
    assert!(deaths
        .iter()
        .all(|(index, record)| *index > draw && record.co_departed.len() == 5));
    assert_eq!(object_zone(&runner, indestructible), Zone::Battlefield);
    for cards in [vec![], vec![preexisting], vec![borrowed]] {
        assert!(runner.act(GameAction::SelectCards { cards }).is_err());
        assert_starfall_choice(runner.state(), spell, &[own, stolen]);
    }
    select_return(&mut runner, stolen);
    assert_eq!(object_zone(&runner, own), Zone::Graveyard);
    assert_settled_by_the_answer(&runner);
}

#[test]
fn starfall_settles_empty_and_singleton_populations_and_declined_gifts() {
    for (creature, promised) in [(false, true), (true, true), (true, false)] {
        let mut scenario = GameScenario::new();
        let spell = starfall(&mut scenario);
        let old = scenario
            .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
            .id();
        let fresh = creature.then(|| scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id());
        let gift = scenario.add_card_to_library_top(P1, "Plains");
        fund(&mut scenario, &[(ManaType::White, 5)]);
        let mut runner = scenario.build();
        let cast = runner.cast(spell);
        let outcome = if promised {
            cast.accept_optional().resolve()
        } else {
            cast.resolve()
        };
        assert_eq!(gift_resolved(outcome.events()), usize::from(promised));
        assert_eq!(
            object_zone(&runner, gift),
            if promised { Zone::Hand } else { Zone::Library }
        );
        assert_eq!(object_zone(&runner, old), Zone::Graveyard);
        if let Some(fresh) = fresh {
            assert!(outcome.events().iter().any(|event| matches!(event,
                GameEvent::ZoneChanged { object_id, to: Zone::Graveyard, .. } if *object_id == fresh
            )));
            assert_eq!(
                object_zone(&runner, fresh),
                if promised {
                    Zone::Battlefield
                } else {
                    Zone::Graveyard
                }
            );
        }
        assert_settled_by_the_answer(&runner);
    }
}

#[test]
fn starfall_recovers_destroy_result_across_replacement_pause_and_restore() {
    let mut scenario = GameScenario::new();
    let spell = starfall(&mut scenario);
    let dauthi = scenario
        .add_creature_from_oracle(P0, "Dauthi Voidwalker", 3, 2, DAUTHI)
        .id();
    let leyline = scenario
        .add_enchantment_from_oracle(P0, "Leyline of the Void", LEYLINE)
        .id();
    let own = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let foe = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    let old = scenario
        .add_creature_to_graveyard(P0, "Alpine Grizzly", 4, 2)
        .id();
    let gift = scenario.add_card_to_library_top(P1, "Plains");
    fund(&mut scenario, &[(ManaType::White, 5)]);
    let mut runner = scenario.build();
    runner.cast(spell).accept_optional().resolve();
    // CR 616.1: the affected opponent chooses which redirect applies.
    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = &runner.state().waiting_for
    else {
        panic!("expected competing destruction replacements");
    };
    assert_eq!(*player, P1);
    assert_eq!(candidates.len(), 2);
    assert!(candidates
        .iter()
        .any(|candidate| candidate.source_id == dauthi));
    let leyline_index = candidates
        .iter()
        .position(|candidate| candidate.source_id == leyline)
        .unwrap();
    assert_eq!(object_zone(&runner, gift), Zone::Hand);
    let saved =
        serde_json::to_string(&PersistedGameState::capture(runner.state().clone())).unwrap();
    *runner.state_mut() = serde_json::from_str::<PersistedGameState>(&saved)
        .unwrap()
        .into_game_state()
        .unwrap();
    runner
        .act(GameAction::ChooseReplacement {
            index: leyline_index,
        })
        .unwrap();
    assert_eq!(object_zone(&runner, foe), Zone::Exile);
    assert_starfall_choice(runner.state(), spell, &[dauthi, own]);
    select_return(&mut runner, own);
    assert_eq!(object_zone(&runner, old), Zone::Graveyard);
    assert_settled_by_the_answer(&runner);
}

#[test]
fn starfall_reads_merged_creature_cards_and_excludes_replaced_components() {
    const PORCUPARROT: &str = "Mutate {2}{R} (If you cast this spell for its mutate cost, put it \
over or under target non-Human creature you own. They mutate into the creature on top plus all \
abilities from under it.)\n{T}: This creature deals X damage to any target, where X is the \
number of times this creature has mutated.";
    for token_topmost in [false, true] {
        let mut scenario = GameScenario::new();
        let spell = starfall(&mut scenario);
        let host_card =
            (!token_topmost).then(|| scenario.add_creature(P0, "Grizzly Bears", 2, 2).id());
        let token_maker = token_topmost.then(|| {
            scenario.add_enchantment_from_oracle(P1, "Leyline of the Void", LEYLINE);
            scenario
                .add_spell_to_hand_from_oracle(
                    P0,
                    "Raise the Alarm",
                    true,
                    "Create two 1/1 white Soldier creature tokens.",
                )
                .with_mana_cost(ManaCost::Cost {
                    generic: 1,
                    shards: vec![ManaCostShard::White],
                })
                .id()
        });
        let absorbed = scenario
            .add_creature_to_hand_from_oracle(P0, "Porcuparrot", 3, 4, PORCUPARROT)
            .with_mana_cost(ManaCost::Cost {
                generic: 3,
                shards: vec![ManaCostShard::Red],
            })
            .id();
        let old = scenario
            .add_creature_to_graveyard(P0, "Alpine Grizzly", 4, 2)
            .id();
        let gift = scenario.add_card_to_library_top(P1, "Plains");
        fund(&mut scenario, &[(ManaType::Red, 3), (ManaType::White, 7)]);
        let mut runner = scenario.build();
        let host = if let Some(host) = host_card {
            host
        } else {
            runner.cast(token_maker.unwrap()).resolve();
            *runner
                .state()
                .battlefield
                .iter()
                .find(|id| runner.state().objects[id].is_token)
                .expect("Raise the Alarm made a non-Human token")
        };
        let merge = runner
            .cast(absorbed)
            .alternative_cast(AlternativeCastDecision::Alternative)
            .casting_variant(CastingVariant::Mutate)
            .target_object(host)
            .resolve();
        assert!(matches!(
            merge.final_waiting_for(),
            WaitingFor::MutateMergeChoice { player: P0, .. }
        ));
        runner
            .act(GameAction::ChooseMutateMergeSide {
                side: MergeSide::Bottom,
            })
            .unwrap();
        // CR 702.140c + CR 702.140e: the spell merges underneath the host.
        assert_eq!(
            runner.state().objects[&host].merged_components,
            [host, absorbed]
        );
        assert_eq!(runner.state().objects[&host].is_token, token_topmost);

        let outcome = runner.cast(spell).accept_optional().resolve();
        assert_eq!(object_zone(&runner, gift), Zone::Hand);
        if token_topmost {
            // CR 730.3e + CR 108.2b: Leyline redirects only the absorbed card.
            // Neither that exiled card nor the token that reached the graveyard is eligible.
            assert!(outcome.events().iter().any(|event| matches!(event,
                GameEvent::ZoneChanged { object_id, to: Zone::Graveyard, .. } if *object_id == host
            )));
            assert_eq!(object_zone(&runner, absorbed), Zone::Exile);
            assert!(!runner.state().objects.contains_key(&host));
        } else {
            // CR 730.3c: both component cards are findable. Choose the absorbed card.
            assert_starfall_choice(runner.state(), spell, &[host, absorbed]);
            select_return(&mut runner, absorbed);
            assert_eq!(object_zone(&runner, host), Zone::Graveyard);
        }
        assert_eq!(object_zone(&runner, old), Zone::Graveyard);
        assert_settled_by_the_answer(&runner);
    }
}

#[test]
fn copied_starfall_inherits_gift_and_uses_only_its_own_destroy_result() {
    let mut scenario = GameScenario::new();
    let spell = starfall(&mut scenario);
    let twincast = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Twincast",
            true,
            "Copy target instant or sorcery spell. You may choose new targets for the copy.",
        )
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
        })
        .id();
    let first = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let second = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
    let gift_one = scenario.add_card_to_library_top(P1, "Plains");
    let gift_two = scenario.add_card_to_library_top(P1, "Island");
    fund(&mut scenario, &[(ManaType::White, 5), (ManaType::Blue, 2)]);
    let mut runner = scenario.build();
    let mut original = runner.cast(spell).accept_optional().commit();
    let copy_outcome = original.cast(twincast).target_object(spell).resolve();
    let copy_id = copy_outcome
        .events()
        .iter()
        .find_map(|event| match event {
            GameEvent::SpellCopied { object_id, .. } => Some(*object_id),
            _ => None,
        })
        .expect("Twincast copies the promised Starfall");
    // CR 707.10 + CR 702.174j: the copy gives the announced opponent the same
    // gift, but creates a new result and a new resolution choice.
    assert_eq!(gift_resolved(copy_outcome.events()), 1);
    assert_starfall_choice(original.state(), copy_id, &[first, second]);
    original
        .act(GameAction::SelectCards {
            cards: vec![second],
        })
        .unwrap();
    assert_eq!(original.state().objects[&second].zone, Zone::Battlefield);
    assert!(original
        .state()
        .stack
        .iter()
        .any(|entry| entry.source_id == spell));
    let original_outcome = original.resolve();
    assert_eq!(object_zone(&runner, gift_one), Zone::Hand);
    assert_eq!(object_zone(&runner, gift_two), Zone::Hand);
    assert!(original_outcome.events().iter().any(|event| matches!(event,
        GameEvent::ZoneChanged { object_id, to: Zone::Graveyard, .. } if *object_id == second
    )));
    assert_eq!(object_zone(&runner, first), Zone::Graveyard);
    assert_eq!(object_zone(&runner, second), Zone::Battlefield);
    assert_settled_by_the_answer(&runner);
}

const DJINN: &str = "({U/R} can be paid with either {U} or {R}.)\nFlying\nEach instant and sorcery spell you cast has replicate. The \
replicate cost is equal to its mana cost. (When you cast it, copy it for each time you paid \
its replicate cost. You may choose new targets for the copies.)";

fn cast_with_replicate_gift_declined(
    runner: &mut GameRunner,
    spell: ObjectId,
    target: ObjectId,
    djinn: ObjectId,
    events: &mut Vec<GameEvent>,
) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    let mut replicate_paid = false;
    let mut gift_declined = false;
    for _ in 0..20 {
        let action = match &runner.state().waiting_for {
            WaitingFor::OptionalCostChoice {
                origin: AdditionalCostOrigin::Replicate,
                ..
            } => {
                let index = usize::from(replicate_paid);
                replicate_paid = true;
                GameAction::DecideOptionalCost { index }
            }
            WaitingFor::OptionalCostChoice {
                origin: AdditionalCostOrigin::Gift,
                ..
            } => {
                gift_declined = true;
                GameAction::DecideOptionalCost { index: 1 }
            }
            WaitingFor::TargetSelection { .. } => GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            },
            WaitingFor::ManaPayment { .. } => GameAction::PassPriority,
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => break,
            other => panic!("unexpected targeted Gift cast prompt: {other:?}"),
        };
        events.extend(runner.act(action).unwrap().events);
    }
    // CR 702.56a + CR 702.174k: the paid Replicate instance and declined
    // Gift are distinct cast decisions.
    assert!(replicate_paid && gift_declined);
    let paid = &runner.state().stack_paid_facts[&spell];
    assert!(paid.additional_cost_paid);
    assert_eq!(paid.additional_cost_payments.len(), 1);
    let payment = &paid.additional_cost_payments[0];
    assert_eq!(
        (payment.origin, payment.grant_source, payment.count),
        (AdditionalCostOrigin::Replicate, Some(djinn), 1)
    );
    for _ in 0..20 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } | WaitingFor::OrderTriggers { .. } => {
                GameAction::PassPriority
            }
            WaitingFor::CopyRetarget { .. } => GameAction::KeepAllCopyTargets,
            other => panic!("unexpected replicated Gift resolution prompt: {other:?}"),
        };
        events.extend(runner.act(action).unwrap().events);
    }
    assert!(events
        .iter()
        .any(|event| matches!(event, GameEvent::SpellCopied { .. })));
}

#[test]
fn coiling_rebirth_gift_rider_reads_gift_payment_instead_of_paid_replicate() {
    for promised in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let target = scenario
            .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
            .id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Coiling Rebirth", false, COILING_REBIRTH)
            .with_mana_cost(ManaCost::Cost {
                generic: 3,
                shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            })
            .id();
        let djinn = (!promised).then(|| {
            scenario
                .add_creature_from_oracle(P0, "Djinn Illuminatus", 3, 5, DJINN)
                .id()
        });
        let gift = scenario.add_card_to_library_top(P1, "Plains");
        fund(&mut scenario, &[(ManaType::Black, 10)]);
        let mut runner = scenario.build();
        let mut events = Vec::new();
        if promised {
            assert_eq!(
                cast_promised_coiling(&mut runner, spell, target, &mut events),
                GiftResolution::Resolved
            );
        } else {
            cast_with_replicate_gift_declined(
                &mut runner,
                spell,
                target,
                djinn.expect("the test grants replicate"),
                &mut events,
            );
        }
        // CR 608.2b: the first resolving spell returns the shared target and
        // the other spell's target becomes illegal.
        assert_eq!(object_zone(&runner, target), Zone::Battlefield);
        assert_eq!(
            object_zone(&runner, gift),
            if promised { Zone::Hand } else { Zone::Library }
        );
        let tokens = runner
            .state()
            .battlefield
            .iter()
            .filter(|id| runner.state().objects[id].is_token)
            .count();
        assert_eq!(
            tokens,
            usize::from(promised),
            "only the Gift payment enables the token rider"
        );
        assert_settled_by_the_answer(&runner);
    }
}

#[test]
fn nocturnal_hunger_negates_gift_independently_of_paid_replicate() {
    const NOCTURNAL_HUNGER: &str = "Gift a Food (You may promise an opponent a gift as you cast \
this spell. If you do, they create a Food token before its other effects. It's an artifact with \
\"{2}, {T}, Sacrifice this token: You gain 3 life.\")\nDestroy target creature. If the gift \
wasn't promised, you lose 2 life.";
    for promised in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let target = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Nocturnal Hunger", true, NOCTURNAL_HUNGER)
            .with_mana_cost(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Black],
            })
            .id();
        let djinn = (!promised).then(|| {
            scenario
                .add_creature_from_oracle(P0, "Djinn Illuminatus", 3, 5, DJINN)
                .id()
        });
        fund(&mut scenario, &[(ManaType::Black, 6)]);
        let mut runner = scenario.build();

        if promised {
            let outcome = runner
                .cast(spell)
                .accept_optional()
                .target_object(target)
                .resolve();
            assert_eq!(gift_resolved(outcome.events()), 1);
        } else {
            let mut events = Vec::new();
            cast_with_replicate_gift_declined(
                &mut runner,
                spell,
                target,
                djinn.expect("the test grants replicate"),
                &mut events,
            );
            assert_eq!(gift_resolved(&events), 0);
        }
        assert_eq!(object_zone(&runner, target), Zone::Graveyard);
        // CR 702.174d/k + CR 119.3: Gift enables the Food and suppresses the
        // life loss. A Replicate payment alone still causes exactly one loss
        // of 2 life, since the first spell kills the shared target (CR 608.2b).
        assert_eq!(runner.life(P0), if promised { 20 } else { 18 });
        assert_eq!(
            runner
                .state()
                .objects
                .values()
                .filter(|object| {
                    object.zone == Zone::Battlefield
                        && object.controller == P1
                        && object.is_token
                        && object
                            .card_types
                            .subtypes
                            .iter()
                            .any(|subtype| subtype == "Food")
                })
                .count(),
            usize::from(promised)
        );
        assert_settled_by_the_answer(&runner);
    }
}
