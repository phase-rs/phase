//! Mog, Moogle Warrior — "Dance" (issue #9736).
//!
//! Oracle text (verbatim, MTGJSON): "Lifelink / Dance — At the beginning of your
//! end step, each player may discard a card. Each player who discarded a card
//! this way draws a card. If a creature card was discarded this way, you create
//! a 1/2 white Moogle creature token with lifelink. Then if a noncreature card
//! was discarded this way, put a +1/+1 counter on each Moogle you control."
//!
//! Two defects, both fixed in this change:
//! - the subject "each player who discarded a card this way" lowered to an
//!   `unbound_subject` gap, so nobody drew (CR 608.2c + CR 701.9a);
//! - the "… was discarded this way" gates read whatever the node before them
//!   moved — the drawn cards after the Draw, the created Moogle after the gated
//!   Token — instead of the discarded cards they name (CR 608.2c + CR 121.1).
//!
//! Runtime rows drive the real end-step trigger through `GameRunner`.

use std::collections::HashMap;

use engine::database::{card_db::CardDatabase, mtgjson::AtomicCard, synthesis::build_oracle_face};
use engine::game::coverage::{analyze_coverage, card_face_gaps};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, AbilityDefinition, Effect, PlayerFilter, PlayerRelation, PossessionAxis,
    ThisWayCause,
};
use engine::types::actions::GameAction;
use engine::types::confirmed_misparse::ConfirmedMisparses;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);
const SEATS: [PlayerId; 4] = [P0, P1, P2, P3];

const MOG: &str = "Lifelink\nDance — At the beginning of your end step, each player may discard a card. Each player who discarded a card this way draws a card. If a creature card was discarded this way, you create a 1/2 white Moogle creature token with lifelink. Then if a noncreature card was discarded this way, put a +1/+1 counter on each Moogle you control.";

/// MTGJSON AtomicCards entry for Mog (fields the face builder reads).
const MOG_ATOMIC: &str = r#"{"colorIdentity": ["R", "W"], "colors": ["R", "W"], "identifiers": {"scryfallOracleId": "f18cce8c-8fb6-49a7-8f79-2529c0ee3ecc"}, "keywords": ["Dance", "Lifelink"], "layout": "normal", "manaCost": "{1}{R}{W}", "manaValue": 3.0, "name": "Mog, Moogle Warrior", "power": "1", "subtypes": ["Moogle", "Warrior"], "supertypes": ["Legendary"], "text": "Lifelink\nDance — At the beginning of your end step, each player may discard a card. Each player who discarded a card this way draws a card. If a creature card was discarded this way, you create a 1/2 white Moogle creature token with lifelink. Then if a noncreature card was discarded this way, put a +1/+1 counter on each Moogle you control.", "toughness": "2", "type": "Legendary Creature — Moogle Warrior", "types": ["Creature"]}"#;

const SYPHON_MIND: &str =
    "Each other player discards a card. You draw a card for each card discarded this way.";

fn chain(def: &AbilityDefinition) -> Vec<&AbilityDefinition> {
    let mut nodes = vec![def];
    let mut cur = def;
    while let Some(sub) = cur.sub_ability.as_deref() {
        nodes.push(sub);
        cur = sub;
    }
    nodes
}

fn this_way_head_filter_debug(condition: Option<&AbilityCondition>) -> String {
    let condition = condition.expect("gated node carries a condition");
    assert!(
        condition.zone_changed_this_way_head().is_some(),
        "the gate must be a root 'this way' head, got {condition:?}"
    );
    format!("{condition:?}")
}

/// R2 (shape). CR 608.2c + CR 701.9a: the four sentences lower to
/// Discard (optional, every player) → Draw scoped to the players who possessed
/// a discarded card → creature-gated Token → noncreature-gated PutCounterAll,
/// with no gap anywhere on the face.
#[test]
fn mog_dance_parses_without_gaps() {
    let parsed = parse_oracle_text(
        MOG,
        "Mog, Moogle Warrior",
        &["Dance".to_string(), "Lifelink".to_string()],
        &["Creature".to_string()],
        &["Moogle".to_string(), "Warrior".to_string()],
    );
    // Reach guard: one trigger (the Lifelink line is a keyword, not an ability).
    assert_eq!(parsed.triggers.len(), 1, "one Dance trigger");
    assert!(parsed.abilities.is_empty(), "{:?}", parsed.abilities);
    let dbg = format!("{parsed:?}");
    assert!(!dbg.contains("Unimplemented"), "no gap: {dbg}");

    let execute = parsed.triggers[0].execute.as_deref().expect("trigger body");
    let nodes = chain(execute);
    assert_eq!(nodes.len(), 4, "Discard → Draw → Token → PutCounterAll");

    assert!(matches!(&*nodes[0].effect, Effect::Discard { .. }));
    assert!(nodes[0].optional, "each player MAY discard");
    assert_eq!(nodes[0].player_scope, Some(PlayerFilter::All));

    assert!(matches!(&*nodes[1].effect, Effect::Draw { .. }));
    match &nodes[1].player_scope {
        Some(PlayerFilter::TrackedSetPossessor {
            relation: PlayerRelation::All,
            possession: PossessionAxis::Owner,
            caused_by: Some(ThisWayCause::Discarded),
            ..
        }) => {}
        other => panic!("the Draw is scoped to the discarders, got {other:?}"),
    }

    assert!(matches!(&*nodes[2].effect, Effect::Token { .. }));
    let creature_gate = this_way_head_filter_debug(nodes[2].condition.as_ref());
    assert!(creature_gate.contains("Creature"), "{creature_gate}");
    assert!(matches!(&*nodes[3].effect, Effect::PutCounterAll { .. }));
    let noncreature_gate = this_way_head_filter_debug(nodes[3].condition.as_ref());
    assert!(noncreature_gate.contains("Non"), "{noncreature_gate}");

    // Coverage: the published face has no gap.
    let atomic: AtomicCard = serde_json::from_str(MOG_ATOMIC).unwrap();
    let mut face = build_oracle_face(&atomic, None);
    let key = "mog, moogle warrior";
    ConfirmedMisparses::committed().stamp_all(std::iter::once((key, &mut face)));
    assert!(
        face.keywords.contains(&Keyword::Lifelink),
        "reach: Lifelink"
    );
    assert_eq!(card_face_gaps(&face), Vec::<String>::new());
    let faces = std::collections::BTreeMap::from([(key, face)]);
    let db = CardDatabase::from_json_str(&serde_json::to_string(&faces).unwrap()).unwrap();
    let coverage = analyze_coverage(&db);
    assert!(coverage.cards[0].supported, "{:?}", coverage.cards[0]);
}

// ---------------------------------------------------------------------------
// Runtime rows
// ---------------------------------------------------------------------------

/// Four seats; P0 controls Mog (Legendary Creature — Moogle Warrior).
fn four_seats() -> GameScenario {
    let mut scenario = GameScenario::new_n_player(4, 11);
    scenario.at_phase(Phase::PostCombatMain);
    scenario
}

fn add_mog(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, "Mog, Moogle Warrior", 1, 2, MOG)
        .with_subtypes(vec!["Moogle", "Warrior"])
        .id()
}

fn hand_len(runner: &GameRunner, player: PlayerId) -> i64 {
    runner.state().players[usize::from(player.0)].hand.len() as i64
}

fn library_len(runner: &GameRunner, player: PlayerId) -> i64 {
    runner.state().players[usize::from(player.0)].library.len() as i64
}

fn snapshot(runner: &GameRunner) -> Vec<(i64, i64)> {
    SEATS
        .iter()
        .map(|&p| (hand_len(runner, p), library_len(runner, p)))
        .collect()
}

fn deltas(after: &[(i64, i64)], before: &[(i64, i64)]) -> Vec<(i64, i64)> {
    after
        .iter()
        .zip(before)
        .map(|(a, b)| (a.0 - b.0, a.1 - b.1))
        .collect()
}

fn moogle_tokens(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.is_token && o.zone == Zone::Battlefield && o.name == "Moogle")
        .map(|o| o.id)
        .collect()
}

fn p1p1(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

#[derive(Debug, PartialEq, Eq)]
enum Prompt {
    Optional(PlayerId),
    Discard(PlayerId),
}

/// Advance to P0's end step and resolve Mog's trigger. Each seat answers its
/// optional prompt with `accept[seat]`; a `DiscardChoice` picks `picks[seat]`.
/// Returns every prompt in the order it appeared.
fn run_end_step(
    runner: &mut GameRunner,
    accept: [bool; 4],
    picks: &HashMap<PlayerId, ObjectId>,
) -> Vec<Prompt> {
    runner.advance_to_end_step();
    assert_eq!(runner.state().phase, Phase::End);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the Dance trigger is on the stack"
    );
    settle(runner, accept, picks)
}

fn settle(
    runner: &mut GameRunner,
    accept: [bool; 4],
    picks: &HashMap<PlayerId, ObjectId>,
) -> Vec<Prompt> {
    let mut prompts = Vec::new();
    for _ in 0..64 {
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OptionalEffectChoice { player, .. } => {
                prompts.push(Prompt::Optional(*player));
                GameAction::DecideOptionalEffect {
                    accept: accept[usize::from(player.0)],
                }
            }
            WaitingFor::DiscardChoice { player, cards, .. } => {
                prompts.push(Prompt::Discard(*player));
                let pick = picks
                    .get(player)
                    .copied()
                    .unwrap_or_else(|| panic!("no discard pick staged for {player:?}"));
                assert!(cards.contains(&pick), "{player:?}'s pick is offered");
                GameAction::SelectCards { cards: vec![pick] }
            }
            other => panic!("unexpected prompt {other:?}"),
        };
        runner.act(action).expect("action accepted");
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "resolution completed: {:?}",
        runner.state().waiting_for
    );
    prompts
}

/// Stage four basic lands on top of every seat's library.
fn land_libraries(scenario: &mut GameScenario) {
    for seat in SEATS {
        for n in 0..4 {
            scenario.add_land_to_library_top(seat, &format!("Plains {}", n));
        }
    }
}

/// R3 — the issue's acceptance case. CR 608.2c + CR 701.9a + CR 101.4: P0
/// discards a creature card, P1 an instant, P2 declines, P3 a creature card.
/// The discarders each draw one; one Moogle (Mog ruling: additional creature
/// cards make no additional tokens); Mog and the token each get one counter.
///
/// Every library top is a land, so a gate that read the drawn cards would see
/// no creature card (no token); a counter gate that read the created Moogle
/// would see no noncreature card (no counters).
#[test]
fn mog_dance_four_players_acceptance() {
    let mut scenario = four_seats();
    let mog = add_mog(&mut scenario);
    land_libraries(&mut scenario);
    let p0_creature = scenario
        .add_creature_to_hand(P0, "Stitcher's Supplier", 1, 1)
        .id();
    scenario.add_bolt_to_hand(P0);
    let p1_bolt = scenario.add_bolt_to_hand(P1);
    scenario.add_bolt_to_hand(P2);
    scenario.add_land_to_hand(P2, "Island");
    let p3_creature = scenario
        .add_creature_to_hand(P3, "Grizzly Bears", 2, 2)
        .id();
    scenario.add_land_to_hand(P3, "Swamp");
    let mut runner = scenario.build();
    let picks = HashMap::from([(P0, p0_creature), (P3, p3_creature)]);

    let before_tokens = moogle_tokens(&runner);
    assert!(before_tokens.is_empty());
    // Measure from the end step's start (nothing else draws in between).
    let before = snapshot(&runner);
    let prompts = run_end_step(&mut runner, [true, true, false, true], &picks);
    let optional: Vec<&Prompt> = prompts
        .iter()
        .filter(|p| matches!(p, Prompt::Optional(_)))
        .collect();
    assert_eq!(
        optional,
        vec![
            &Prompt::Optional(P0),
            &Prompt::Optional(P1),
            &Prompt::Optional(P2),
            &Prompt::Optional(P3)
        ],
        "APNAP order (CR 101.4): {prompts:?}"
    );
    let discards: Vec<&Prompt> = prompts
        .iter()
        .filter(|p| matches!(p, Prompt::Discard(_)))
        .collect();
    assert_eq!(
        discards,
        vec![&Prompt::Discard(P0), &Prompt::Discard(P3)],
        "only the two-card accepting seats choose; P1's lone card is forced"
    );

    // Reach guard: the discarded cards are in their owners' graveyards.
    for id in [p0_creature, p1_bolt, p3_creature] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard);
    }
    assert_eq!(
        deltas(&snapshot(&runner), &before),
        vec![(0, -1), (0, -1), (0, 0), (0, -1)],
        "P0, P1 and P3 discard one and draw one; P2 is untouched"
    );

    let tokens = moogle_tokens(&runner);
    assert_eq!(tokens.len(), 1, "exactly one Moogle token: {tokens:?}");
    let token = &runner.state().objects[&tokens[0]];
    assert_eq!(token.controller, P0);
    assert_eq!((token.base_power, token.base_toughness), (Some(1), Some(2)));
    assert_eq!(token.color, vec![ManaColor::White]);
    assert!(token.has_keyword(&Keyword::Lifelink));
    assert_eq!(p1p1(&runner, mog), 1, "Mog gets one counter");
    assert_eq!(p1p1(&runner, tokens[0]), 1, "the token gets one counter");
}

/// R4 — nobody discards. CR 608.2c + CR 608.2h: an earlier resolution's
/// "discarded this way" set (Syphon Mind's, this turn) must not admit anyone to
/// Mog's Draw clause.
#[test]
fn mog_dance_nobody_discards_ignores_an_earlier_discard_set() {
    let mut scenario = four_seats();
    let mog = add_mog(&mut scenario);
    land_libraries(&mut scenario);
    let syphon = scenario
        .add_spell_to_hand_from_oracle(P0, "Syphon Mind", false, SYPHON_MIND)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.add_bolt_to_hand(P0);
    let mut picks = HashMap::new();
    for seat in [P1, P2, P3] {
        let bolt = scenario.add_bolt_to_hand(seat);
        scenario.add_land_to_hand(seat, "Mountain");
        picks.insert(seat, bolt);
    }
    let mut runner = scenario.build();

    let p0_hand = hand_len(&runner, P0);
    runner.cast(syphon).commit();
    settle(&mut runner, [false; 4], &picks);
    // Positive control: Syphon Mind's tracked discard set exists (P0 drew 3).
    assert_eq!(
        hand_len(&runner, P0),
        p0_hand - 1 + 3,
        "Syphon Mind drew three"
    );

    let before = snapshot(&runner);
    let prompts = run_end_step(&mut runner, [false; 4], &HashMap::new());
    assert_eq!(
        prompts,
        SEATS
            .iter()
            .map(|&p| Prompt::Optional(p))
            .collect::<Vec<_>>(),
        "every seat is asked and declines"
    );
    assert_eq!(deltas(&snapshot(&runner), &before), vec![(0, 0); 4]);
    assert!(moogle_tokens(&runner).is_empty(), "no Moogle token");
    assert_eq!(p1p1(&runner, mog), 0, "no counters");
}

/// R5 — only a noncreature card is discarded. CR 608.2c + CR 121.1: the token
/// gate names the discarded cards, not the drawn creature card on top of P1's
/// library, so no token; the counter gate is met (Mog ruling).
#[test]
fn mog_dance_only_noncreature_discarded_counts_but_no_token() {
    let mut scenario = four_seats();
    let mog = add_mog(&mut scenario);
    for seat in SEATS {
        for n in 0..3 {
            scenario
                .add_spell_to_library_top(seat, &format!("Library Beast {n}"), false)
                .as_creature();
        }
        scenario.add_land_to_hand(seat, "Forest");
    }
    let p1_bolt = scenario.add_bolt_to_hand(P1);
    let mut runner = scenario.build();
    let picks = HashMap::from([(P1, p1_bolt)]);

    let before = snapshot(&runner);
    let prompts = run_end_step(&mut runner, [false, true, false, false], &picks);
    assert!(prompts.contains(&Prompt::Discard(P1)), "{prompts:?}");
    assert_eq!(runner.state().objects[&p1_bolt].zone, Zone::Graveyard);
    assert_eq!(
        deltas(&snapshot(&runner), &before),
        vec![(0, 0), (0, -1), (0, 0), (0, 0)],
        "only P1 discards and draws"
    );
    assert!(
        moogle_tokens(&runner).is_empty(),
        "no creature card discarded"
    );
    assert_eq!(p1p1(&runner, mog), 1, "a noncreature card was discarded");
}
