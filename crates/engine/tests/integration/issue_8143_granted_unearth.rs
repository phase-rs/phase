//! Issue #8143: an unearth GRANTED to a card in a graveyard is never offered.
//!
//! CR 702.84a: "Unearth is an activated ability that functions while the card
//! with unearth is in a graveyard. 'Unearth [cost]' means '[Cost]: Return this
//! card from your graveyard to the battlefield. It gains haste. Exile it at the
//! beginning of the next end step. If it would leave the battlefield, exile it
//! instead of putting it anywhere else. Activate only as a sorcery.'"
//! CR 604.1 + CR 113.6: a static that gives a graveyard card unearth gives that
//! card the activated ability itself.
//!
//! The grant reached the off-zone keyword layer, but the runtime synthesis seam
//! (`casting::runtime_granted_graveyard_activated_abilities` →
//! `synthesis::graveyard_activated_ability_for_keyword`) built activated
//! abilities only for Encore / Scavenge / Embalm / Eternalize, so a granted
//! Unearth produced no activation. Printed unearth worked because card-load
//! synthesis had already put the ability in `obj.abilities`.
//!
//! Verbatim Oracle text (Scryfall):
//! - Dregscape Sliver {1}{B}: "Each Sliver creature card in your graveyard has
//!   unearth {2}. / Unearth {2}"
//! - Sedris, the Traitor King: "Each creature card in your graveyard has
//!   unearth {2}{B}."
//! - Mishra, Tamer of Mak Fawa: "Permanents you control have "Ward—Sacrifice a
//!   permanent." / Each artifact card in your graveyard has unearth {1}{B}{R}."
//! - Solemn Doomguide: "Flying / Each creature card in your graveyard that's a
//!   Cleric, Rogue, Warrior, and/or Wizard has unearth {1}{B}."
//! - Muscle Sliver {1}{G}: "All Sliver creatures get +1/+1."
//! - Sliver Gravemother: "The "legend rule" doesn't apply to Slivers you
//!   control. / Each Sliver creature card in your graveyard has encore {X},
//!   where X is its mana value. / Encore {5}"

use engine::ai_support::legal_actions;
use engine::game::casting::activated_ability_definitions;
use engine::game::keywords::object_has_effective_keyword_kind;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::zones::move_to_zone;
use engine::types::ability::{AbilityCost, ContinuousModification, Duration, Effect, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, KeywordKind};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// Reminder text is included where the card prints it; the parser strips it.
const DREGSCAPE: &str = "Each Sliver creature card in your graveyard has unearth {2}.\nUnearth {2} ({2}: Return this card from your graveyard to the battlefield. It gains haste. Exile it at the beginning of the next end step or if it would leave the battlefield. Unearth only as a sorcery.)";
const SEDRIS: &str = "Each creature card in your graveyard has unearth {2}{B}. ({2}{B}: Return the card to the battlefield. The creature gains haste. Exile it at the beginning of the next end step or if it would leave the battlefield. Unearth only as a sorcery.)";
const MISHRA: &str = "Permanents you control have \"Ward—Sacrifice a permanent.\"\nEach artifact card in your graveyard has unearth {1}{B}{R}. ({1}{B}{R}: Return the card from your graveyard to the battlefield. It gains haste. Exile it at the beginning of the next end step or if it would leave the battlefield. Unearth only as a sorcery.)";
const SOLEMN_DOOMGUIDE: &str = "Flying\nEach creature card in your graveyard that's a Cleric, Rogue, Warrior, and/or Wizard has unearth {1}{B}. ({1}{B}: Return the card to the battlefield. The creature gains haste. Exile it at the beginning of the next end step or if it would leave the battlefield. Unearth only as a sorcery.)";
const MUSCLE_SLIVER: &str = "All Sliver creatures get +1/+1.";
const GRAVEMOTHER: &str = "The \"legend rule\" doesn't apply to Slivers you control.\nEach Sliver creature card in your graveyard has encore {X}, where X is its mana value.\nEncore {5} ({5}, Exile this card from your graveyard: For each opponent, create a token copy that attacks that opponent this turn if able. They gain haste. Sacrifice them at the beginning of the next end step. Activate only as a sorcery.)";

fn cost(generic: u32, shards: Vec<ManaCostShard>) -> ManaCost {
    ManaCost::Cost { generic, shards }
}

fn pool(s: &mut GameScenario, colors: &[ManaType]) {
    let units = colors
        .iter()
        .map(|&c| ManaUnit::new(c, ObjectId(9_999), false, vec![]))
        .collect();
    s.with_mana_pool(P0, units);
}

fn pool_total(r: &GameRunner) -> usize {
    r.state().players[0].mana_pool.total()
}

fn add_dregscape(s: &mut GameScenario, zone: Zone) -> ObjectId {
    let mut b = match zone {
        Zone::Battlefield => s.add_creature(P0, "Dregscape Sliver", 2, 2),
        Zone::Graveyard => s.add_creature_to_graveyard(P0, "Dregscape Sliver", 2, 2),
        other => panic!("unsupported fixture zone {other:?}"),
    };
    b.with_subtypes(vec!["Sliver"])
        .with_mana_cost(cost(1, vec![ManaCostShard::Black]))
        .from_oracle_text(DREGSCAPE);
    b.id()
}

fn add_muscle_sliver_to_graveyard(s: &mut GameScenario) -> ObjectId {
    s.add_creature_to_graveyard(P0, "Muscle Sliver", 1, 1)
        .with_subtypes(vec!["Sliver"])
        .with_mana_cost(cost(1, vec![ManaCostShard::Green]))
        .from_oracle_text(MUSCLE_SLIVER)
        .id()
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

/// The mana sub-cost of `source`'s activated ability at `index`.
fn mana_cost_of(r: &GameRunner, source: ObjectId, index: usize) -> ManaCost {
    let defs = activated_ability_definitions(r.state(), source);
    let (_, def) = defs
        .iter()
        .find(|(i, _)| *i == index)
        .expect("offered index must have a definition");
    match def.cost.as_ref() {
        Some(AbilityCost::Mana { cost }) => cost.clone(),
        other => panic!("unearth's activation cost is its mana cost, got {other:?}"),
    }
}

/// The single offered activation for `source`, asserted to be an unearth
/// (a graveyard `ChangeZone` → battlefield of the card itself).
fn sole_unearth_offer(r: &GameRunner, source: ObjectId) -> usize {
    let offered = offered_indices(r, source);
    assert_eq!(
        offered.len(),
        1,
        "exactly one unearth activation must be offered for {source:?}, got {offered:?}"
    );
    let index = offered[0];
    let defs = activated_ability_definitions(r.state(), source);
    let (_, def) = defs.iter().find(|(i, _)| *i == index).unwrap();
    assert!(
        matches!(
            def.effect.as_ref(),
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::SelfRef,
                ..
            }
        ),
        "the offered ability must be unearth's self-return"
    );
    assert_eq!(def.activation_zone, Some(Zone::Graveyard));
    index
}

fn activate(r: &mut GameRunner, source: ObjectId, index: usize) {
    r.act(GameAction::ActivateAbility {
        source_id: source,
        ability_index: index,
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("unearth activation is legal");
}

fn assert_settled(r: &GameRunner) {
    assert!(r.state().stack.is_empty(), "stack must be empty");
    assert!(
        matches!(r.state().waiting_for, WaitingFor::Priority { .. }),
        "expected Priority, got {:?}",
        r.state().waiting_for
    );
}

/// Walk to the end step, declaring no attackers on the way (untapped creatures
/// make `advance_to_end_step` halt at the CR 508.1 declaration), then resolve
/// the end-step triggers.
fn advance_through_end_step(r: &mut GameRunner) {
    for _ in 0..8 {
        r.advance_to_end_step();
        if r.state().phase == Phase::End {
            break;
        }
        if matches!(r.state().waiting_for, WaitingFor::DeclareAttackers { .. }) {
            r.act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            })
            .expect("the active player may declare no attackers");
        }
    }
    assert_eq!(r.state().phase, Phase::End, "reached the end step");
    r.advance_until_stack_empty();
}

/// The full CR 702.84a obligation for an unearthed card: back on the
/// battlefield, hasty, and exiled at the next end step.
fn assert_unearthed_then_exiled(r: &mut GameRunner, card: ObjectId) {
    assert_eq!(r.state().objects[&card].zone, Zone::Battlefield);
    assert!(
        object_has_effective_keyword_kind(r.state(), card, KeywordKind::Haste),
        "CR 702.84a: the unearthed card gains haste"
    );
    advance_through_end_step(r);
    assert_eq!(
        r.state().objects[&card].zone,
        Zone::Exile,
        "CR 702.84a: exiled at the beginning of the next end step"
    );
}

// ---------------------------------------------------------------------------
// Static grants
// ---------------------------------------------------------------------------

/// Jeff's board: Dregscape Sliver on the battlefield, a Sliver WITHOUT printed
/// unearth in the graveyard. The granted unearth {2} is offered, costs exactly
/// {2}, and resolves through the full unearth chain. A non-Sliver creature card
/// in the same graveyard is not offered (the grant's filter).
#[test]
fn dregscape_sliver_grants_unearth_to_a_sliver_card_in_the_graveyard() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    add_dregscape(&mut s, Zone::Battlefield);
    let muscle = add_muscle_sliver_to_graveyard(&mut s);
    let bear = s.add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2).id();
    pool(&mut s, &[ManaType::Black; 3]);
    let mut r = s.build();

    // Reach guard: the grant itself reaches the graveyard Sliver.
    assert!(object_has_effective_keyword_kind(
        r.state(),
        muscle,
        KeywordKind::Unearth
    ));
    let index = sole_unearth_offer(&r, muscle);
    assert_eq!(mana_cost_of(&r, muscle, index), cost(2, vec![]));
    assert!(
        offered_indices(&r, bear).is_empty(),
        "Dregscape Sliver grants unearth only to Sliver creature cards"
    );

    activate(&mut r, muscle, index);
    r.advance_until_stack_empty();
    assert_settled(&r);
    assert_eq!(pool_total(&r), 1, "unearth {{2}} spends exactly two mana");
    assert_unearthed_then_exiled(&mut r, muscle);
}

/// Sedris grants unearth {2}{B} to every creature card, Sliver or not. With
/// only {2} available it is not affordable; with {2}{B} it resolves.
#[test]
fn sedris_grants_unearth_two_black_to_any_creature_card() {
    for (mana, affordable) in [
        (vec![ManaType::Colorless, ManaType::Colorless], false),
        (
            vec![ManaType::Colorless, ManaType::Colorless, ManaType::Black],
            true,
        ),
    ] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        s.add_creature(P0, "Sedris, the Traitor King", 5, 5)
            .as_legendary()
            .with_subtypes(vec!["Zombie", "Warrior"])
            .from_oracle_text(SEDRIS);
        let bear = s.add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2).id();
        pool(&mut s, &mana);
        let mut r = s.build();

        assert!(object_has_effective_keyword_kind(
            r.state(),
            bear,
            KeywordKind::Unearth
        ));
        if !affordable {
            assert!(
                offered_indices(&r, bear).is_empty(),
                "unearth {{2}}{{B}} is not affordable with {{2}}"
            );
            continue;
        }
        let index = sole_unearth_offer(&r, bear);
        assert_eq!(
            mana_cost_of(&r, bear, index),
            cost(2, vec![ManaCostShard::Black])
        );
        activate(&mut r, bear, index);
        r.advance_until_stack_empty();
        assert_settled(&r);
        assert_eq!(pool_total(&r), 0);
        assert_unearthed_then_exiled(&mut r, bear);
    }
}

/// Mishra grants unearth to every ARTIFACT card — including a noncreature one.
#[test]
fn mishra_grants_unearth_to_a_noncreature_artifact_card() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_creature(P0, "Mishra, Tamer of Mak Fawa", 4, 4)
        .as_legendary()
        .with_subtypes(vec!["Human", "Artificer"])
        .from_oracle_text(MISHRA);
    let stone = s
        .add_creature_to_graveyard(P0, "Mind Stone", 0, 0)
        .as_artifact()
        .with_mana_cost(cost(2, vec![]))
        .id();
    let bear = s.add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2).id();
    pool(
        &mut s,
        &[ManaType::Colorless, ManaType::Black, ManaType::Red],
    );
    let mut r = s.build();

    let index = sole_unearth_offer(&r, stone);
    assert_eq!(
        mana_cost_of(&r, stone, index),
        cost(1, vec![ManaCostShard::Black, ManaCostShard::Red])
    );
    assert!(
        offered_indices(&r, bear).is_empty(),
        "Mishra grants unearth only to artifact cards"
    );
    activate(&mut r, stone, index);
    r.advance_until_stack_empty();
    assert_settled(&r);
    assert_eq!(pool_total(&r), 0);
    assert_eq!(r.state().objects[&stone].zone, Zone::Battlefield);
    advance_through_end_step(&mut r);
    assert_eq!(r.state().objects[&stone].zone, Zone::Exile);
}

/// Solemn Doomguide's compound subject: a Cleric creature card is granted
/// unearth {1}{B}; a creature card of none of the listed types is not.
#[test]
fn solemn_doomguide_grants_unearth_only_to_listed_creature_types() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_creature(P0, "Solemn Doomguide", 4, 5)
        .with_subtypes(vec!["Tiefling", "Cleric"])
        .from_oracle_text(SOLEMN_DOOMGUIDE);
    let cleric = s
        .add_creature_to_graveyard(P0, "Graveyard Cleric", 2, 2)
        .with_subtypes(vec!["Human", "Cleric"])
        .id();
    let bear = s
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    pool(&mut s, &[ManaType::Colorless, ManaType::Black]);
    let mut r = s.build();

    let index = sole_unearth_offer(&r, cleric);
    assert_eq!(
        mana_cost_of(&r, cleric, index),
        cost(1, vec![ManaCostShard::Black])
    );
    assert!(!object_has_effective_keyword_kind(
        r.state(),
        bear,
        KeywordKind::Unearth
    ));
    assert!(offered_indices(&r, bear).is_empty());
    activate(&mut r, cleric, index);
    r.advance_until_stack_empty();
    assert_settled(&r);
    assert_unearthed_then_exiled(&mut r, cleric);
}

// ---------------------------------------------------------------------------
// Printed vs granted (CR 113.2c)
// ---------------------------------------------------------------------------

/// A Dregscape Sliver in the graveyard prints unearth {2}; Sedris grants it a
/// DIFFERENT unearth {2}{B}. Both instances are offered, and activating the
/// granted one spends {2}{B}.
#[test]
fn printed_and_different_granted_unearth_are_both_offered() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_creature(P0, "Sedris, the Traitor King", 5, 5)
        .as_legendary()
        .with_subtypes(vec!["Zombie", "Warrior"])
        .from_oracle_text(SEDRIS);
    let dregscape = add_dregscape(&mut s, Zone::Graveyard);
    pool(
        &mut s,
        &[ManaType::Colorless, ManaType::Colorless, ManaType::Black],
    );
    let mut r = s.build();

    let printed_len = r.state().objects[&dregscape].abilities.len();
    let mut offered = offered_indices(&r, dregscape);
    offered.sort_unstable();
    assert_eq!(
        offered.len(),
        2,
        "printed {{2}} and granted {{2}}{{B}} must both be offered, got {offered:?}"
    );
    let costs: Vec<ManaCost> = offered
        .iter()
        .map(|&i| mana_cost_of(&r, dregscape, i))
        .collect();
    assert!(costs.contains(&cost(2, vec![])));
    assert!(costs.contains(&cost(2, vec![ManaCostShard::Black])));

    let granted = *offered
        .iter()
        .find(|&&i| i >= printed_len)
        .expect("the granted unearth is synthesized past the printed abilities");
    assert_eq!(
        mana_cost_of(&r, dregscape, granted),
        cost(2, vec![ManaCostShard::Black])
    );
    activate(&mut r, dregscape, granted);
    r.advance_until_stack_empty();
    assert_settled(&r);
    assert_eq!(pool_total(&r), 0, "the granted instance costs {{2}}{{B}}");
    assert_unearthed_then_exiled(&mut r, dregscape);
}

/// A Dregscape Sliver grantor and a Dregscape Sliver in the graveyard: the
/// granted unearth {2} is identical to the printed one and is not offered a
/// second time.
#[test]
fn identical_granted_unearth_is_not_offered_twice() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    add_dregscape(&mut s, Zone::Battlefield);
    let dregscape = add_dregscape(&mut s, Zone::Graveyard);
    pool(&mut s, &[ManaType::Black; 2]);
    let r = s.build();

    let index = sole_unearth_offer(&r, dregscape);
    assert!(
        index < r.state().objects[&dregscape].abilities.len(),
        "the one offer is the printed unearth"
    );
}

// ---------------------------------------------------------------------------
// Timing, grant lifetime, and departures
// ---------------------------------------------------------------------------

/// CR 602.5d + CR 307.1: "Activate only as a sorcery" — the granted unearth is
/// not offered during the upkeep, and is offered in the main phase of the same
/// board.
#[test]
fn granted_unearth_is_sorcery_speed() {
    for (phase, expected) in [(Phase::Upkeep, false), (Phase::PreCombatMain, true)] {
        let mut s = GameScenario::new();
        s.at_phase(phase);
        add_dregscape(&mut s, Zone::Battlefield);
        let muscle = add_muscle_sliver_to_graveyard(&mut s);
        pool(&mut s, &[ManaType::Black; 2]);
        let r = s.build();
        assert!(object_has_effective_keyword_kind(
            r.state(),
            muscle,
            KeywordKind::Unearth
        ));
        assert_eq!(
            !offered_indices(&r, muscle).is_empty(),
            expected,
            "phase {phase:?}: granted unearth offered only at sorcery speed"
        );
    }
}

/// CR 604.1: the grant lasts only while Dregscape Sliver is on the battlefield.
/// Once it leaves, the graveyard Sliver no longer has unearth.
#[test]
fn granted_unearth_ends_when_the_grantor_leaves() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let grantor = add_dregscape(&mut s, Zone::Battlefield);
    let muscle = add_muscle_sliver_to_graveyard(&mut s);
    pool(&mut s, &[ManaType::Black; 2]);
    let mut r = s.build();
    sole_unearth_offer(&r, muscle);

    move_to_zone(r.state_mut(), grantor, Zone::Exile, &mut Vec::new());
    assert!(!object_has_effective_keyword_kind(
        r.state(),
        muscle,
        KeywordKind::Unearth
    ));
    assert!(offered_indices(&r, muscle).is_empty());
}

/// CR 113.7a: once activated, the granted unearth exists on the stack
/// independently of the grant. Dregscape Sliver leaving the battlefield in
/// response does not stop the Sliver from returning.
#[test]
fn granted_unearth_resolves_after_the_grantor_leaves_in_response() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let grantor = add_dregscape(&mut s, Zone::Battlefield);
    let muscle = add_muscle_sliver_to_graveyard(&mut s);
    pool(&mut s, &[ManaType::Black; 2]);
    let mut r = s.build();

    let index = sole_unearth_offer(&r, muscle);
    activate(&mut r, muscle, index);
    assert_eq!(r.state().stack.len(), 1, "unearth is on the stack");
    move_to_zone(r.state_mut(), grantor, Zone::Exile, &mut Vec::new());
    r.advance_until_stack_empty();
    assert_settled(&r);
    assert_unearthed_then_exiled(&mut r, muscle);
}

/// CR 400.7: if the Sliver leaves the graveyard in response, it is a new object
/// and unearth does not return it.
#[test]
fn granted_unearth_does_nothing_if_the_card_leaves_the_graveyard_in_response() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    add_dregscape(&mut s, Zone::Battlefield);
    let muscle = add_muscle_sliver_to_graveyard(&mut s);
    pool(&mut s, &[ManaType::Black; 2]);
    let mut r = s.build();

    let index = sole_unearth_offer(&r, muscle);
    activate(&mut r, muscle, index);
    assert_eq!(r.state().stack.len(), 1, "unearth is on the stack");
    move_to_zone(r.state_mut(), muscle, Zone::Exile, &mut Vec::new());
    r.advance_until_stack_empty();
    assert_settled(&r);
    assert_eq!(r.state().objects[&muscle].zone, Zone::Exile);
    assert!(!r.state().battlefield.contains(&muscle));
}

// ---------------------------------------------------------------------------
// Self-referential cost (engine-accepted shape)
// ---------------------------------------------------------------------------

/// An unearth granted with a self-referential cost ("the unearth cost is equal
/// to the card's mana cost" lowers to `ManaCost::SelfManaCost`) must cost the
/// recipient's mana cost — {1}{B} here — not {0}.
#[test]
fn granted_self_cost_unearth_costs_the_card_mana_cost() {
    for (mana, affordable) in [
        (vec![ManaType::Black], false),
        (vec![ManaType::Colorless, ManaType::Black], true),
    ] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let grantor = s.add_creature(P0, "Grantor", 1, 1).id();
        let card = s
            .add_creature_to_graveyard(P0, "Graveyard Zombie", 2, 2)
            .with_mana_cost(cost(1, vec![ManaCostShard::Black]))
            .id();
        pool(&mut s, &mana);
        let mut r = s.build();
        r.state_mut().add_transient_continuous_effect(
            grantor,
            P0,
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: card },
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Unearth(ManaCost::SelfManaCost),
            }],
            None,
        );
        engine::game::layers::flush_layers(r.state_mut());
        assert!(object_has_effective_keyword_kind(
            r.state(),
            card,
            KeywordKind::Unearth
        ));

        if !affordable {
            assert!(
                offered_indices(&r, card).is_empty(),
                "a self-cost unearth must not be free"
            );
            continue;
        }
        let index = sole_unearth_offer(&r, card);
        assert_eq!(
            mana_cost_of(&r, card, index),
            cost(1, vec![ManaCostShard::Black])
        );
        activate(&mut r, card, index);
        r.advance_until_stack_empty();
        assert_settled(&r);
        assert_eq!(pool_total(&r), 0);
        assert_eq!(r.state().objects[&card].zone, Zone::Battlefield);
    }
}

/// CR 118.6 + CR 202.1b: a self-cost unearth on a card with NO mana cost (Dryad
/// Arbor) has an unpayable cost, so it is never offered and can't be activated —
/// it must not resolve to a free activation. The control, Ornithopter, has a
/// printed {0}: its self-cost unearth is a payable {0} and returns it.
#[test]
fn granted_self_cost_unearth_on_a_card_with_no_mana_cost_is_unpayable() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let grantor = s.add_creature(P0, "Grantor", 1, 1).id();
    let arbor = s
        .add_creature_to_graveyard(P0, "Dryad Arbor", 1, 1)
        .as_land()
        .as_creature()
        .with_subtypes(vec!["Forest", "Dryad"])
        .with_mana_cost(ManaCost::NoCost)
        .id();
    let thopter = s
        .add_creature_to_graveyard(P0, "Ornithopter", 0, 2)
        .as_artifact_creature()
        .with_subtypes(vec!["Thopter"])
        .with_mana_cost(cost(0, vec![]))
        .from_oracle_text("Flying")
        .id();
    let mut r = s.build();
    for card in [arbor, thopter] {
        r.state_mut().add_transient_continuous_effect(
            grantor,
            P0,
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: card },
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Unearth(ManaCost::SelfManaCost),
            }],
            None,
        );
    }
    engine::game::layers::flush_layers(r.state_mut());

    // Reach guards: the recipients' mana-cost representations, and the grant on both.
    assert_eq!(r.state().objects[&arbor].mana_cost, ManaCost::NoCost);
    assert_eq!(r.state().objects[&thopter].mana_cost, cost(0, vec![]));
    for card in [arbor, thopter] {
        assert!(object_has_effective_keyword_kind(
            r.state(),
            card,
            KeywordKind::Unearth
        ));
    }
    assert_eq!(
        r.state().players[0].mana_pool.total(),
        0,
        "no mana available"
    );

    // Control: Ornithopter's {0} unearth is offered with no mana and returns it.
    let index = sole_unearth_offer(&r, thopter);
    assert_eq!(mana_cost_of(&r, thopter, index), cost(0, vec![]));

    // CR 118.6: Dryad Arbor's unearth is unpayable — not offered, and an
    // activation attempt at the index a granted ability would occupy is refused.
    assert!(
        offered_indices(&r, arbor).is_empty(),
        "an unearth based on a missing mana cost must not be offered"
    );
    let arbor_index = r.state().objects[&arbor].abilities.len();
    assert!(
        r.act(GameAction::ActivateAbility {
            source_id: arbor,
            ability_index: arbor_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .is_err(),
        "the unpayable unearth can't be activated"
    );
    assert_eq!(r.state().objects[&arbor].zone, Zone::Graveyard);
    assert!(r.state().stack.is_empty());

    activate(&mut r, thopter, index);
    r.advance_until_stack_empty();
    assert_settled(&r);
    assert_eq!(r.state().objects[&thopter].zone, Zone::Battlefield);
    assert_eq!(r.state().objects[&arbor].zone, Zone::Graveyard);
}

// ---------------------------------------------------------------------------
// Controls
// ---------------------------------------------------------------------------

/// Control: a graveyard Dregscape Sliver's PRINTED unearth, with no grantor.
#[test]
fn control_printed_unearth_is_offered_and_resolves() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let dregscape = add_dregscape(&mut s, Zone::Graveyard);
    pool(&mut s, &[ManaType::Black; 2]);
    let mut r = s.build();

    let index = sole_unearth_offer(&r, dregscape);
    activate(&mut r, dregscape, index);
    r.advance_until_stack_empty();
    assert_settled(&r);
    assert_eq!(pool_total(&r), 0);
    assert_unearthed_then_exiled(&mut r, dregscape);
}

/// Control: the sibling grant on the same seam, Sliver Gravemother's encore.
#[test]
fn control_gravemother_granted_encore_is_offered() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_creature(P0, "Sliver Gravemother", 6, 6)
        .as_legendary()
        .with_subtypes(vec!["Sliver"])
        .from_oracle_text(GRAVEMOTHER);
    let muscle = add_muscle_sliver_to_graveyard(&mut s);
    pool(&mut s, &[ManaType::Black; 2]);
    let r = s.build();

    let offered = offered_indices(&r, muscle);
    assert_eq!(offered.len(), 1);
    let defs = activated_ability_definitions(r.state(), muscle);
    assert!(defs
        .iter()
        .any(|(i, d)| *i == offered[0] && matches!(d.effect.as_ref(), Effect::Encore)));
}
