//! Valki, God of Lies // Tibalt, Cosmic Impostor (Kaldheim MDFC) — runtime coverage.
//!
//! ETB: "When Valki enters, each opponent reveals their hand. For each opponent,
//! exile a creature card they revealed this way until Valki leaves the
//! battlefield."
//! - CR 701.20a: each opponent's hand is revealed.
//! - CR 608.2c: that instruction completes for every opponent before the next
//!   begins; then, for each opponent (CR 101.4 APNAP order), Valki's controller
//!   chooses one creature card that opponent revealed ("exile a creature card"
//!   is an instruction to the controller, CR 608.2d + CR 109.5) and exiles it.
//! - CR 610.3: the chosen cards return when Valki leaves the battlefield; CR 610.3b:
//!   if Valki left before the ETB resolved, nothing is exiled.
//!
//! {X}: "Choose a creature card exiled with Valki with mana value X. Valki
//! becomes a copy of that card."
//! - CR 607.2a + CR 406.6: the pool is exactly the cards exiled with Valki.
//! - CR 707.2: Valki becomes a copy of the chosen card; CR 400.7: becoming a
//!   copy is not a zone change, so Valki stays the same object and the exile links persist.
//!
//! The "CR 610.3b: chain hand-off class regressions" section pins the fix Valki's
//! bounced-in-response test depends on: the "until" latch recorded on the exile
//! node below a non-ChangeZone parent survives the parent→child hand-off
//! (Kitesail Freebooter's hand reveal, Shire Shirriff's reflexive trigger).

use engine::game::scenario::{CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{ExileLink, ExileLinkKind, StackEntryKind, WaitingFor};
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

/// Resolve a just-committed permanent spell, stopping as soon as `source` is on
/// the battlefield with its ETB trigger on the stack — the response window.
/// (`resolve_top` waits for the stack to get shorter, which the trigger
/// replacing the spell on the stack never does, so it would pass through that
/// window.)
fn pass_until_etb_on_stack(runner: &mut GameRunner, source: ObjectId) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..16 {
        if zone(runner, source) == Zone::Battlefield && !runner.state().stack.is_empty() {
            break;
        }
        let result = runner
            .act(GameAction::PassPriority)
            .expect("passing priority must be accepted");
        events.extend(result.events);
    }
    events
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

fn until_leaves_links(runner: &GameRunner, source: ObjectId) -> Vec<ExileLink> {
    runner
        .state()
        .exile_links
        .iter()
        .filter(|link| {
            link.source_id == source && matches!(link.kind, ExileLinkKind::UntilSourceLeaves { .. })
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

// ── ETB ─────────────────────────────────────────────────────────────

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
    assert_eq!(until_leaves_links(&runner, valki).len(), 2);

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

/// CR 608.2c + CR 701.20a: "Each opponent reveals their hand" completes for every
/// opponent before "For each opponent, exile …" begins — no card choice is
/// offered until every opponent has revealed, and the choice pass reveals
/// nothing again.
#[test]
fn valki_etb_reveals_every_opponents_hand_before_the_first_choice() {
    let mut scenario = new_three_player();
    let valki = add_valki(&mut scenario);
    let p1_bear = add_hand_creature(&mut scenario, P1, "Opp Bear", 2, 2, 2);
    let p2_ogre = add_hand_creature(&mut scenario, P2, "Opp Ogre", 3, 3, 3);
    let mut runner = scenario.build();

    let mut events = cast_valki(&mut runner, valki);
    let (prompts, marks) =
        answer_reveal_choices_with_marks(&mut runner, &mut events, |cards| cards[0]);

    // Reach-guards: one prompt per opponent in APNAP order, chosen by Valki's
    // controller from that opponent's creature cards, and both picks landed.
    assert_eq!(prompts, vec![(P0, vec![p1_bear]), (P0, vec![p2_ogre])]);
    assert_eq!(zone(&runner, p1_bear), Zone::Exile);
    assert_eq!(zone(&runner, p2_ogre), Zone::Exile);
    assert_eq!(until_leaves_links(&runner, valki).len(), 2);

    // Every opponent's hand was revealed before the first choice was answered.
    assert_eq!(
        revealed_players(&events[..marks[0]]),
        vec![P1, P2],
        "both hands are revealed before the first card choice"
    );
    // No hand is revealed a second time over the whole resolution.
    assert_eq!(revealed_players(&events), vec![P1, P2]);
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
    let (prompts, marks) = answer_reveal_choices_with_marks(&mut runner, &mut events, |_| p1_bear);

    assert_eq!(prompts, vec![(P0, vec![p1_bear])]);
    assert!(
        revealed_players(&events).contains(&P2),
        "reach-guard: P2's iteration ran"
    );
    assert!(
        events[marks[0]..].iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Reveal,
                source_id,
                ..
            } if *source_id == valki
        )),
        "reach-guard: P2's choice-pass iteration ran after P1's pick"
    );
    assert_eq!(zone(&runner, p1_bear), Zone::Exile);
    assert_eq!(until_leaves_links(&runner, valki).len(), 1);
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
    // Reach-guard: both passes ran for both opponents (one reveal-pass and one
    // choice-pass Reveal resolution each), so P2's empty-hand choice was reached.
    let is_valki_reveal_resolution = |e: &GameEvent| {
        matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Reveal,
                source_id,
                ..
            } if *source_id == valki
        )
    };
    assert_eq!(
        events
            .iter()
            .filter(|e| is_valki_reveal_resolution(e))
            .count(),
        4,
        "reach-guard: one reveal-pass and one choice-pass resolution per opponent"
    );
    assert!(
        events[marks[0]..].iter().any(is_valki_reveal_resolution),
        "reach-guard: P2's choice-pass iteration ran after P1's pick"
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
    assert_eq!(until_leaves_links(&runner, valki).len(), 1);
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

    runner.cast(valki).commit();
    let mut events = pass_until_etb_on_stack(&mut runner, valki);
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
    assert!(
        !prompts.is_empty(),
        "reach-guard: the exile choice was reached, so the no-move is the CR 610.3b guard"
    );

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
    assert!(until_leaves_links(&runner, valki).is_empty());
}

// ── CR 610.3b: chain hand-off class regressions (Kitesail Freebooter, Shire Shirriff) ──

/// Verbatim Oracle text (Scryfall / MTGJSON).
const KITESAIL_FULL: &str = "Flying\nWhen this creature enters, target opponent reveals their hand. You choose a noncreature, nonland card from it. Exile that card until this creature leaves the battlefield.";
/// Verbatim Oracle text (Scryfall / MTGJSON).
const SHIRRIFF_FULL: &str = "Vigilance\nWhen this creature enters, you may sacrifice a token. When you do, exile target creature an opponent controls until this creature leaves the battlefield.";

struct FreebooterFixture {
    runner: GameRunner,
    freebooter: ObjectId,
    bounce: ObjectId,
    p1_divination: ObjectId,
    p1_bear: ObjectId,
}

/// Two players; Kitesail Freebooter ({1}{B} Bird Pirate 1/2, verbatim text) in
/// P0's hand with the mana for it, a bounce instant in P0's hand, and one
/// noncreature nonland card plus one creature card in P1's hand. Freebooter is
/// cast and its ETB trigger (targeting P1) is left on the stack with P0 holding
/// priority.
fn freebooter_etb_on_stack() -> (FreebooterFixture, Vec<GameEvent>) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, vec![mana(ManaType::Black), mana(ManaType::Black)]);
    let freebooter = {
        let mut card = scenario.add_creature_to_hand_from_oracle(
            P0,
            "Kitesail Freebooter",
            1,
            2,
            KITESAIL_FULL,
        );
        card.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .with_subtypes(vec!["Bird", "Pirate"]);
        card.id()
    };
    let bounce = scenario
        .add_spell_to_hand_from_oracle(P0, "Bounce Probe", true, RETURN_TARGET_CREATURE)
        .id();
    let p1_divination = {
        let mut card =
            scenario.add_spell_to_hand_from_oracle(P1, "Opp Divination", false, "Draw two cards.");
        card.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        });
        card.id()
    };
    let p1_bear = add_hand_creature(&mut scenario, P1, "Opp Bear", 2, 2, 2);
    let mut runner = scenario.build();

    runner.cast(freebooter).target_player(P1).commit();
    let events = pass_until_etb_on_stack(&mut runner, freebooter);
    assert_response_window(&runner, freebooter);
    (
        FreebooterFixture {
            runner,
            freebooter,
            bounce,
            p1_divination,
            p1_bear,
        },
        events,
    )
}

/// Every precondition of the response cast window: `source` is on the
/// battlefield, its ETB trigger is the only stack object, and P0 holds priority.
fn assert_response_window(runner: &GameRunner, source: ObjectId) {
    assert_eq!(zone(runner, source), Zone::Battlefield);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "P0 holds priority with the ETB trigger on the stack: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "only the ETB trigger is on the stack"
    );
    assert_eq!(runner.state().stack[0].source_id, source);
}

/// Every exile link, of any kind, whose source is `source`.
fn links_from(runner: &GameRunner, source: ObjectId) -> Vec<ExileLink> {
    runner
        .state()
        .exile_links
        .iter()
        .filter(|link| link.source_id == source)
        .cloned()
        .collect()
}

/// CR 610.3b (Kitesail Freebooter ruling: "If Kitesail Freebooter leaves the
/// battlefield before its enters-the-battlefield ability resolves, the opponent
/// will reveal their hand, but no card will be exiled."): the latch sits on the
/// exile node below the hand-reveal parent, so it must survive the parent→child
/// hand-off.
#[test]
fn kitesail_freebooter_bounced_before_etb_resolves_reveals_but_exiles_nothing() {
    let (fixture, mut events) = freebooter_etb_on_stack();
    let FreebooterFixture {
        mut runner,
        freebooter,
        bounce,
        p1_divination,
        p1_bear,
    } = fixture;

    runner.cast(bounce).target_object(freebooter).commit();
    let result = runner
        .act(GameAction::PassPriority)
        .expect("pass priority on the bounce");
    events.extend(result.events);
    let prompts = answer_reveal_choices(&mut runner, &mut events, |cards| cards[0]);

    assert_eq!(
        zone(&runner, freebooter),
        Zone::Hand,
        "reach-guard: Freebooter was bounced"
    );
    assert!(
        runner.state().stack.is_empty(),
        "reach-guard: the ETB resolved"
    );
    assert!(
        revealed_players(&events).contains(&P1),
        "reach-guard: P1's hand is still revealed"
    );
    assert_eq!(
        prompts,
        vec![(P0, vec![p1_divination])],
        "exactly the one choice from P1's hand, offering its noncreature, nonland card"
    );
    assert_eq!(zone(&runner, p1_divination), Zone::Hand);
    assert_eq!(zone(&runner, p1_bear), Zone::Hand);
    assert!(
        links_from(&runner, freebooter).is_empty(),
        "no card is exiled with Freebooter"
    );
}

/// CR 701.20a + CR 610.3: positive twin — not bounced, the chosen noncreature,
/// nonland card is exiled until Freebooter leaves, then returns.
#[test]
fn kitesail_freebooter_etb_exiles_chosen_card_until_it_leaves() {
    let (fixture, mut events) = freebooter_etb_on_stack();
    let FreebooterFixture {
        mut runner,
        freebooter,
        bounce,
        p1_divination,
        p1_bear,
    } = fixture;

    let prompts = answer_reveal_choices(&mut runner, &mut events, |cards| cards[0]);
    assert_eq!(
        prompts,
        vec![(P0, vec![p1_divination])],
        "P0 chooses from P1's noncreature, nonland cards only"
    );
    assert!(revealed_players(&events).contains(&P1));
    assert_eq!(zone(&runner, p1_divination), Zone::Exile);
    assert_eq!(zone(&runner, p1_bear), Zone::Hand);
    assert_eq!(until_leaves_links(&runner, freebooter).len(), 1);

    cast_spell_targeting(&mut runner, bounce, freebooter);
    assert_eq!(zone(&runner, freebooter), Zone::Hand);
    assert_eq!(
        zone(&runner, p1_divination),
        Zone::Hand,
        "CR 610.3: the card returns when Freebooter leaves"
    );
}

struct ShirriffFixture {
    runner: GameRunner,
    shirriff: ObjectId,
    bounce: ObjectId,
    p0_token: ObjectId,
    p1_bear: ObjectId,
}

/// Two players; Shire Shirriff ({1}{W} Halfling Soldier 2/2, verbatim text) in
/// P0's hand with the mana for it, a bounce instant in P0's hand, exactly one
/// token creature under P0's control and one creature under P1's control.
/// Shirriff is cast and its ETB trigger is left on the stack with P0 holding
/// priority.
fn shirriff_etb_on_stack() -> (ShirriffFixture, Vec<GameEvent>) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, vec![mana(ManaType::White), mana(ManaType::White)]);
    let shirriff = {
        let mut card =
            scenario.add_creature_to_hand_from_oracle(P0, "Shire Shirriff", 2, 2, SHIRRIFF_FULL);
        card.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 1,
        })
        .with_subtypes(vec!["Halfling", "Soldier"]);
        card.id()
    };
    let bounce = scenario
        .add_spell_to_hand_from_oracle(P0, "Bounce Probe", true, RETURN_TARGET_CREATURE)
        .id();
    let p0_token = scenario.add_creature(P0, "Soldier Token", 1, 1).id();
    let p1_bear = scenario.add_creature(P1, "Opp Bear", 2, 2).id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&p0_token)
        .unwrap()
        .is_token = true;

    runner.cast(shirriff).commit();
    let events = pass_until_etb_on_stack(&mut runner, shirriff);
    assert_response_window(&runner, shirriff);
    (
        ShirriffFixture {
            runner,
            shirriff,
            bounce,
            p0_token,
            p1_bear,
        },
        events,
    )
}

/// `id` is no longer on the battlefield (a sacrificed token ceases to exist,
/// CR 111.7, so it may be gone from the object map entirely).
fn left_battlefield(runner: &GameRunner, id: ObjectId) -> bool {
    runner
        .state()
        .objects
        .get(&id)
        .is_none_or(|object| object.zone != Zone::Battlefield)
}

/// What driving Shirriff's ETB to an empty stack observed.
struct ShirriffDrive {
    /// Kinds of the prompts answered, in order.
    prompts: Vec<&'static str>,
    /// A Shirriff-sourced stack object targeting `victim` was seen after the
    /// ETB resolved (the CR 603.12 reflexive trigger).
    reflexive_targeted_victim: bool,
}

/// Drive Shirriff's ETB and the reflexive trigger it creates to an empty stack:
/// accept "you may sacrifice a token", sacrifice `token`, and target `victim`.
fn drive_shirriff_etb(
    runner: &mut GameRunner,
    events: &mut Vec<GameEvent>,
    shirriff: ObjectId,
    token: ObjectId,
    victim: ObjectId,
) -> ShirriffDrive {
    let mut drive = ShirriffDrive {
        prompts: Vec::new(),
        reflexive_targeted_victim: false,
    };
    for _ in 0..32 {
        drive.reflexive_targeted_victim |= runner.state().stack.iter().any(|entry| {
            entry.source_id == shirriff
                && matches!(
                    &entry.kind,
                    StackEntryKind::TriggeredAbility { ability, .. }
                        if ability.targets.contains(&TargetRef::Object(victim))
                )
        });
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return drive,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OptionalEffectChoice { .. } => {
                drive.prompts.push("OptionalEffectChoice");
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                drive.prompts.push("TriggerTargetSelection");
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(victim)),
                }
            }
            other => panic!(
                "unexpected prompt while driving Shirriff's ETB (token {token:?}): {other:?}"
            ),
        };
        let result = runner
            .act(action)
            .expect("the Shirriff ETB prompt must accept the answer");
        events.extend(result.events);
    }
    panic!(
        "Shirriff's ETB did not settle: {:?}",
        runner.state().waiting_for
    );
}

/// CR 610.3b + CR 603.12 (Shire Shirriff ruling: "If Shire Shirriff leaves the
/// battlefield before its last ability resolves, you can still sacrifice a
/// token. However, when you do, the target creature won't be exiled when the
/// "reflexive" triggered ability resolves."): the latch recorded on the
/// reflexive exile node rides the reflexive trigger's hand-off onto its own
/// stack object.
#[test]
fn shire_shirriff_bounced_before_etb_resolves_sacrifices_but_exiles_nothing() {
    let (fixture, mut events) = shirriff_etb_on_stack();
    let ShirriffFixture {
        mut runner,
        shirriff,
        bounce,
        p0_token,
        p1_bear,
    } = fixture;

    runner.cast(bounce).target_object(shirriff).commit();
    let result = runner
        .act(GameAction::PassPriority)
        .expect("pass priority on the bounce");
    events.extend(result.events);
    let drive = drive_shirriff_etb(&mut runner, &mut events, shirriff, p0_token, p1_bear);
    assert_eq!(
        drive.prompts,
        ["OptionalEffectChoice", "TriggerTargetSelection"],
        "reach-guard: the \"you may sacrifice\" was accepted and the reflexive target chosen"
    );

    assert_eq!(
        zone(&runner, shirriff),
        Zone::Hand,
        "reach-guard: Shirriff was bounced"
    );
    assert!(
        left_battlefield(&runner, p0_token),
        "reach-guard: the token was still sacrificed"
    );
    assert!(
        drive.reflexive_targeted_victim,
        "reach-guard: the reflexive trigger went on the stack targeting P1's creature (prompts {:?})",
        drive.prompts
    );
    assert!(
        runner.state().stack.is_empty(),
        "reach-guard: everything resolved"
    );
    assert_eq!(zone(&runner, p1_bear), Zone::Battlefield);
    assert!(
        links_from(&runner, shirriff).is_empty(),
        "no creature is exiled with Shirriff"
    );
}

/// CR 603.12 + CR 610.3: positive twin — not bounced, the reflexive trigger
/// exiles the target until Shirriff leaves, then it returns.
#[test]
fn shire_shirriff_etb_sacrifice_exiles_target_until_it_leaves() {
    let (fixture, mut events) = shirriff_etb_on_stack();
    let ShirriffFixture {
        mut runner,
        shirriff,
        bounce,
        p0_token,
        p1_bear,
    } = fixture;

    let drive = drive_shirriff_etb(&mut runner, &mut events, shirriff, p0_token, p1_bear);
    assert_eq!(
        drive.prompts,
        ["OptionalEffectChoice", "TriggerTargetSelection"],
        "reach-guard: the \"you may sacrifice\" was accepted and the reflexive target chosen"
    );
    assert!(
        left_battlefield(&runner, p0_token),
        "the token was sacrificed"
    );
    assert!(
        drive.reflexive_targeted_victim,
        "prompts {:?}",
        drive.prompts
    );
    assert_eq!(zone(&runner, p1_bear), Zone::Exile);
    assert_eq!(until_leaves_links(&runner, shirriff).len(), 1);

    cast_spell_targeting(&mut runner, bounce, shirriff);
    assert_eq!(zone(&runner, shirriff), Zone::Hand);
    assert_eq!(
        zone(&runner, p1_bear),
        Zone::Battlefield,
        "CR 610.3: the creature returns when Shirriff leaves"
    );
}

// ── {X} ─────────────────────────────────────────────────────────────

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

/// CR 607.2a + CR 707.2 + CR 610.3: X=3 offers only the linked MV3
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
    // CR 400.7: becoming a copy is not a zone change — same object, so both links persist.
    let linked: Vec<_> = until_leaves_links(&runner, valki)
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

// ── The exported card data carries the same shapes ──────────────────

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
/// reveal of every opponent's hand, the co-scoped per-opponent card choice whose
/// `ParentTarget` exile consumes it, and the linked-pile `{X}` choose.
#[test]
fn valki_real_card_export_has_no_unimplemented() {
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::ability::{PlayerFilter, TargetFilter, ZoneChoiceCandidateSource};

    let db = fixture_db();
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
            card_filter: TargetFilter::None,
            ..
        }
    ));
    assert_eq!(etb.player_scope, Some(PlayerFilter::Opponent));
    let choice = etb.sub_ability.as_deref().expect("choice step");
    assert!(matches!(
        &*choice.effect,
        Effect::RevealHand {
            target: TargetFilter::Controller,
            card_filter: TargetFilter::Typed(_),
            reveal: false,
            ..
        }
    ));
    assert_eq!(choice.player_scope, Some(PlayerFilter::Opponent));
    let exile = choice.sub_ability.as_deref().expect("exile consumer");
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

// ── Tibalt, Cosmic Impostor ─────────────────────────────────────────
//
// "As Tibalt enters, you get an emblem with "You may play cards exiled with
// Tibalt, Cosmic Impostor, and you may spend mana as though it were mana of any
// color to cast those spells.""
// - CR 614.1c + CR 603.6d: an "As [this permanent] enters" instruction is a
//   replacement effect that happens as part of the entering event (no stack).
// - CR 114.2 + CR 114.4: "you get an emblem" puts an emblem owned and
//   controlled by that player into the command zone; its abilities function
//   there.
// - CR 607.1d + CR 607.2a + CR 400.7: the emblem's "cards exiled with Tibalt"
//   are the cards exiled by that Tibalt object, even after it leaves.
// - CR 609.4b: the any-color spend concession applies to those spells only.

/// Verbatim Oracle text of the back face (Scryfall / MTGJSON).
const TIBALT_FULL: &str = "As Tibalt enters, you get an emblem with \"You may play cards exiled with Tibalt, Cosmic Impostor, and you may spend mana as though it were mana of any color to cast those spells.\"\n[+2]: Exile the top card of each player's library.\n[−3]: Exile target artifact or creature.\n[−8]: Exile all graveyards. Add {R}{R}{R}.";

/// Verbatim Oracle texts of helper cards (Murder-class removal extended to
/// planeswalkers; Unsummon-class bounce; Swords-class exile).
const DESTROY_CREATURE_OR_PLANESWALKER: &str = "Destroy target creature or planeswalker.";
const RETURN_TARGET_PERMANENT: &str = "Return target permanent to its owner's hand.";
const EXILE_TARGET_CREATURE: &str = "Exile target creature.";

const TIBALT_PLUS_TWO: usize = 0;
const TIBALT_MINUS_THREE: usize = 1;
const TIBALT_MINUS_EIGHT: usize = 2;

fn fixture_db() -> &'static engine::database::card_db::CardDatabase {
    crate::support::shared_card_db().expect("integration fixture must load")
}

fn shards(shards: &[ManaCostShard], generic: u32) -> ManaCost {
    ManaCost::Cost {
        shards: shards.to_vec(),
        generic,
    }
}

/// Mana for Tibalt's {5}{B}{R}.
fn tibalt_mana() -> Vec<ManaUnit> {
    let mut pool = vec![mana(ManaType::Black), mana(ManaType::Red)];
    pool.extend((0..5).map(|_| mana(ManaType::Colorless)));
    pool
}

/// Three-player main phase with the real Valki // Tibalt card in `owner`'s hand
/// and `setup` applied; the card database is rehydrated so the modal back face
/// is castable (CR 712.11b).
fn tibalt_scenario(
    owner: PlayerId,
    setup: impl FnOnce(&mut GameScenario),
) -> (GameRunner, ObjectId) {
    use engine::game::scenario_db::GameScenarioDbExt;

    let db = fixture_db();
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let card = scenario.add_real_card(owner, "Valki, God of Lies", Zone::Hand, db);
    scenario.with_mana_pool(owner, tibalt_mana());
    setup(&mut scenario);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    (runner, card)
}

/// CR 712.11b: cast the modal back face.
fn cast_tibalt(runner: &mut GameRunner, card: ObjectId) -> CastOutcome {
    runner.cast(card).modal_back_face(true).resolve()
}

fn emblems_of(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    let state = runner.state();
    state
        .command_zone
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|obj| obj.is_emblem && obj.owner == player)
        })
        .collect()
}

fn the_emblem_of(runner: &GameRunner, player: PlayerId) -> ObjectId {
    let emblems = emblems_of(runner, player);
    assert_eq!(emblems.len(), 1, "exactly one emblem for {player:?}");
    emblems[0]
}

fn linked(runner: &GameRunner, exiled: ObjectId, source: ObjectId) -> bool {
    runner
        .state()
        .exile_links
        .iter()
        .any(|link| link.exiled_id == exiled && link.source_id == source)
}

fn can_cast(runner: &GameRunner, player: PlayerId, card: ObjectId) -> bool {
    engine::game::casting::can_cast_object_now(runner.state(), player, card)
}

fn playable_exiled_lands(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    engine::game::casting::exile_lands_playable_by_permission(runner.state(), player)
        .into_iter()
        .map(|(land, _)| land)
        .collect()
}

fn add_pool(runner: &mut GameRunner, player: PlayerId, kinds: &[ManaType]) {
    for &kind in kinds {
        let _ = runner.state_mut().add_mana_to_pool(player, mana(kind));
    }
}

fn pool_count(runner: &GameRunner, player: PlayerId, kind: ManaType) -> usize {
    runner.state().players[player.0 as usize]
        .mana_pool
        .mana
        .iter()
        .filter(|unit| unit.color == kind)
        .count()
}

fn put_on_library_top(runner: &mut GameRunner, id: ObjectId, owner: PlayerId) {
    let state = runner.state_mut();
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(state, id, Zone::Library, &mut events);
    let player = state.players.iter_mut().find(|p| p.id == owner).unwrap();
    player.library.retain(|card| *card != id);
    player.library.insert(0, id);
}

/// An instant card of cost {2} in `player`'s hand (verbatim "Draw a card.").
fn add_instant(scenario: &mut GameScenario, player: PlayerId, name: &str) -> ObjectId {
    let mut card = scenario.add_spell_to_hand_from_oracle(player, name, true, "Draw a card.");
    card.with_mana_cost(generic_cost(2));
    card.id()
}

/// Hand the priority window of a main phase to `player` (a new turn of theirs).
fn begin_main_phase_of(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.active_player = player;
    state.priority_player = player;
    state.phase = Phase::PreCombatMain;
    state.turn_number += 1;
    state.waiting_for = WaitingFor::Priority { player };
}

/// Activate one of Tibalt's loyalty abilities and resolve it.
fn activate_tibalt(runner: &mut GameRunner, tibalt: ObjectId, index: usize) {
    runner.activate(tibalt, index).resolve();
}

/// CR 614.1c + CR 603.6d + CR 114.2 + CR 306.5b + CR 607.1d: casting the back
/// face creates, as it enters, exactly one emblem owned and controlled by its
/// caster, hosting the persistent any-color exile-play permission, and latched
/// to the Tibalt object that entered. Tibalt enters with 5 loyalty.
#[test]
fn tibalt_back_face_cast_creates_one_emblem_owned_by_caster() {
    use engine::game::game_object::LinkedAbilitySource;
    use engine::types::ability::CharacteristicSetRef;
    use engine::types::identifiers::ObjectIncarnationRef;
    use engine::types::statics::{
        CastFrequency, ExileCardPool, ExileCastCost, ExileCastGrantee, ExileCastTiming, StaticMode,
    };

    // Verbatim Oracle text: the fixture's back face is the card under test.
    let face = fixture_db()
        .get_face_by_name("Tibalt, Cosmic Impostor")
        .expect("the back face is in the fixture");
    assert_eq!(face.oracle_text.as_deref(), Some(TIBALT_FULL));

    let (mut runner, tibalt) = tibalt_scenario(P0, |_| {});
    let outcome = cast_tibalt(&mut runner, tibalt);

    // No ReplacementChoice — the run reaches priority on an empty stack.
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    let state = runner.state();
    assert!(state.stack.is_empty());
    let object = &state.objects[&tibalt];
    assert_eq!(object.zone, Zone::Battlefield);
    assert_eq!(object.name, "Tibalt, Cosmic Impostor");
    // CR 306.5b: enters with its printed loyalty.
    assert_eq!(object.loyalty, Some(5));

    // CR 114.2: only "you" gets the emblem.
    assert_eq!(state.command_zone.len(), 1, "exactly one emblem in play");
    assert!(emblems_of(&runner, P1).is_empty());
    assert!(emblems_of(&runner, P2).is_empty());
    let emblem = &runner.state().objects[&the_emblem_of(&runner, P0)];
    assert_eq!(emblem.zone, Zone::Command);
    assert_eq!(emblem.owner, P0);
    assert_eq!(emblem.controller, P0);
    assert_eq!(emblem.static_definitions.len(), 1);
    let permission = &emblem.static_definitions[0];
    assert_eq!(
        permission.mode,
        StaticMode::ExileCastPermission {
            frequency: CastFrequency::Unlimited,
            play_mode: engine::types::ability::CardPlayMode::Play,
            cost: ExileCastCost::PayNormalCost,
            pool: ExileCardPool::Persistent,
            timing: ExileCastTiming::AnyTime,
            mana_spend_permission: Some(engine::types::ability::ManaSpendPermission::AnyColor),
            grants_flash: false,
            extra_cost: None,
            enters_with_counter: None,
            grantee: ExileCastGrantee::SourceController,
        }
    );
    assert!(permission.active_zones.contains(&Zone::Command));
    // CR 607.1d + CR 607.5: the latched creator is the Tibalt object now on
    // the battlefield, paired through its own printed abilities.
    assert_eq!(
        emblem.linked_ability_source,
        Some(LinkedAbilitySource {
            creator: ObjectIncarnationRef::from_object(object),
            characteristic_set: CharacteristicSetRef::Own,
        })
    );

    // CR 603.6d: nothing but the spell itself was put on the stack — the
    // emblem is created by the replacement, not by a triggered ability.
    let pushed: Vec<ObjectId> = outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::StackPushed { object_id } => Some(*object_id),
            _ => None,
        })
        .collect();
    assert!(
        pushed.iter().all(|id| *id == tibalt),
        "no triggered ability was stacked: {pushed:?}"
    );
}

/// Reach-guard (CR 712.8f): casting the front face consults only Valki's
/// abilities — its ETB trigger fires and no emblem is created.
#[test]
fn valki_front_face_cast_creates_no_emblem() {
    // Each opponent holds a non-creature card, so the reveal has something to
    // show and no card choice is parked.
    let (mut runner, valki) = tibalt_scenario(P0, |scenario| {
        add_instant(scenario, P1, "P1 Hand Instant");
        add_instant(scenario, P2, "P2 Hand Instant");
    });
    let outcome = runner.cast(valki).modal_back_face(false).resolve();
    let mut events = outcome.events().to_vec();
    settle(&mut runner, &mut events);
    assert_eq!(zone(&runner, valki), Zone::Battlefield);
    assert_eq!(runner.state().objects[&valki].name, "Valki, God of Lies");
    // Reach-guard: Valki's ETB reached the stack and revealed each opponent's hand.
    let revealed = revealed_players(&events);
    assert!(
        revealed.contains(&P1) && revealed.contains(&P2),
        "{revealed:?}"
    );
    assert!(runner.state().command_zone.is_empty(), "no emblem");
}

/// CR 614.1c + CR 603.6d + CR 119.3: the class building block — "As this
/// creature enters, you lose 2 life." happens as the creature enters, for its
/// controller only, with nothing put on the stack.
#[test]
fn as_enters_one_shot_life_loss_applies_as_permanent_enters() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Entry Tax",
            2,
            2,
            "As this creature enters, you lose 2 life.",
        )
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(creature).resolve();
    assert_eq!(outcome.zone_of(creature), Zone::Battlefield);
    outcome.assert_life_delta(P0, -2);
    // Reach-guard: "you" resolved to the controller, not an opponent.
    outcome.assert_life_delta(P1, 0);
    assert!(
        outcome.events().iter().all(
            |event| !matches!(event, GameEvent::StackPushed { object_id } if *object_id != creature)
        ),
        "no triggered ability was stacked"
    );
}

/// CR 614.1c + CR 603.6d + CR 119.3: "you gain 3 life" under the drain's
/// injected entrant target still resolves "you" from the context.
#[test]
fn as_enters_one_shot_life_gain_applies_as_permanent_enters() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Entry Gift",
            2,
            2,
            "As this creature enters, you gain 3 life.",
        )
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(creature).resolve();
    assert_eq!(outcome.zone_of(creature), Zone::Battlefield);
    outcome.assert_life_delta(P0, 3);
    outcome.assert_life_delta(P1, 0);
    assert!(
        outcome.events().iter().all(
            |event| !matches!(event, GameEvent::StackPushed { object_id } if *object_id != creature)
        ),
        "no triggered ability was stacked"
    );
}

/// The +2 fixture: library tops are a {1}{G} creature (P0), a {2} instant (P1)
/// and a basic Forest (P2); P1 and P2 each hold a {2} instant and two mana, so a
/// grantee negative is not a mana failure. P0 holds the helper spells the
/// hostile tests cast; P1 holds an instant-speed removal spell.
struct PlusTwoFixture {
    runner: GameRunner,
    tibalt: ObjectId,
    emblem: ObjectId,
    creature: ObjectId,
    instant: ObjectId,
    forest: ObjectId,
    p1_hand_instant: ObjectId,
    p2_hand_instant: ObjectId,
    destroy: ObjectId,
    bounce: ObjectId,
    exile_creature: ObjectId,
    opp_creature: ObjectId,
}

struct HandIds {
    creature: ObjectId,
    instant: ObjectId,
    forest: ObjectId,
    p1_hand_instant: ObjectId,
    p2_hand_instant: ObjectId,
    destroy: ObjectId,
    bounce: ObjectId,
    exile_creature: ObjectId,
    opp_creature: ObjectId,
}

/// Build the +2 fixture up to (not including) casting Tibalt.
fn plus_two_setup() -> (GameRunner, ObjectId, HandIds) {
    use engine::game::scenario_db::GameScenarioDbExt;

    let mut ids = None;
    let (mut runner, tibalt) = tibalt_scenario(P0, |scenario| {
        let db = fixture_db();
        let creature = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
        let instant = add_instant(scenario, P1, "Opp Instant");
        let forest = scenario.add_real_card(P2, "Forest", Zone::Hand, db);
        let p1_hand_instant = add_instant(scenario, P1, "P1 Hand Instant");
        let p2_hand_instant = add_instant(scenario, P2, "P2 Hand Instant");
        let destroy = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Walker Removal",
                false,
                DESTROY_CREATURE_OR_PLANESWALKER,
            )
            .id();
        let bounce = scenario
            .add_spell_to_hand_from_oracle(P0, "Bounce", false, RETURN_TARGET_PERMANENT)
            .id();
        let exile_creature = scenario
            .add_spell_to_hand_from_oracle(P0, "Exile Probe", false, EXILE_TARGET_CREATURE)
            .id();
        let opp_creature = {
            let mut card = scenario.add_creature(P1, "Opp Guard", 2, 2);
            card.with_mana_cost(generic_cost(2));
            card.id()
        };
        scenario.with_mana_pool(
            P1,
            vec![mana(ManaType::Colorless), mana(ManaType::Colorless)],
        );
        scenario.with_mana_pool(
            P2,
            vec![mana(ManaType::Colorless), mana(ManaType::Colorless)],
        );
        // Cards under each exiled top, so a "Draw a card." never decks anyone.
        for player in [P0, P1, P2] {
            scenario.with_library_top(player, &["Library Filler A", "Library Filler B"]);
        }
        ids = Some(HandIds {
            creature,
            instant,
            forest,
            p1_hand_instant,
            p2_hand_instant,
            destroy,
            bounce,
            exile_creature,
            opp_creature,
        });
    });
    let ids = ids.expect("setup ran");
    put_on_library_top(&mut runner, ids.creature, P0);
    put_on_library_top(&mut runner, ids.instant, P1);
    put_on_library_top(&mut runner, ids.forest, P2);
    (runner, tibalt, ids)
}

fn tibalt_after_plus_two() -> PlusTwoFixture {
    let (mut runner, tibalt, ids) = plus_two_setup();
    cast_tibalt(&mut runner, tibalt);
    let emblem = the_emblem_of(&runner, P0);
    activate_tibalt(&mut runner, tibalt, TIBALT_PLUS_TWO);
    PlusTwoFixture {
        runner,
        tibalt,
        emblem,
        creature: ids.creature,
        instant: ids.instant,
        forest: ids.forest,
        p1_hand_instant: ids.p1_hand_instant,
        p2_hand_instant: ids.p2_hand_instant,
        destroy: ids.destroy,
        bounce: ids.bounce,
        exile_creature: ids.exile_creature,
        opp_creature: ids.opp_creature,
    }
}

/// CR 607.1d + CR 607.2a + CR 114.2 + CR 114.4 + CR 305.1: +2 exiles each
/// player's top card face up; the emblem's owner — and only its owner — may
/// play them.
#[test]
fn tibalt_plus_two_cards_playable_only_by_emblem_owner() {
    let mut fx = tibalt_after_plus_two();
    for card in [fx.creature, fx.instant, fx.forest] {
        let object = &fx.runner.state().objects[&card];
        assert_eq!(object.zone, Zone::Exile, "{}", object.name);
        assert!(!object.face_down, "{}", object.name);
        // Tibalt's exile recorded its own link; the mirror added the emblem's.
        assert!(linked(&fx.runner, card, fx.tibalt), "{}", object.name);
        assert!(linked(&fx.runner, card, fx.emblem), "{}", object.name);
    }
    add_pool(
        &mut fx.runner,
        P0,
        &[ManaType::Green, ManaType::Green, ManaType::Colorless],
    );
    assert!(can_cast(&fx.runner, P0, fx.creature));
    assert!(can_cast(&fx.runner, P0, fx.instant));
    assert!(playable_exiled_lands(&fx.runner, P0).contains(&fx.forest));

    // Grantee negatives (CR 114.2): P1 and P2 could pay for an identical
    // instant from hand, but the exiled one is not theirs to play.
    assert!(can_cast(&fx.runner, P1, fx.p1_hand_instant));
    assert!(!can_cast(&fx.runner, P1, fx.instant));
    assert!(can_cast(&fx.runner, P2, fx.p2_hand_instant));
    assert!(!can_cast(&fx.runner, P2, fx.instant));
    assert!(!playable_exiled_lands(&fx.runner, P1).contains(&fx.forest));
    assert!(!playable_exiled_lands(&fx.runner, P2).contains(&fx.forest));
}

/// CR 607.1d + CR 114.4: after Tibalt dies its own links are gone, but the
/// emblem's mirror links remain, so the cards stay playable.
#[test]
fn tibalt_emblem_still_grants_after_tibalt_dies() {
    let mut fx = tibalt_after_plus_two();
    cast_spell_targeting(&mut fx.runner, fx.destroy, fx.tibalt);
    assert_eq!(zone(&fx.runner, fx.tibalt), Zone::Graveyard);

    // Premise observed directly: Tibalt's own link is gone, the emblem's remains.
    assert!(!linked(&fx.runner, fx.creature, fx.tibalt));
    assert!(linked(&fx.runner, fx.creature, fx.emblem));
    assert_eq!(
        zone(&fx.runner, fx.emblem),
        Zone::Command,
        "CR 114.5: the emblem stays"
    );
    add_pool(&mut fx.runner, P0, &[ManaType::Green, ManaType::Green]);
    assert!(can_cast(&fx.runner, P0, fx.creature));
}

/// CR 609.4b: the emblem's concession lets P0 cast P1's {G}{G} card with only
/// {R}{R}{B}; the same pool cannot cast an identical card from hand.
#[test]
fn tibalt_emblem_any_color_mana_casts_opponents_gg_card() {
    let mut twin = None;
    let mut gg = None;
    let (mut runner, tibalt) = tibalt_scenario(P0, |scenario| {
        let mut card = scenario.add_creature_to_hand(P1, "Opp GG Beast", 3, 3);
        card.with_mana_cost(shards(&[ManaCostShard::Green, ManaCostShard::Green], 0));
        gg = Some(card.id());
        let mut card = scenario.add_creature_to_hand(P0, "Own GG Beast", 3, 3);
        card.with_mana_cost(shards(&[ManaCostShard::Green, ManaCostShard::Green], 0));
        twin = Some(card.id());
    });
    let (gg, twin) = (gg.unwrap(), twin.unwrap());
    put_on_library_top(&mut runner, gg, P1);
    cast_tibalt(&mut runner, tibalt);
    activate_tibalt(&mut runner, tibalt, TIBALT_PLUS_TWO);
    assert_eq!(zone(&runner, gg), Zone::Exile);
    add_pool(
        &mut runner,
        P0,
        &[ManaType::Red, ManaType::Red, ManaType::Black],
    );
    // Scoping twin: the concession belongs to the permission, not the player.
    assert!(!can_cast(&runner, P0, twin));
    assert!(can_cast(&runner, P0, gg));
    runner.cast(gg).resolve();
    assert_eq!(zone(&runner, gg), Zone::Battlefield);
    assert_eq!(runner.state().objects[&gg].controller, P0);
    assert_eq!(
        runner.state().players[0].mana_pool.mana.len(),
        1,
        "two of the three mana paid the {{G}}{{G}}"
    );
}

/// CR 305.2 + CR 305.1: an exiled land is playable through the emblem only
/// while P0 has a land drop left; the permission itself persists.
#[test]
fn tibalt_emblem_exiled_land_playable_only_with_land_drop() {
    use engine::game::scenario_db::GameScenarioDbExt;

    let mut forest = None;
    let mut hand_land = None;
    let (mut runner, tibalt) = tibalt_scenario(P0, |scenario| {
        let db = fixture_db();
        forest = Some(scenario.add_real_card(P1, "Forest", Zone::Hand, db));
        hand_land = Some(scenario.add_real_card(P0, "Forest", Zone::Hand, db));
    });
    let (forest, hand_land) = (forest.unwrap(), hand_land.unwrap());
    put_on_library_top(&mut runner, forest, P1);
    cast_tibalt(&mut runner, tibalt);
    activate_tibalt(&mut runner, tibalt, TIBALT_PLUS_TWO);
    assert_eq!(zone(&runner, forest), Zone::Exile);

    let play_forest = GameAction::PlayLand {
        object_id: forest,
        card_id: runner.state().objects[&forest].card_id,
    };
    assert!(playable_exiled_lands(&runner, P0).contains(&forest));
    assert!(engine::ai_support::legal_actions(runner.state()).contains(&play_forest));
    let mut clone = GameRunner::from_state(runner.state().clone());
    clone
        .act(play_forest.clone())
        .expect("the exiled Forest is playable while a land drop remains");
    assert_eq!(zone(&clone, forest), Zone::Battlefield);

    runner
        .act(GameAction::PlayLand {
            object_id: hand_land,
            card_id: runner.state().objects[&hand_land].card_id,
        })
        .expect("the land drop from hand");
    assert!(!engine::ai_support::legal_actions(runner.state()).contains(&play_forest));
    assert!(
        runner.act(play_forest).is_err(),
        "CR 305.2: no land drop left"
    );
    // Reach-guard: the permission persists; only CR 305.2 stops the play.
    assert!(playable_exiled_lands(&runner, P0).contains(&forest));
}

/// CR 607.2a: −3's exile feeds the emblem too.
#[test]
fn tibalt_minus_three_exile_is_playable_via_emblem() {
    let mut victim = None;
    let (mut runner, tibalt) = tibalt_scenario(P0, |scenario| {
        let mut card = scenario.add_creature(P1, "Opp Bear", 2, 2);
        card.with_mana_cost(generic_cost(2));
        victim = Some(card.id());
    });
    let victim = victim.unwrap();
    cast_tibalt(&mut runner, tibalt);
    let emblem = the_emblem_of(&runner, P0);
    runner
        .activate(tibalt, TIBALT_MINUS_THREE)
        .target_object(victim)
        .resolve();
    assert_eq!(zone(&runner, victim), Zone::Exile);
    assert!(linked(&runner, victim, tibalt), "reach: Tibalt's own link");
    assert!(linked(&runner, victim, emblem));
    add_pool(&mut runner, P0, &[ManaType::Colorless, ManaType::Colorless]);
    assert!(can_cast(&runner, P0, victim));
}

/// Set Tibalt's loyalty to `loyalty` (field and counters together, CR 306.5b).
fn set_loyalty(runner: &mut GameRunner, tibalt: ObjectId, loyalty: u32) {
    use engine::types::counter::CounterType;

    let object = runner.state_mut().objects.get_mut(&tibalt).unwrap();
    object.loyalty = Some(loyalty);
    object.counters.insert(CounterType::Loyalty, loyalty);
}

/// CR 607.2a + CR 106.4: −8 exiles every graveyard into the emblem's pool and
/// adds {R}{R}{R} — even when there is nothing to exile.
#[test]
fn tibalt_minus_eight_exiles_graveyards_playable_and_adds_rrr() {
    let mut cards = Vec::new();
    let (mut runner, tibalt) = tibalt_scenario(P0, |scenario| {
        for (player, name) in [(P0, "Own Grave Bear"), (P1, "Opp Grave Bear")] {
            let mut card = scenario.add_creature_to_hand(player, name, 2, 2);
            card.with_mana_cost(generic_cost(2));
            cards.push((card.id(), player));
        }
    });
    for &(card, owner) in &cards {
        let mut events = Vec::new();
        engine::game::zones::move_to_zone(runner.state_mut(), card, Zone::Graveyard, &mut events);
        assert_eq!(zone(&runner, card), Zone::Graveyard, "{owner:?}");
    }
    cast_tibalt(&mut runner, tibalt);
    let emblem = the_emblem_of(&runner, P0);
    set_loyalty(&mut runner, tibalt, 8);
    let red_before = pool_count(&runner, P0, ManaType::Red);
    activate_tibalt(&mut runner, tibalt, TIBALT_MINUS_EIGHT);
    assert_eq!(pool_count(&runner, P0, ManaType::Red), red_before + 3);
    add_pool(&mut runner, P0, &[ManaType::Colorless, ManaType::Colorless]);
    for &(card, _) in &cards {
        assert_eq!(zone(&runner, card), Zone::Exile);
        assert!(linked(&runner, card, emblem));
        assert!(can_cast(&runner, P0, card));
    }

    // Ruling twin: with empty graveyards −8 still adds {R}{R}{R}.
    let (mut runner, tibalt) = tibalt_scenario(P0, |_| {});
    cast_tibalt(&mut runner, tibalt);
    set_loyalty(&mut runner, tibalt, 8);
    let red_before = pool_count(&runner, P0, ManaType::Red);
    activate_tibalt(&mut runner, tibalt, TIBALT_MINUS_EIGHT);
    assert_eq!(pool_count(&runner, P0, ManaType::Red), red_before + 3);
}

/// CR 400.7: a new Tibalt object (bounced and recast) is a different creator;
/// its exiles feed only its own new emblem, not the old one.
#[test]
fn new_tibalt_object_does_not_extend_old_emblem() {
    let mut fx = tibalt_after_plus_two();
    let old_emblem = fx.emblem;
    // Next library tops for the second +2.
    let mut next_tops = Vec::new();
    for owner in [P0, P1, P2] {
        let id = engine::game::zones::create_object(
            fx.runner.state_mut(),
            engine::types::identifiers::CardId(7000 + owner.0 as u64),
            owner,
            format!("Second Top {}", owner.0),
            Zone::Library,
        );
        put_on_library_top(&mut fx.runner, id, owner);
        next_tops.push(id);
    }

    cast_spell_targeting(&mut fx.runner, fx.bounce, fx.tibalt);
    assert_eq!(zone(&fx.runner, fx.tibalt), Zone::Hand);
    add_pool(
        &mut fx.runner,
        P0,
        &tibalt_mana().iter().map(|u| u.color).collect::<Vec<_>>(),
    );
    cast_tibalt(&mut fx.runner, fx.tibalt);
    let emblems = emblems_of(&fx.runner, P0);
    assert_eq!(emblems.len(), 2);
    let new_emblem = *emblems.iter().find(|id| **id != old_emblem).unwrap();
    let old_creator = fx.runner.state().objects[&old_emblem]
        .linked_ability_source
        .unwrap()
        .creator;
    let new_creator = fx.runner.state().objects[&new_emblem]
        .linked_ability_source
        .unwrap()
        .creator;
    assert_eq!(
        old_creator.object_id, new_creator.object_id,
        "same storage id"
    );
    assert_ne!(old_creator.incarnation, new_creator.incarnation);

    activate_tibalt(&mut fx.runner, fx.tibalt, TIBALT_PLUS_TWO);
    for card in next_tops {
        assert_eq!(zone(&fx.runner, card), Zone::Exile);
        assert!(linked(&fx.runner, card, new_emblem));
        assert!(!linked(&fx.runner, card, old_emblem));
    }
    // Reach: the first batch is still linked to the old emblem.
    assert!(linked(&fx.runner, fx.creature, old_emblem));
}

/// CR 114.2 + CR 607.1d: P1's own Tibalt gives P1 its own emblem; the cards it
/// exiles are P1's to play, never P0's.
#[test]
fn opponents_tibalt_emblem_grants_only_its_owner() {
    let mut ids = None;
    let (mut runner, tibalt) = tibalt_scenario(P1, |scenario| {
        let top = add_instant(scenario, P0, "P0 Top Instant");
        let p0_hand = add_instant(scenario, P0, "P0 Hand Instant");
        ids = Some((top, p0_hand));
    });
    let (top, p0_hand) = ids.unwrap();
    put_on_library_top(&mut runner, top, P0);
    begin_main_phase_of(&mut runner, P1);
    cast_tibalt(&mut runner, tibalt);
    let p1_emblem = the_emblem_of(&runner, P1);
    assert!(emblems_of(&runner, P0).is_empty(), "P0 got no emblem");
    activate_tibalt(&mut runner, tibalt, TIBALT_PLUS_TWO);
    assert_eq!(zone(&runner, top), Zone::Exile);
    assert!(linked(&runner, top, p1_emblem), "reach: P1's emblem link");

    add_pool(&mut runner, P1, &[ManaType::Colorless, ManaType::Colorless]);
    add_pool(&mut runner, P0, &[ManaType::Colorless, ManaType::Colorless]);
    assert!(can_cast(&runner, P1, top));
    // Grantee negative, paired with an identical payable card in P0's hand.
    assert!(can_cast(&runner, P0, p0_hand));
    assert!(!can_cast(&runner, P0, top));
}

/// CR 400.7 + CR 607.1d: control of Tibalt changing is not a new object — the
/// exiles of the stolen Tibalt still feed its original controller's emblem.
#[test]
fn stolen_tibalt_exiles_still_feed_original_emblem() {
    use engine::game::scenario_db::GameScenarioDbExt;

    let mut ids = None;
    let (mut runner, tibalt) = tibalt_scenario(P0, |scenario| {
        let db = fixture_db();
        let confiscate = scenario.add_real_card(P1, "Confiscate", Zone::Hand, db);
        let top = add_instant(scenario, P1, "P1 Top Instant");
        let p0_hand = add_instant(scenario, P0, "P0 Hand Instant");
        let p1_hand = add_instant(scenario, P1, "P1 Hand Instant");
        ids = Some((confiscate, top, p0_hand, p1_hand));
    });
    let (confiscate, top, p0_hand, p1_hand) = ids.unwrap();
    put_on_library_top(&mut runner, top, P1);
    cast_tibalt(&mut runner, tibalt);
    let p0_emblem = the_emblem_of(&runner, P0);

    begin_main_phase_of(&mut runner, P1);
    add_pool(
        &mut runner,
        P1,
        &[
            ManaType::Blue,
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );
    runner.cast(confiscate).target_object(tibalt).resolve();
    assert_eq!(runner.state().objects[&tibalt].controller, P1);
    activate_tibalt(&mut runner, tibalt, TIBALT_PLUS_TWO);
    assert_eq!(zone(&runner, top), Zone::Exile);
    assert!(linked(&runner, top, p0_emblem));
    assert!(emblems_of(&runner, P1).is_empty(), "P1 has no emblem");

    // On P0's priority, P0 may cast it.
    {
        let state = runner.state_mut();
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    add_pool(&mut runner, P0, &[ManaType::Colorless, ManaType::Colorless]);
    add_pool(&mut runner, P1, &[ManaType::Colorless, ManaType::Colorless]);
    assert!(can_cast(&runner, P0, p0_hand));
    assert!(can_cast(&runner, P0, top));
    // P1 activated the ability but has no emblem: grantee negative, paired.
    assert!(can_cast(&runner, P1, p1_hand));
    assert!(!can_cast(&runner, P1, top));
}

/// CR 113.7a + CR 400.7: Tibalt destroyed in response to its +2 — the ability
/// still resolves, and its exiles feed the emblem of the object that
/// activated it.
#[test]
fn tibalt_destroyed_in_response_plus_two_still_feeds_emblem() {
    let mut ids = None;
    let (mut runner, tibalt) = tibalt_scenario(P0, |scenario| {
        let removal = scenario
            .add_spell_to_hand_from_oracle(
                P1,
                "Opp Walker Removal",
                true,
                DESTROY_CREATURE_OR_PLANESWALKER,
            )
            .id();
        let top = add_instant(scenario, P1, "P1 Top Instant");
        ids = Some((removal, top));
    });
    let (removal, top) = ids.unwrap();
    put_on_library_top(&mut runner, top, P1);
    cast_tibalt(&mut runner, tibalt);
    let emblem = the_emblem_of(&runner, P0);

    runner
        .act(GameAction::ActivateAbility {
            source_id: tibalt,
            ability_index: TIBALT_PLUS_TWO,
        })
        .expect("+2 activation");
    assert_eq!(runner.state().stack.len(), 1, "+2 on the stack");
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes with +2 on the stack");
    runner.cast(removal).target_object(tibalt).commit();
    assert_eq!(runner.state().stack.len(), 2, "removal above the +2");
    runner.resolve_top();
    // Reach: Tibalt left before the +2 resolved.
    assert_eq!(zone(&runner, tibalt), Zone::Graveyard);
    assert_eq!(runner.state().stack.len(), 1, "+2 still waiting");
    runner.resolve_top();
    assert!(runner.state().stack.is_empty());

    assert_eq!(zone(&runner, top), Zone::Exile);
    assert!(linked(&runner, top, emblem));
    add_pool(&mut runner, P0, &[ManaType::Colorless, ManaType::Colorless]);
    assert!(can_cast(&runner, P0, top));
}

/// CR 607.2a: a card exiled by an unrelated source is not "exiled with
/// Tibalt".
#[test]
fn card_exiled_by_other_source_not_playable_via_emblem() {
    let mut fx = tibalt_after_plus_two();
    cast_spell_targeting(&mut fx.runner, fx.exile_creature, fx.opp_creature);
    assert_eq!(zone(&fx.runner, fx.opp_creature), Zone::Exile);
    assert!(!linked(&fx.runner, fx.opp_creature, fx.emblem));
    add_pool(
        &mut fx.runner,
        P0,
        &[ManaType::Green, ManaType::Green, ManaType::Colorless],
    );
    assert!(!can_cast(&fx.runner, P0, fx.opp_creature));
    // Reach: a +2 card in the same state is castable.
    assert!(can_cast(&fx.runner, P0, fx.creature));
}

/// CR 400.7 + CR 607.2a: a card that left exile is a new object and is no
/// longer "exiled with" Tibalt. (Leave-exile route: P0 casts the exiled
/// instant through the emblem; it resolves into its owner's graveyard. The
/// "Put target face-up exiled card into its owner's graveyard." helper offers
/// only player targets today, so it cannot move the card.)
#[test]
fn card_that_left_exile_not_playable_via_emblem() {
    let mut fx = tibalt_after_plus_two();
    add_pool(
        &mut fx.runner,
        P0,
        &[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Green,
            ManaType::Green,
        ],
    );
    assert!(
        can_cast(&fx.runner, P0, fx.instant),
        "reach: castable via the emblem"
    );
    fx.runner.cast(fx.instant).resolve();
    assert_eq!(zone(&fx.runner, fx.instant), Zone::Graveyard);
    assert_eq!(fx.runner.state().objects[&fx.instant].owner, P1);
    assert!(
        !fx.runner
            .state()
            .exile_links
            .iter()
            .any(|link| link.exiled_id == fx.instant),
        "no link survives the card leaving exile"
    );
    assert!(!can_cast(&fx.runner, P0, fx.instant));
    // Reach: the other +2 card is still castable.
    add_pool(&mut fx.runner, P0, &[ManaType::Green, ManaType::Green]);
    assert!(can_cast(&fx.runner, P0, fx.creature));
}

// ── Possessive-shift hand reveal: "target <x>'s controller|owner reveals their hand" ──
//
// CR 608.2c + CR 109.4 + CR 108.3: in "target <filter>'s controller/owner
// reveals their hand" the spell targets the object, and the player whose hand
// is revealed (CR 701.20a) is that object's controller/owner — never the
// caster. The parser binds the reveal's player to `ParentTargetController` /
// `ParentTargetOwner`; these tests pin that `RevealHand` resolves those
// anaphors from the inherited object target.

/// Verbatim Oracle text (Scryfall).
const DENIED: &str = "Choose a card name, then target spell's controller reveals their hand. If a card with the chosen name is revealed this way, counter that spell.";
/// Grammar fixtures (not cards) for the controller and owner shifts.
const CONTROLLER_REVEALS: &str = "Target creature's controller reveals their hand.";
const OWNER_REVEALS: &str = "Target creature's owner reveals their hand.";

/// A {U} instant for P0 with verbatim/grammar `oracle`, P0's pool funded, one
/// card in each player's hand (so a wrong-player reveal would be observable).
fn possessive_reveal_scenario(
    oracle: &str,
    name: &str,
) -> (GameScenario, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, vec![mana(ManaType::Blue)]);
    let mut spell = scenario.add_spell_to_hand_from_oracle(P0, name, true, oracle);
    spell.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 0,
    });
    let spell = spell.id();
    let p0_card = add_hand_creature(&mut scenario, P0, "Caster Card", 2, 2, 2);
    let p1_card = add_hand_creature(&mut scenario, P1, "Opponent Card", 2, 2, 2);
    (scenario, spell, p0_card, p1_card)
}

/// The card ids revealed for `player` across `events`.
fn revealed_cards_of(events: &[GameEvent], player: PlayerId) -> Vec<ObjectId> {
    events
        .iter()
        .filter_map(|e| match e {
            GameEvent::CardsRevealed {
                player: p,
                card_ids,
                ..
            } if *p == player => Some(card_ids.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

/// CR 608.2c + CR 109.4 + CR 701.20a: "Target creature's controller reveals
/// their hand." targeting an opponent's creature reveals that opponent's hand,
/// not the caster's. Reverting the `ParentTargetController` resolution in
/// `reveal_hand::resolve` makes the reveal fail (`MissingParam`) — no
/// `CardsRevealed` for P1.
#[test]
fn target_creatures_controller_reveals_their_hand() {
    let (mut scenario, spell, p0_card, p1_card) =
        possessive_reveal_scenario(CONTROLLER_REVEALS, "Controller Reveal");
    let creature = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_object(creature).resolve();
    let events = outcome.events();

    assert_eq!(
        revealed_players(events),
        vec![P1],
        "exactly the targeted creature's controller reveals: {events:#?}"
    );
    assert_eq!(revealed_cards_of(events, P1), vec![p1_card]);
    assert!(revealed_cards_of(events, P0).is_empty());
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .hand
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![p0_card],
        "reach-guard: the caster held a card that a wrong-player reveal would show"
    );
}

/// CR 608.2c + CR 108.3 + CR 701.20a: "Target creature's owner reveals their
/// hand." targeting a creature P1 owns but P0 controls reveals the OWNER's (P1)
/// hand — not the controller's (P0, the caster). The controller/owner split
/// makes the fixture discriminate owner from controller.
#[test]
fn target_creatures_owner_reveals_their_hand_even_when_stolen() {
    let (mut scenario, spell, _p0_card, p1_card) =
        possessive_reveal_scenario(OWNER_REVEALS, "Owner Reveal");
    let stolen = scenario
        .add_creature(P1, "Stolen Bear", 2, 2)
        .controlled_by(P0)
        .id();
    let mut runner = scenario.build();
    assert_eq!(runner.state().objects[&stolen].owner, P1);
    assert_eq!(
        runner.state().objects[&stolen].controller,
        P0,
        "reach-guard: owner and controller differ"
    );

    let outcome = runner.cast(spell).target_object(stolen).resolve();
    let events = outcome.events();

    assert_eq!(
        revealed_players(events),
        vec![P1],
        "exactly the targeted creature's owner reveals: {events:#?}"
    );
    assert_eq!(revealed_cards_of(events, P1), vec![p1_card]);
    assert!(revealed_cards_of(events, P0).is_empty());
}

/// CR 608.2c + CR 109.4 + CR 701.20a: Denied! (verbatim) targeting an
/// opponent's spell reveals that spell's controller's hand. The counter clause
/// is a separate, unsupported gap and is deliberately NOT asserted here.
#[test]
fn denied_reveals_target_spells_controllers_hand() {
    let (scenario, denied, _p0_card, p1_card) = possessive_reveal_scenario(DENIED, "Denied!");
    let mut runner = scenario.build();
    // CR 201.4: the chosen name must be a real card name; seed the name pool
    // (mirrors `anointed_peacekeeper_chosen_opponent`). What the counter
    // clause does with this name is out of scope here (unsupported gap).
    runner.state_mut().all_card_names = std::sync::Arc::from(["Lightning Bolt".to_string()]);

    // An opponent's instant on the stack (mirrors `counter_spell_zone_redirect`).
    let opponent_spell = engine::game::zones::create_object(
        runner.state_mut(),
        engine::types::identifiers::CardId(7701),
        P1,
        "Shock".to_string(),
        Zone::Stack,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&opponent_spell)
        .expect("opponent spell")
        .card_types
        .core_types = vec![engine::types::card_type::CoreType::Instant];
    runner
        .state_mut()
        .stack
        .push_back(engine::types::game_state::StackEntry {
            id: opponent_spell,
            source_id: opponent_spell,
            controller: P1,
            kind: StackEntryKind::Spell {
                card_id: engine::types::identifiers::CardId(7701),
                ability: None,
                casting_variant: engine::types::game_state::CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

    let outcome = runner
        .cast(denied)
        .target_object(opponent_spell)
        .choose_option("Lightning Bolt")
        .resolve();
    let events = outcome.events();

    assert_eq!(
        revealed_players(events),
        vec![P1],
        "Denied! reveals exactly the targeted spell's controller's hand: {events:#?}"
    );
    assert_eq!(revealed_cards_of(events, P1), vec![p1_card]);
    assert!(revealed_cards_of(events, P0).is_empty());
}

// ── CR 607.1 + CR 607.5: the emblem pairs only with the printed supplier ──
//
// "Cards exiled with Tibalt" on the emblem (CR 607.1d) refers only to cards
// exiled by Tibalt's own activated or triggered abilities that instruct exile
// (CR 607.2a) — "and not by any other ability" (CR 607.1). An ability Tibalt
// gained in layer 6 (CR 613.1f), or one it acquired from a later copy effect
// (CR 607.5: linked only to the abilities acquired from that same effect), is
// a different ability and never feeds the emblem.

/// Verbatim Oracle text of a grant helper ("Planeswalkers you control have
/// \"{1}: Exile target card from a graveyard.\"").
const GRANT_GRAVEYARD_EXILE: &str =
    "Planeswalkers you control have \"{1}: Exile target card from a graveyard.\"";
/// Verbatim Oracle text of the granted / copied exile ability.
const EXILE_CARD_FROM_GRAVEYARD: &str = "{1}: Exile target card from a graveyard.";
/// Verbatim Oracle text of a sacrifice-cost exile helper.
const SACRIFICE_ARTIFACT_EXILE: &str =
    "{1}, Sacrifice an artifact: Exile target card from a graveyard.";
/// Verbatim Oracle text of a modal activated helper.
const MODAL_LIFE: &str = "{T}: Choose one —\n• You gain 1 life.\n• You lose 1 life.";

/// Index of the granted ability on Tibalt: after its three printed loyalty
/// abilities (CR 613.1f: layer 6 appends it).
const TIBALT_GRANTED: usize = 3;

/// A {2} creature card named `name` in P1's graveyard.
fn add_graveyard_creature(scenario: &mut GameScenario, name: &str) -> ObjectId {
    let mut card = scenario.add_creature_to_graveyard(P1, name, 2, 2);
    card.with_mana_cost(generic_cost(2));
    card.id()
}

/// A {2} creature named `name` on P1's battlefield.
fn add_opponent_creature(scenario: &mut GameScenario, name: &str) -> ObjectId {
    let mut card = scenario.add_creature(P1, name, 2, 2);
    card.with_mana_cost(generic_cost(2));
    card.id()
}

/// P0 can cast `card` with two colorless mana (the emblem's any-color
/// concession is irrelevant for a generic cost).
fn funded_can_cast(runner: &mut GameRunner, card: ObjectId) -> bool {
    add_pool(runner, P0, &[ManaType::Colorless, ManaType::Colorless]);
    can_cast(runner, P0, card)
}

/// CR 607.1 + CR 607.1d + CR 607.2a + CR 613.1f: two exile abilities on one
/// Tibalt — a granted "{1}: Exile target card from a graveyard." and the
/// printed −3. Only the printed supplier feeds the emblem.
#[test]
fn tibalt_granted_exile_ability_does_not_feed_emblem() {
    let mut ids = None;
    let (mut runner, tibalt) = tibalt_scenario(P0, |scenario| {
        scenario.add_artifact_from_oracle(P0, "Grant Relic", GRANT_GRAVEYARD_EXILE);
        let grave = add_graveyard_creature(scenario, "Grave Bear");
        let victim = add_opponent_creature(scenario, "Opp Bear");
        ids = Some((grave, victim));
    });
    let (grave, victim) = ids.unwrap();
    cast_tibalt(&mut runner, tibalt);
    let emblem = the_emblem_of(&runner, P0);
    // Granted-slot reach: three printed loyalty abilities plus the grant.
    assert_eq!(
        runner.state().objects[&tibalt].abilities.len(),
        TIBALT_GRANTED + 1
    );

    add_pool(&mut runner, P0, &[ManaType::Colorless]);
    runner
        .activate(tibalt, TIBALT_GRANTED)
        .target_object(grave)
        .resolve();
    assert_eq!(
        zone(&runner, grave),
        Zone::Exile,
        "reach: the grant exiled it"
    );
    assert!(linked(&runner, grave, tibalt), "reach: Tibalt's own link");
    assert!(
        !linked(&runner, grave, emblem),
        "a granted ability is not the emblem's paired supplier"
    );
    assert!(!funded_can_cast(&mut runner, grave));

    // Positive twin: the printed −3 on the same Tibalt feeds the emblem.
    runner
        .activate(tibalt, TIBALT_MINUS_THREE)
        .target_object(victim)
        .resolve();
    assert_eq!(zone(&runner, victim), Zone::Exile);
    assert!(linked(&runner, victim, emblem));
    assert!(funded_can_cast(&mut runner, victim));
}

/// Answer an activation's announcement prompts (mode 0, the first offered
/// target, mana from the pool) until it is on the stack, then return the
/// provenance its stack entry carries — before it resolves.
fn announced_provenance(
    runner: &mut GameRunner,
    source: ObjectId,
    ability_index: usize,
    target: Option<ObjectId>,
) -> Option<engine::types::ability::AbilityProvenance> {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index,
        })
        .expect("activation is accepted");
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::AbilityModeChoice { .. } => {
                runner
                    .act(GameAction::SelectModes { indices: vec![0] })
                    .expect("mode 0");
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target.expect("a target"))),
                    })
                    .expect("target");
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt {other:?}"),
        }
    }
    let entry = runner
        .state()
        .stack
        .last()
        .expect("the ability is on the stack");
    assert_eq!(entry.source_id, source, "reach: this activation is on top");
    entry
        .ability()
        .expect("an activated ability")
        .context
        .source_ability_provenance
}

/// CR 602.2a + CR 607.1 + CR 613.1f: every activated path binds the
/// activated slot's provenance at announcement and carries it to the stack
/// entry — the loyalty fast path, the general casting path for a granted
/// ability, and the modal path.
#[test]
fn tibalt_activation_announcement_records_ability_provenance() {
    use engine::types::ability::{AbilityProvenance, CharacteristicSetRef};
    let own = Some(AbilityProvenance::Characteristic(CharacteristicSetRef::Own));

    // Loyalty fast path: Tibalt's printed +2.
    let (mut runner, tibalt) = tibalt_scenario(P0, |_| {});
    cast_tibalt(&mut runner, tibalt);
    assert_eq!(
        announced_provenance(&mut runner, tibalt, TIBALT_PLUS_TWO, None),
        own
    );

    // General casting path: the granted graveyard exile.
    let mut grave = None;
    let (mut runner, tibalt) = tibalt_scenario(P0, |scenario| {
        scenario.add_artifact_from_oracle(P0, "Grant Relic", GRANT_GRAVEYARD_EXILE);
        grave = Some(add_graveyard_creature(scenario, "Grave Bear"));
    });
    cast_tibalt(&mut runner, tibalt);
    add_pool(&mut runner, P0, &[ManaType::Colorless]);
    assert_eq!(
        announced_provenance(&mut runner, tibalt, TIBALT_GRANTED, grave),
        Some(AbilityProvenance::Granted)
    );

    // Modal path: a printed modal activated ability.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let relic = scenario
        .add_artifact_from_oracle(P0, "Mode Relic", MODAL_LIFE)
        .id();
    let mut runner = scenario.build();
    assert_eq!(announced_provenance(&mut runner, relic, 0, None), own);
}

/// CR 607.5 + CR 613.1a: a copy effect installed later on the same Tibalt
/// incarnation gives it a different ability set — the copied exile ability is
/// not the emblem's supplier, although the same incarnation's printed −3 was.
#[test]
fn tibalt_later_copy_effect_exile_does_not_feed_emblem() {
    use engine::types::ability::{
        CharacteristicSetRef, ContinuousModification, CopyEffectInstanceRef, Duration, TargetFilter,
    };

    let mut ids = None;
    let (mut runner, tibalt) = tibalt_scenario(P0, |scenario| {
        let donor = scenario
            .add_artifact_from_oracle(P0, "Relic Donor", EXILE_CARD_FROM_GRAVEYARD)
            .id();
        let victim = add_opponent_creature(scenario, "Opp Bear");
        let grave = add_graveyard_creature(scenario, "Grave Bear");
        ids = Some((donor, victim, grave));
    });
    let (donor, victim, grave) = ids.unwrap();
    cast_tibalt(&mut runner, tibalt);
    let emblem = the_emblem_of(&runner, P0);

    // Positive twin first: this incarnation's printed −3 feeds the emblem.
    runner
        .activate(tibalt, TIBALT_MINUS_THREE)
        .target_object(victim)
        .resolve();
    assert!(linked(&runner, victim, emblem));

    // Copy effect D: Tibalt becomes a copy of the donor (CR 707.2).
    let values =
        engine::game::printed_cards::intrinsic_copiable_values(&runner.state().objects[&donor]);
    let copy_id = runner.state_mut().add_transient_continuous_effect(
        tibalt,
        P0,
        Duration::Permanent,
        TargetFilter::SpecificObject { id: tibalt },
        vec![ContinuousModification::CopyValues {
            values: Box::new(values),
            display_source: engine::game::game_object::DisplaySource::Card,
            printed_ref: None,
            token_image_ref: None,
        }],
        None,
    );
    engine::game::layers::mark_layers_full(runner.state_mut());
    engine::game::layers::flush_layers(runner.state_mut());
    let object = &runner.state().objects[&tibalt];
    assert_eq!(object.name, "Relic Donor", "reach: the copy applied");
    assert_eq!(object.abilities.len(), 1);
    let copy = CopyEffectInstanceRef {
        continuous_effect_id: copy_id,
        modification_index: 0,
    };
    assert_eq!(object.layer1_copy_effect, Some(copy));
    assert_eq!(
        object.characteristic_set(),
        CharacteristicSetRef::Copied(copy)
    );
    assert_eq!(
        runner.state().objects[&emblem]
            .linked_ability_source
            .map(|link| link.characteristic_set),
        Some(CharacteristicSetRef::Own),
        "the emblem stays paired with the printed set"
    );

    add_pool(&mut runner, P0, &[ManaType::Colorless]);
    runner.activate(tibalt, 0).target_object(grave).resolve();
    assert_eq!(
        zone(&runner, grave),
        Zone::Exile,
        "reach: the copied ability exiled it"
    );
    assert!(linked(&runner, grave, tibalt), "reach: Tibalt's own link");
    assert!(
        !linked(&runner, grave, emblem),
        "D's ability is linked only to D's abilities (CR 607.5)"
    );
    assert!(!funded_can_cast(&mut runner, grave));
    assert!(
        funded_can_cast(&mut runner, victim),
        "the −3 card stays playable"
    );
}

/// CR 602.2a + CR 607.1: which ability was activated is fixed at
/// announcement. A granted exile ability announced while an ability-removing
/// effect hid Tibalt's printed abilities stays granted even though paying its
/// sacrifice cost ends that effect and restores the printed abilities before
/// it reaches the stack.
#[test]
fn tibalt_granted_ability_provenance_is_fixed_at_announcement() {
    use engine::types::ability::{ContinuousModification, Duration, TargetFilter};

    let mut ids = None;
    let (mut runner, tibalt) = tibalt_scenario(P0, |scenario| {
        let trinket = scenario
            .add_artifact_from_oracle(P0, "Brass Trinket", "")
            .id();
        let warden = scenario
            .add_creature_from_oracle(P0, "Grave Warden", 1, 1, SACRIFICE_ARTIFACT_EXILE)
            .id();
        let grave = add_graveyard_creature(scenario, "Grave Bear");
        ids = Some((trinket, warden, grave));
    });
    let (trinket, warden, grave) = ids.unwrap();
    cast_tibalt(&mut runner, tibalt);
    let emblem = the_emblem_of(&runner, P0);

    let granted = runner.state().objects[&warden].abilities[0].clone();
    let state = runner.state_mut();
    state.add_transient_continuous_effect(
        trinket,
        P0,
        Duration::UntilHostLeavesPlay,
        TargetFilter::SpecificObject { id: tibalt },
        vec![ContinuousModification::RemoveAllAbilities],
        None,
    );
    state.add_transient_continuous_effect(
        warden,
        P0,
        Duration::Permanent,
        TargetFilter::SpecificObject { id: tibalt },
        vec![ContinuousModification::GrantAbility {
            definition: Box::new(granted.clone()),
        }],
        None,
    );
    engine::game::layers::mark_layers_full(runner.state_mut());
    engine::game::layers::flush_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&tibalt].abilities.as_slice(),
        &[granted],
        "reach: only the grant is live at announcement"
    );

    add_pool(&mut runner, P0, &[ManaType::Colorless]);
    runner
        .activate(tibalt, 0)
        .target_object(grave)
        .pay_with(&[trinket])
        .resolve();
    assert_eq!(zone(&runner, trinket), Zone::Graveyard, "reach: sacrificed");
    assert_eq!(
        zone(&runner, grave),
        Zone::Exile,
        "reach: the grant exiled it"
    );
    assert!(linked(&runner, grave, tibalt), "reach: Tibalt's own link");
    assert_eq!(
        runner.state().objects[&tibalt].abilities.len(),
        TIBALT_GRANTED + 1,
        "reach: the removal ended, so slot 0 is the printed +2 again"
    );
    assert!(!linked(&runner, grave, emblem));
    assert!(!funded_can_cast(&mut runner, grave));
}

// ── CR 114.4: command-zone emblem permission sources ──

/// Stage `card` as exiled with `emblem` by `exiler` (the link and exiling
/// player the exile resolver writes, CR 406.6 + CR 607.1d).
fn stage_exiled_with_emblem(
    runner: &mut GameRunner,
    card: ObjectId,
    emblem: ObjectId,
    exiler: PlayerId,
) {
    let state = runner.state_mut();
    state.exile_links.push(ExileLink {
        exiled_id: card,
        source_id: emblem,
        kind: ExileLinkKind::TrackedBySource,
    });
    state.objects.get_mut(&card).unwrap().exiled_by = Some(exiler);
}

/// An emblem owned by P0 whose exile-play permission addresses `grantee`.
fn grant_permission_emblem(
    runner: &mut GameRunner,
    grantee: engine::types::statics::ExileCastGrantee,
) -> ObjectId {
    use engine::types::ability::{CardPlayMode, StaticDefinition, TargetFilter};
    use engine::types::statics::{
        CastFrequency, ExileCardPool, ExileCastCost, ExileCastTiming, StaticMode,
    };

    engine::game::effects::create_emblem::grant_emblem(
        runner.state_mut(),
        P0,
        vec![StaticDefinition::new(StaticMode::ExileCastPermission {
            frequency: CastFrequency::Unlimited,
            play_mode: CardPlayMode::Play,
            cost: ExileCastCost::PayNormalCost,
            pool: ExileCardPool::Persistent,
            timing: ExileCastTiming::AnyTime,
            mana_spend_permission: None,
            grants_flash: false,
            extra_cost: None,
            enters_with_counter: None,
            grantee,
        })
        .affected(TargetFilter::Any)],
        Vec::new(),
        Vec::new(),
    )
}

/// CR 114.4 + CR 114.2: an emblem's abilities function for every player they
/// address. An "each player may play cards they exiled with it" emblem owned by
/// P0 lets P1 cast the card P1 exiled with it — not the one P0 exiled — while
/// a "you may" emblem addresses only its owner and controller.
#[test]
fn emblem_each_player_grant_reaches_non_owner() {
    use engine::game::casting::spell_objects_available_to_cast;
    use engine::types::statics::ExileCastGrantee;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let by_p1 = scenario.add_spell_to_exile(P0, "Exiled By P1", true).id();
    let by_p0 = scenario.add_spell_to_exile(P1, "Exiled By P0", true).id();
    let mut runner = scenario.build();
    let emblem = grant_permission_emblem(&mut runner, ExileCastGrantee::EachPlayerOwnExiles);
    stage_exiled_with_emblem(&mut runner, by_p1, emblem, P1);
    stage_exiled_with_emblem(&mut runner, by_p0, emblem, P0);

    let p1_castable = spell_objects_available_to_cast(runner.state(), P1);
    let p0_castable = spell_objects_available_to_cast(runner.state(), P0);
    assert!(
        p1_castable.contains(&by_p1),
        "P1 exiled it with P0's emblem, so the emblem lets P1 cast it"
    );
    assert!(!p1_castable.contains(&by_p0));
    assert!(
        p0_castable.contains(&by_p0),
        "reach: the own-exiles gate admits P0's"
    );
    assert!(!p0_castable.contains(&by_p1));

    // Hostile twin: a "you may" emblem addresses only its controller (= owner).
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let card = scenario.add_spell_to_exile(P1, "Exiled By P1", true).id();
    let mut runner = scenario.build();
    let emblem = grant_permission_emblem(&mut runner, ExileCastGrantee::SourceController);
    stage_exiled_with_emblem(&mut runner, card, emblem, P1);
    assert!(!spell_objects_available_to_cast(runner.state(), P1).contains(&card));
    assert!(spell_objects_available_to_cast(runner.state(), P0).contains(&card));
}
