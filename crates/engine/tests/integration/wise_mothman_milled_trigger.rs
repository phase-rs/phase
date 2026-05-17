//! Regression: GitHub issue #406 — The Wise Mothman ("Whenever one or more
//! nonland cards are milled, …") and the broader milled-trigger class.
//!
//! Bug: the Oracle parser never emitted `TriggerMode::Milled` for any
//! "…cards are milled" / "…mills a card" condition. Every milled-trigger card
//! (The Wise Mothman, Glowing One, Infesting Radroach, Mirelurk Queen,
//! Screeching Scorchbeast, Zellix Sanity Flayer) parsed its mill trigger to
//! `TriggerMode::Unknown`, so the trigger never fired even though the runtime
//! matcher (`game/trigger_matchers.rs::match_milled`) was already correct.
//!
//! Fix: `parser/oracle_trigger.rs::try_parse_event` now recognizes both the
//! passive ("are milled") and active ("mills <object>") mill predicates and
//! emits `TriggerMode::Milled` (CR 701.17a). The "one or more …" batched
//! semantics are stamped by the existing caller plumbing (`def.batched`).
//!
//! These tests drive a *real* mill through `apply` — a Tome Scour spell
//! ("Target player mills five cards") resolves and produces genuine
//! `ZoneChanged { from: Library, to: Graveyard }` events — and assert the
//! milled trigger fires as a consequence. No synthetic `GameEvent` is injected.

use std::path::Path;
use std::sync::OnceLock;

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{ActionResult, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::PlayerId;

fn load_db() -> Option<&'static CardDatabase> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/public/card-data.json");
    if !path.exists() {
        return None;
    }
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    Some(DB.get_or_init(|| CardDatabase::from_export(&path).expect("export should load")))
}

/// Give P0 the mana to cast Tome Scour ({U}).
fn add_blue_mana(runner: &mut engine::game::scenario::GameRunner) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    pool.add(ManaUnit::new(ManaType::Blue, dummy, false, vec![]));
}

/// Cast P0's Tome Scour ("Target player mills five cards") aimed at
/// `mill_target`'s library and return the `ActionResult` after the target is
/// chosen — the spell is on the stack, not yet resolved. The mill events are
/// produced when the caller resolves the stack.
fn cast_tome_scour(
    runner: &mut engine::game::scenario::GameRunner,
    tome_scour: ObjectId,
    mill_target: PlayerId,
) -> ActionResult {
    let card_id = runner.state().objects[&tome_scour].card_id;
    let mut result = runner
        .act(GameAction::CastSpell {
            object_id: tome_scour,
            card_id,
            targets: vec![],
        })
        .expect("Tome Scour cast should be accepted");

    // Tome Scour targets a player — choose `mill_target` explicitly so the
    // mill lands on the intended library.
    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        result = runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(mill_target)),
            })
            .expect("Tome Scour should accept the chosen player target");
    }
    result
}

/// Issue #406 — the issue card. The Wise Mothman's passive batched milled
/// trigger ("Whenever one or more nonland cards are milled, put a +1/+1
/// counter on each of up to X target creatures…") must FIRE when nonland cards
/// are milled. Because the trigger has up-to-X creature targets, a fired
/// trigger surfaces an interactive `TriggerTargetSelection`; pre-fix the
/// `Unknown` mode meant the trigger never fired and no prompt ever appeared.
#[test]
fn wise_mothman_passive_milled_trigger_fires() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The Wise Mothman on P0's battlefield — the milled-trigger payoff card.
    scenario.add_real_card(P0, "The Wise Mothman", Zone::Battlefield, db);

    // Tome Scour in P0's hand — the real mill source ({U}: mill five cards).
    let tome_scour = scenario.add_real_card(P0, "Tome Scour", Zone::Hand, db);

    // P0's library top: five nonland cards (Lightning Bolt is an instant), so
    // the Mill 5 mills exactly five nonland cards. Padding keeps the library
    // non-empty so the mill is not truncated.
    for _ in 0..9 {
        scenario.add_real_card(P0, "Lightning Bolt", Zone::Library, db);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_blue_mana(&mut runner);

    // Mill P0's own library — the passive "are milled" trigger fires
    // regardless of whose cards are milled.
    let result = cast_tome_scour(&mut runner, tome_scour, P0);

    // Drive the stack until the milled trigger surfaces its target selection.
    // A `TriggerTargetSelection` here is direct proof the milled trigger fired:
    // pre-fix (mode == Unknown) the trigger never entered `process_triggers`.
    let mut result = result;
    let mut guard = 0;
    while !matches!(
        result.waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ) {
        guard += 1;
        assert!(
            guard < 64,
            "The Wise Mothman's milled trigger never fired — expected a \
             TriggerTargetSelection prompt after milling five nonland cards; \
             last waiting_for = {:?}",
            result.waiting_for
        );
        result = match runner.act(GameAction::PassPriority) {
            Ok(r) => r,
            Err(_) => panic!(
                "stack stalled before the milled trigger fired; \
                 last waiting_for = {:?}",
                result.waiting_for
            ),
        };
    }

    // The five nonland cards really moved Library -> Graveyard.
    assert_eq!(
        runner.state().players[0].graveyard.len(),
        6, // 5 milled + Tome Scour itself
        "five cards should have been milled into P0's graveyard (plus Tome Scour)"
    );

    // Resolve the milled trigger by choosing zero of the up-to-X targets.
    runner
        .act(GameAction::SelectTargets { targets: vec![] })
        .expect("choosing zero up-to-X targets should be legal");
    runner.advance_until_stack_empty();
}

/// Active-voice milled trigger: Glowing One ("Whenever a player mills a nonland
/// card, you gain 1 life."). This per-card (non-batched) trigger fires once per
/// milled nonland card, with an observable life-gain effect — so we can assert
/// the exact firing count end-to-end.
#[test]
fn glowing_one_active_milled_trigger_gains_life_per_card() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);

    // Glowing One on P0's battlefield — the active-voice milled-trigger card.
    scenario.add_real_card(P0, "Glowing One", Zone::Battlefield, db);

    // Tome Scour in P0's hand; it mills the *opponent's* library so the
    // active-voice "a player mills" subject is satisfied.
    let tome_scour = scenario.add_real_card(P0, "Tome Scour", Zone::Hand, db);

    // P1's library top: five nonland cards — each milled nonland card fires
    // Glowing One's trigger once (CR 603.2c — per-event, not batched).
    for _ in 0..9 {
        scenario.add_real_card(P1, "Lightning Bolt", Zone::Library, db);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_blue_mana(&mut runner);

    let life_before = runner.life(P0);

    // Mill P1's library — the active-voice "a player mills" subject fires for
    // any milling player.
    cast_tome_scour(&mut runner, tome_scour, P1);
    runner.advance_until_stack_empty();

    // Five nonland cards milled => Glowing One's trigger fired five times =>
    // P0 gained five life. Pre-fix (mode == Unknown) the trigger never fired
    // and life would be unchanged.
    assert_eq!(
        runner.life(P0),
        life_before + 5,
        "Glowing One's active-voice milled trigger must fire once per milled \
         nonland card (5 cards => +5 life)"
    );

    // The five cards genuinely left P1's library for their graveyard.
    assert_eq!(
        runner.state().players[1].graveyard.len(),
        5,
        "five cards should have been milled into P1's graveyard"
    );
}

/// Drain priority/state-based passes until either an `OptionalEffectChoice`
/// prompt appears (a fired optional trigger) or the stack settles. Returns
/// `true` if an `OptionalEffectChoice` was surfaced.
fn run_until_optional_choice_or_settled(runner: &mut engine::game::scenario::GameRunner) -> bool {
    for _ in 0..64 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ) {
            return true;
        }
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return false;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            return false;
        }
    }
    false
}

/// Opponent-scoped active-voice milled trigger: Infesting Radroach ("Whenever
/// an opponent mills a nonland card, if this creature is in your graveyard, you
/// may return it to your hand."). Its parsed `valid_card` carries
/// `controller: Opponent`, and the trigger source lives in the *graveyard*.
///
/// This pins the subtlest part of the #406 fix — the `controller: Opponent`
/// match path for a graveyard-resident trigger source. A milled card was never
/// on the battlefield, so its `controller` equals its `owner`; the trigger
/// fires only when the milling player is an opponent of the trigger's
/// controller.
///
/// POSITIVE: P0's Infesting Radroach (in P0's graveyard) sees P1 — an opponent
/// — mill a nonland card, so the trigger fires (surfacing its optional
/// return-to-hand choice).
#[test]
fn infesting_radroach_opponent_milled_trigger_fires_on_opponent_mill() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Infesting Radroach in P0's graveyard — the opponent-scoped milled trigger
    // is active from the graveyard (`trigger_zones: [Graveyard]`).
    scenario.add_real_card(P0, "Infesting Radroach", Zone::Graveyard, db);

    // Tome Scour in P0's hand; aimed at P1's library so the milling player is
    // an opponent of Infesting Radroach's controller (P0).
    let tome_scour = scenario.add_real_card(P0, "Tome Scour", Zone::Hand, db);

    // P1's library top: nonland cards to mill.
    for _ in 0..9 {
        scenario.add_real_card(P1, "Lightning Bolt", Zone::Library, db);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_blue_mana(&mut runner);

    cast_tome_scour(&mut runner, tome_scour, P1);

    // The opponent-scoped trigger must fire — surfacing the optional
    // return-to-hand choice. Pre-fix (mode == Unknown) no prompt ever appears.
    assert!(
        run_until_optional_choice_or_settled(&mut runner),
        "Infesting Radroach's opponent-scoped milled trigger must fire when an \
         opponent mills a nonland card; expected an OptionalEffectChoice prompt, \
         got {:?}",
        runner.state().waiting_for
    );

    // Decline the optional return — Infesting Radroach stays in P0's graveyard.
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining the optional return-to-hand should be legal");
    runner.advance_until_stack_empty();
}

/// Opponent-scoped active-voice milled trigger — NEGATIVE case. Infesting
/// Radroach's `controller: Opponent` filter must NOT match when the
/// trigger's controller (P0) mills their OWN library: a card P0 milled has
/// `controller == owner == P0`, which is not an opponent of P0, so the trigger
/// must not fire. This locks the opponent-scoping against false positives.
#[test]
fn infesting_radroach_opponent_milled_trigger_silent_on_own_mill() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_real_card(P0, "Infesting Radroach", Zone::Graveyard, db);
    let tome_scour = scenario.add_real_card(P0, "Tome Scour", Zone::Hand, db);

    // P0's OWN library — milling it must NOT fire the opponent-scoped trigger.
    for _ in 0..9 {
        scenario.add_real_card(P0, "Lightning Bolt", Zone::Library, db);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_blue_mana(&mut runner);

    // Mill P0's own library through a real Tome Scour cast.
    cast_tome_scour(&mut runner, tome_scour, P0);

    // No `OptionalEffectChoice` may appear — the opponent-scoped trigger must
    // stay silent when P0 mills their own cards.
    assert!(
        !run_until_optional_choice_or_settled(&mut runner),
        "Infesting Radroach's opponent-scoped milled trigger must NOT fire when \
         the trigger's controller mills their OWN library; got an unexpected \
         OptionalEffectChoice prompt"
    );

    // The five cards really were milled (so the negative result is not just an
    // empty mill) and Infesting Radroach is untouched in P0's graveyard.
    assert_eq!(
        runner.state().players[0].graveyard.len(),
        7, // Infesting Radroach + 5 milled + Tome Scour
        "five cards should have been milled into P0's own graveyard"
    );
}
