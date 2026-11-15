//! A modal spell none of whose mode selections can be chosen is not castable.
//!
//! CR 700.2a: a mode that would be illegal to choose (here: it requires a
//! target and has none) can't be chosen. CR 601.2b-c: a spell whose announced
//! modes need targets that don't exist can't be cast, so a modal spell with no
//! legal mode selection can't be cast at all, and must not be offered.
//!
//! Abrade ({1}{R} instant, "Choose one — • Abrade deals 3 damage to target
//! creature. • Destroy target artifact.") with no creature and no artifact on
//! the battlefield: `spell_has_legal_targets` already said false (both modes
//! are in `spell_modal_unavailable_modes`), but the legal-action gate
//! (`can_cast_prepared_now_with_probe`) short-circuited every modal spell to
//! `targets_ok = prepared.modal.is_some() || ...`, so the cast was offered; the
//! cast then stopped at `WaitingFor::ModeChoice` with every mode unavailable,
//! and `SelectModes` refused every answer ("Must choose at least 1 modes"),
//! leaving the caster nothing but cancelling.
//!
//! The class: the minimum number of modes must be choosable (CR 700.2d —
//! "choose two" with one choosable mode has no legal selection), a mode with
//! no targets is always choosable, and Spree / Tiered spells (CR 702.172a /
//! CR 702.183a) need a mode that is both choosable and affordable. The same
//! holds when the cost is waived: a free-cast permission (CR 118.9) or a
//! cast during resolution (cascade, CR 702.85a).
//!
//! A cast submitted directly, without the offer, is refused at the mode
//! announcement and reversed (CR 601.2e). A repeat-allowed modal (CR 700.2d)
//! fills every pick from one choosable mode, and a "choose up to" spell
//! (CR 700.2) is cast with no modes — even through cascade — when none can be
//! chosen.

use engine::ai_support::legal_actions;
use engine::game::casting::{can_cast_for_free_now, can_cast_object_now};
use engine::game::engine::EngineError;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::{CastChoice, GameAction};
use engine::types::game_state::{CastOfferKind, CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::statics::CastFrequency;
use engine::types::zones::Zone;

use super::support::shared_card_db;

/// P0 holds `spell` with `lands` on the battlefield; `p0_creature` /
/// `p1_creature` put a Grizzly Bears on that side.
fn scenario_with(
    spell: &str,
    lands: &[&str],
    p0_creature: bool,
    p1_creature: bool,
) -> Option<(GameRunner, ObjectId)> {
    let db = shared_card_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario.add_real_card(P0, spell, Zone::Hand, db);
    for land in lands {
        scenario.add_real_card(P0, land, Zone::Battlefield, db);
    }
    if p0_creature {
        scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    }
    if p1_creature {
        scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    Some((runner, spell))
}

fn abrade_scenario(with_bear: bool) -> Option<(GameRunner, ObjectId)> {
    scenario_with("Abrade", &["Mountain", "Mountain"], false, with_bear)
}

fn offers_cast(runner: &GameRunner, object: ObjectId) -> bool {
    legal_actions(runner.state())
        .iter()
        .any(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == object))
}

/// The castability verdict and the legal-action offer agree, and are `expected`.
fn assert_castable(runner: &GameRunner, object: ObjectId, expected: bool, why: &str) {
    assert_eq!(
        can_cast_object_now(runner.state(), P0, object),
        expected,
        "can_cast_object_now: {why}"
    );
    assert_eq!(
        offers_cast(runner, object),
        expected,
        "legal_actions: {why}"
    );
}

#[test]
fn modal_spell_with_no_choosable_mode_is_not_offered() {
    let Some((runner, abrade)) = abrade_scenario(false) else {
        return;
    };
    assert_castable(
        &runner,
        abrade,
        false,
        "CR 700.2a + 601.2c: neither of Abrade's modes has a legal target, so it can't be cast",
    );
}

#[test]
fn modal_spell_with_a_choosable_mode_is_offered() {
    let Some((runner, abrade)) = abrade_scenario(true) else {
        return;
    };
    assert_castable(&runner, abrade, true, "the damage mode has a target");
}

#[test]
fn modal_spell_becomes_castable_once_a_creature_enters() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let abrade = scenario.add_real_card(P0, "Abrade", Zone::Hand, db);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    for land in ["Mountain", "Mountain", "Forest", "Forest"] {
        scenario.add_real_card(P0, land, Zone::Battlefield, db);
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    assert_castable(&runner, abrade, false, "no creature and no artifact yet");
    runner.cast(bears).resolve();
    assert_eq!(
        runner.state().objects.get(&bears).map(|obj| obj.zone),
        Some(Zone::Battlefield),
        "Grizzly Bears resolved"
    );
    assert_castable(
        &runner,
        abrade,
        true,
        "CR 700.2a: the damage mode now has a legal target",
    );
}

#[test]
fn modal_spell_with_an_untargeted_mode_stays_castable() {
    // Izzet Charm: counter target noncreature spell / 2 damage to target
    // creature / draw two then discard two. Empty stack, no creature: only the
    // untargeted mode can be chosen, and that is enough for "choose one".
    let Some((runner, charm)) = scenario_with("Izzet Charm", &["Island", "Mountain"], false, false)
    else {
        return;
    };
    assert_castable(
        &runner,
        charm,
        true,
        "a mode with no targets is always choosable",
    );
}

#[test]
fn choose_two_with_one_choosable_mode_is_not_castable() {
    // Dromoka's Command, "Choose two": prevent damage from target instant or
    // sorcery spell / target player sacrifices an enchantment / +1/+1 counter
    // on target creature / target creature you control fights target creature
    // you don't control. With an empty stack and no creature, only the
    // player-targeting mode can be chosen.
    let Some((runner, command)) =
        scenario_with("Dromoka's Command", &["Forest", "Plains"], false, false)
    else {
        return;
    };
    assert_castable(
        &runner,
        command,
        false,
        "CR 700.2d + 601.2b: two different modes are required and only one can be chosen",
    );
}

#[test]
fn choose_two_with_two_choosable_modes_is_castable() {
    // A creature of P0's own makes the counter mode choosable too.
    let Some((runner, command)) =
        scenario_with("Dromoka's Command", &["Forest", "Plains"], true, false)
    else {
        return;
    };
    assert_castable(
        &runner,
        command,
        true,
        "the sacrifice and counter modes can both be chosen",
    );
}

#[test]
fn spree_spell_with_no_choosable_mode_is_not_castable() {
    // Return the Favor ({R}{R} spree, +{1} each): both modes target a spell or
    // ability on the stack. The stack is empty, so although every mode cost is
    // affordable, no mode can be chosen.
    let Some((runner, favor)) = scenario_with(
        "Return the Favor",
        &["Mountain", "Mountain", "Mountain", "Mountain"],
        false,
        false,
    ) else {
        return;
    };
    assert_castable(
        &runner,
        favor,
        false,
        "CR 702.172a + 700.2a: an affordable spree mode must also be choosable",
    );
}

#[test]
fn spree_spell_needs_one_mode_both_choosable_and_affordable() {
    // Phantom Interference ({U} spree): +{3} create a 2/2 Spirit token,
    // +{1} counter target spell unless its controller pays {2}. With an empty
    // stack only the token mode can be chosen, and three Islands can't pay
    // its {3}{U}; the counter mode is affordable but has no target. Choosable
    // and affordable must hold of the same mode.
    let Some((runner, interference)) = scenario_with(
        "Phantom Interference",
        &["Island", "Island", "Island"],
        false,
        false,
    ) else {
        return;
    };
    assert_castable(
        &runner,
        interference,
        false,
        "CR 702.172a + 700.2a: the choosable mode is unaffordable and the affordable one unchoosable",
    );

    let Some((runner, interference)) = scenario_with(
        "Phantom Interference",
        &["Island", "Island", "Island", "Island"],
        false,
        false,
    ) else {
        return;
    };
    assert_castable(
        &runner,
        interference,
        true,
        "a fourth Island pays for the token mode",
    );
}

#[test]
fn tiered_spell_needs_a_choosable_mode() {
    // Tifa's Limit Break ({G} tiered): every tier targets a creature.
    let Some((runner, tifa)) =
        scenario_with("Tifa's Limit Break", &["Forest", "Forest"], false, false)
    else {
        return;
    };
    assert_castable(
        &runner,
        tifa,
        false,
        "CR 702.183a + 700.2a: no tier has a legal target",
    );

    let Some((runner, tifa)) =
        scenario_with("Tifa's Limit Break", &["Forest", "Forest"], false, true)
    else {
        return;
    };
    assert_castable(
        &runner,
        tifa,
        true,
        "the {0} tier has a legal target and is affordable",
    );
}

#[test]
fn escalate_spell_needs_its_minimum_of_one_choosable_mode() {
    // Borrowed Hostility ({R} instant, escalate {3}, "Choose one or both"):
    // both modes target a creature.
    let Some((runner, hostility)) =
        scenario_with("Borrowed Hostility", &["Mountain"], false, false)
    else {
        return;
    };
    assert!(
        runner
            .state()
            .objects
            .get(&hostility)
            .and_then(|obj| obj.modal.as_ref())
            .is_some_and(|modal| modal.min_choices == 1 && modal.mode_count == 2),
        "Borrowed Hostility parses as choose one or both"
    );
    assert_castable(
        &runner,
        hostility,
        false,
        "CR 702.120a + 700.2a: neither mode has a legal target",
    );

    let Some((runner, hostility)) = scenario_with("Borrowed Hostility", &["Mountain"], false, true)
    else {
        return;
    };
    assert_castable(
        &runner,
        hostility,
        true,
        "one choosable mode meets the minimum; escalate is paid only beyond the first",
    );
}

/// P0 controls One with the Multiverse ("Once during each of your turns, you
/// may cast a spell from your hand or the top of your library without paying
/// its mana cost") and holds Abrade, with no land to pay for it.
fn free_cast_scenario(p1_creature: bool) -> Option<(GameRunner, ObjectId, ObjectId)> {
    let db = shared_card_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let permission = scenario.add_real_card(P0, "One with the Multiverse", Zone::Battlefield, db);
    let abrade = scenario.add_real_card(P0, "Abrade", Zone::Hand, db);
    if p1_creature {
        scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    Some((runner, permission, abrade))
}

fn offers_free_cast(runner: &GameRunner, object: ObjectId, permission: ObjectId) -> bool {
    legal_actions(runner.state()).iter().any(|a| {
        matches!(
            a,
            GameAction::CastSpellForFree { object_id, source_id, .. }
                if *object_id == object && *source_id == permission
        )
    })
}

#[test]
fn free_cast_permission_needs_a_legal_mode_selection() {
    // CR 118.9 + CR 700.2a: casting without paying the mana cost waives the
    // cost, not the rule that a mode whose targets can't be chosen can't be
    // chosen.
    let Some((runner, permission, abrade)) = free_cast_scenario(false) else {
        return;
    };
    assert!(
        !can_cast_for_free_now(
            runner.state(),
            P0,
            abrade,
            permission,
            CastFrequency::OncePerTurn
        ),
        "no creature and no artifact: Abrade has no legal mode, cost or no cost"
    );
    assert!(
        !offers_free_cast(&runner, abrade, permission),
        "the free cast must not be offered"
    );

    let Some((runner, permission, abrade)) = free_cast_scenario(true) else {
        return;
    };
    assert!(
        can_cast_for_free_now(
            runner.state(),
            P0,
            abrade,
            permission,
            CastFrequency::OncePerTurn
        ),
        "the damage mode has a target"
    );
    assert!(
        offers_free_cast(&runner, abrade, permission),
        "the free cast is offered once a mode can be chosen"
    );
}

/// P0 casts Enlisted Wurm ({4}{G}{W}, cascade) with `hit_name` the only card
/// in their library, lets the cascade trigger resolve, and answers the cascade
/// offer with "cast". Returns the runner after that answer and the hit.
fn cascade_into(hit_name: &str, p1_creature: bool) -> Option<(GameRunner, ObjectId)> {
    let db = shared_card_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wurm = scenario.add_real_card(P0, "Enlisted Wurm", Zone::Hand, db);
    let hit = scenario.add_real_card(P0, hit_name, Zone::Library, db);
    for land in ["Forest", "Forest", "Forest", "Plains", "Plains", "Plains"] {
        scenario.add_real_card(P0, land, Zone::Battlefield, db);
    }
    if p1_creature {
        scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let card_id = runner.state().objects[&wurm].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: wurm,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Enlisted Wurm is cast");
    runner.act(GameAction::PassPriority).expect("P0 passes");
    runner.act(GameAction::PassPriority).expect("P1 passes");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::Cascade { hit_card, .. },
                ..
            } if hit_card == hit
        ),
        "cascade must offer {hit_name}; waiting_for = {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::CascadeChoice {
            choice: CastChoice::Cast,
        })
        .expect("the cascade cast answer is accepted");
    Some((runner, hit))
}

#[test]
fn cascade_cast_of_choose_two_with_one_choosable_mode_is_rejected() {
    // CR 702.85a + CR 700.2d: while the cascade trigger resolves, the stack
    // holds only Enlisted Wurm, a creature spell, and no creature is on the
    // battlefield, so only "target player sacrifices an enchantment" can be
    // chosen. "Choose two" can't be satisfied: the Command can't be cast and
    // goes to the bottom of the library, rather than stranding the caster at
    // a mode prompt no answer satisfies.
    let Some((runner, command)) = cascade_into("Dromoka's Command", false) else {
        return;
    };
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::ModeChoice { .. }),
        "the uncastable Command must not reach mode choice; waiting_for = {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects.get(&command).map(|obj| obj.zone),
        Some(Zone::Library),
        "the uncastable hit goes to the bottom of the library"
    );
}

#[test]
fn cascade_cast_of_choose_two_with_two_choosable_modes_reaches_mode_choice() {
    // P1's Grizzly Bears makes "+1/+1 counter on target creature" choosable
    // too, so the cascade cast proceeds to the mode prompt.
    let Some((runner, command)) = cascade_into("Dromoka's Command", true) else {
        return;
    };
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::ModeChoice { pending_cast, .. } if pending_cast.object_id == command
        ),
        "the castable Command proceeds to mode choice; waiting_for = {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn cascade_cast_of_choose_up_to_with_no_choosable_mode_casts_with_no_modes() {
    // CR 700.2 + CR 700.2a + CR 702.85a + CR 608.2g: "Choose up to two" admits
    // zero modes, so a cascade hit whose every mode lacks a legal target is
    // still cast — with no modes — rather than rejected to the library.
    //
    // Call Damage Control ({1}{G} sorcery, "Choose up to two. Return those
    // cards from your graveyard to your hand." — target artifact / creature /
    // enchantment / land card): both graveyards are empty, so no mode has a
    // target.
    let Some((mut runner, call)) = cascade_into("Call Damage Control", false) else {
        return;
    };
    assert!(
        runner.state().players[0].graveyard.is_empty()
            && runner.state().players[1].graveyard.is_empty(),
        "both graveyards are empty, so no mode has a legal target"
    );
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::ModeChoice { pending_cast, modal, unavailable_modes, .. }
                if pending_cast.object_id == call
                    && modal.min_choices == 0
                    && (0..modal.mode_count).all(|mode| unavailable_modes.contains(&mode))
        ),
        "the cascade cast reaches mode choice with every mode unavailable; \
         waiting_for = {:?}, Call Damage Control zone = {:?}",
        runner.state().waiting_for,
        runner.state().objects.get(&call).map(|obj| obj.zone)
    );

    runner
        .act(GameAction::SelectModes { indices: vec![] })
        .expect("CR 700.2: choosing no modes meets \"choose up to two\"");
    assert_eq!(
        runner.state().objects[&call].zone,
        Zone::Stack,
        "Call Damage Control is cast with no modes"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "the cast completes and P0 receives priority; waiting_for = {:?}",
        runner.state().waiting_for
    );

    runner.act(GameAction::PassPriority).expect("P0 passes");
    runner.act(GameAction::PassPriority).expect("P1 passes");
    // CR 608.2n: the resolved sorcery goes to its owner's graveyard.
    assert_eq!(
        runner.state().objects[&call].zone,
        Zone::Graveyard,
        "the zero-mode spell resolves"
    );
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| runner.state().objects[&entry.id].name == "Enlisted Wurm"),
        "Enlisted Wurm is still on the stack: the zero-mode spell resolved on its own"
    );
}

#[test]
fn repeatable_mode_fills_every_pick() {
    // Mystic Confluence ({3}{U}{U}, "Choose three. You may choose the same
    // mode more than once."): counter target spell unless its controller pays
    // {3} / return target creature to its owner's hand / draw a card. The
    // stack is empty and there is no creature, so the counter and bounce modes
    // have no target; only the draw mode can be chosen.
    let Some((runner, confluence)) = scenario_with(
        "Mystic Confluence",
        &["Island", "Island", "Island", "Island", "Island"],
        false,
        false,
    ) else {
        return;
    };
    assert_castable(
        &runner,
        confluence,
        true,
        "CR 700.2d: the same mode may be chosen more than once, so the draw mode can be chosen three times",
    );
}

#[test]
fn directly_submitted_cast_with_no_legal_mode_is_refused() {
    // CR 601.2e + CR 733.1 + CR 700.2a: a cast submitted without the offer —
    // Abrade with no creature and no artifact — has no legal mode selection;
    // it is refused at the mode announcement and the proposal is reversed.
    let Some((mut runner, abrade)) = abrade_scenario(false) else {
        return;
    };
    let card_id = runner.state().objects[&abrade].card_id;
    let result = runner.act(GameAction::CastSpell {
        object_id: abrade,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    });
    assert!(
        matches!(result, Err(EngineError::ActionNotAllowed(_))),
        "the cast must be refused; result = {result:?}"
    );
    assert_eq!(
        runner.state().objects[&abrade].zone,
        Zone::Hand,
        "Abrade returns to the hand"
    );
    assert!(
        runner.state().stack.is_empty(),
        "nothing is left on the stack"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "P0 keeps priority; waiting_for = {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn directly_submitted_cast_with_a_legal_mode_reaches_mode_choice_and_resolves() {
    // CR 601.2e + CR 733.1 + CR 700.2a: reach guard for the refusal — with a
    // creature to target, the same direct submission reaches the mode
    // announcement with the damage mode choosable.
    let Some((mut runner, abrade)) = abrade_scenario(true) else {
        return;
    };
    let card_id = runner.state().objects[&abrade].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: abrade,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the cast is accepted");
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::ModeChoice { unavailable_modes, .. } if !unavailable_modes.contains(&0)
        ),
        "the damage mode is choosable; waiting_for = {:?}",
        runner.state().waiting_for
    );

    // The whole pipeline on the same runner: choose the damage mode, then
    // 3 damage to a 2/2 is lethal (CR 704.5g).
    runner
        .act(GameAction::SelectModes { indices: vec![0] })
        .expect("the damage mode is accepted");
    let bear = runner
        .state()
        .objects
        .iter()
        .find(|(_, o)| o.name == "Grizzly Bears" && o.controller == P1)
        .map(|(id, _)| *id)
        .expect("the opponent's Grizzly Bears");
    // Observed on the first run: Abrade's damage mode has exactly one legal
    // target (the opponent's Grizzly Bears), so the engine auto-selects it and
    // no `TargetSelection` prompt is raised; the cast completes and P0 holds
    // priority with the spell on the stack. Asserting that exact state means a
    // regression that skips or mis-routes targeting cannot pass.
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::Priority { player } if *player == P0),
        "the sole legal target is auto-selected; waiting_for = {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().stack.len(), 1, "Abrade is on the stack");
    runner.pass_both_players();
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Graveyard,
        "Abrade's 3 damage kills the 2/2"
    );
    assert!(runner.state().stack.is_empty(), "the spell resolved");
    assert_eq!(
        runner.state().objects[&abrade].zone,
        Zone::Graveyard,
        "the resolved instant goes to its owner's graveyard"
    );
}
