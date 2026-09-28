//! Valki, God of Lies (Kaldheim MDFC front face) — runtime coverage.
//!
//! ETB: "When Valki enters, each opponent reveals their hand. For each opponent,
//! exile a creature card they revealed this way until Valki leaves the
//! battlefield."
//! - CR 701.20a: each opponent's hand is revealed.
//! - CR 608.2c + CR 608.2d + CR 109.5: for each opponent, Valki's controller
//!   chooses one creature card that opponent revealed ("exile a creature card"
//!   is an instruction to the controller), inside that opponent's iteration
//!   (CR 101.4 APNAP order).
//! - CR 610.3: the chosen cards return when Valki leaves the battlefield; CR 610.3b:
//!   if Valki left before the ETB resolved, nothing is exiled.
//!
//! {X}: "Choose a creature card exiled with Valki with mana value X. Valki
//! becomes a copy of that card."
//! - CR 607.2a + CR 406.6: the pool is exactly the cards exiled with Valki.
//! - CR 707.2: Valki becomes a copy of the chosen card; CR 707.4: a copying
//!   permanent stays the same object, so the exile links persist.

use engine::game::scenario::{CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, EffectKind};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{ExileLink, ExileLinkKind, WaitingFor};
use engine::types::identifiers::{ObjectId, TrackedSetId};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

/// Verbatim Oracle text of the front face (Scryfall / MTGJSON).
const VALKI_FULL: &str = "When Valki enters, each opponent reveals their hand. For each opponent, exile a creature card they revealed this way until Valki leaves the battlefield.\n{X}: Choose a creature card exiled with Valki with mana value X. Valki becomes a copy of that card.";

/// Verbatim Oracle text (Murder's first sentence class / Unsummon).
const DESTROY_TARGET_CREATURE: &str = "Destroy target creature.";
const RETURN_TARGET_CREATURE: &str = "Return target creature to its owner's hand.";

fn mana(kind: ManaType) -> ManaUnit {
    ManaUnit::new(kind, ObjectId(0), false, vec![])
}

fn generic_cost(n: u32) -> ManaCost {
    ManaCost::Cost {
        shards: vec![],
        generic: n,
    }
}

/// Valki in P0's hand: {1}{B} Legendary Creature — God 2/1, verbatim text.
fn add_valki(scenario: &mut GameScenario) -> ObjectId {
    let mut valki =
        scenario.add_creature_to_hand_from_oracle(P0, "Valki, God of Lies", 2, 1, VALKI_FULL);
    valki
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .as_legendary()
        .with_subtypes(vec!["God"]);
    valki.id()
}

fn add_hand_creature(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    mana_value: u32,
    power: i32,
    toughness: i32,
) -> ObjectId {
    let mut card = scenario.add_creature_to_hand(player, name, power, toughness);
    card.with_mana_cost(generic_cost(mana_value));
    card.id()
}

fn new_three_player() -> GameScenario {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, vec![mana(ManaType::Black), mana(ManaType::Black)]);
    scenario
}

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn revealed_players(events: &[GameEvent]) -> Vec<PlayerId> {
    events
        .iter()
        .filter_map(|e| match e {
            GameEvent::CardsRevealed { player, .. } => Some(*player),
            _ => None,
        })
        .collect()
}

/// Pass priority until the stack is empty or a non-priority prompt is parked,
/// collecting every emitted event.
fn settle(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                let result = runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must be accepted");
                events.extend(result.events);
            }
            _ => return,
        }
    }
}

/// Upper bound on the `RevealChoice` prompts one Valki ETB may park. Two
/// opponents give at most two; the headroom still bounds the loop, so an ETB
/// that re-fires (its exile consumer lifted onto Valki itself) fails the test
/// instead of hanging it.
const MAX_ETB_REVEAL_PROMPTS: usize = 4;

/// Answer every `RevealChoice` the ETB parks with `pick`, recording each prompt
/// (chooser + offered cards) in order. Also returns, per prompt, the index into
/// `events` at which the answering `SelectCards` began emitting.
fn answer_reveal_choices_with_marks(
    runner: &mut GameRunner,
    events: &mut Vec<GameEvent>,
    mut pick: impl FnMut(&[ObjectId]) -> ObjectId,
) -> (Vec<(PlayerId, Vec<ObjectId>)>, Vec<usize>) {
    let mut prompts = Vec::new();
    let mut marks = Vec::new();
    settle(runner, events);
    while let WaitingFor::RevealChoice { player, cards, .. } = runner.state().waiting_for.clone() {
        assert!(
            prompts.len() < MAX_ETB_REVEAL_PROMPTS,
            "ETB re-fired: the exile consumer is not bound to the chosen revealed card \
             (trigger-lowering boundary); prompts so far: {prompts:?}"
        );
        let chosen = pick(&cards);
        prompts.push((player, cards));
        marks.push(events.len());
        let result = runner
            .act(GameAction::SelectCards {
                cards: vec![chosen],
            })
            .expect("the reveal choice must accept an offered card");
        events.extend(result.events);
        settle(runner, events);
    }
    (prompts, marks)
}

fn answer_reveal_choices(
    runner: &mut GameRunner,
    events: &mut Vec<GameEvent>,
    pick: impl FnMut(&[ObjectId]) -> ObjectId,
) -> Vec<(PlayerId, Vec<ObjectId>)> {
    answer_reveal_choices_with_marks(runner, events, pick).0
}

fn cast_valki(runner: &mut GameRunner, valki: ObjectId) -> Vec<GameEvent> {
    let outcome: CastOutcome = runner.cast(valki).resolve();
    outcome.events().to_vec()
}

fn cast_spell_targeting(runner: &mut GameRunner, spell: ObjectId, target: ObjectId) {
    let mut events = Vec::new();
    runner.cast(spell).target_object(target).resolve();
    settle(runner, &mut events);
}

fn valki_links(runner: &GameRunner, valki: ObjectId) -> Vec<ExileLink> {
    runner
        .state()
        .exile_links
        .iter()
        .filter(|link| {
            link.source_id == valki && matches!(link.kind, ExileLinkKind::UntilSourceLeaves { .. })
        })
        .cloned()
        .collect()
}

fn choose_ability_index(runner: &GameRunner, valki: ObjectId) -> usize {
    runner.state().objects[&valki]
        .abilities
        .iter()
        .position(|a| matches!(*a.effect, Effect::ChooseFromZone { .. }))
        .expect("Valki's {X} ability must be a ChooseFromZone")
}

// ── Row 1: ETB ─────────────────────────────────────────────────────────────

/// CR 701.20a + CR 608.2c + CR 608.2d + CR 109.5 + CR 101.4 + CR 610.3: one
/// controller-chosen creature card per opponent, exiled until Valki leaves.
#[test]
fn valki_etb_exiles_one_chosen_creature_card_per_opponent_until_valki_leaves() {
    let mut scenario = new_three_player();
    let valki = add_valki(&mut scenario);
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Destroy Probe", false, DESTROY_TARGET_CREATURE)
        .id();
    let p0_own = add_hand_creature(&mut scenario, P0, "Own Bear", 2, 2, 2);
    let p1_bear = add_hand_creature(&mut scenario, P1, "Opp Bear", 2, 2, 2);
    let p1_instant = scenario
        .add_spell_to_hand_from_oracle(P1, "Opp Instant", true, "Draw a card.")
        .id();
    let p1_permanent = scenario.add_creature(P1, "Opp Permanent", 3, 3).id();
    let p2_ogre = add_hand_creature(&mut scenario, P2, "Opp Ogre", 3, 3, 3);
    let p2_other = add_hand_creature(&mut scenario, P2, "Opp Goblin", 1, 1, 1);
    let mut runner = scenario.build();

    let mut events = cast_valki(&mut runner, valki);
    let prompts = answer_reveal_choices(&mut runner, &mut events, |cards| {
        if cards.contains(&p1_bear) {
            p1_bear
        } else {
            p2_ogre
        }
    });

    // Exactly one prompt per opponent, APNAP order (P1 then P2), each chosen by
    // Valki's controller and offering only that opponent's creature cards.
    let normalized: Vec<(PlayerId, Vec<ObjectId>)> = prompts
        .iter()
        .map(|(player, cards)| {
            let mut cards = cards.clone();
            cards.sort();
            (*player, cards)
        })
        .collect();
    let mut p2_expected = vec![p2_ogre, p2_other];
    p2_expected.sort();
    assert_eq!(
        normalized,
        vec![(P0, vec![p1_bear]), (P0, p2_expected)],
        "one RevealChoice per opponent, chosen by P0, offering only that opponent's creature cards"
    );
    assert!(prompts
        .iter()
        .all(|(_, cards)| !cards.contains(&p0_own) && !cards.contains(&p1_instant)));
    assert_eq!(revealed_players(&events), vec![P1, P2]);

    assert_eq!(zone(&runner, p1_bear), Zone::Exile);
    assert_eq!(zone(&runner, p2_ogre), Zone::Exile);
    assert_eq!(zone(&runner, p2_other), Zone::Hand);
    assert_eq!(zone(&runner, p1_instant), Zone::Hand);
    assert_eq!(
        zone(&runner, p1_permanent),
        Zone::Battlefield,
        "no battlefield permanent is exiled"
    );
    assert_eq!(zone(&runner, valki), Zone::Battlefield);
    assert_eq!(valki_links(&runner, valki).len(), 2);

    // CR 610.3: Valki leaving the battlefield returns both cards to their
    // owners' hands.
    cast_spell_targeting(&mut runner, destroy, valki);
    assert_eq!(
        zone(&runner, valki),
        Zone::Graveyard,
        "reach-guard: Valki died"
    );
    assert_eq!(zone(&runner, p1_bear), Zone::Hand);
    assert_eq!(zone(&runner, p2_ogre), Zone::Hand);
    assert!(runner.state().players[1].hand.contains(&p1_bear));
    assert!(runner.state().players[2].hand.contains(&p2_ogre));
}

/// The first opponent (APNAP) has no creature card: no prompt, nothing lost,
/// and the other opponent's pick still lands.
#[test]
fn valki_etb_opponent_without_creature_cards_gets_no_prompt() {
    let mut scenario = new_three_player();
    let valki = add_valki(&mut scenario);
    scenario.add_land_to_hand(P1, "Opp Land");
    scenario.add_spell_to_hand_from_oracle(P1, "Opp Instant", true, "Draw a card.");
    let p1_permanent = scenario.add_creature(P1, "Opp Permanent", 3, 3).id();
    let p2_permanent = scenario.add_creature(P2, "Opp Other Permanent", 2, 2).id();
    let p2_ogre = add_hand_creature(&mut scenario, P2, "Opp Ogre", 3, 3, 3);
    let mut runner = scenario.build();
    let p1_hand_before = hand_len(&runner, P1);

    let mut events = cast_valki(&mut runner, valki);
    let prompts = answer_reveal_choices(&mut runner, &mut events, |_| p2_ogre);

    assert_eq!(prompts, vec![(P0, vec![p2_ogre])]);
    assert_eq!(hand_len(&runner, P1), p1_hand_before);
    assert_eq!(zone(&runner, p2_ogre), Zone::Exile, "paired positive");
    assert_eq!(zone(&runner, p1_permanent), Zone::Battlefield);
    assert_eq!(zone(&runner, p2_permanent), Zone::Battlefield);
    assert_eq!(zone(&runner, valki), Zone::Battlefield);
}

/// A later opponent (resumed per-player continuation after P1's paused pick)
/// with no creature card must not reuse the stale chain target holding P1's pick.
#[test]
fn valki_etb_later_opponent_without_creature_cards_after_paused_pick() {
    let mut scenario = new_three_player();
    let valki = add_valki(&mut scenario);
    let p1_bear = add_hand_creature(&mut scenario, P1, "Opp Bear", 2, 2, 2);
    scenario.add_land_to_hand(P2, "Opp Land");
    scenario.add_spell_to_hand_from_oracle(P2, "Opp Instant", true, "Draw a card.");
    let p2_permanent = scenario.add_creature(P2, "Opp Permanent", 3, 3).id();
    let mut runner = scenario.build();
    let p2_hand_before = hand_len(&runner, P2);

    let mut events = cast_valki(&mut runner, valki);
    let prompts = answer_reveal_choices(&mut runner, &mut events, |_| p1_bear);

    assert_eq!(prompts, vec![(P0, vec![p1_bear])]);
    assert!(
        revealed_players(&events).contains(&P2),
        "reach-guard: P2's iteration ran"
    );
    assert_eq!(zone(&runner, p1_bear), Zone::Exile);
    assert_eq!(valki_links(&runner, valki).len(), 1);
    assert_eq!(hand_len(&runner, P2), p2_hand_before);
    assert_eq!(zone(&runner, p2_permanent), Zone::Battlefield);
}

/// Same resumed-leg hazard with a completely empty hand: the later opponent's
/// iteration must not move P1's already-exiled pick again (no stale chain
/// target reuse, CR 608.2c).
#[test]
fn valki_etb_later_opponent_with_empty_hand_after_paused_pick() {
    let mut scenario = new_three_player();
    let valki = add_valki(&mut scenario);
    let p1_bear = add_hand_creature(&mut scenario, P1, "Opp Bear", 2, 2, 2);
    let p2_permanent = scenario.add_creature(P2, "Opp Permanent", 3, 3).id();
    let mut runner = scenario.build();
    assert_eq!(hand_len(&runner, P2), 0, "setup: P2's hand is empty");

    let mut events = cast_valki(&mut runner, valki);
    let (prompts, marks) = answer_reveal_choices_with_marks(&mut runner, &mut events, |_| p1_bear);

    assert_eq!(prompts, vec![(P0, vec![p1_bear])]);
    // Reach-guard: both opponent iterations ran (one Reveal resolution each),
    // so P2's empty-hand iteration was reached.
    let reveal_resolutions = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::Reveal,
                    source_id,
                    ..
                } if *source_id == valki
            )
        })
        .count();
    assert_eq!(
        reveal_resolutions, 2,
        "reach-guard: one reveal per opponent"
    );
    // After P1's pick: exactly one move of the pick (its own exile) and none
    // afterwards.
    let pick_moves: Vec<Zone> = events[marks[0]..]
        .iter()
        .filter_map(|e| match e {
            GameEvent::ZoneChanged { object_id, to, .. } if *object_id == p1_bear => Some(*to),
            _ => None,
        })
        .collect();
    assert_eq!(pick_moves, vec![Zone::Exile]);
    assert_eq!(zone(&runner, p1_bear), Zone::Exile);
    assert_eq!(valki_links(&runner, valki).len(), 1);
    assert_eq!(zone(&runner, p2_permanent), Zone::Battlefield);
    assert_eq!(zone(&runner, valki), Zone::Battlefield);
}

/// CR 610.3b: Valki bounced in response — the hands are still revealed but no
/// card is exiled.
#[test]
fn valki_etb_bounced_in_response_reveals_but_exiles_nothing() {
    let mut scenario = new_three_player();
    let valki = add_valki(&mut scenario);
    let bounce = scenario
        .add_spell_to_hand_from_oracle(P0, "Bounce Probe", true, RETURN_TARGET_CREATURE)
        .id();
    let p1_bear = add_hand_creature(&mut scenario, P1, "Opp Bear", 2, 2, 2);
    let p2_ogre = add_hand_creature(&mut scenario, P2, "Opp Ogre", 3, 3, 3);
    let mut runner = scenario.build();

    let mut events = Vec::new();
    runner.cast(valki).commit();
    // Resolve the creature spell, stopping as soon as its ETB trigger is on the
    // stack. (`resolve_top` waits for the stack to get shorter, which the
    // trigger replacing Valki on the stack never does, so it would pass through
    // the response window.)
    for _ in 0..16 {
        if zone(&runner, valki) == Zone::Battlefield && !runner.state().stack.is_empty() {
            break;
        }
        let result = runner
            .act(GameAction::PassPriority)
            .expect("passing priority must be accepted");
        events.extend(result.events);
    }
    // Every precondition of the response cast window.
    assert_eq!(zone(&runner, valki), Zone::Battlefield);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "P0 holds priority with the ETB trigger on the stack: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(zone(&runner, bounce), Zone::Hand);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "only the ETB trigger is on the stack"
    );
    assert_eq!(runner.state().stack[0].source_id, valki);
    // Respond: bounce Valki, then let the trigger resolve.
    runner.cast(bounce).target_object(valki).commit();
    let result = runner
        .act(GameAction::PassPriority)
        .expect("pass priority on the bounce");
    events.extend(result.events);
    let prompts = answer_reveal_choices(&mut runner, &mut events, |cards| cards[0]);
    let _ = prompts;

    assert_eq!(
        zone(&runner, valki),
        Zone::Hand,
        "reach-guard: Valki was bounced"
    );
    assert!(
        runner.state().stack.is_empty(),
        "reach-guard: the ETB resolved"
    );
    let revealed = revealed_players(&events);
    assert!(revealed.contains(&P1) && revealed.contains(&P2));
    assert_eq!(zone(&runner, p1_bear), Zone::Hand);
    assert_eq!(zone(&runner, p2_ogre), Zone::Hand);
}

// ── Row 2: {X} ─────────────────────────────────────────────────────────────

struct XFixture {
    runner: GameRunner,
    valki: ObjectId,
    p1_bear: ObjectId,
    p2_ogre: ObjectId,
    destroy: ObjectId,
}

/// Cast Valki and exile P1's MV2 "Opp Bear" and P2's MV3 flying "Opp Ogre".
fn valki_with_exiled_mv2_and_mv3(extra: impl FnOnce(&mut GameScenario)) -> XFixture {
    let mut scenario = new_three_player();
    let valki = add_valki(&mut scenario);
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Destroy Probe", false, DESTROY_TARGET_CREATURE)
        .id();
    let p1_bear = add_hand_creature(&mut scenario, P1, "Opp Bear", 2, 2, 2);
    let p2_ogre = {
        let mut ogre = scenario.add_creature_to_hand(P2, "Opp Ogre", 3, 3);
        ogre.with_mana_cost(generic_cost(3)).flying();
        ogre.id()
    };
    extra(&mut scenario);
    let mut runner = scenario.build();
    let mut events = cast_valki(&mut runner, valki);
    answer_reveal_choices(&mut runner, &mut events, |cards| cards[0]);
    assert_eq!(zone(&runner, p1_bear), Zone::Exile, "setup: MV2 exiled");
    assert_eq!(zone(&runner, p2_ogre), Zone::Exile, "setup: MV3 exiled");
    XFixture {
        runner,
        valki,
        p1_bear,
        p2_ogre,
        destroy,
    }
}

fn fund_colorless(runner: &mut GameRunner, amount: usize) {
    for _ in 0..amount {
        let _ = runner
            .state_mut()
            .add_mana_to_pool(P0, mana(ManaType::Colorless));
    }
}

/// Activate {X} and return the offered `ChooseFromZoneChoice` cards, if a
/// prompt was parked.
fn activate_x(runner: &mut GameRunner, valki: ObjectId, x: u32) -> Option<Vec<ObjectId>> {
    fund_colorless(runner, x as usize);
    let index = choose_ability_index(runner, valki);
    runner.activate(valki, index).x(x).resolve();
    let mut events = Vec::new();
    settle(runner, &mut events);
    match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneChoice { cards, .. } => Some(cards.clone()),
        _ => None,
    }
}

/// CR 607.2a + CR 707.2 + CR 707.4 + CR 610.3: X=3 offers only the linked MV3
/// creature card; Valki becomes a copy of it; the exile links persist.
#[test]
fn valki_x_becomes_copy_of_linked_exiled_card_with_mana_value_x() {
    let XFixture {
        mut runner,
        valki,
        p1_bear,
        p2_ogre,
        destroy,
    } = valki_with_exiled_mv2_and_mv3(|_| {});

    let cards = activate_x(&mut runner, valki, 3).expect("X=3 must prompt");
    assert_eq!(cards, vec![p2_ogre], "only the linked MV3 creature card");
    runner
        .act(GameAction::SelectCards {
            cards: vec![p2_ogre],
        })
        .expect("choose the MV3 card");
    let mut events = Vec::new();
    settle(&mut runner, &mut events);

    let copy = &runner.state().objects[&valki];
    assert_eq!(copy.name, "Opp Ogre");
    assert_eq!(copy.power, Some(3));
    assert_eq!(copy.toughness, Some(3));
    assert!(copy.keywords.contains(&Keyword::Flying));
    assert!(
        !copy
            .abilities
            .iter()
            .any(|a| matches!(*a.effect, Effect::ChooseFromZone { .. })),
        "CR 707.2: the copy has the Ogre's abilities, not Valki's {{X}}"
    );
    assert_eq!(
        zone(&runner, p2_ogre),
        Zone::Exile,
        "the card stays in exile"
    );
    assert_eq!(zone(&runner, valki), Zone::Battlefield);
    // CR 707.4 + A8: same object — both links persist.
    let linked: Vec<_> = valki_links(&runner, valki)
        .iter()
        .map(|l| l.exiled_id)
        .collect();
    assert!(linked.contains(&p1_bear) && linked.contains(&p2_ogre));

    // CR 610.3: destroying the copied Valki returns both cards.
    cast_spell_targeting(&mut runner, destroy, valki);
    assert_eq!(zone(&runner, valki), Zone::Graveyard);
    assert_eq!(zone(&runner, p1_bear), Zone::Hand);
    assert_eq!(zone(&runner, p2_ogre), Zone::Hand);
}

/// No linked creature card with mana value X: nothing happens.
#[test]
fn valki_x_with_no_matching_card_does_nothing() {
    let XFixture {
        mut runner, valki, ..
    } = valki_with_exiled_mv2_and_mv3(|_| {});

    assert_eq!(activate_x(&mut runner, valki, 5), None);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    let obj = &runner.state().objects[&valki];
    assert_eq!(obj.name, "Valki, God of Lies");
    assert_eq!(obj.power, Some(2));
    assert_eq!(obj.toughness, Some(1));
    // Reach-guard: the same activation with X=3 does prompt.
    assert!(activate_x(&mut runner, valki, 3).is_some());
}

/// CR 607.2a: an MV3 creature card exiled by another source is not offered.
#[test]
fn valki_x_ignores_mv_x_card_exiled_by_another_source() {
    let mut foreign = None;
    let mut other_source = None;
    let XFixture {
        mut runner,
        valki,
        p2_ogre,
        ..
    } = valki_with_exiled_mv2_and_mv3(|scenario| {
        let mut card = scenario.add_creature_to_exile(P1, "Foreign Beast", 3, 3);
        card.with_mana_cost(generic_cost(3));
        foreign = Some(card.id());
        other_source = Some(scenario.add_creature(P1, "Other Exiler", 1, 1).id());
    });
    let (foreign, other_source) = (foreign.unwrap(), other_source.unwrap());
    runner.state_mut().exile_links.push(ExileLink {
        exiled_id: foreign,
        source_id: other_source,
        kind: ExileLinkKind::TrackedBySource,
    });

    let cards = activate_x(&mut runner, valki, 3).expect("X=3 must prompt");
    assert!(!cards.contains(&foreign));
    assert_eq!(cards, vec![p2_ogre], "paired positive");
}

/// A stale, newer tracked set holding an unrelated exiled card does not hide
/// the Valki-linked card (the pool is defined by linkage, CR 607.2a / 406.6).
#[test]
fn valki_x_stale_tracked_set_does_not_hide_linked_card() {
    let mut unrelated = None;
    let XFixture {
        mut runner,
        valki,
        p2_ogre,
        ..
    } = valki_with_exiled_mv2_and_mv3(|scenario| {
        let mut card = scenario.add_creature_to_exile(P1, "Unrelated Beast", 3, 3);
        card.with_mana_cost(generic_cost(3));
        unrelated = Some(card.id());
    });
    let unrelated = unrelated.unwrap();
    {
        let state = runner.state_mut();
        let id = TrackedSetId(state.next_tracked_set_id);
        state.next_tracked_set_id += 1;
        state.tracked_object_sets.insert(id, vec![unrelated]);
    }

    let cards = activate_x(&mut runner, valki, 3).expect("a prompt must be parked");
    assert!(cards.contains(&p2_ogre));
    assert!(!cards.contains(&unrelated));
}

// ── Row 4: the exported card data carries the same shapes ──────────────────

/// Every node of a chain, following `sub_ability` / `else_ability`.
fn chain_effects(def: &engine::types::ability::AbilityDefinition) -> Vec<&Effect> {
    let mut effects = vec![&*def.effect];
    if let Some(sub) = def.sub_ability.as_deref() {
        effects.extend(chain_effects(sub));
    }
    if let Some(branch) = def.else_ability.as_deref() {
        effects.extend(chain_effects(branch));
    }
    effects
}

/// The real card-data export of Valki's face parses both abilities fully: the
/// per-opponent reveal choice bound to a co-scoped `ParentTarget` exile (rows
/// 1f/1n) and the linked-pile `{X}` choose (row 2e).
#[test]
fn valki_real_card_export_has_no_unimplemented() {
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::ability::{PlayerFilter, TargetFilter, ZoneChoiceCandidateSource};

    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    let valki = scenario.add_real_card(P0, "Valki, God of Lies", Zone::Hand, db);
    let runner = scenario.build();
    let object = &runner.state().objects[&valki];

    let trigger = object
        .trigger_definitions
        .first()
        .expect("Valki's ETB trigger");
    let etb = trigger.definition.execute.as_deref().expect("ETB execute");
    assert!(chain_effects(etb)
        .iter()
        .all(|e| !matches!(e, Effect::Unimplemented { .. })));
    assert!(matches!(
        &*etb.effect,
        Effect::RevealHand {
            target: TargetFilter::Controller,
            card_filter: TargetFilter::Typed(_),
            ..
        }
    ));
    assert_eq!(etb.player_scope, Some(PlayerFilter::Opponent));
    let exile = etb.sub_ability.as_deref().expect("exile consumer");
    assert!(matches!(
        &*exile.effect,
        Effect::ChangeZone {
            destination: Zone::Exile,
            target: TargetFilter::ParentTarget,
            ..
        }
    ));
    assert_eq!(exile.player_scope, Some(PlayerFilter::Opponent));

    let x = object
        .abilities
        .iter()
        .find(|a| matches!(*a.effect, Effect::ChooseFromZone { .. }))
        .expect("Valki's {X} ability");
    assert!(chain_effects(x)
        .iter()
        .all(|e| !matches!(e, Effect::Unimplemented { .. })));
    assert!(matches!(
        &*x.effect,
        Effect::ChooseFromZone {
            zone: Zone::Exile,
            candidate_source: ZoneChoiceCandidateSource::Direct,
            ..
        }
    ));
}
