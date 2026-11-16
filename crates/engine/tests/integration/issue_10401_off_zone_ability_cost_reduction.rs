//! Issue #10401 — CR 109.2: an ability-cost reducer whose scope describes a
//! *battlefield permanent* ("creatures you control", "artifacts you control")
//! must not discount an activation whose source is not one.
//!
//! CR 109.2: a description that includes a card type or subtype and names no
//! zone (and does not say "card", "spell" or "source") means a permanent of
//! that type **on the battlefield**. CR 110.1: a permanent is a card or token
//! on the battlefield. CR 108.4a gives an off-battlefield card an *owner* in
//! place of a controller; it does not make it a permanent, and CR 109.4 says
//! only stack/battlefield objects have a controller at all. So a graveyard unearth (CR 702.84a) or a hand
//! cycling ability (CR 702.29a) is not named by "creatures you control", and
//! Training Grounds must not reduce its cost.
//!
//! CR 109.2c: "source" means a source of an ability **in any zone**,
//! so the chosen-name shape (Skyseer's Chariot — pinned by the unit test
//! `skyseer_increases_chosen_name_activated_ability_cost`) keeps taxing an
//! off-battlefield activation. And an ACTIVATOR-scoped reducer ("abilities you
//! activate" — Zirda, the Dawnwaker; Fluctuator) never described the source, so
//! it keeps discounting in every zone (CR 602.2a).
//!
//! Verbatim Oracle text (Scryfall; cards not in the committed integration
//! card-data subset are built from their printed text so the parser still runs):
//! - Training Grounds: "Activated abilities of creatures you control cost {2}
//!   less to activate. This effect can't reduce the mana in that cost to less
//!   than one mana."
//! - Zirda, the Dawnwaker: "Abilities you activate that aren't mana abilities
//!   cost {2} less to activate. This effect can't reduce the mana in that cost
//!   to less than one mana."
//! - Heartstone: "Activated abilities of creatures cost {1} less to activate.
//!   This effect can't reduce the mana in that cost to less than one mana."
//! - Forensic Gadgeteer: "Activated abilities of artifacts you control cost {1}
//!   less to activate. This effect can't reduce the mana in that cost to less
//!   than one mana."
//! - Fluctuator: "Cycling abilities you activate cost {2} less to activate."
//! - Angel of the God-Pharaoh: "Flying / Cycling {2} ({2}, Discard this card:
//!   Draw a card.)"
//! - Dregscape Sliver (printed unearth {2}) and Sliver Queen ("{2}: Create a
//!   1/1 colorless Sliver creature token.") come from the card fixture.
//!
//! Every negative row names its paired positive, so no assertion is a vacuous
//! "nothing is ever offered": T1 (graveyard Training Grounds) is paired with T3
//! (the same reducer and the same {2} cost on a battlefield source) and T2 (the
//! same graveyard unearth under an activator-scoped reducer).

use engine::ai_support::legal_actions;
use engine::database::card_db::CardDatabase;
use engine::game::casting::{activated_ability_definitions, can_activate_ability_now};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{AbilityCost, AbilityTag, Effect, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const TRAINING_GROUNDS: &str = "Activated abilities of creatures you control cost {2} less to activate. This effect can't reduce the mana in that cost to less than one mana.";
const HEARTSTONE: &str = "Activated abilities of creatures cost {1} less to activate. This effect can't reduce the mana in that cost to less than one mana.";
const FORENSIC_GADGETEER: &str = "Activated abilities of artifacts you control cost {1} less to activate. This effect can't reduce the mana in that cost to less than one mana.";
const FLUCTUATOR: &str = "Cycling abilities you activate cost {2} less to activate.";
const ANGEL_OF_THE_GOD_PHARAOH: &str = "Flying\nCycling {2} ({2}, Discard this card: Draw a card.)";

fn pool_of(s: &mut GameScenario, player: PlayerId, colors: &[ManaType]) {
    let units = colors
        .iter()
        .map(|&c| ManaUnit::new(c, ObjectId(9_999), false, vec![]))
        .collect();
    s.with_mana_pool(player, units);
}

fn pool_total(r: &GameRunner) -> usize {
    r.state().players[0].mana_pool.total()
}

/// Every `ActivateAbility` the engine offers for `source` (the action list the
/// client renders).
fn offered_indices(r: &GameRunner, source: ObjectId) -> Vec<usize> {
    legal_actions(r.state())
        .into_iter()
        .filter_map(|a| match a {
            GameAction::ActivateAbility {
                source_id,
                ability_index,
                ..
            } if source_id == source => Some(ability_index),
            _ => None,
        })
        .collect()
}

/// The single unearth activation of `source`. The printed unearth is untagged,
/// so its shape and function identify it (CR 702.84a: a graveyard ability that
/// returns the card itself).
fn unearth_index(r: &GameRunner, source: ObjectId) -> usize {
    let defs = activated_ability_definitions(r.state(), source);
    let found: Vec<usize> = defs
        .iter()
        .filter(|(_, def)| {
            matches!(
                def.effect.as_ref(),
                Effect::ChangeZone {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Battlefield,
                    target: TargetFilter::SelfRef,
                    ..
                }
            )
        })
        .map(|(index, _)| *index)
        .collect();
    assert_eq!(
        found.len(),
        1,
        "exactly one unearth activation must exist for {source:?}, got {found:?}"
    );
    found[0]
}

/// The index of the sole activated ability whose printed cost is bare `{generic}`.
fn generic_ability_index(r: &GameRunner, source: ObjectId, generic: u32) -> usize {
    let defs = activated_ability_definitions(r.state(), source);
    let found: Vec<usize> = defs
        .iter()
        .filter(|(_, def)| {
            matches!(
                def.cost.as_ref(),
                Some(AbilityCost::Mana {
                    cost: ManaCost::Cost { generic: g, shards },
                }) if *g == generic && shards.is_empty()
            )
        })
        .map(|(index, _)| *index)
        .collect();
    assert_eq!(
        found.len(),
        1,
        "exactly one {{{generic}}} ability must exist for {source:?}, got {found:?}"
    );
    found[0]
}

/// The card's hand cycling ability (CR 702.29a).
fn cycling_index(r: &GameRunner, source: ObjectId) -> usize {
    let defs = activated_ability_definitions(r.state(), source);
    let found: Vec<usize> = defs
        .iter()
        .filter(|(_, def)| def.ability_tag == Some(AbilityTag::Cycling))
        .map(|(index, _)| *index)
        .collect();
    assert_eq!(
        found.len(),
        1,
        "exactly one cycling ability must exist for {source:?}, got {found:?}"
    );
    found[0]
}

fn activate(r: &mut GameRunner, source: ObjectId, index: usize) {
    r.act(GameAction::ActivateAbility {
        source_id: source,
        ability_index: index,
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("the activation is legal");
}

fn assert_settled(r: &GameRunner) {
    assert!(r.state().stack.is_empty(), "stack must be empty");
    assert!(
        matches!(r.state().waiting_for, WaitingFor::Priority { .. }),
        "expected Priority, got {:?}",
        r.state().waiting_for
    );
}

/// Board for the graveyard-unearth rows: a Dregscape Sliver (printed unearth
/// {2}) in P0's graveyard, plus whatever `setup` adds, paid from `pool`.
fn unearth_board(
    pool: &[ManaType],
    setup: impl FnOnce(&mut GameScenario, &'static CardDatabase),
) -> Option<(GameRunner, ObjectId)> {
    let db = load_db()?;
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let dregscape = s.add_real_card(P0, "Dregscape Sliver", Zone::Graveyard, db);
    setup(&mut s, db);
    pool_of(&mut s, P0, pool);
    Some((s.build(), dregscape))
}

/// Board for the hand-cycling rows: Angel of the God-Pharaoh (cycling {2}) in
/// P0's hand, a drawable library, plus `setup`.
fn cycling_board(pool: &[ManaType], setup: impl FnOnce(&mut GameScenario)) -> GameRunner {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_library_top(P0, &["Island"]);
    s.add_creature_to_hand(P0, "Angel of the God-Pharaoh", 4, 4)
        .from_oracle_text(ANGEL_OF_THE_GOD_PHARAOH);
    setup(&mut s);
    pool_of(&mut s, P0, pool);
    s.build()
}

fn hand_cycling_angel(r: &GameRunner) -> ObjectId {
    r.state().players[0]
        .hand
        .iter()
        .copied()
        .find(|id| {
            r.state()
                .objects
                .get(id)
                .is_some_and(|object| object.name == "Angel of the God-Pharaoh")
        })
        .expect("the angel is in hand")
}

// ---------------------------------------------------------------------------
// T1 — the same graveyard unearth, under a battlefield-scoped reducer
// ---------------------------------------------------------------------------

/// Training Grounds ("creatures you control") must not discount a graveyard
/// unearth. At a pool of 1 the activation is not offered; at a pool of 2 it is
/// offered and consumes both mana. Reverting the gate offers it at pool 1 and
/// charges 1.
#[test]
fn training_grounds_does_not_discount_a_graveyard_unearth() {
    let Some((r, dregscape)) = unearth_board(&[ManaType::Colorless], |s, db| {
        s.add_real_card(P0, "Training Grounds", Zone::Battlefield, db);
    }) else {
        eprintln!("skipping: card fixture unavailable");
        return;
    };
    let index = unearth_index(&r, dregscape);
    assert!(
        !can_activate_ability_now(r.state(), P0, dregscape, index),
        "unearth {{2}} must not be payable at one mana while Training Grounds is refused"
    );
    assert!(
        offered_indices(&r, dregscape).is_empty(),
        "Training Grounds must not make a graveyard unearth affordable: {:?}",
        offered_indices(&r, dregscape)
    );

    // Reach guard: at the undiscounted {2} the very same activation is offered.
    let Some((mut r, dregscape)) = unearth_board(&[ManaType::Colorless; 2], |s, db| {
        s.add_real_card(P0, "Training Grounds", Zone::Battlefield, db);
    }) else {
        return;
    };
    let index = unearth_index(&r, dregscape);
    assert!(
        offered_indices(&r, dregscape).contains(&index),
        "at two mana the unearth must be offered ({index}): {:?}",
        offered_indices(&r, dregscape)
    );

    activate(&mut r, dregscape, index);
    r.advance_until_stack_empty();
    assert_settled(&r);
    assert_eq!(
        pool_total(&r),
        0,
        "unearth {{2}} must spend exactly two mana (a discounted {{1}} leaves one)"
    );
    assert_eq!(r.state().objects[&dregscape].zone, Zone::Battlefield);
}

// ---------------------------------------------------------------------------
// T2 — activator-scoped reducers keep discounting in the graveyard
// ---------------------------------------------------------------------------

/// Zirda, the Dawnwaker ("abilities you activate") is not source-scoped, so it
/// must still reduce the graveyard unearth: {2} - {2} floored at {1}. The
/// mirrored board (P1's Zirda, P0's activation) proves the activator axis still
/// excludes the non-activator.
#[test]
fn an_activator_scoped_reducer_still_discounts_the_same_graveyard_unearth() {
    let Some((mut r, dregscape)) = unearth_board(&[ManaType::Colorless; 2], |s, db| {
        s.add_real_card(P0, "Zirda, the Dawnwaker", Zone::Battlefield, db);
    }) else {
        eprintln!("skipping: card fixture unavailable");
        return;
    };
    let index = unearth_index(&r, dregscape);
    assert!(
        can_activate_ability_now(r.state(), P0, dregscape, index),
        "Zirda's {{2}} discount applies to any ability P0 activates, graveyard included"
    );
    assert!(offered_indices(&r, dregscape).contains(&index));

    activate(&mut r, dregscape, index);
    r.advance_until_stack_empty();
    assert_settled(&r);
    assert_eq!(
        pool_total(&r),
        1,
        "unearth {{2}} - Zirda {{2}} floors at {{1}} (CR 118.7), leaving one mana"
    );

    // Mirror: P1's Zirda must not discount P0's activation (CR 602.2a).
    let Some((r, dregscape)) = unearth_board(&[ManaType::Colorless], |s, db| {
        s.add_real_card(P1, "Zirda, the Dawnwaker", Zone::Battlefield, db);
    }) else {
        return;
    };
    let index = unearth_index(&r, dregscape);
    assert!(
        !can_activate_ability_now(r.state(), P0, dregscape, index),
        "an opponent's activator-scoped reducer must not discount P0's activation"
    );
}

// ---------------------------------------------------------------------------
// T3 — preservation: the same reducer and cost on a battlefield source
// ---------------------------------------------------------------------------

/// Training Grounds + Sliver Queen's battlefield "{2}:" ability. The reducer
/// still applies ({2} - {2} floored at {1}), so the activation is offered at a
/// pool of 1 and consumes it. Pairs with T1: identical reducer, identical cost,
/// only the source's zone differs.
#[test]
fn training_grounds_still_discounts_a_battlefield_activation() {
    let Some(db) = load_db() else {
        eprintln!("skipping: card fixture unavailable");
        return;
    };
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let queen = s.add_real_card(P0, "Sliver Queen", Zone::Battlefield, db);
    s.add_real_card(P0, "Training Grounds", Zone::Battlefield, db);
    pool_of(&mut s, P0, &[ManaType::Colorless]);
    let mut r = s.build();

    let index = generic_ability_index(&r, queen, 2);
    assert!(
        can_activate_ability_now(r.state(), P0, queen, index),
        "Training Grounds must floor {{2}} to {{1}}, so one mana pays"
    );
    assert!(offered_indices(&r, queen).contains(&index));

    activate(&mut r, queen, index);
    r.advance_until_stack_empty();
    assert_settled(&r);
    assert_eq!(
        pool_total(&r),
        0,
        "the floored {{1}} activation cost consumes the single mana"
    );
}

// ---------------------------------------------------------------------------
// T4 — other battlefield scopes, two different type descriptions
// ---------------------------------------------------------------------------

/// Heartstone ("activated abilities of creatures", no controller axis) and
/// Forensic Gadgeteer ("artifacts you control") are both battlefield-denoting
/// descriptions (CR 109.2), so neither may discount an off-battlefield
/// activation. Two different scope types prove the gate is class-level, not
/// creature-shaped.
#[test]
fn off_battlefield_activations_are_not_discounted_by_other_scopes() {
    // (a) Heartstone + graveyard unearth {2}: charged {2}.
    let Some((mut r, dregscape)) = unearth_board(&[ManaType::Colorless; 2], |s, _db| {
        s.add_artifact_from_oracle(P0, "Heartstone", HEARTSTONE);
    }) else {
        eprintln!("skipping: card fixture unavailable");
        return;
    };
    let index = unearth_index(&r, dregscape);
    assert!(offered_indices(&r, dregscape).contains(&index));
    activate(&mut r, dregscape, index);
    r.advance_until_stack_empty();
    assert_settled(&r);
    assert_eq!(
        pool_total(&r),
        0,
        "Heartstone describes creatures ON THE BATTLEFIELD, so unearth {{2}} stays {{2}}"
    );

    // (b) Forensic Gadgeteer + graveyard Scrapwork Mutt (unearth {1}{R}):
    // charged {1}{R}; a discounted {R} would leave one mana.
    let Some(db) = load_db() else {
        return;
    };
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let mutt = s.add_real_card(P0, "Scrapwork Mutt", Zone::Graveyard, db);
    s.add_artifact_from_oracle(P0, "Forensic Gadgeteer", FORENSIC_GADGETEER);
    pool_of(&mut s, P0, &[ManaType::Colorless, ManaType::Red]);
    let mut r = s.build();

    let index = unearth_index(&r, mutt);
    assert!(offered_indices(&r, mutt).contains(&index));
    activate(&mut r, mutt, index);
    r.advance_until_stack_empty();
    assert_settled(&r);
    assert_eq!(
        pool_total(&r),
        0,
        "Forensic Gadgeteer describes artifacts ON THE BATTLEFIELD, so unearth {{1}}{{R}} stays {{1}}{{R}}"
    );
}

// ---------------------------------------------------------------------------
// T5 — hand cycling, and the activator-scoped sibling
// ---------------------------------------------------------------------------

/// Training Grounds must not discount a hand cycling ability (CR 702.29a: the
/// source is a card in hand, CR 109.2). Fluctuator's activator-scoped "cycling
/// abilities you activate" still does: cycling {2} - {2} = {0} (no floor,
/// CR 118.7), so it is offered even at a pool of 1.
#[test]
fn training_grounds_does_not_discount_hand_cycling() {
    let r = cycling_board(&[ManaType::Colorless], |s| {
        s.add_artifact_from_oracle(P0, "Training Grounds", TRAINING_GROUNDS);
    });
    let angel = hand_cycling_angel(&r);
    let index = cycling_index(&r, angel);
    assert!(
        !can_activate_ability_now(r.state(), P0, angel, index),
        "cycling {{2}} must not be payable at one mana while Training Grounds is refused"
    );
    assert!(
        offered_indices(&r, angel).is_empty(),
        "Training Grounds must not make hand cycling affordable: {:?}",
        offered_indices(&r, angel)
    );

    // Reach guard: at the undiscounted {2} the same cycling is offered and
    // charged in full.
    let mut r = cycling_board(&[ManaType::Colorless; 2], |s| {
        s.add_artifact_from_oracle(P0, "Training Grounds", TRAINING_GROUNDS);
    });
    let angel = hand_cycling_angel(&r);
    let index = cycling_index(&r, angel);
    assert!(
        offered_indices(&r, angel).contains(&index),
        "at two mana the cycling must be offered ({index}): {:?}",
        offered_indices(&r, angel)
    );
    activate(&mut r, angel, index);
    r.advance_until_stack_empty();
    assert_settled(&r);
    assert_eq!(r.state().objects[&angel].zone, Zone::Graveyard);
    assert_eq!(
        pool_total(&r),
        0,
        "cycling {{2}} must spend exactly two mana"
    );

    // Fluctuator sibling: the same cycling is discounted to {0}, so it is
    // offered at one mana (the gate keys on the scope filter, not the zone).
    let r = cycling_board(&[ManaType::Colorless], |s| {
        s.add_artifact_from_oracle(P0, "Fluctuator", FLUCTUATOR);
    });
    let angel = hand_cycling_angel(&r);
    let index = cycling_index(&r, angel);
    assert!(
        can_activate_ability_now(r.state(), P0, angel, index),
        "Fluctuator's {{2}} activator-scoped discount applies in the hand"
    );
    assert!(offered_indices(&r, angel).contains(&index));

    let mut r = cycling_board(&[ManaType::Colorless; 2], |s| {
        s.add_artifact_from_oracle(P0, "Fluctuator", FLUCTUATOR);
    });
    let angel = hand_cycling_angel(&r);
    let index = cycling_index(&r, angel);
    activate(&mut r, angel, index);
    r.advance_until_stack_empty();
    assert_settled(&r);
    assert_eq!(r.state().objects[&angel].zone, Zone::Graveyard);
    assert_eq!(
        pool_total(&r),
        2,
        "cycling {{2}} - Fluctuator {{2}} = {{0}}, so both mana remain (CR 118.7)"
    );
}
