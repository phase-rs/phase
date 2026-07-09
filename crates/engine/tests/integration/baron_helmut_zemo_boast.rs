//! Baron Helmut Zemo (MSH) — Boast: "Exile any number of black cards from your
//! graveyard with fifteen or more black mana symbols among their mana costs:
//! Copy those exiled cards. You may cast up to three of the copies without paying
//! their mana costs."
//!
//! Drives the REAL pipeline (parser + `GameRunner::act` activation → interactive
//! `PayCost { ExileAggregate }` → resolution → `ChooseFromZoneChoice`) through the
//! four gaps: the aggregate-threshold exile cost (`AbilityCost::ExileWithAggregate`),
//! the cost→effect tracked-set binding (CR 608.2c), the "up to three" copy cap
//! (`Effect::CastCopyOfCard.count`), and the parser fold.
//!
//! MSH is release-gated and not in the local fixture, so the tests parse the real
//! Oracle text. Each test documents the revert probe that flips its assertions.

use std::sync::Arc;

use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, AggregateFunction, Comparator, Effect,
    ObjectProperty, QuantityExpr, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId, TrackedSetId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

const ZEMO: &str = "Whenever you cast a black spell from your hand, Baron Helmut Zemo connives.\nBoast — Exile any number of black cards from your graveyard with fifteen or more black mana symbols among their mana costs: Copy those exiled cards. You may cast up to three of the copies without paying their mana costs. (Activate only if this creature attacked this turn and only once each turn.)";

fn zemo_parse() -> engine::parser::oracle::ParsedAbilities {
    parse_oracle_text(
        ZEMO,
        "Baron Helmut Zemo",
        &["Boast".to_string(), "Connive".to_string()],
        &["Creature".to_string()],
        &[
            "Human".to_string(),
            "Noble".to_string(),
            "Villain".to_string(),
        ],
    )
}

/// Create a black instant in `player`'s graveyard with `black_pips` black mana
/// symbols and a real Spell ability so its copy is a castable spell.
fn add_black_spell_to_gy(
    state: &mut GameState,
    player: PlayerId,
    name: &str,
    black_pips: usize,
) -> ObjectId {
    let cid = CardId(state.next_object_id);
    let id = create_object(
        state,
        cid,
        player,
        name.to_string(),
        engine::types::zones::Zone::Graveyard,
    );
    let cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Black; black_pips],
        generic: 0,
    };
    let obj = state.objects.get_mut(&id).expect("created");
    obj.card_types.core_types.push(CoreType::Instant);
    obj.mana_cost = cost.clone();
    obj.color = engine::game::printed_cards::derive_colors_from_mana_cost(&cost);
    obj.abilities = Arc::new(vec![AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )]);
    id
}

/// Index of Zemo's Boast activated ability (the only `CastCopyOfCard` ability).
fn boast_index(runner: &GameRunner, zemo: ObjectId) -> usize {
    runner.state().objects[&zemo]
        .abilities
        .iter()
        .position(|a| matches!(a.effect.as_ref(), Effect::CastCopyOfCard { .. }))
        .expect("Zemo must carry a CastCopyOfCard (Boast) activated ability")
}

/// Build a battlefield Zemo that has attacked this turn, with `gy` black graveyard
/// spells, P0 holding priority on an empty stack in a main phase.
fn setup(gy: &[usize]) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let zemo = scenario
        .add_creature_from_oracle(P0, "Baron Helmut Zemo", 3, 3, ZEMO)
        .id();
    let mut runner = scenario.build();
    let gy_ids: Vec<ObjectId> = gy
        .iter()
        .enumerate()
        .map(|(i, &pips)| add_black_spell_to_gy(runner.state_mut(), P0, &format!("Gy{i}"), pips))
        .collect();
    // CR 702.142a: Boast requires the source to have attacked this turn.
    runner.state_mut().creatures_attacked_this_turn.insert(zemo);
    runner.state_mut().turn_number = 3;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    (runner, zemo, gy_ids)
}

/// Pass priority (both players) until the wait is no longer a priority window
/// (e.g. an interactive choice opens) or the stack drains.
fn drain_priority(runner: &mut GameRunner) {
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            _ => return,
        }
    }
}

// ---------------------------------------------------------------------------
// Gap A/B/C/D — parser AST (the load-bearing shape the runtime consumes).
// ---------------------------------------------------------------------------

/// Zemo's Boast must parse to the aggregate-threshold exile cost and the
/// count-capped copy-and-cast effect, with ZERO parse warnings.
///
/// Revert probe: with the pre-change parser the cost is `EffectCost{ChangeZone}`,
/// the effect is `CopySpell{Any}`+`CastFromZone{Any}`, `count` is dropped, and a
/// `TargetFallback "those exiled cards"` warning fires — every assertion below
/// flips.
#[test]
fn zemo_parses_to_exile_aggregate_cost_and_capped_cast_copy() {
    let parsed = zemo_parse();
    assert_eq!(
        parsed.parse_warnings.len(),
        0,
        "expected zero parse warnings, got {:?}",
        parsed.parse_warnings
    );
    let boast = parsed
        .abilities
        .iter()
        .find(|a| matches!(a.effect.as_ref(), Effect::CastCopyOfCard { .. }))
        .expect("Boast activated ability");

    // Cost: aggregate-threshold exile.
    match boast.cost.as_ref().expect("Boast has a cost") {
        AbilityCost::ExileWithAggregate {
            function,
            property,
            comparator,
            value,
            zone,
            filter,
        } => {
            assert_eq!(*function, AggregateFunction::Sum);
            assert_eq!(*property, ObjectProperty::ManaSymbolCount(ManaColor::Black));
            assert_eq!(*comparator, Comparator::GE);
            assert_eq!(*value, 15);
            assert_eq!(*zone, engine::types::zones::Zone::Graveyard);
            // Filter is black cards in your graveyard.
            assert!(
                matches!(filter, TargetFilter::Typed(_)),
                "filter should be a typed black-graveyard-card filter, got {filter:?}"
            );
        }
        other => panic!("expected ExileWithAggregate cost, got {other:?}"),
    }

    // Effect: copy the exiled set, cast up to three.
    match boast.effect.as_ref() {
        Effect::CastCopyOfCard { target, count, .. } => {
            assert_eq!(
                *target,
                TargetFilter::TrackedSet {
                    id: TrackedSetId(0)
                }
            );
            assert_eq!(*count, Some(QuantityExpr::Fixed { value: 3 }));
        }
        other => panic!("expected CastCopyOfCard, got {other:?}"),
    }
    // The fold dropped the redundant CopySpell sub-ability.
    assert!(
        boast.sub_ability.is_none(),
        "the CopySpell/CastFromZone pair must fold into a single CastCopyOfCard"
    );
}

// ---------------------------------------------------------------------------
// Gap A — cost payability threshold (production path: ActivateAbility).
// ---------------------------------------------------------------------------

/// 15 black symbols (5 × {B}{B}{B}) → the Boast activation is payable and opens
/// the `PayCost { ExileAggregate }` prompt over all five graveyard cards.
///
/// Revert probe: changing the parsed threshold `value` to 14 (or `comparator` to
/// `GT`) makes the 14-symbol graveyard below payable, flipping the negative test.
#[test]
fn zemo_boast_payable_at_fifteen_symbols() {
    let (mut runner, zemo, gy) = setup(&[3, 3, 3, 3, 3]); // 15 black symbols
    let idx = boast_index(&runner, zemo);
    runner
        .act(GameAction::ActivateAbility {
            source_id: zemo,
            ability_index: idx,
        })
        .expect("Boast is payable with 15 black symbols");
    match &runner.state().waiting_for {
        WaitingFor::PayCost { choices, .. } => {
            assert_eq!(choices.len(), 5, "all five graveyard cards are eligible");
            for id in &gy {
                assert!(choices.contains(id));
            }
        }
        other => panic!("expected PayCost(ExileAggregate), got {other:?}"),
    }
}

/// 14 black symbols (4 × {B}{B}{B} + {B}{B}) → the Boast activation is NOT payable
/// (CR 118.3): exiling EVERY eligible card still falls one symbol short of 15.
///
/// Revert probe: as above — dropping the threshold to 14 accepts this activation.
#[test]
fn zemo_boast_unpayable_at_fourteen_symbols() {
    let (mut runner, zemo, _gy) = setup(&[3, 3, 3, 3, 2]); // 14 black symbols
    let idx = boast_index(&runner, zemo);
    let result = runner.act(GameAction::ActivateAbility {
        source_id: zemo,
        ability_index: idx,
    });
    assert!(
        result.is_err(),
        "Boast must be unpayable with only 14 black symbols, got {result:?}"
    );
    assert!(
        runner.state().stack.is_empty(),
        "a rejected activation must not put the ability on the stack"
    );
}

/// With 15 symbols available, selecting a SUBSET that totals only 14 is rejected
/// at payment time (CR 118.3 — the chosen set must meet the threshold).
///
/// Revert probe: dropping the handler's threshold re-check would accept the 14
/// subset, flipping `is_err()`.
#[test]
fn zemo_boast_rejects_subset_below_threshold() {
    // Six cards: five {B}{B}{B} (=15) plus one {B}{B} (so a 14-subset exists).
    let (mut runner, zemo, gy) = setup(&[3, 3, 3, 3, 3, 2]);
    let idx = boast_index(&runner, zemo);
    runner
        .act(GameAction::ActivateAbility {
            source_id: zemo,
            ability_index: idx,
        })
        .expect("payable: 17 symbols available");
    // Choose four {B}{B}{B} (12) + the {B}{B} (2) = 14 symbols — one short.
    let subset = vec![gy[0], gy[1], gy[2], gy[3], gy[5]];
    let result = runner.act(GameAction::SelectCards { cards: subset });
    assert!(
        result.is_err(),
        "a 14-symbol subset must be rejected (threshold is 15), got {result:?}"
    );
}

// ---------------------------------------------------------------------------
// Gap D — "up to three" cap on the cast choice (production path).
// ---------------------------------------------------------------------------

/// Exiling five black spells (≥15) opens a `ChooseFromZoneChoice` capped at THREE
/// of the five copies, and the choice is `up_to` (CR 707.12a — may cast fewer).
///
/// Revert probe: reverting `count` to `None` (or removing the resolver cap) makes
/// the choice offer all FIVE copies — `count == 5` instead of `3`.
#[test]
fn zemo_boast_caps_cast_choice_at_three() {
    let (mut runner, zemo, gy) = setup(&[3, 3, 3, 3, 3]); // 15 symbols, 5 cards
    let idx = boast_index(&runner, zemo);
    runner
        .act(GameAction::ActivateAbility {
            source_id: zemo,
            ability_index: idx,
        })
        .expect("payable");
    // Pay the cost by exiling all five graveyard cards.
    runner
        .act(GameAction::SelectCards { cards: gy.clone() })
        .expect("cost paid by exiling all five");
    // Resolve the Boast ability off the stack → opens the copy-cast choice.
    drain_priority(&mut runner);
    match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneChoice {
            cards,
            count,
            up_to,
            ..
        } => {
            assert_eq!(
                *count, 3,
                "the 'up to three' cap limits the cast choice to 3"
            );
            assert!(
                *up_to,
                "CR 707.12a: each copy is cast at the player's option"
            );
            assert_eq!(cards.len(), 5, "all five copies are offered as candidates");
        }
        other => panic!("expected ChooseFromZoneChoice, got {other:?}"),
    }
    // Selecting four copies is rejected (cap is three).
    let four: Vec<ObjectId> = {
        let WaitingFor::ChooseFromZoneChoice { cards, .. } = &runner.state().waiting_for else {
            unreachable!()
        };
        cards.iter().take(4).copied().collect()
    };
    assert!(
        runner.act(GameAction::SelectCards { cards: four }).is_err(),
        "selecting four copies must be rejected — the cap is three"
    );
}

/// Casting exactly three of the copies puts three spell copies on the stack, each
/// cast without paying its mana cost (CR 118.9).
#[test]
fn zemo_boast_casts_three_copies_for_free() {
    let (mut runner, zemo, gy) = setup(&[3, 3, 3, 3, 3]);
    let idx = boast_index(&runner, zemo);
    runner
        .act(GameAction::ActivateAbility {
            source_id: zemo,
            ability_index: idx,
        })
        .expect("payable");
    runner
        .act(GameAction::SelectCards { cards: gy.clone() })
        .expect("cost paid");
    drain_priority(&mut runner);
    let chosen: Vec<ObjectId> = {
        let WaitingFor::ChooseFromZoneChoice { cards, .. } = &runner.state().waiting_for else {
            panic!("expected ChooseFromZoneChoice");
        };
        cards.iter().take(3).copied().collect()
    };
    runner
        .act(GameAction::SelectCards { cards: chosen })
        .expect("cast three copies");
    drain_priority(&mut runner);
    // The three copies were cast as spell copies (CR 707.12) — each is a fresh
    // object distinct from the five exiled sources.
    let copy_count = runner
        .state()
        .objects
        .values()
        .filter(|o| o.is_copy)
        .count();
    assert_eq!(
        copy_count, 3,
        "exactly three copies must be created and cast (the 'up to three' cap)"
    );
}

// ---------------------------------------------------------------------------
// Gap C — cost→effect tracked-set binding survives the activation→resolution gap.
// ---------------------------------------------------------------------------

/// The cost-exiled cards are published as a tracked set and bound to the effect's
/// `CastCopyOfCard` BEFORE the ability is pushed to the stack. Even when an
/// UNRELATED tracked set is published between activation and resolution, the
/// copy-cast choice must offer exactly the cost-exiled cards — not the intervening
/// set.
///
/// Revert probe: replacing the concrete-id rewrite with the sentinel/"latest set"
/// path (`bind_tracked_set_sentinel_recursive` removed) makes the choice resolve
/// against the intervening tracked set instead — `cards` would be the decoy ids.
#[test]
fn zemo_boast_binds_cost_exiled_set_across_intervening_set() {
    let (mut runner, zemo, gy) = setup(&[3, 3, 3, 3, 3]);
    let idx = boast_index(&runner, zemo);
    runner
        .act(GameAction::ActivateAbility {
            source_id: zemo,
            ability_index: idx,
        })
        .expect("payable");
    runner
        .act(GameAction::SelectCards { cards: gy.clone() })
        .expect("cost paid; tracked set published + bound");

    // Simulate an intervening instant-speed effect publishing its OWN, newer
    // tracked set between activation and resolution. The sentinel/"latest" path
    // would bind to THIS decoy; the concrete-id rewrite must ignore it.
    let decoy_a = create_object(
        runner.state_mut(),
        CardId(9001),
        P1,
        "Decoy A".to_string(),
        engine::types::zones::Zone::Exile,
    );
    let decoy_b = create_object(
        runner.state_mut(),
        CardId(9002),
        P1,
        "Decoy B".to_string(),
        engine::types::zones::Zone::Exile,
    );
    let next_id = runner.state().next_tracked_set_id;
    runner
        .state_mut()
        .tracked_object_sets
        .insert(TrackedSetId(next_id), vec![decoy_a, decoy_b]);
    runner.state_mut().next_tracked_set_id = next_id + 1;
    runner.state_mut().chain_tracked_set_id = Some(TrackedSetId(next_id));

    // Resolve the Boast ability → the copy-cast choice.
    drain_priority(&mut runner);
    match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneChoice { cards, .. } => {
            let set: std::collections::HashSet<_> = cards.iter().copied().collect();
            let expected: std::collections::HashSet<_> = gy.iter().copied().collect();
            assert_eq!(
                set, expected,
                "the copy-cast choice must offer the COST-exiled cards, not the intervening decoy set"
            );
            assert!(
                !cards.contains(&decoy_a) && !cards.contains(&decoy_b),
                "the intervening tracked set must not leak into the bound effect"
            );
        }
        other => panic!("expected ChooseFromZoneChoice, got {other:?}"),
    }
}

/// Regression guard for the `fold_cast_copy_of_card_defs` broadening (PR-4b/Zemo).
/// Extending the fold's copy-half match to `CopySpell { TrackedSet(0) }` (for
/// Zemo's "Copy those exiled cards") must NOT fuse the legacy "copy the exiled
/// card. If you do, cast the copy" parent-sub idiom, whose copy half is also
/// `TrackedSet(0)`. Fusing it dropped the conditional cast sub-ability and
/// orphaned the "If you do" clause (a swallowed `Condition_If`), regressing
/// Isochron Scepter / Spellbinder / Spellweaver Helix.
///
/// Discrimination: broaden Case 2 of `fold_cast_copy_of_card_defs` back to accept
/// a `TrackedSet(0)` copy half and each of these flips to a swallowed clause.
#[test]
fn copy_then_conditional_cast_idiom_not_fused_away() {
    let cards = [
        (
            "Isochron Scepter",
            "Imprint — When this artifact enters, you may exile an instant card with mana value 2 or less from your hand.\n{2}, {T}: You may copy the exiled card. If you do, you may cast the copy without paying its mana cost.",
            vec!["Artifact".to_string()],
            Vec::<String>::new(),
        ),
        (
            "Spellbinder",
            "Imprint — When this Equipment enters, you may exile an instant card from your hand.\nWhenever equipped creature deals combat damage to a player, you may copy the exiled card. If you do, you may cast the copy without paying its mana cost.\nEquip {4}",
            vec!["Artifact".to_string()],
            Vec::<String>::new(),
        ),
        (
            "Spellweaver Helix",
            "Imprint — When this artifact enters, you may exile two target sorcery cards from a single graveyard.\nWhenever a player casts a card, if it has the same name as one of the cards exiled with this artifact, you may copy the other. If you do, you may cast the copy without paying its mana cost.",
            vec!["Artifact".to_string()],
            Vec::<String>::new(),
        ),
    ];
    for (name, oracle, types, subs) in &cards {
        let parsed = parse_oracle_text(oracle, name, &[], types, subs);
        let swallowed: Vec<_> = parsed
            .parse_warnings
            .iter()
            .filter(|w| format!("{w:?}").contains("Swallow"))
            .collect();
        assert!(
            swallowed.is_empty(),
            "{name}: the copy-then-conditional-cast idiom must not orphan its 'If you do' clause, got {swallowed:?}"
        );
    }
}
