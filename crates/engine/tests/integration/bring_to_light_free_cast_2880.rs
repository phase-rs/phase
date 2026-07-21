//! Issue #2880 — Bring to Light's free cast must happen DURING resolution and
//! leave NO lingering casting permission, rather than stamping an indefinite
//! `CastingPermission::ExileWithAltCost { duration: None }` the player could
//! exploit on any future turn.
//!
//! Bring to Light tutors a card into the controller's OWN exile, then "you may
//! cast that card without paying its mana cost." This is the same
//! `Effect::CastFromZone { without_paying_mana_cost: true, target: ParentTarget,
//! driver: DuringResolution }` shape as Suspend's last-counter cast (CR 608.2g):
//! the card must go on the stack AS the granting effect resolves, with no
//! standing permission afterward.
//!
//! The reported bug: accepting the prompt stamped an indefinite free-cast
//! `ExileWithAltCost` permission on the tutored card and never put it on the
//! stack — `cast_from_zone.rs`'s during-resolution guard required
//! `target == source` (a Suspend-specific clause), so Bring to Light's
//! `target != source` tutored card fell through to `grant_lingering_permissions`.
//!
//! This drives the REAL parser (`from_oracle_text`) and the REAL resolver
//! through the cast-during-resolution pipeline. The tutor mechanism here is an
//! `ExileFromTopUntil` (deterministic, no library-search prompt) instead of
//! Converge search, but the fix-relevant clause — "You may cast that card
//! without paying its mana cost" → `CastFromZone { ParentTarget,
//! DuringResolution }` over a card in the controller's own exile (target !=
//! source) — is byte-for-byte the Bring to Light shape.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{CastingPermission, Effect};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::rules::run_combat;

/// Tutor-and-cast oracle: exile from the top of the controller's OWN library
/// until a nonland card is exiled (it lands in the controller's exile, owner ==
/// controller), then offer to cast it for free DURING resolution. Identical to
/// The Infamous Cruelclaw minus its discard alt cost — dropping the alt cost is
/// what flips the cast onto the during-resolution driver (a non-`None`
/// `alt_ability_cost` keeps the lingering-permission path).
const TUTOR_CAST_ORACLE: &str = "Whenever this creature deals combat damage to a player, \
exile cards from the top of your library until you exile a nonland card. \
You may cast that card without paying its mana cost.";

fn put_on_library_top(state: &mut GameState, obj_id: ObjectId, owner: PlayerId) {
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(state, obj_id, Zone::Library, &mut events);
    let player = state.players.iter_mut().find(|p| p.id == owner).unwrap();
    player.library.retain(|id| *id != obj_id);
    player.library.insert(0, obj_id);
}

/// True when `id` retains any `ExileWithAltCost` permission — the indefinite
/// lingering grant this fix eliminates (issue #2880).
fn has_exile_alt_cost(state: &GameState, id: ObjectId) -> bool {
    state.objects[&id]
        .casting_permissions
        .iter()
        .any(|p| matches!(p, CastingPermission::ExileWithAltCost { .. }))
}

/// Build a scenario: a 3/3 attacker with the tutor-and-cast trigger, and a
/// target-less castable sorcery ("Draw a card.") sitting on top of P0's library
/// so the `ExileFromTopUntil` deterministically exiles it into P0's own exile.
/// Returns the runner, the attacker id, and the tutored card id.
fn setup() -> (engine::game::scenario::GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let attacker = scenario
        .add_creature_from_oracle(P0, "Tutoring Beast", 3, 3, TUTOR_CAST_ORACLE)
        .id();

    // A land first so `ExileFromTopUntil` exiles at least one nonmatching card,
    // then the target-less sorcery as the nonland stop. A target-less spell
    // lands straight on the stack on accept (no intervening target prompt).
    let land = scenario.add_basic_land(P0, ManaColor::Blue);
    let tutored = scenario
        .add_spell_to_library_top(P0, "Free Cantrip", false)
        .from_oracle_text("Draw a card.")
        .with_mana_cost(ManaCost::generic(3))
        .id();

    let mut runner = scenario.build();
    // Library top → bottom: land, then the sorcery (the nonland stop).
    put_on_library_top(runner.state_mut(), tutored, P0);
    put_on_library_top(runner.state_mut(), land, P0);

    (runner, attacker, tutored)
}

/// (a) Accepting the free-cast prompt casts the card DURING resolution — it
/// reaches `Zone::Stack` (the cast is genuinely under way) — and afterward the
/// card carries NO standing `ExileWithAltCost` permission. Pre-fix: the card
/// stayed in exile with an indefinite `ExileWithAltCost { duration: None }`.
#[test]
fn free_cast_offered_during_resolution_then_no_lingering_permission() {
    let (mut runner, attacker, tutored) = setup();

    // Sanity: the trigger's cast sub-ability must parse to a during-resolution
    // `CastFromZone` (no alt cost, no duration). If this regresses to a
    // lingering-permission driver, the runtime asserts below would no longer
    // discriminate, so anchor the input shape here.
    let trigger = &runner.state().objects[&attacker].trigger_definitions[0];
    let execute = trigger
        .definition
        .execute
        .as_ref()
        .expect("trigger execute");
    let cast = execute.sub_ability.as_ref().expect("cast sub-ability");
    let Effect::CastFromZone {
        without_paying_mana_cost,
        alt_ability_cost,
        driver,
        ..
    } = cast.effect.as_ref()
    else {
        panic!("expected CastFromZone sub-ability, got {:?}", cast.effect);
    };
    assert!(without_paying_mana_cost, "free cast must be without paying");
    assert!(alt_ability_cost.is_none(), "no alt cost on this clause");
    assert!(
        driver.is_during_resolution(),
        "bare free cast with no alt cost / duration must use the DuringResolution driver"
    );

    run_combat(&mut runner, vec![attacker], vec![]);
    runner.advance_until_stack_empty();

    // The exiled nonland is in P0's OWN exile (owner == controller, target !=
    // source) — the exact Bring to Light input that the Suspend-specific
    // `target == source` guard used to reject.
    assert_eq!(
        runner.state().objects[&tutored].zone,
        Zone::Exile,
        "tutored card must be exiled into the controller's own exile"
    );
    assert_eq!(
        runner.state().objects[&tutored].owner,
        P0,
        "tutored card is owned by the controller (own exile)"
    );

    // CR 608.2g: the free cast is offered as the trigger resolves.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player, .. } if player == P0
        ),
        "expected an optional 'you may cast' prompt for P0; got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the free-cast prompt");

    // CR 608.2g: accepting casts the card DURING resolution — it is the topmost
    // object on the stack. (Target-less sorcery → straight to the stack, no
    // intervening target prompt.) This assertion flips when the fix is reverted:
    // pre-fix the card stayed in Exile and only a lingering permission was stamped.
    assert_eq!(
        runner.state().objects[&tutored].zone,
        Zone::Stack,
        "accepting the free cast must put the card on the stack; zone = {:?}, waiting_for = {:?}",
        runner.state().objects[&tutored].zone,
        runner.state().waiting_for,
    );

    // The card must carry NO standing ExileWithAltCost permission — the indefinite
    // lingering grant (issue #2880) is gone. This is the second discriminating
    // assertion: pre-fix an `ExileWithAltCost { duration: None }` persisted.
    assert!(
        !has_exile_alt_cost(runner.state(), tutored),
        "the cast card must not retain a lingering ExileWithAltCost permission"
    );
}

/// (b) Declining the free-cast prompt leaves the card in exile with NO standing
/// `ExileWithAltCost` permission — there is no indefinite free-cast grant to
/// exploit later.
#[test]
fn declining_free_cast_leaves_no_lingering_permission() {
    let (mut runner, attacker, tutored) = setup();

    run_combat(&mut runner, vec![attacker], vec![]);
    runner.advance_until_stack_empty();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player, .. } if player == P0
        ),
        "expected an optional 'you may cast' prompt for P0; got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline the free-cast prompt");

    assert_eq!(
        runner.state().objects[&tutored].zone,
        Zone::Exile,
        "declining must leave the tutored card in exile"
    );
    assert!(
        !has_exile_alt_cost(runner.state(), tutored),
        "declining must not leave a lingering ExileWithAltCost permission"
    );
}
