//! Regression coverage for the reflexive **"When you discard a card this way,
//! <effect>"** trigger class created by a preceding "discard a card" instruction
//! in the same ability (CR 603.12 reflexive triggered abilities; CR 701.9a
//! discard = hand → graveyard).
//!
//! Two Standard-legal cards motivate the class:
//!
//!   * **Talion's Messenger** — attack trigger body "draw a card, then discard a
//!     card. When you discard a card this way, put a +1/+1 counter on target
//!     Faerie you control."
//!   * **The Ancient One** — activated body "Draw a card, then discard a card.
//!     When you discard a card this way, target player mills cards equal to its
//!     mana value." ("its" = the discarded card → CR 202.3 mana value, resolved
//!     via the CR 608.2k/CR 400.7j anaphoric referent captured when the card
//!     reaches the public graveyard.)
//!
//! These tests drive the REAL pipeline: the authoritative Oracle body is parsed
//! with `parse_effect_chain` (which routes the reflexive clause through
//! `strip_if_you_do_conditional` → `parse_you_discard_this_way_clause` →
//! `AbilityCondition::ZoneChangedThisWay`), built into a `ResolvedAbility`, and
//! resolved through `resolve_ability_chain`. On revert of the parser fix the
//! reflexive clause parses to `Effect::Unimplemented { name: "when" }`, the
//! gated sub never runs, and every positive assertion below flips.
//!
//! CR ANCHORS:
//!   * CR 603.12 — reflexive triggered abilities ("when [something happens] this
//!     way") are checked immediately after creation against events earlier in
//!     the same resolution.
//!   * CR 701.9a — discard = move from hand to graveyard.
//!   * CR 202.3 — mana value (The Ancient One's "its mana value").
//!   * CR 608.2k / CR 400.7j — an effect referring to the discarded object finds
//!     it in the public graveyard via the anaphoric referent.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, ResolvedAbility, TargetRef};
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::player::PlayerId;
use engine::types::GameAction;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

const TALION_BODY: &str = "draw a card, then discard a card. When you discard a card this way, put a +1/+1 counter on target Faerie you control.";
const ANCIENT_BODY: &str = "Draw a card, then discard a card. When you discard a card this way, target player mills cards equal to its mana value.";

/// Set an explicit printed mana cost on an already-created object so its mana
/// value is deterministic for "its mana value" assertions.
fn set_mv(runner: &mut GameRunner, id: ObjectId, generic: u32) {
    runner.state_mut().objects.get_mut(&id).unwrap().mana_cost = ManaCost::Cost {
        shards: Vec::new(),
        generic,
    };
}

fn p1p1_on(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn graveyard_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].graveyard.len()
}

/// CR 603.12 + CR 701.9a: Talion's Messenger — after the forced single discard,
/// the reflexive "When you discard a card this way" sub puts a +1/+1 counter on
/// the target Faerie. DISCRIMINATION: on revert the sub is `Unimplemented`, so
/// the Faerie keeps 0 counters and this assertion fails.
#[test]
fn talion_messenger_discard_this_way_puts_counter_on_faerie() {
    let mut scenario = GameScenario::new();

    // Target Faerie the controller controls.
    let faerie = scenario
        .add_creature(P0, "Faerie Token", 1, 1)
        .with_subtypes(vec!["Faerie"])
        .id();

    // Empty hand pre-draw + a single library card: "draw a card" pulls it into
    // hand, "discard a card" then force-discards the ONLY hand card (no choice
    // prompt — the discard resolves inline and populates last_zone_changed_ids
    // synchronously so the reflexive gate sees it).
    scenario.with_library_top(P0, &["Drawn Card"]);

    let mut runner = scenario.build();

    let def = parse_effect_chain(TALION_BODY, AbilityKind::Spell);
    // The reflexive sub targets "target Faerie you control"; supply it up front
    // so the inline-resolved gated sub binds to the Faerie (parent-target
    // propagation seeds the sub's target).
    let ability: ResolvedAbility = build_resolved_from_def(&def, faerie, P0);
    let ability = ResolvedAbility {
        targets: vec![TargetRef::Object(faerie)],
        ..ability
    };

    assert_eq!(
        p1p1_on(&runner, faerie),
        0,
        "Faerie starts with no counters"
    );

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("draw→discard→reflexive chain resolves");

    assert_eq!(
        p1p1_on(&runner, faerie),
        1,
        "reflexive 'When you discard a card this way' must place a +1/+1 counter on the Faerie"
    );
    assert_eq!(
        graveyard_len(&runner, P0),
        1,
        "exactly one card was discarded this way"
    );
}

/// CR 603.12: NEGATIVE — when no discard occurs (empty hand AND empty library so
/// the draw fails too), the reflexive condition is false and NO counter lands.
/// This proves the counter is gated on the discard event, not on the trigger
/// firing. On revert the sub is `Unimplemented` (no-op) and would also leave 0
/// counters, so this case alone does not discriminate — it pairs with the
/// positive test to pin the gate semantics.
#[test]
fn talion_messenger_no_discard_no_counter() {
    let mut scenario = GameScenario::new();
    let faerie = scenario
        .add_creature(P0, "Faerie Token", 1, 1)
        .with_subtypes(vec!["Faerie"])
        .id();
    // No library and no hand → draw is a no-op, discard finds an empty hand.
    let mut runner = scenario.build();

    let def = parse_effect_chain(TALION_BODY, AbilityKind::Spell);
    let ability = build_resolved_from_def(&def, faerie, P0);
    let ability = ResolvedAbility {
        targets: vec![TargetRef::Object(faerie)],
        ..ability
    };

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("chain resolves even with nothing to discard");

    assert_eq!(
        p1p1_on(&runner, faerie),
        0,
        "with no discard the reflexive gate is false — no counter may land"
    );
    assert_eq!(graveyard_len(&runner, P0), 0, "nothing was discarded");
}

/// CR 603.12 + CR 202.3 + CR 608.2k: The Ancient One — "target player mills
/// cards equal to its mana value", where "its" = the card discarded this way.
/// The drawn card (MV 5) is the only hand card and is force-discarded, so the
/// reflexive mill count must equal 5. DISCRIMINATION: on revert the sub is
/// `Unimplemented` and the target player mills 0; a wrong referent ("its" → the
/// source, MV 0) also mills 0. MV 5 (not 0/1) makes both failure modes visible.
#[test]
fn the_ancient_one_mills_equal_to_discarded_card_mana_value() {
    let mut scenario = GameScenario::new();

    let source = scenario.add_creature(P0, "The Ancient One", 8, 8).id();

    // Drawn-then-discarded card (the referent for "its mana value"). It is the
    // only card the controller will hold after the draw, so the discard is
    // forced and resolves inline. Its mana value (5) is set after build.
    let drawn = scenario.add_card_to_library_top(P0, "MV5 Card");

    // Mill target: the opponent, with enough library to mill 5.
    let opp_lib: Vec<&str> = vec![
        "Opp Lib 0",
        "Opp Lib 1",
        "Opp Lib 2",
        "Opp Lib 3",
        "Opp Lib 4",
        "Opp Lib 5",
        "Opp Lib 6",
        "Opp Lib 7",
    ];
    scenario.with_library_top(P1, &opp_lib);

    let mut runner = scenario.build();
    set_mv(&mut runner, drawn, 5);
    let opp_lib_before = runner.state().players[P1.0 as usize].library.len();

    let def = parse_effect_chain(ANCIENT_BODY, AbilityKind::Spell);
    // The reflexive sub targets "target player"; supply the opponent up front.
    let ability = build_resolved_from_def(&def, source, P0);
    let ability = ResolvedAbility {
        targets: vec![TargetRef::Player(P1)],
        ..ability
    };

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("draw→discard→reflexive mill chain resolves");

    let opp_lib_after = runner.state().players[P1.0 as usize].library.len();
    let milled = opp_lib_before - opp_lib_after;
    assert_eq!(
        milled, 5,
        "target player must mill exactly the discarded card's mana value (5), got {milled}"
    );
    assert_eq!(
        graveyard_len(&runner, P0),
        1,
        "the controller discarded exactly one card this way"
    );
}

/// CR 603.12 + CR 701.9a — INTERACTIVE discard path. With ≥2 cards in hand after
/// the draw the controller must CHOOSE which card to discard, so the discard
/// pauses at `WaitingFor::DiscardChoice` instead of resolving inline. The
/// reflexive "When you discard a card this way" sub must still fire AFTER the
/// player answers the choice through the real `apply()` pipeline
/// (`GameAction::SelectCards`).
///
/// DISCRIMINATION: if the gated `ZoneChangedThisWay` sub is not stashed across
/// the `DiscardChoice` pause (or `last_zone_changed_ids` is not populated when
/// the choice resolves), the reflexive gate reads an empty ledger, the counter
/// never lands, and `p1p1_on(faerie)` stays 0 — this `assert_eq!(.., 1)` flips.
#[test]
fn talion_messenger_interactive_discard_puts_counter_on_faerie() {
    let mut scenario = GameScenario::new();

    let faerie = scenario
        .add_creature(P0, "Faerie Token", 1, 1)
        .with_subtypes(vec!["Faerie"])
        .id();

    // Pre-existing hand card + a library card the draw pulls in → hand has TWO
    // cards when "discard a card" runs, forcing an interactive DiscardChoice.
    let hand_card = scenario.add_card_to_hand(P0, "Pre-Existing Hand Card");
    scenario.with_library_top(P0, &["Drawn Card"]);

    let mut runner = scenario.build();

    let def = parse_effect_chain(TALION_BODY, AbilityKind::Spell);
    let ability = build_resolved_from_def(&def, faerie, P0);
    let ability = ResolvedAbility {
        targets: vec![TargetRef::Object(faerie)],
        ..ability
    };

    assert_eq!(
        p1p1_on(&runner, faerie),
        0,
        "Faerie starts with no counters"
    );

    // Start the chain: draw resolves inline, then the discard pauses for the
    // controller's choice (hand size 2 > 1).
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("draw resolves; discard pauses for choice");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::DiscardChoice { .. }),
        "interactive discard must pause at DiscardChoice, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        p1p1_on(&runner, faerie),
        0,
        "no counter may land before the discard choice is answered"
    );

    // Answer the choice through the REAL production pipeline. Discard the
    // pre-existing card (any eligible card discharges the reflexive identically).
    runner
        .act(GameAction::SelectCards {
            cards: vec![hand_card],
        })
        .expect("discard choice resolves and the reflexive sub fires");

    assert_eq!(
        p1p1_on(&runner, faerie),
        1,
        "reflexive 'When you discard a card this way' must place a +1/+1 counter \
         on the Faerie AFTER the interactive discard choice is answered"
    );
    assert_eq!(
        graveyard_len(&runner, P0),
        1,
        "exactly one card was discarded this way"
    );
}

/// CR 603.12 + CR 202.3 + CR 608.2k — INTERACTIVE discard for The Ancient One.
/// The controller holds two cards after the draw, so the discard is a choice;
/// the controller discards the MV-5 card. The reflexive mill count must equal
/// the discarded card's mana value (5), resolved AFTER the choice is answered.
///
/// DISCRIMINATION: if the reflexive does not fire across the interactive
/// discard the target player mills 0; if "its" binds to the wrong referent the
/// count differs from 5. Discarding the MV-5 card while a MV-0 card stays in
/// hand makes "wrong card discarded" and "reflexive dropped" both visible.
#[test]
fn the_ancient_one_interactive_discard_mills_discarded_card_mana_value() {
    let mut scenario = GameScenario::new();

    let source = scenario.add_creature(P0, "The Ancient One", 8, 8).id();

    // Two hand cards distinguish the discard choice. The MV-5 card is the one
    // the controller will discard; the MV-0 card is the decoy that stays in hand.
    let keep = scenario.add_card_to_hand(P0, "Keep MV0");
    let discard_target = scenario.add_card_to_hand(P0, "Discard MV5");
    // Library card the draw pulls in (gives the third hand card so the discard
    // is still interactive after the draw).
    scenario.with_library_top(P0, &["Drawn Card"]);

    let opp_lib: Vec<&str> = vec![
        "Opp Lib 0",
        "Opp Lib 1",
        "Opp Lib 2",
        "Opp Lib 3",
        "Opp Lib 4",
        "Opp Lib 5",
        "Opp Lib 6",
        "Opp Lib 7",
    ];
    scenario.with_library_top(P1, &opp_lib);

    let mut runner = scenario.build();
    set_mv(&mut runner, discard_target, 5);
    set_mv(&mut runner, keep, 0);
    let opp_lib_before = runner.state().players[P1.0 as usize].library.len();

    let def = parse_effect_chain(ANCIENT_BODY, AbilityKind::Spell);
    let ability = build_resolved_from_def(&def, source, P0);
    let ability = ResolvedAbility {
        targets: vec![TargetRef::Player(P1)],
        ..ability
    };

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("draw resolves; discard pauses for choice");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::DiscardChoice { .. }),
        "interactive discard must pause at DiscardChoice, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].library.len(),
        opp_lib_before,
        "no mill may happen before the discard choice is answered"
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![discard_target],
        })
        .expect("discard choice resolves and the reflexive mill fires");

    let opp_lib_after = runner.state().players[P1.0 as usize].library.len();
    let milled = opp_lib_before - opp_lib_after;
    assert_eq!(
        milled, 5,
        "target player must mill exactly the discarded card's mana value (5) \
         after the interactive discard choice, got {milled}"
    );
    assert_eq!(
        graveyard_len(&runner, P0),
        1,
        "the controller discarded exactly one card this way"
    );
}
