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

/// CR 701.9a + CR 701.17a + CR 608.2c: P0 (the caster) discards a 2-card hand
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
/// legal no-op (CR 107.3a), not an erroneous fixed-1 sacrifice.
#[test]
fn malfegor_empty_hand_discard_causes_no_sacrifice() {
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
