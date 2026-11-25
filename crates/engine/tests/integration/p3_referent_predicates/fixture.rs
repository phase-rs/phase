//! Shared fixture helpers: real printed cards (`add_*`), typed mana for printed costs, the cast/pass driver, and
//! counter reads.
#![allow(dead_code)]

use super::oracle::Printed;
use engine::game::scenario::{CardBuilder, GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{parse_mtgjson_mana_cost, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

pub fn cost(printed: &str) -> ManaCost {
    parse_mtgjson_mana_cost(printed)
}

/// Split a printed type line ("Legendary Creature — Human Wizard") into (supertype and card-type words, subtypes).
fn split_type_line(type_line: &str) -> (Vec<&str>, Vec<&str>) {
    let (head, subs) = type_line.split_once(" — ").unwrap_or((type_line, ""));
    (
        head.split_whitespace().collect(),
        subs.split_whitespace().collect(),
    )
}

/// Give a freshly staged object the printed cost, supertypes, card types and subtypes of `card`. The scenario builder
/// keeps the types of the seed it staged (`scenario.rs`, `from_oracle_text`), so a card whose printed types differ
/// from the seed (an artifact creature, a legendary creature, any subtype) needs them set here;
/// `printed_cards_carry_their_printed_type_lines` checks the result for every card in the table.
fn apply_printed(b: &mut CardBuilder<'_>, card: &Printed) {
    let (words, subtypes) = split_type_line(card.type_line);
    let has = |w: &str| words.contains(&w);
    if has("Legendary") {
        b.as_legendary();
    }
    // The staging helper seeds Creature, Artifact, Enchantment, Instant or Sorcery; only the artifact creature needs
    // a second core type.
    if has("Artifact") && has("Creature") {
        b.as_artifact_creature();
    }
    if !subtypes.is_empty() {
        b.with_subtypes(subtypes);
    }
    // The production path hands the parser the card's types and its lower-cased MTGJSON `keywords`
    // (`prepare_oracle_parser_input`), which is how a keyword-only line ("Indestructible", "Epic") is read as a keyword.
    let keywords: Vec<String> = card
        .keywords
        .iter()
        .map(|k| k.to_ascii_lowercase())
        .collect();
    let keywords: Vec<&str> = keywords.iter().map(String::as_str).collect();
    b.from_oracle_text_with_keywords(&keywords, card.oracle);
    b.with_mana_cost(cost(card.cost));
}

fn power_toughness(card: &Printed) -> (i32, i32) {
    card.pt
        .unwrap_or_else(|| panic!("{} has no printed power/toughness", card.name))
}

/// A printed instant or sorcery in `player`'s hand, with its verbatim Oracle text, cost and type line.
pub fn add_spell(sc: &mut GameScenario, player: PlayerId, card: &Printed) -> ObjectId {
    let is_instant = split_type_line(card.type_line).0.contains(&"Instant");
    let mut b = sc.add_spell_to_hand_from_oracle(player, card.name, is_instant, card.oracle);
    apply_printed(&mut b, card);
    b.id()
}

/// A printed creature card in `player`'s hand.
pub fn add_creature_in_hand(sc: &mut GameScenario, player: PlayerId, card: &Printed) -> ObjectId {
    let (p, t) = power_toughness(card);
    let mut b = sc.add_creature_to_hand_from_oracle(player, card.name, p, t, card.oracle);
    apply_printed(&mut b, card);
    b.id()
}

/// A printed creature on `player`'s battlefield.
pub fn add_creature_on_battlefield(
    sc: &mut GameScenario,
    player: PlayerId,
    card: &Printed,
) -> ObjectId {
    let (p, t) = power_toughness(card);
    let mut b = sc.add_creature_from_oracle(player, card.name, p, t, card.oracle);
    apply_printed(&mut b, card);
    b.id()
}

/// A printed artifact or enchantment on `player`'s battlefield.
pub fn add_noncreature_permanent(
    sc: &mut GameScenario,
    player: PlayerId,
    card: &Printed,
) -> ObjectId {
    let (words, _) = split_type_line(card.type_line);
    let mut b = if words.contains(&"Artifact") {
        sc.add_artifact_from_oracle(player, card.name, card.oracle)
    } else {
        sc.add_enchantment_from_oracle(player, card.name, card.oracle)
    };
    apply_printed(&mut b, card);
    b.id()
}

/// Mana that pays every printed cost in `costs` (digits as colorless, one unit per colored symbol; `{X}`
/// contributes nothing) plus `extra` colorless units for an announced X.
pub fn mana_for(costs: &[&str], extra: usize) -> Vec<ManaUnit> {
    let mut out = Vec::new();
    for printed in costs {
        for sym in printed.split('{').filter_map(|s| s.strip_suffix('}')) {
            let kinds: Vec<ManaType> = match sym {
                "W" => vec![ManaType::White],
                "U" => vec![ManaType::Blue],
                "B" => vec![ManaType::Black],
                "R" => vec![ManaType::Red],
                "G" => vec![ManaType::Green],
                "X" => vec![],
                n => vec![ManaType::Colorless; n.parse::<usize>().expect("generic symbol")],
            };
            out.extend(kinds);
        }
    }
    out.extend(std::iter::repeat_n(ManaType::Colorless, extra));
    out.into_iter()
        .map(|t| ManaUnit::new(t, ObjectId(0), false, vec![]))
        .collect()
}

pub fn scenario() -> GameScenario {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    sc
}

/// Announce `object_id` through the production `CastSpell` action (targets chosen by the engine when only one is legal).
pub fn cast_spell(runner: &mut GameRunner, object_id: ObjectId) {
    let card_id = runner.state().objects[&object_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap_or_else(|e| panic!("cast {object_id:?}: {e:?}"));
}

/// CR 107.3a + CR 202.3e: announce X, then read the committed spell's mana value.
pub fn cast_ballista_at_x(runner: &mut GameRunner, ballista: ObjectId, x: u32) -> u32 {
    let cast = runner.cast(ballista).x(x).commit();
    let entry = cast
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == ballista)
        .expect("Ballista was committed to the stack");
    assert!(matches!(entry.kind, StackEntryKind::Spell { .. }));
    let object = &cast.state().objects[&entry.id];
    assert_eq!(object.zone, Zone::Stack);
    assert_eq!(
        object.cost_x_paid,
        Some(x),
        "announced X on the committed spell"
    );
    object.effective_mana_value()
}

/// Pass priority until the stack is empty or a non-priority decision appears; returns the decision.
pub fn drain(runner: &mut GameRunner) -> WaitingFor {
    for _ in 0..60 {
        if runner.state().stack.is_empty() {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => return other,
        }
    }
    runner.state().waiting_for.clone()
}

pub fn is_proliferate(w: &WaitingFor) -> bool {
    matches!(w, WaitingFor::ProliferateChoice { .. })
}

pub fn plus_counters(state: &GameState, id: ObjectId) -> u32 {
    state
        .objects
        .get(&id)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

pub fn life(state: &GameState, p: PlayerId) -> i32 {
    state.players[p.0 as usize].life
}

pub fn zone(state: &GameState, id: ObjectId) -> Zone {
    state.objects[&id].zone
}

pub const COUNTER_HOLDER: &str = "Counter Holder";

/// A textless filler permanent that carries a +1/+1 counter, so a proliferate prompt has something to offer
/// (reach guard for every "no proliferate" negative).
pub fn add_counter_holder(sc: &mut GameScenario) -> ObjectId {
    let mut b = sc.add_creature(P1, COUNTER_HOLDER, 1, 1);
    b.with_plus_counters(1);
    b.id()
}

pub fn opp(p: PlayerId) -> PlayerId {
    if p == P0 {
        P1
    } else {
        P0
    }
}

/// Every card in the table, staged through the helper for its kind, carries exactly its printed type line,
/// printed power/toughness, mana cost and name.
#[test]
fn printed_cards_carry_their_printed_type_lines() {
    use super::oracle::ALL;
    for card in ALL {
        let mut sc = scenario();
        let (words, subtypes) = split_type_line(card.type_line);
        let id = if words.contains(&"Instant") || words.contains(&"Sorcery") {
            add_spell(&mut sc, P0, card)
        } else if words.contains(&"Creature") {
            add_creature_on_battlefield(&mut sc, P0, card)
        } else {
            add_noncreature_permanent(&mut sc, P0, card)
        };
        let runner = sc.build();
        let obj = &runner.state().objects[&id];
        let mut want: Vec<String> = words.iter().map(|w| w.to_string()).collect();
        let mut got: Vec<String> = obj
            .card_types
            .supertypes
            .iter()
            .map(|t| format!("{t:?}"))
            .chain(obj.card_types.core_types.iter().map(|t| format!("{t:?}")))
            .collect();
        want.sort();
        got.sort();
        assert_eq!(got, want, "{}: supertypes and card types", card.name);
        assert_eq!(obj.card_types.subtypes, subtypes, "{}: subtypes", card.name);
        assert_eq!(
            obj.card_types, obj.base_card_types,
            "{}: base types",
            card.name
        );
        assert_eq!(obj.name, card.name);
        assert_eq!(obj.mana_cost, cost(card.cost), "{}: mana cost", card.name);
        assert_eq!(
            card.pt
                .map(|(p, t)| (Some(p), Some(t)))
                .unwrap_or((None, None)),
            (obj.power, obj.toughness),
            "{}: power/toughness",
            card.name
        );
    }
}
