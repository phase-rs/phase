//! Regression: end-to-end foretell pipeline against real card data
//! (Tergrid's Shadow). Issue #320 reported that the foretell special action
//! correctly exiled the card but no later-turn cast surfaced.
//!
//! These tests exercise the full pipeline against `card-data.json`:
//!   1. `Foretell` legal action surfaces from hand on the controller's turn.
//!   2. Submitting `Foretell` pays {2}, exiles face-down, grants the
//!      `Foretold` casting permission.
//!   3. `CastSpell` is rejected on the same turn (CR 702.143a "later turn").
//!   4. After turn advance, `CastSpell` is accepted, the spell is on the
//!      stack with `CastingVariant::Foretell`, and the foretell cost
//!      ({2}{B}{B}) is the announced mana cost — not the printed mana cost
//!      ({3}{B}{B}).
//!   5. `legal_actions` surfaces the cast-from-foretell-exile action when
//!      conditions are met (the action surface AI / UI consult).
//!   6. After resolution, "Each player sacrifices two creatures" fires for
//!      every player (covered by the symptom-2 fix in dd91a9b91, included
//!      here as the resolution check).
//!
//! The last test is the CR 116.2h pre-spend regression: a foretell special
//! action whose {2} is auto-tapped from a land under Selesnya Loft Gardens must
//! settle the choice-bearing chaos echo BEFORE it spends and exiles, exactly
//! like a Room unlock.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::triggers::process_triggers;
use engine::types::ability::CastingPermission;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, CastingVariant, ManaChoice, ManaChoicePrompt, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn add_mana_to(
    runner: &mut engine::game::scenario::GameRunner,
    player: engine::types::player::PlayerId,
    mana: &[ManaType],
) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

fn foretell_cost_22bb() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Black, ManaCostShard::Black],
        generic: 2,
    }
}

#[test]
fn tergrids_shadow_foretell_surfaces_in_legal_actions() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let shadow = scenario.add_real_card(P0, "Tergrid's Shadow", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    add_mana_to(&mut runner, P0, &[ManaType::Colorless, ManaType::Colorless]);

    // CR 702.143a: Foretell special action must surface as a legal action
    // for the controller during their own turn.
    let actions = legal_actions(runner.state());
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, GameAction::Foretell { object_id, .. } if *object_id == shadow)),
        "Foretell should be a legal action for Tergrid's Shadow on its controller's turn"
    );
}

#[test]
fn tergrids_shadow_foretell_special_action_exiles_with_permission() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let shadow = scenario.add_real_card(P0, "Tergrid's Shadow", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    add_mana_to(&mut runner, P0, &[ManaType::Colorless, ManaType::Colorless]);

    let card_id = runner.state().objects[&shadow].card_id;
    runner
        .act(GameAction::Foretell {
            object_id: shadow,
            card_id,
        })
        .expect("foretell special action should succeed");

    // CR 702.143a-b: After foretelling, the card is in exile, face-down,
    // marked foretold, and carries a `Foretold` casting permission.
    let obj = &runner.state().objects[&shadow];
    assert_eq!(obj.zone, Zone::Exile);
    assert!(runner.state().exile.contains(&shadow));
    assert!(obj.foretold);
    assert!(obj.face_down);
    assert!(matches!(
        obj.casting_permissions.as_slice(),
        [CastingPermission::Foretold { cost, turn_foretold }]
            if *cost == foretell_cost_22bb() && *turn_foretold == runner.state().turn_number
    ));

    // Mana paid: {2} consumed.
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
}

#[test]
fn tergrids_shadow_cannot_be_cast_from_foretell_exile_same_turn() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let shadow = scenario.add_real_card(P0, "Tergrid's Shadow", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    add_mana_to(&mut runner, P0, &[ManaType::Colorless, ManaType::Colorless]);
    let card_id = runner.state().objects[&shadow].card_id;
    runner
        .act(GameAction::Foretell {
            object_id: shadow,
            card_id,
        })
        .expect("foretell special action should succeed");

    // Even with full mana available, the cast must be rejected on the same
    // turn (CR 702.143a — "Cast it on a later turn").
    add_mana_to(
        &mut runner,
        P0,
        &[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Black,
            ManaType::Black,
        ],
    );

    let actions = legal_actions(runner.state());
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == shadow)),
        "Foretold card must not be castable on the same turn it was foretold"
    );
}

#[test]
fn tergrids_shadow_cast_from_foretell_exile_on_later_turn() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let shadow = scenario.add_real_card(P0, "Tergrid's Shadow", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    add_mana_to(&mut runner, P0, &[ManaType::Colorless, ManaType::Colorless]);
    let card_id = runner.state().objects[&shadow].card_id;
    runner
        .act(GameAction::Foretell {
            object_id: shadow,
            card_id,
        })
        .expect("foretell special action should succeed");

    // CR 702.143a — "after the current turn has ended". Bump turn_number to
    // simulate a later turn (the actual turn pass is exercised elsewhere;
    // the cast pathway only consults `state.turn_number > turn_foretold`).
    runner.state_mut().turn_number += 1;

    // Pay the foretell cost {2}{B}{B}.
    add_mana_to(
        &mut runner,
        P0,
        &[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Black,
            ManaType::Black,
        ],
    );

    // CR 702.143a: The cast surfaces in legal actions on a later turn.
    let actions = legal_actions(runner.state());
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == shadow)),
        "Foretold card must be castable on a later turn (got actions: {:?})",
        actions
            .iter()
            .filter(|a| matches!(
                a,
                GameAction::CastSpell { .. } | GameAction::Foretell { .. }
            ))
            .collect::<Vec<_>>()
    );

    runner
        .act(GameAction::CastSpell {
            object_id: shadow,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting from foretell exile on a later turn must succeed");

    // Stack entry uses CastingVariant::Foretell so resolution-time
    // bookkeeping (CR 702.143c "was foretold") is correct.
    let entry = runner
        .state()
        .stack
        .iter()
        .find(|e| e.id == shadow)
        .expect("Tergrid's Shadow must be on the stack after foretell-cast");
    match entry.kind {
        StackEntryKind::Spell {
            casting_variant, ..
        } => {
            assert_eq!(casting_variant, CastingVariant::Foretell);
        }
        ref other => panic!("expected Spell entry, got {other:?}"),
    }

    // CR 702.143b: The cast paid the foretell cost, not the printed mana
    // cost. After paying {2}{B}{B} the pool is empty.
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
}

#[test]
fn tergrids_shadow_foretell_cast_resolves_each_player_sacrifices() {
    // End-to-end: foretell on turn N, advance to turn N+1, cast from exile,
    // resolve, and verify the two-player sac-two-creatures effect prompts
    // both players. This is the user's repro from #320 — they paid foretell,
    // expected to cast on a later turn, and the cast never surfaced.
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let shadow = scenario.add_real_card(P0, "Tergrid's Shadow", Zone::Hand, db);
    // Each player needs two creatures so the resolve actually fires.
    for _ in 0..2 {
        scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
        scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Pay foretell special action {2}.
    add_mana_to(&mut runner, P0, &[ManaType::Colorless, ManaType::Colorless]);
    let card_id = runner.state().objects[&shadow].card_id;
    runner
        .act(GameAction::Foretell {
            object_id: shadow,
            card_id,
        })
        .expect("foretell special action should succeed");
    let foretold_turn = runner.state().turn_number;

    // Advance to a later turn.
    runner.state_mut().turn_number = foretold_turn + 1;

    // Pay foretell cost {2}{B}{B} and cast.
    add_mana_to(
        &mut runner,
        P0,
        &[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Black,
            ManaType::Black,
        ],
    );
    // Cast from foretell exile (pool-funded foretell cost {2}{B}{B}) and
    // resolve through the canonical pipeline. Both players must sacrifice two
    // creatures (covered architecturally by dd91a9b91's player_scope sweep).
    let outcome = runner.cast(shadow).resolve();

    // After resolution each player has zero creatures (they had two each).
    let count_creatures = |player: engine::types::player::PlayerId| {
        outcome
            .state()
            .battlefield
            .iter()
            .filter(|id| {
                outcome.state().objects.get(id).is_some_and(|o| {
                    o.controller == player
                        && o.card_types
                            .core_types
                            .contains(&engine::types::card_type::CoreType::Creature)
                })
            })
            .count()
    };
    assert_eq!(
        count_creatures(P0),
        0,
        "P0 should have sacrificed both creatures (Tergrid's Shadow resolution)"
    );
    assert_eq!(
        count_creatures(P1),
        0,
        "P1 should have sacrificed both creatures (Tergrid's Shadow resolution)"
    );
}

// CR 702.143a + CR 605.4a + CR 608.2d: the Selesnya chaos bonus is a
// choice-bearing delayed mana trigger, so paying the {2} foretell cost must
// settle its choice BEFORE spending and BEFORE exiling the card.
const SELESNYA_ORACLE: &str = "If an effect would create one or more tokens, it creates twice that many of those tokens instead.\nIf an effect would put one or more counters on a permanent, it puts twice that many of those counters on that permanent instead.\nWhenever chaos ensues, until end of turn, whenever you tap a land for mana, add one mana of any type that land produced.";
const CHAMBER_ORACLE: &str =
    "This land enters tapped.\nWhen this land enters, return a land you control to its owner's hand.\n{T}: Add {G}{U}.";
const DOOMSKAR_ORACLE: &str = "Destroy all creatures.\nForetell {1}{W}{W} (During your turn, you may pay {2} and exile this card from your hand face down. Cast it on a later turn for its foretell cost.)";

fn count_color(runner: &GameRunner, color: ManaType) -> usize {
    runner.state().players[P0.0 as usize]
        .mana_pool
        .count_color(color)
}

fn foretell_cost_11ww() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::White, ManaCostShard::White],
        generic: 1,
    }
}

/// Doomskar ({1}{W}{W} foretell) in hand, one untapped Simic Growth Chamber as
/// the only mana source, and Selesnya Loft Gardens' chaos bonus installed.
/// Tapping the Chamber fires the bonus, whose "of any type that land produced"
/// text is a mandatory choice, so the {2} special-action payment owes that
/// choice before it can spend.
fn runner_with_foretell_echo() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let selesnya = scenario
        .add_land_from_oracle(P0, "Selesnya Loft Gardens", SELESNYA_ORACLE)
        .id();
    let chamber = scenario
        .add_land_from_oracle(P0, "Simic Growth Chamber", CHAMBER_ORACLE)
        .id();
    let doomskar = scenario
        .add_spell_to_hand_from_oracle(P0, "Doomskar", false, DOOMSKAR_ORACLE)
        .id();

    let mut runner = scenario.build();
    assert!(
        runner.state().objects[&doomskar]
            .base_keywords
            .iter()
            .any(|keyword| matches!(keyword, Keyword::Foretell(_))),
        "premise: the parsed Doomskar carries its printed foretell keyword"
    );

    // CR 311.7 / CR 901.9b: synthesize the chaos event keyed by Selesnya's
    // object id; the real plane-identity binding installs the delayed trigger.
    let events = vec![GameEvent::ChaosEnsued { plane_id: selesnya }];
    process_triggers(runner.state_mut(), &events);
    while runner.state().delayed_triggers.is_empty() && !runner.state().stack.is_empty() {
        runner
            .act(GameAction::PassPriority)
            .expect("resolving the chaos trigger must succeed");
    }
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "chaos must install exactly one Selesnya delayed trigger"
    );
    (runner, doomskar, chamber)
}

/// CR 116.2h + CR 702.143a + CR 605.4a + CR 608.2d: the foretell special
/// action's {2} payment pauses on the mandatory Selesnya echo with the card
/// still in hand, unspent, and not yet foretold; the answer resumes the exact
/// action and only then exiles the card and stamps its permission.
#[test]
fn foretell_special_action_requires_bonus_choice_before_spending() {
    let (mut runner, doomskar, chamber) = runner_with_foretell_echo();
    let card_id = runner.state().objects[&doomskar].card_id;
    let turn = runner.state().turn_number;

    runner
        .act(GameAction::Foretell {
            object_id: doomskar,
            card_id,
        })
        .expect("the foretell special action must reach the delayed-mana choice");

    // The echo must NOT resolve silently or after a partial spend: the base
    // {G}{U} is still pooled and the card is still in hand while it is pending.
    match &runner.state().waiting_for {
        WaitingFor::ChooseManaColor {
            player,
            choice: ManaChoicePrompt::SingleColor { options },
            ..
        } => {
            assert_eq!(*player, P0, "the choice belongs to the foretelling player");
            assert_eq!(
                options.len(),
                2,
                "the choice must offer exactly the chamber's produced types; got {options:?}"
            );
            assert!(
                options.contains(&ManaType::Green) && options.contains(&ManaType::Blue),
                "the choice must offer Green and Blue; got {options:?}"
            );
        }
        other => panic!("expected the Selesnya echo choice, got {other:?}"),
    }
    let obj = &runner.state().objects[&doomskar];
    assert_eq!(
        obj.zone,
        Zone::Hand,
        "the card is not exiled until its {{2}} is paid"
    );
    assert!(
        !obj.foretold,
        "the card is not foretold before it is exiled"
    );
    assert!(
        obj.casting_permissions.is_empty(),
        "no foretell permission exists while the payment is pending"
    );
    assert_eq!(
        count_color(&runner, ManaType::Green),
        1,
        "the chamber's base {{G}} is still unspent while the choice is pending"
    );
    assert_eq!(
        count_color(&runner, ManaType::Blue),
        1,
        "the chamber's base {{U}} is still unspent while the choice is pending"
    );
    assert!(
        runner.state().objects[&chamber].tapped,
        "the Chamber is the source whose tap owes the choice"
    );
    assert!(
        runner.state().stack.is_empty(),
        "the echo resolves inline, not on the stack"
    );

    let resumed = runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("answering the bonus must resume the foretell payment");

    let obj = &runner.state().objects[&doomskar];
    assert_eq!(obj.zone, Zone::Exile);
    assert!(runner.state().exile.contains(&doomskar));
    assert!(obj.foretold, "the resumed payment marks the card foretold");
    assert!(obj.face_down, "a foretold card is exiled face down");
    assert!(
        matches!(
            obj.casting_permissions.as_slice(),
            [CastingPermission::Foretold { cost, turn_foretold }]
                if *cost == foretell_cost_11ww() && *turn_foretold == turn
        ),
        "the permission latches the card's foretell cost from the announcing turn"
    );
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        1,
        "base {{G}}{{U}} pays the {{2}}; the chosen bonus is the only unit left"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "the completed special action returns priority to its controller, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        resumed.events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                to: Zone::Exile,
                ..
            } if *object_id == doomskar
        )),
        "the exile happens exactly when the payment completes"
    );
}
