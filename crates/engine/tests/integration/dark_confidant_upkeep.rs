//! Runtime regression for issue #511 (and #512a) — anaphoric "its" in a
//! triggered ability binds to the object an earlier *effect instruction*
//! introduced, not to the cost-paid object.
//!
//! Dark Confidant: "At the beginning of your upkeep, reveal the top card of
//! your library and put that card into your hand. You lose life equal to its
//! mana value." The "its" refers to the *revealed* card.
//!
//! Root cause (pre-fix): `RevealTop` emits `GameEvent::CardsRevealed`, not a
//! `ZoneChanged` (CR 701.20b — revealing does not move the card). The chained
//! `ChangeZone` moves the card to Hand, a hidden zone. The instruction-order
//! referent extractor `parent_referent_context_from_events` recognized only
//! sacrifice and public-zone moves, so the revealed card was never captured
//! into `effect_context_object`. The grandchild `LoseLife`'s
//! `ObjectManaValue{ Anaphoric }` arm then fell through every slot to
//! `unwrap_or(0)` → Dark Confidant's controller lost 0 life.
//!
//! Fix: (1) `revealed_object_context_from_events` captures the single revealed
//! card's LKI snapshot at reveal time (CR 608.2h — once the card moves to a
//! hidden zone, LKI applies). (2) the `Anaphoric` runtime arm consults
//! `effect_context_object` (CR 608.2c instruction-order referent) first,
//! before the CR 608.2k trigger-condition / cost referents.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 608.2c: a spell or ability's controller follows its instructions in
//!     the order written; anaphora reads the whole text per English rules.
//!   - CR 608.2h: an effect uses last-known information when the object it
//!     refers to has moved from a public zone to a hidden zone.
//!   - CR 608.2k: an effect referring to an object previously named by the
//!     ability's cost or trigger condition still affects that object.
//!   - CR 701.20b: revealing a card doesn't cause it to leave its zone.
//!
//! Non-interference note (plan B6 double-bind trace): `RevealTop` also writes
//! `state.last_revealed_ids`, consumed by the effect chain to inject revealed
//! card IDs as *targets* for an else-branch sub-ability following a peek. Dark
//! Confidant has no else branch and its sub-`ChangeZone` carries an explicit
//! `ParentTarget`; the grandchild `LoseLife` reads a quantity, not a target.
//! The new `effect_context_object` path and the `last_revealed_ids` target
//! injection therefore do not double-bind for this card.

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// CR 608.2c — Dark Confidant's upkeep trigger reveals the top card of P0's
/// library and P0 loses life equal to *that revealed card's* mana value.
///
/// Discriminator: the library top is a 3-CMC card, distinct from Dark
/// Confidant's own CMC of 2. P0 must end at 17 (20 − 3). With Step 1+2
/// reverted the revealed card is never captured → P0 loses 0 (stays at 20).
/// With Step 3's slot order reverted to cost-first the chain is still empty at
/// an upkeep trigger → also 0. The 3-vs-2 gap proves the resolver reads the
/// revealed card, not Dark Confidant's CMC.
#[test]
fn dark_confidant_upkeep_loses_life_equal_to_revealed_card_mana_value() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    let confidant = scenario.add_real_card(P0, "Dark Confidant", Zone::Battlefield, db);
    // Cancel is {1}{U}{U} — mana value 3, distinct from Dark Confidant's 2.
    let revealed = scenario.add_real_card(P0, "Cancel", Zone::Library, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Pre-existing battlefield permanent — entered a previous turn.
    runner.state_mut().turn_number = 2;
    runner.state_mut().phase = Phase::Untap;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    // Confirm the discriminator preconditions.
    assert_eq!(
        runner
            .state()
            .objects
            .get(&confidant)
            .unwrap()
            .mana_cost
            .mana_value(),
        2,
        "precondition: Dark Confidant's own CMC is 2"
    );
    assert_eq!(
        runner
            .state()
            .objects
            .get(&revealed)
            .unwrap()
            .mana_cost
            .mana_value(),
        3,
        "precondition: the revealed library-top card has CMC 3"
    );
    assert_eq!(runner.life(P0), 20, "precondition: P0 starts at 20 life");

    // Drive Untap → Upkeep (trigger fires + resolves) → Draw → PreCombatMain.
    // The upkeep trigger lands on the stack as `auto_advance` enters Upkeep;
    // the priority drain resolves it (the ability is non-targeted).
    runner.auto_advance_to_main_phase();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P0),
        17,
        "P0 must lose life equal to the REVEALED card's mana value (3), not \
         Dark Confidant's CMC (2): 20 - 3 = 17"
    );

    // SHAPE sub-assertion: the revealed card ended up in P0's hand.
    assert_eq!(
        runner.state().objects.get(&revealed).unwrap().zone,
        Zone::Hand,
        "the revealed card must be put into P0's hand"
    );
}

/// Issue #512a — trigger-subject anaphora. Conclave Mentor (a 2/1): "When ~
/// dies, you gain life equal to its power." The "its" refers to the dying
/// creature itself (the trigger subject), resolved at runtime via the
/// `Anaphoric` arm's slot 2 (trigger-event source → LKI for a dies trigger).
/// No earlier effect instruction introduces an object, so slot 1
/// (`effect_context_object`) is empty and slot 2 carries the referent.
///
/// This test drives the real pipeline: Conclave Mentor dies to a Lightning
/// Bolt, the dies-trigger fires and resolves, P0 gains life equal to the dead
/// creature's power.
///
/// The slot-2-over-slot-3 (trigger-condition vs cost referent) priority pin —
/// i.e. an unrelated `cost_paid_object` must not hijack the anaphoric
/// pronoun — is exhaustively discriminated at the resolver level by
/// `quantity.rs::resolve_object_mana_value_anaphoric_vs_cost_paid_divergent_priority`,
/// because no exported card pairs a dies-trigger anaphoric pronoun with a
/// cost-paid object in the same continuation chain.
#[test]
fn conclave_mentor_dies_trigger_gains_life_equal_to_its_power() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mentor = scenario.add_real_card(P0, "Conclave Mentor", Zone::Battlefield, db);
    // P0 holds the bolt so it can be cast during P0's own main-phase priority.
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    scenario.with_life(P0, 20);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Fund P0's pool with {R} to pay Lightning Bolt's cost.
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool
        .add(ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]));

    let mentor_power = runner
        .state()
        .objects
        .get(&mentor)
        .unwrap()
        .power
        .expect("Conclave Mentor has a power");
    assert_eq!(mentor_power, 2, "precondition: Conclave Mentor is a 2/1");

    // P0 bolts their own Conclave Mentor — 3 damage kills the 2/1 via SBA.
    // The dies-trigger fires regardless of who dealt the lethal damage.
    let bolt_card_id = runner.state().objects[&bolt].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            object_id: bolt,
            card_id: bolt_card_id,
            targets: vec![],
        })
        .expect("cast Lightning Bolt");
    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(mentor)],
            })
            .expect("target Conclave Mentor");
    }
    runner.advance_until_stack_empty();

    // Conclave Mentor died; its dies-trigger then resolves.
    assert_eq!(
        runner.state().objects.get(&mentor).unwrap().zone,
        Zone::Graveyard,
        "Conclave Mentor must be destroyed by lethal damage"
    );
    assert_eq!(
        runner.life(P0),
        22,
        "P0 must gain life equal to the dying creature's power (2): 20 + 2 = 22"
    );
}
