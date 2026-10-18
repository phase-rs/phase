//! Ordinary instruction-wide reveal membership. All synthetic texts are explicit
//! consumer fixtures; printed siblings retain their complete pinned Oracle text.
use engine::game::derived_views::ClientGameStateRef;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, Effect, EffectKind, QuantityExpr, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const SCALAR: &str = "Each player reveals the top card of their library. You gain life equal to the number of cards revealed this way.";
const TOP_TWO: &str = "Each player reveals the top two cards of their library. You gain life equal to the number of cards revealed this way.";
const PAYMENT: &str =
    "Each player reveals the top card of their library unless they pay 1 life. You gain 3 life.";
const SEER: &str = "Flying\nAt the beginning of your upkeep, each player reveals the top card of their library, loses life equal to that card's mana value, then puts it into their hand.";
const PARKER: &str = "At the beginning of your end step, two target players each reveal the top card of their library. They each lose life equal to the mana value of the card revealed by the other player. Then they each put the card they revealed into their hand.";
const CONFIDANT: &str = "At the beginning of your upkeep, reveal the top card of your library and put that card into your hand. You lose life equal to its mana value.";

fn supported_chain(def: &AbilityDefinition) {
    assert!(
        !matches!(*def.effect, Effect::Unimplemented { .. }),
        "{def:#?}"
    );
    if let Some(sub) = &def.sub_ability {
        supported_chain(sub);
    }
    if let Some(other) = &def.else_ability {
        supported_chain(other);
    }
    for mode in &def.mode_abilities {
        supported_chain(mode);
    }
}

fn assert_supported(text: &str, name: &str, types: &[&str], keywords: &[&str]) {
    let parsed = parse_oracle_text(
        text,
        name,
        &keywords.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        &types.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        &[],
    );
    assert!(parsed.parse_warnings.is_empty(), "{parsed:#?}");
    assert!(
        !parsed.abilities.is_empty() || !parsed.triggers.is_empty(),
        "{parsed:#?}"
    );
    for def in &parsed.abilities {
        supported_chain(def);
    }
    for trigger in &parsed.triggers {
        supported_chain(trigger.execute.as_deref().expect("trigger body"));
    }
}

fn spell(scenario: &mut GameScenario, player: PlayerId, text: &str) -> ObjectId {
    assert_supported(text, "Synthetic aggregate", &["Instant"], &[]);
    scenario
        .add_spell_to_hand_from_oracle(player, "Synthetic aggregate", true, text)
        .id()
}

fn reveals(events: &[GameEvent]) -> Vec<(PlayerId, Vec<ObjectId>)> {
    events
        .iter()
        .filter_map(|e| match e {
            GameEvent::CardsRevealed {
                player, card_ids, ..
            } => Some((*player, card_ids.clone())),
            _ => None,
        })
        .collect()
}

fn gains(events: &[GameEvent]) -> Vec<(PlayerId, i32)> {
    events
        .iter()
        .filter_map(|e| match e {
            GameEvent::LifeChanged {
                player_id, amount, ..
            } if *amount > 0 => Some((*player_id, *amount)),
            _ => None,
        })
        .collect()
}

fn assert_no_library_moves(events: &[GameEvent], ids: &[ObjectId]) {
    assert!(!events
        .iter()
        .any(|e| matches!(e, GameEvent::ZoneChanged { object_id, .. } if ids.contains(object_id))));
}

#[test]
fn ordinary_reveal_scalar_cast_counts_complete_instruction() {
    for caster in [P0, P1] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        s.with_library_top(P0, &["Public zero", "Secret zero"]);
        s.with_library_top(P1, &["Public one", "Secret one"]);
        let id = spell(&mut s, caster, SCALAR);
        let mut r = s.build();
        r.state_mut().active_player = caster;
        r.state_mut().priority_player = caster;
        r.state_mut().waiting_for = WaitingFor::Priority { player: caster };
        let libraries: Vec<_> = [P0, P1]
            .iter()
            .map(|&p| r.state().library_of(p).to_vec())
            .collect();
        let out = r.cast(id).resolve();
        out.assert_life_delta(caster, 2);
        out.assert_life_delta(if caster == P0 { P1 } else { P0 }, 0);
        let actual = reveals(out.events());
        assert_eq!(actual.len(), 2);
        for (p, library) in [P0, P1].into_iter().zip(&libraries) {
            assert!(actual.contains(&(p, vec![library[0]])));
            assert_eq!(out.state().library_of(p), library.as_slice());
            out.assert_zone(library, Zone::Library);
        }
        assert_eq!(gains(out.events()), vec![(caster, 2)]);
        assert_no_library_moves(out.events(), &libraries.concat());
    }
}

#[test]
fn ordinary_reveal_tail_runs_after_all_seats_once() {
    for empty_first in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        if !empty_first {
            s.with_library_top(P0, &["Zero top"]);
        }
        s.with_library_top(P1, &["One top"]);
        let id = spell(&mut s, P0, &format!("{SCALAR} You gain 3 life."));
        let mut r = s.build();
        let out = r.cast(id).resolve();
        let count = if empty_first { 1 } else { 2 };
        out.assert_life_delta(P0, count + 3);
        out.assert_life_delta(P1, 0);
        assert_eq!(reveals(out.events()).len(), count as usize);
        assert_eq!(gains(out.events()), vec![(P0, count), (P0, 3)]);
        let last_reveal = out
            .events()
            .iter()
            .rposition(|e| matches!(e, GameEvent::CardsRevealed { .. }))
            .unwrap();
        let first_gain = out
            .events()
            .iter()
            .position(|e| matches!(e, GameEvent::LifeChanged { amount, .. } if *amount > 0))
            .unwrap();
        assert!(last_reveal < first_gain);
    }
}

#[test]
fn ordinary_reveal_empty_seats_publish_fresh_empty() {
    for empty in 0..3 {
        let mut s = GameScenario::new_n_player(3, 42);
        s.at_phase(Phase::PreCombatMain);
        for seat in 0..3 {
            if seat != empty {
                s.with_library_top(PlayerId(seat), &["Nonempty seat"]);
            }
        }
        let id = spell(&mut s, P0, SCALAR);
        let mut r = s.build();
        let expected: Vec<_> = (0..3)
            .filter(|&seat| seat != empty)
            .map(|seat| {
                (
                    PlayerId(seat),
                    r.state().library_of(PlayerId(seat)).to_vec(),
                )
            })
            .collect();
        let out = r.cast(id).resolve();
        out.assert_life_delta(P0, 2);
        assert_eq!(reveals(out.events()), expected);
    }
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let decoy = s.add_card_to_library_top(P0, "Earlier real reveal");
    let id = spell(
        &mut s,
        P0,
        &format!("Reveal the top card of your library. You mill a card. {SCALAR} You gain 3 life."),
    );
    let mut r = s.build();
    let out = r.cast(id).resolve();
    assert_eq!(reveals(out.events()), vec![(P0, vec![decoy])]);
    out.assert_zone(&[decoy], Zone::Graveyard);
    assert!(out
        .events()
        .iter()
        .any(|e| matches!(e, GameEvent::ZoneChanged { object_id, .. } if *object_id == decoy)));
    out.assert_life_delta(P0, 3);
    assert!(out.state().last_revealed_ids.is_empty());
    assert!(out
        .state()
        .tracked_object_sets
        .iter()
        .max_by_key(|(id, _)| id.0)
        .unwrap()
        .1
        .is_empty());
    assert_eq!(
        gains(out.events()).iter().filter(|(_, n)| *n == 3).count(),
        1
    );
}

#[test]
fn ordinary_reveal_replaces_previous_immediate_result() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_library_top(P0, &["Decoy", "Current zero", "Secret zero"]);
    s.with_library_top(P1, &["Current one", "Secret one"]);
    let id = spell(
        &mut s,
        P0,
        &format!("Reveal the top card of your library. You mill a card. {SCALAR}"),
    );
    let mut r = s.build();
    let a = r.state().library_of(P0).to_vec();
    let b = r.state().library_of(P1).to_vec();
    let out = r.cast(id).resolve();
    assert_eq!(
        reveals(out.events()),
        vec![(P0, vec![a[0]]), (P0, vec![a[1]]), (P1, vec![b[0]])]
    );
    out.assert_zone(&[a[0]], Zone::Graveyard);
    out.assert_life_delta(P0, 2);
    assert_eq!(out.state().last_revealed_ids, vec![a[1], b[0]]);
    assert_eq!(
        out.state()
            .tracked_object_sets
            .iter()
            .max_by_key(|(id, _)| id.0)
            .unwrap()
            .1,
        &vec![a[1], b[0]]
    );
}

#[test]
fn ordinary_reveal_count_is_cards_not_seats() {
    for second_count in [0, 1, 2] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        s.with_library_top(P0, &["Zero first", "Zero second", "Zero secret"]);
        let names = ["One first", "One second"];
        s.with_library_top(P1, &names[..second_count]);
        let id = spell(&mut s, P0, TOP_TWO);
        let mut r = s.build();
        let a = r.state().library_of(P0).to_vec();
        let b = r.state().library_of(P1).to_vec();
        let out = r.cast(id).resolve();
        out.assert_life_delta(P0, 2 + second_count as i32);
        assert!(reveals(out.events()).contains(&(P0, a[..2].to_vec())));
        if !b.is_empty() {
            assert!(reveals(out.events()).contains(&(P1, b.clone())));
        }
        assert_eq!(out.state().library_of(P0), a);
        assert_eq!(out.state().library_of(P1), b);
        assert!(!reveals(out.events())
            .iter()
            .any(|(_, cards)| cards.contains(&a[2])));
        assert_no_library_moves(out.events(), &[a, b].concat());
    }
    let mut s = GameScenario::new_with_format(FormatConfig::dandan(), 2, 42);
    s.at_phase(Phase::PreCombatMain);
    s.with_library_top(P0, &["Shared first", "Shared second", "Shared secret"]);
    let id = spell(&mut s, P0, TOP_TWO);
    let mut r = s.build();
    let top = r.state().library_of(P0)[..2].to_vec();
    assert_eq!(r.state().library_of(P1)[..2], top);
    let out = r.cast(id).resolve();
    assert_eq!(
        reveals(out.events()),
        vec![(P0, top.clone()), (P1, top.clone())]
    );
    out.assert_life_delta(P0, 2);
    assert_eq!(out.state().last_revealed_ids, top);
}

#[test]
fn ordinary_scoped_reveal_unless_payment_uses_normal_choices() {
    for p0_pays in [true, false] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        s.with_library_top(P0, &["Zero top", "Zero secret"]);
        s.with_library_top(P1, &["One top", "One secret"]);
        let id = spell(&mut s, P0, PAYMENT);
        let mut r = s.build();
        let libraries: Vec<_> = [P0, P1]
            .iter()
            .map(|&p| r.state().library_of(p).to_vec())
            .collect();
        let mut cast = r.cast(id).commit();
        assert!(!cast.state().stack.is_empty(), "committed public spell");
        let mut events = Vec::new();
        for (expected, pay) in [(P0, p0_pays), (P1, !p0_pays)] {
            for _ in 0..32 {
                if matches!(cast.state().waiting_for, WaitingFor::UnlessPayment { .. }) {
                    break;
                }
                assert!(matches!(
                    cast.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                events.extend(cast.act(GameAction::PassPriority).unwrap().events);
            }
            let WaitingFor::UnlessPayment {
                player,
                ref cost,
                ref pending_effect,
                ..
            } = cast.state().waiting_for
            else {
                panic!("natural payment prompt: {:?}", cast.state().waiting_for)
            };
            assert_eq!(player, expected);
            assert!(matches!(
                pending_effect.effect,
                Effect::RevealTop { count: 1, .. }
            ));
            assert_eq!(pending_effect.controller, expected);
            assert_eq!(
                *cost,
                AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 1 }
                }
            );
            events.extend(cast.act(GameAction::PayUnlessCost { pay }).unwrap().events);
        }
        let out = cast.resolve();
        events.extend_from_slice(out.events());
        out.assert_life_delta(P0, if p0_pays { 2 } else { 3 });
        out.assert_life_delta(P1, if p0_pays { 0 } else { -1 });
        let decliner = if p0_pays { P1 } else { P0 };
        assert_eq!(
            reveals(&events),
            vec![(decliner, vec![libraries[decliner.0 as usize][0]])]
        );
        assert_eq!(
            gains(&events)
                .iter()
                .filter(|(_, amount)| *amount == 3)
                .count(),
            1
        );
        for p in [P0, P1] {
            assert_eq!(out.state().library_of(p), libraries[p.0 as usize]);
        }
        assert_no_library_moves(&events, &libraries.concat());
        assert!(events.iter().any(|event| matches!(event, GameEvent::LifeChanged { player_id, amount: -1, .. } if *player_id == if p0_pays { P0 } else { P1 })));
        let reveal_index = events
            .iter()
            .position(|event| matches!(event, GameEvent::CardsRevealed { .. }))
            .unwrap();
        let tail_index = events
            .iter()
            .position(|event| matches!(event, GameEvent::LifeChanged { amount: 3, .. }))
            .unwrap();
        assert!(reveal_index < tail_index);
    }
}

#[test]
fn ordinary_reveal_context_authority_ignores_inherited_player_target() {
    let text = "Target player gains 1 life. Reveal the top card of your library. You gain life equal to the number of cards revealed this way.";
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_library_top(P0, &["Controller top", "Controller secret"]);
    s.with_library_top(P1, &["Target top", "Target secret"]);
    let id = spell(&mut s, P0, text);
    let mut r = s.build();
    let a = r.state().library_of(P0).to_vec();
    let b = r.state().library_of(P1).to_vec();
    let out = r.cast(id).target_player(P1).resolve();
    out.assert_life_delta(P1, 1);
    out.assert_life_delta(P0, 1);
    assert_eq!(reveals(out.events()), vec![(P0, vec![a[0]])]);
    assert_eq!(out.state().library_of(P1), b);
}

#[test]
fn ordinary_reveal_repetition_nested_and_roots_are_fresh() {
    let text = format!(
        "{SCALAR} Each player mills a card. You gain 3 life. Repeat this process one more time."
    );
    let parsed = parse_oracle_text(&text, "Synthetic aggregate", &[], &["Instant".into()], &[]);
    assert_eq!(
        parsed.abilities[0].repeat_for,
        Some(QuantityExpr::Fixed { value: 2 }),
        "real counted process"
    );
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_library_top(P0, &["First zero"]);
    s.with_library_top(P1, &["First one"]);
    let id = spell(&mut s, P0, &text);
    let mut r = s.build();
    let tops = [r.state().library_of(P0)[0], r.state().library_of(P1)[0]];
    let out = r.cast(id).resolve();
    out.assert_life_delta(P0, 8);
    assert_eq!(
        reveals(out.events()),
        vec![(P0, vec![tops[0]]), (P1, vec![tops[1]])]
    );
    out.assert_zone(&tops, Zone::Graveyard);
    assert_eq!(
        out.events()
            .iter()
            .filter(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::Reveal,
                    ..
                }
            ))
            .count(),
        2
    );
    assert_eq!(
        gains(out.events()).iter().filter(|(_, n)| *n == 3).count(),
        2
    );
    assert!(out.state().last_revealed_ids.is_empty());

    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_library_top(P0, &["Mode zero"]);
    s.with_library_top(P1, &["Mode one"]);
    let mode = spell(
        &mut s,
        P0,
        &format!("Choose one —\n• {SCALAR}\n• You gain 7 life."),
    );
    let second = spell(
        &mut s,
        P0,
        &format!("Each player mills a card. {SCALAR} You gain 3 life."),
    );
    let mut r = s.build();
    let out = r.cast(mode).modes(&[0]).resolve();
    out.assert_life_delta(P0, 2);
    assert_eq!(reveals(out.events()).len(), 2);
    assert_eq!(gains(out.events()), vec![(P0, 2)]);
    let out = r.cast(second).resolve();
    out.assert_life_delta(P0, 3);
    assert!(reveals(out.events()).is_empty());
    assert!(out.state().last_revealed_ids.is_empty());
}

#[test]
fn ordinary_reveal_public_events_and_private_views() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_library_top(P0, &["Public top zero", "Private depth zero canary"]);
    s.with_library_top(P1, &["Public top one", "Private depth one canary"]);
    let id = spell(&mut s, P0, SCALAR);
    let mut r = s.build();
    let libraries: Vec<_> = [P0, P1]
        .iter()
        .map(|&p| r.state().library_of(p).to_vec())
        .collect();
    let out = r.cast(id).resolve();
    out.assert_life_delta(P0, 2);
    for (p, cards) in &reveals(out.events()) {
        assert_eq!(cards, &vec![libraries[p.0 as usize][0]]);
        assert!(out.events().iter().any(|e| matches!(e, GameEvent::CardsRevealed { player, card_names, .. } if player == p && card_names == &vec![format!("Public top {}", if *p == P0 { "zero" } else { "one" })])));
    }
    assert_eq!(reveals(out.events()).len(), 2);
    let before = out.state().last_revealed_ids.clone();
    for viewer in [Some(P0), Some(P1), None] {
        let projection = match viewer {
            Some(p) => filter_state_for_viewer(out.state(), p),
            None => filter_state_for_unseated_viewer(out.state()),
        };
        let wire = serde_json::to_string(&ClientGameStateRef::wrap(out.state(), viewer)).unwrap();
        for (seat, canary) in ["Private depth zero canary", "Private depth one canary"]
            .iter()
            .enumerate()
        {
            let secret = libraries[seat][1];
            assert_eq!(out.state().objects[&secret].name, *canary);
            assert_eq!(projection.objects[&secret].name, "Hidden Card");
            assert!(!wire.contains(canary));
            assert!(!projection
                .library_of(PlayerId(seat as u8))
                .contains(&secret));
        }
        assert_eq!(projection.players[0].life, 22);
    }
    assert_eq!(out.state().last_revealed_ids, before);
    assert_no_library_moves(out.events(), &libraries.concat());
}

fn printed_permanent(s: &mut GameScenario, name: &str, text: &str, mv: u32) -> ObjectId {
    let creature = name != "Parker Luck";
    let keywords: &[&str] = if name == "Duskmantle Seer" {
        &["Flying"]
    } else {
        &[]
    };
    assert_supported(
        text,
        name,
        &[if creature { "Creature" } else { "Enchantment" }],
        keywords,
    );
    let mut card = if creature {
        s.add_creature_to_hand(
            P0,
            name,
            if mv == 4 { 4 } else { 2 },
            if mv == 4 { 4 } else { 1 },
        )
    } else {
        let mut card = s.add_spell_to_hand(P0, name, false);
        card.as_enchantment();
        card
    };
    card.with_mana_cost(ManaCost::generic(mv))
        .from_oracle_text_with_keywords(keywords, text)
        .id()
}

fn upkeep(r: &mut GameRunner) {
    r.state_mut().phase = Phase::Untap;
    r.state_mut().active_player = P0;
    r.state_mut().priority_player = P0;
    r.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    r.advance_to_upkeep();
}

fn drain_triggers(r: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..64 {
        let action = match &r.state().waiting_for {
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            WaitingFor::Priority { .. } if r.state().stack.is_empty() => return events,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected reveal sibling prompt: {other:?}"),
        };
        events.extend(r.act(action).unwrap().events);
    }
    panic!("reveal sibling did not finish before the draw step");
}

#[test]
fn reveal_sibling_authorities_remain_distinct() {
    for (name, text, source_mv) in [
        ("Duskmantle Seer", SEER, 4),
        ("Parker Luck", PARKER, 3),
        ("Dark Confidant", CONFIDANT, 2),
    ] {
        for present in [[true, true], [false, true], [true, false], [false, false]] {
            let mut s = GameScenario::new();
            s.at_phase(Phase::PreCombatMain);
            let source = printed_permanent(&mut s, name, text, source_mv);
            let mut tops = [None, None];
            for (seat, mv) in [(P0, if name == "Dark Confidant" { 3 } else { 1 }), (P1, 2)] {
                if present[seat.0 as usize] {
                    tops[seat.0 as usize] = Some(
                        s.add_spell_to_library_top(
                            seat,
                            if seat == P0 { "Zero MV" } else { "One MV" },
                            false,
                        )
                        .with_mana_cost(ManaCost::generic(mv))
                        .id(),
                    );
                }
            }
            let mut r = s.build();
            let out = r.cast(source).free_cast().resolve();
            out.assert_zone(&[source], Zone::Battlefield);
            if name == "Parker Luck" {
                r.advance_to_end_step();
                assert!(matches!(
                    r.state().waiting_for,
                    WaitingFor::TriggerTargetSelection { .. }
                ));
                for player in [P0, P1] {
                    r.act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(player)),
                    })
                    .unwrap();
                }
            } else {
                upkeep(&mut r);
            }
            assert!(
                !r.state().stack.is_empty(),
                "natural printed trigger reached the stack"
            );
            let events = drain_triggers(&mut r);
            let expected_life = match name {
                "Duskmantle Seer" => (20 - i32::from(present[0]), 20 - 2 * i32::from(present[1])),
                "Parker Luck" => (20 - 2 * i32::from(present[1]), 20 - i32::from(present[0])),
                "Dark Confidant" => (20 - 3 * i32::from(present[0]), 20),
                _ => unreachable!(),
            };
            assert_eq!(
                (r.life(P0), r.life(P1)),
                expected_life,
                "{name} {present:?}"
            );
            let expected_reveals: Vec<_> = [P0, P1]
                .into_iter()
                .filter(|&p| name != "Dark Confidant" || p == P0)
                .filter_map(|p| tops[p.0 as usize].map(|id| (p, vec![id])))
                .collect();
            assert_eq!(reveals(&events), expected_reveals, "{name} {present:?}");
            for (seat, top) in [P0, P1].into_iter().zip(tops) {
                if let Some(top) = top {
                    let moved = name != "Dark Confidant" || seat == P0;
                    assert_eq!(
                        r.state().objects[&top].zone,
                        if moved { Zone::Hand } else { Zone::Library }
                    );
                    if moved {
                        assert!(r.state().players[seat.0 as usize].hand.contains(&top));
                    }
                }
            }
            assert_eq!(r.state().objects[&source].zone, Zone::Battlefield);
        }
    }
}

#[test]
fn ordinary_reveal_multiple_trigger_roots_survive_source_departure() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let a = printed_permanent(&mut s, "Duskmantle Seer", SEER, 4);
    let b = printed_permanent(&mut s, "Duskmantle Seer", SEER, 4);
    let response = spell(&mut s, P0, "Destroy target creature.");
    let mut tops = [Vec::new(), Vec::new()];
    for (seat, values) in [(P0, [1, 3]), (P1, [2, 4])] {
        for mv in values.into_iter().rev() {
            let id = s
                .add_spell_to_library_top(seat, "Successive top", false)
                .with_mana_cost(ManaCost::generic(mv))
                .id();
            tops[seat.0 as usize].insert(0, id);
        }
    }
    let mut r = s.build();
    r.cast(a)
        .free_cast()
        .resolve()
        .assert_zone(&[a], Zone::Battlefield);
    r.cast(b)
        .free_cast()
        .resolve()
        .assert_zone(&[b], Zone::Battlefield);
    upkeep(&mut r);
    if let WaitingFor::OrderTriggers { ref triggers, .. } = r.state().waiting_for {
        let count = triggers.len();
        assert_eq!(count, 2);
        r.act(GameAction::OrderTriggers {
            order: (0..count).collect(),
        })
        .unwrap();
    }
    assert_eq!(r.state().stack.len(), 2);
    assert!(r.state().stack.iter().any(|entry| entry.source_id == a));
    assert!(r.state().stack.iter().any(|entry| entry.source_id == b));
    // The response cast resolves through the public stack; its outcome also
    // includes the already-created upkeep abilities underneath it.
    let out = r.cast(response).target_object(a).resolve();
    out.assert_zone(&[a], Zone::Graveyard);
    out.assert_zone(&[b], Zone::Battlefield);
    out.assert_life_delta(P0, -4);
    out.assert_life_delta(P1, -6);
    assert_eq!(
        reveals(out.events()),
        vec![
            (P0, vec![tops[0][0]]),
            (P1, vec![tops[1][0]]),
            (P0, vec![tops[0][1]]),
            (P1, vec![tops[1][1]])
        ]
    );
    for (seat, ids) in [P0, P1].into_iter().zip(tops) {
        out.assert_zone(&ids, Zone::Hand);
        for id in ids {
            assert!(out.state().players[seat.0 as usize].hand.contains(&id));
        }
    }
}
