//! Regression for GitHub issue #5991 — Malfegor's ETB ability that forces the
//! opponent to sacrifice creatures scaled to the discarded hand size.
//!
//! Oracle (verified real MTG card text, Alara Reborn):
//!   "Flying\nWhen Malfegor enters the battlefield, discard your hand, then
//!   each opponent sacrifices a creature of their choice for each card
//!   discarded this way."
//!
//! A `parse_oracle_text` probe against this exact text (dumped in the issue
//! investigation, not included here) confirmed the AST parses correctly and
//! matches this test's `TRIGGER_BODY` shape exactly:
//!
//!   DiscardCard { count: HandSize(Controller), target: Controller }
//!   sub_ability:
//!     Sacrifice {
//!       target: Typed([Creature]),
//!       count: FilteredTrackedSetSize { filter: Typed([Card]), caused_by: Discarded },
//!     }
//!     player_scope: Opponent
//!
//! This test drives the real resolution chain end to end via
//! `resolve_ability_chain` (the same production entry point every triggered
//! ability resolves through — mirrors the established convention for
//! player_scope mass-effect triggers, see
//! `aclazotz_attack_discard_multi_opponent.rs`), not a hand-built
//! `ResolvedAbility` — the ability comes from the real parser
//! (`parse_effect_chain`) resolved via `build_resolved_from_def`.
//!
//! Discriminating scenario: the controller discards a 2-card hand, and the
//! single opponent controls 3 creatures. Per the real card, the opponent must
//! be asked to sacrifice exactly 2 of their 3 creatures (a real interactive
//! `EffectZoneChoice` prompt with `count == 2`), not a fixed count of 1 and
//! not 0 — the two most plausible "malfunctioning" symptoms a scoped defect
//! in this class of card could produce.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::engine::apply;
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, ResolvedAbility};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const TRIGGER_BODY: &str = "discard your hand, then each opponent sacrifices a creature \
    of their choice for each card discarded this way.";

fn malfegor_etb_ability(controller: PlayerId, source_id: ObjectId) -> ResolvedAbility {
    let def = parse_effect_chain(TRIGGER_BODY, AbilityKind::Spell);
    build_resolved_from_def(&def, source_id, controller)
}

fn add_hand_cards(state: &mut GameState, base_card_id: u64, player: PlayerId, n: usize) {
    for i in 0..n {
        create_object(
            state,
            CardId(base_card_id + i as u64),
            player,
            "Forest".to_string(),
            Zone::Hand,
        );
    }
}

/// Bare `create_object` leaves `card_types` at its empty default (it does not
/// look up real card data by name), so a battlefield creature must have
/// `CoreType::Creature` pushed onto BOTH `card_types` and `base_card_types`
/// (the latter survives a layer recompute, which reverts `card_types` from
/// it) — otherwise the Sacrifice effect's `Typed([Creature])` target filter
/// matches nothing and the eligible pool is silently empty. Mirrors
/// `make_creature` in `descendants_fury_sacrificed_referent_4795.rs`.
fn add_battlefield_creatures(state: &mut GameState, base_card_id: u64, player: PlayerId, n: usize) {
    for i in 0..n {
        let id = create_object(
            state,
            CardId(base_card_id + i as u64),
            player,
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).expect("object just created");
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types.core_types.push(CoreType::Creature);
    }
}

fn battlefield_count(state: &GameState, player: PlayerId) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|obj| obj.controller == player)
        })
        .count()
}

fn hand_len(state: &GameState, player: PlayerId) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .len()
}

/// CR 701.9a (discard) + CR 701.21a (sacrifice) + CR 608.2c: P0 (the caster)
/// discards a 2-card hand
/// (a mandatory whole-hand discard, no choice needed since hand size ==
/// discard count), then P1 (the lone opponent, controlling 3 creatures) must
/// sacrifice exactly 2 of them — the discarded-card count, not a fixed 1 and
/// not 0. This is the exact shape reported in #5991: "forces the opponent to
/// sacrifice their creatures after I discard my hand."
#[test]
fn malfegor_sacrifice_count_scales_with_discarded_hand_size() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Malfegor".to_string(),
        Zone::Battlefield,
    );

    add_hand_cards(&mut state, 100, PlayerId(0), 2);
    add_battlefield_creatures(&mut state, 200, PlayerId(1), 3);

    assert_eq!(hand_len(&state, PlayerId(0)), 2, "P0 starts with 2 cards");
    assert_eq!(
        battlefield_count(&state, PlayerId(1)),
        3,
        "P1 starts with 3 creatures"
    );

    let ability = malfegor_etb_ability(PlayerId(0), source);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    // The whole-hand discard is mandatory and unambiguous (hand size ==
    // discard count), so it resolves without an interactive prompt, straight
    // into the sacrifice's own EffectZoneChoice prompt.
    assert_eq!(
        hand_len(&state, PlayerId(0)),
        0,
        "P0's whole hand must be discarded before the sacrifice count is read"
    );

    let WaitingFor::EffectZoneChoice {
        player,
        count,
        cards,
        up_to,
        ..
    } = state.waiting_for.clone()
    else {
        panic!(
            "expected an EffectZoneChoice sacrifice prompt for the opponent, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(
        player,
        PlayerId(1),
        "the OPPONENT must be the one prompted to sacrifice"
    );
    assert!(!up_to, "the sacrifice is mandatory, not up-to");
    assert_eq!(
        count, 2,
        "sacrifice count must equal the 2 cards discarded this way, not a fixed 1"
    );
    assert_eq!(cards.len(), 3, "all 3 of P1's creatures must be eligible");

    // Complete the interactive choice: P1 picks 2 of their 3 creatures.
    let picks: Vec<ObjectId> = cards.iter().take(2).copied().collect();
    apply(
        &mut state,
        PlayerId(1),
        GameAction::SelectCards {
            cards: picks.clone(),
        },
    )
    .expect("sacrifice choice should succeed");

    assert_eq!(
        battlefield_count(&state, PlayerId(1)),
        1,
        "P1 must end with exactly 1 creature left (3 - 2 sacrificed)"
    );
    for picked in &picks {
        assert!(
            !state.battlefield.contains(picked),
            "sacrificed creature {picked:?} must have left the battlefield"
        );
    }
}

/// Negative/no-regression guard: an empty hand discards nothing, so the
/// tracked set is empty and the opponent must sacrifice 0 creatures — a
/// legal no-op, not an erroneous fixed-1 sacrifice.
#[test]
fn malfegor_empty_hand_discard_causes_no_sacrifice() {
    use engine::types::ability::EffectKind;
    use engine::types::events::GameEvent;

    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Malfegor".to_string(),
        Zone::Battlefield,
    );

    add_battlefield_creatures(&mut state, 200, PlayerId(1), 2);

    let ability = malfegor_etb_ability(PlayerId(0), source);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    // Reach guard: the negative assertions below are only meaningful if the
    // discard resolution step actually ran. The discard resolver emits
    // `EffectResolved` for this source even when the hand is empty, so an
    // upstream short-circuit that skipped the discard/tracked-set path
    // entirely would fail here rather than vacuously pass.
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Discard | EffectKind::DiscardCard,
                source_id,
                ..
            } if *source_id == source
        )),
        "the (empty) discard resolution step must have run; events: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, GameEvent::Discarded { .. })),
        "no card may actually be discarded from an empty hand"
    );

    assert_eq!(
        battlefield_count(&state, PlayerId(1)),
        2,
        "opponent must keep both creatures when nothing was discarded"
    );
    assert!(
        !matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. }),
        "an empty discard must not raise a sacrifice prompt, got {:?}",
        state.waiting_for
    );
}

/// CR 608.2c + CR 122.6: matcher-boundary sibling of the Malfegor shape, from
/// the #5991 review — a producer (whole-hand discard) chained into a token
/// with FIXED count and FIXED P/T whose only tracked-set reference is a
/// SECONDARY quantity slot: `enter_with_counters: [(+1/+1, TrackedSetSize)]`
/// ("... with a +1/+1 counter on it for each card discarded this way").
///
/// The chain is hand-built (the parent discard comes from the real parser;
/// the token continuation is constructed directly) because this guards the
/// tracked-set publish CLASSIFICATION at the `resolve_ability_chain`
/// production boundary, not any single card's parse. With a primary-count-only
/// classification (`count`/`power`/`toughness` inspection), the discard never
/// publishes its set and the token enters with ZERO counters; the exhaustive
/// `for_each_quantity_expr` walk makes it enter with one counter per
/// discarded card.
#[test]
fn tracked_entry_counter_token_receives_discarded_count_counters() {
    use engine::types::ability::Effect;
    use engine::types::ability::{PtValue, QuantityExpr, QuantityRef, TargetFilter};
    use engine::types::counter::CounterType;
    use engine::types::mana::ManaColor;

    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Entry Counter Producer".to_string(),
        Zone::Battlefield,
    );

    add_hand_cards(&mut state, 100, PlayerId(0), 2);

    // Parent: the same real-parser whole-hand discard the Malfegor test uses.
    let def = parse_effect_chain("discard your hand.", AbilityKind::Spell);
    let mut ability = build_resolved_from_def(&def, source, PlayerId(0));
    assert!(
        ability.sub_ability.is_none(),
        "the bare discard must have no continuation before we attach one"
    );

    // Continuation: fixed-count, fixed-P/T token whose ONLY tracked-set
    // reference is the entry-counter quantity.
    ability.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::Token {
            name: "Zombie".to_string(),
            power: PtValue::Fixed(2),
            toughness: PtValue::Fixed(2),
            types: vec!["Creature".to_string(), "Zombie".to_string()],
            colors: vec![ManaColor::Black],
            keywords: vec![],
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            supertypes: vec![],
            static_abilities: vec![],
            enter_with_counters: vec![(
                CounterType::Plus1Plus1,
                QuantityExpr::Ref {
                    qty: QuantityRef::TrackedSetSize,
                },
            )],
        },
        vec![],
        source,
        PlayerId(0),
    )));

    let before: Vec<ObjectId> = state.battlefield.iter().copied().collect();
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert_eq!(
        hand_len(&state, PlayerId(0)),
        0,
        "the whole hand must be discarded before the token is created"
    );

    let new_tokens: Vec<ObjectId> = state
        .battlefield
        .iter()
        .filter(|id| !before.contains(id))
        .copied()
        .collect();
    assert_eq!(
        new_tokens.len(),
        1,
        "exactly one token must be created (fixed count 1)"
    );
    let token = state
        .objects
        .get(&new_tokens[0])
        .expect("token object exists");
    assert_eq!(
        token.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(2),
        "the token must enter with one +1/+1 counter per discarded card (2), \
         not zero — zero means the discard never published its tracked set"
    );
}

/// Builds the shared producer→consumer chain of the nested-carrier seam
/// regressions below: the same real-parser whole-hand discard the Malfegor
/// test uses (the tracked-set PRODUCER), with `continuation` attached as the
/// hand-built consumer sub-ability. Mirrors
/// `tracked_entry_counter_token_receives_discarded_count_counters` — these
/// tests guard the tracked-set publish CLASSIFICATION at the
/// `resolve_ability_chain` production boundary, not any single card's parse.
fn discard_hand_with_continuation(
    source: ObjectId,
    continuation: engine::types::ability::Effect,
) -> ResolvedAbility {
    let def = parse_effect_chain("discard your hand.", AbilityKind::Spell);
    let mut ability = build_resolved_from_def(&def, source, PlayerId(0));
    assert!(
        ability.sub_ability.is_none(),
        "the bare discard must have no continuation before we attach one"
    );
    ability.sub_ability = Some(Box::new(ResolvedAbility::new(
        continuation,
        vec![],
        source,
        PlayerId(0),
    )));
    ability
}

/// CR 608.2c + CR 106.1b: nested-carrier seam from the #5991 review — a
/// producer (whole-hand discard) chained into an `Effect::Mana` whose
/// `ManaProduction::Colorless` count is the ONLY tracked-set reference
/// ("add {C} for each card discarded this way"). The dynamic production
/// count is resolved live in `game/effects/mana.rs::resolve_count`, so the
/// discard must publish its set; with `Effect::Mana` classified quantity-free
/// (the pre-fix state of `for_each_quantity_expr`), the set is never
/// published and ZERO mana is added.
#[test]
fn tracked_mana_production_count_adds_discarded_count_mana() {
    use engine::types::ability::{Effect, ManaProduction, QuantityExpr, QuantityRef};
    use engine::types::mana::ManaType;

    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Mana Producer".to_string(),
        Zone::Battlefield,
    );

    add_hand_cards(&mut state, 100, PlayerId(0), 2);

    let ability = discard_hand_with_continuation(
        source,
        Effect::Mana {
            produced: ManaProduction::Colorless {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::TrackedSetSize,
                },
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    );

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert_eq!(
        hand_len(&state, PlayerId(0)),
        0,
        "the whole hand must be discarded before the mana count is read"
    );
    assert_eq!(
        state.players[0].mana_pool.count_color(ManaType::Colorless),
        2,
        "the pool must gain one {{C}} per discarded card (2), not zero — \
         zero means the discard never published its tracked set"
    );
}

/// CR 608.2c + CR 119.4 + CR 118.12: nested-carrier seam from the #5991
/// review — a producer (whole-hand discard) chained into an `Effect::PayCost`
/// whose `AbilityCost::PayLife` amount is the ONLY tracked-set reference
/// ("pay 1 life for each card discarded this way"). The cost's dynamic amount
/// is resolved live in the payment authority
/// (`costs::pay_ability_cost_for_resolution`), so the discard must publish
/// its set; with `PayCost` visiting only `scale` (the pre-fix state of
/// `for_each_quantity_expr`), the set is never published and ZERO life is
/// paid.
#[test]
fn tracked_pay_cost_life_amount_pays_discarded_count_life() {
    use engine::types::ability::{AbilityCost, Effect, QuantityExpr, QuantityRef, TargetFilter};

    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Life Payer".to_string(),
        Zone::Battlefield,
    );

    add_hand_cards(&mut state, 100, PlayerId(0), 2);
    let life_before = state.players[0].life;

    let ability = discard_hand_with_continuation(
        source,
        Effect::PayCost {
            cost: AbilityCost::PayLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::TrackedSetSize,
                },
            },
            scale: None,
            payer: TargetFilter::Controller,
        },
    );

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert_eq!(
        hand_len(&state, PlayerId(0)),
        0,
        "the whole hand must be discarded before the life payment is read"
    );
    assert!(
        !state.cost_payment_failed_flag,
        "the fixed-amount life payment must succeed"
    );
    assert_eq!(
        state.players[0].life,
        life_before - 2,
        "the payer must lose one life per discarded card (2), not zero — \
         zero means the discard never published its tracked set"
    );
}

/// CR 608.2c + CR 706.2: nested-carrier seam from the #5991 review — a
/// producer (whole-hand discard) chained into an `Effect::RollDie` whose
/// `DieRollModifier::Add` value is the ONLY tracked-set reference ("roll a
/// die and add the number of cards discarded this way"). The modifier is
/// resolved live against the natural roll in `game/effects/roll_die.rs`, so
/// the discard must publish its set. A one-sided die pins the natural roll
/// to 1, making the modified result deterministic: 1 + 2 discarded = 3; with
/// `RollDie` visiting only `count` (the pre-fix state of
/// `for_each_quantity_expr`), the set is never published and the result
/// stays at the natural 1.
#[test]
fn tracked_die_roll_modifier_adds_discarded_count_to_result() {
    use engine::types::ability::{DieRollModifier, Effect, QuantityExpr, QuantityRef};
    use engine::types::events::GameEvent;

    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Die Roller".to_string(),
        Zone::Battlefield,
    );

    add_hand_cards(&mut state, 100, PlayerId(0), 2);

    let ability = discard_hand_with_continuation(
        source,
        Effect::RollDie {
            count: QuantityExpr::Fixed { value: 1 },
            sides: 1,
            results: vec![],
            modifier: Some(DieRollModifier::Add {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::TrackedSetSize,
                },
            }),
        },
    );

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert_eq!(
        hand_len(&state, PlayerId(0)),
        0,
        "the whole hand must be discarded before the roll modifier is read"
    );
    let rolled: Vec<Option<u8>> = events
        .iter()
        .filter_map(|e| match e {
            GameEvent::DieRolled { result, .. } => Some(*result),
            _ => None,
        })
        .collect();
    assert_eq!(
        rolled,
        vec![Some(3)],
        "the d1 natural roll of 1 plus one per discarded card (2) must give \
         a modified result of 3 — a result of 1 means the discard never \
         published its tracked set"
    );
}
