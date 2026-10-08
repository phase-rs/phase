//! CR 702.37c / CR 702.168b + CR 601.2b: a morph/megamorph/disguise card is
//! castable face down for a fixed generic {3}, so the cast must be OFFERED
//! whenever that face-down cast is feasible — even when the printed cost is not
//! payable (issue #7770: three Islands could not cast a green morph creature,
//! although dispatching `CastSpell` directly succeeded and auto-routed face
//! down).
//!
//! The defect was offer-side only: `castable_spell_verdict_with_probe` judged a
//! successfully prepared spell by its printed cost plus the casting-variant
//! menu, and `CastingVariant::FaceDown` only enters that menu under an
//! `unlimited_hand_cast_free_source` permission (Omniscience). Without one, the
//! offer said no while the reducer said yes. The fix asks the same three
//! questions the dispatch gate asks (`face_down_cast_is_feasible`): effective
//! keyword, permitted against the blanked 2/2 profile, {3} payable after cost
//! modification.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const KADENA_TEXT: &str =
    "The first face-down creature spell you cast each turn costs {3} less to cast.";

fn green_morph_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 1,
    }
}

fn offered_cast(state: &engine::types::game_state::GameState, spell: ObjectId) -> bool {
    legal_actions(state).iter().any(|action| {
        matches!(
            action,
            GameAction::CastSpell { object_id, .. } if *object_id == spell
        )
    })
}

fn cast(runner: &mut engine::game::scenario::GameRunner, spell: ObjectId) -> bool {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .is_ok()
}

/// Issue #7770, the reported board: a {1}{G} morph creature in hand and three
/// untapped Islands. The printed cost is unpayable, the generic {3} face-down
/// cost is payable by auto-tapping the Islands, so the cast must be offered.
///
/// Discriminating: without the fix the offer gate never asks the face-down
/// question in the prepare-success branch, and this assertion fails.
#[test]
fn a_morph_creature_is_offered_when_only_off_color_mana_can_pay_the_3() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let morph = scenario
        .add_creature_to_hand(P0, "Ainok Survivalist", 2, 1)
        .with_keyword(Keyword::Morph(green_morph_cost()))
        .with_mana_cost(green_morph_cost())
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }
    let runner = scenario.build();

    assert!(
        offered_cast(runner.state(), morph),
        "three untapped Islands pay the generic {{3}}, so the cast must be offered"
    );
}

/// Offer→dispatch parity on the same board: the offered action, dispatched
/// verbatim, must produce the face-down spell on the stack (the reducer
/// auto-routes when only the {3} is payable). Guards against an offer that
/// surfaces an action the reducer then rejects.
#[test]
fn the_offered_off_color_cast_dispatches_face_down() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let morph = scenario
        .add_creature_to_hand(P0, "Ainok Survivalist", 2, 1)
        .with_keyword(Keyword::Morph(green_morph_cost()))
        .with_mana_cost(green_morph_cost())
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }
    let mut runner = scenario.build();

    assert!(
        offered_cast(runner.state(), morph),
        "reach-guard: the cast must be offered before it is dispatched"
    );
    assert!(
        cast(&mut runner, morph),
        "the offered cast must be accepted"
    );
    assert!(
        runner.state().objects[&morph].face_down
            && runner.state().objects[&morph].zone == Zone::Stack,
        "only the {{3}} is payable, so the reducer must auto-route FACE DOWN"
    );
}

/// CR 601.2f: the offer gate runs cost modifiers, so Kadena's reduction to {0}
/// makes the face-down cast offered with NO mana source at all. This is the
/// offer-side half of the empty-pool playtest find from #7769; it needs both
/// that fix (modifier-aware affordability) and this one (the gate asks at all).
#[test]
fn kadena_makes_the_face_down_cast_offered_with_no_mana_at_all() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Snake Sorcerer", 3, 3, KADENA_TEXT);
    let morph = scenario
        .add_creature_to_hand(P0, "Secret Beast", 4, 5)
        .with_keyword(Keyword::Morph(ManaCost::generic(4)))
        .with_mana_cost(ManaCost::generic(5))
        .id();
    let mut runner = scenario.build();

    assert!(
        offered_cast(runner.state(), morph),
        "Kadena takes the {{3}} to {{0}} — the cast must be offered with an empty board"
    );
    assert!(
        cast(&mut runner, morph),
        "the offered cast must be accepted"
    );
    assert!(
        runner.state().objects[&morph].face_down,
        "the creature must reach the stack face down"
    );
}

/// PIN (green without the fix, Regel 5): the rescue must not bypass creature
/// -spell timing. Outside a main phase the blanked-profile prepare fails
/// (CR 302.1 via CR 708.4's spell rules), so the cast stays unoffered even
/// though the {3} is payable.
#[test]
fn the_face_down_offer_respects_sorcery_speed_timing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareBlockers);
    let morph = scenario
        .add_creature_to_hand(P0, "Ainok Survivalist", 2, 1)
        .with_keyword(Keyword::Morph(green_morph_cost()))
        .with_mana_cost(green_morph_cost())
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }
    let mut runner = scenario.build();
    runner.state_mut().phase = Phase::DeclareBlockers;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    assert!(
        !offered_cast(runner.state(), morph),
        "CR 302.1: a creature spell — face down included — is not castable in combat"
    );
}

/// PIN (green without the fix, Regel 5): no leak onto morphless cards. A plain
/// creature with the same unpayable printed cost has no face-down cast and must
/// stay unoffered.
#[test]
fn a_creature_without_morph_stays_unoffered_when_unaffordable() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let plain = scenario
        .add_creature_to_hand(P0, "Plain Bear", 2, 2)
        .with_mana_cost(green_morph_cost())
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }
    let runner = scenario.build();

    assert!(
        !offered_cast(runner.state(), plain),
        "no morph, printed cost unpayable — the cast must not be offered"
    );
}

/// PIN (green without the fix, Regel 5): the positive counter-direction. With
/// on-color sources the printed cast is payable and the offer must keep saying
/// yes exactly as before.
#[test]
fn the_printed_cast_stays_offered_with_on_color_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let morph = scenario
        .add_creature_to_hand(P0, "Ainok Survivalist", 2, 1)
        .with_keyword(Keyword::Morph(green_morph_cost()))
        .with_mana_cost(green_morph_cost())
        .id();
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Green);
    let runner = scenario.build();

    assert!(
        offered_cast(runner.state(), morph),
        "the printed {{1}}{{G}} is payable — offered exactly as before the fix"
    );
}

/// CR 702.37b (Megamorph) — same face-down rule, distinct keyword variant.
/// `object_has_effective_face_down_keyword` spans Morph/Megamorph/Disguise; a
/// regression narrowing that scan would leave the 31 Megamorph cards
/// unoffered while every Morph test stays green.
#[test]
fn a_megamorph_creature_is_offered_and_dispatches_face_down() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let morph = scenario
        .add_creature_to_hand(P0, "Ainok Survivalist", 2, 1)
        .with_keyword(Keyword::Megamorph(green_morph_cost()))
        .with_mana_cost(green_morph_cost())
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }
    let mut runner = scenario.build();

    assert!(
        offered_cast(runner.state(), morph),
        "a Megamorph card must be offered when only the {{3}} is payable"
    );
    assert!(
        cast(&mut runner, morph),
        "the offered cast must be accepted"
    );
    assert!(
        runner.state().objects[&morph].face_down
            && runner.state().objects[&morph].zone == Zone::Stack,
        "the Megamorph spell must reach the stack face down"
    );
}

/// CR 702.168b (Disguise) — the third keyword of the class (48 cards), cast
/// face down as a 2/2 with ward {2} for the same fixed {3}.
#[test]
fn a_disguise_creature_is_offered_and_dispatches_face_down() {
    use engine::types::keywords::DisguiseCost;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let disguised = scenario
        .add_creature_to_hand(P0, "Faceless Hunter", 3, 2)
        .with_keyword(Keyword::Disguise(DisguiseCost::Mana(green_morph_cost())))
        .with_mana_cost(green_morph_cost())
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }
    let mut runner = scenario.build();

    assert!(
        offered_cast(runner.state(), disguised),
        "a Disguise card must be offered when only the {{3}} is payable"
    );
    assert!(
        cast(&mut runner, disguised),
        "the offered cast must be accepted"
    );
    assert!(
        runner.state().objects[&disguised].face_down
            && runner.state().objects[&disguised].zone == Zone::Stack,
        "the Disguise spell must reach the stack face down"
    );
}
