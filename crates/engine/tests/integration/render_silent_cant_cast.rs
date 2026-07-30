//! Render Silent (DGM) — "Counter target spell. Its controller can't cast
//! spells this turn."
//!
//! CR 701.6a: the countered spell is put into its owner's graveyard. CR 109.4 +
//! CR 608.2c + CR 608.2h: the chained "its controller can't cast spells this
//! turn" restriction binds to the CONTROLLER of the countered spell (the object
//! target of the parent Counter), captured from last-known information as the
//! restriction is created — NOT the caster of Render Silent.
//!
//! The first fixture has the caster (P0) counter a spell OWNED and controlled by
//! the opponent (P1) — owner == controller for the countered spell. The second
//! fixture (`render_silent_binds_to_controller_not_owner_when_they_differ`)
//! covers the owner != controller boundary: a spell OWNED by P0 (the caster) but
//! CONTROLLED by P1, so the restriction must bind to the controller (P1) and not
//! collapse to the owner or the caster (both P0).

use engine::game::casting::can_cast_object_now;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    GameRestriction, ProhibitedActivity, RestrictionExpiry, RestrictionPlayerScope,
};
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const RENDER_SILENT: &str = "Counter target spell. Its controller can't cast spells this turn.";

/// Put an opponent spell on the stack whose owner/controller is `controller`,
/// mirroring the helper in `counter_spell_zone_redirect.rs`.
fn put_spell_on_stack(runner: &mut GameRunner, controller: PlayerId) -> ObjectId {
    let spell = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(701),
        controller,
        "Shock".to_string(),
        Zone::Stack,
    );
    if let Some(obj) = runner.state_mut().objects.get_mut(&spell) {
        obj.card_types.core_types = vec![CoreType::Instant];
    }
    runner.state_mut().stack.push_back(StackEntry {
        id: spell,
        source_id: spell,
        controller,
        kind: StackEntryKind::Spell {
            card_id: CardId(701),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    spell
}

/// CR 109.4 fail-closed enforcement: if an "its controller can't cast spells"
/// restriction is ever stored with an UNRESOLVED `ParentObjectTargetController`
/// scope (no object referent — the malformed/hostile state that
/// `add_restriction`'s `parent_object_target_controller_unresolved_without_object_target`
/// proves `fill_runtime_fields` can leave), a later castability query must return
/// "unrestricted" and MUST NOT panic. This drives the public `can_cast_object_now`
/// against exactly that stored state — it panicked before the enforcement arm's
/// `debug_assert!(false)` was removed, and now returns fail-closed (restrict no
/// one). Guards `casting::restriction_scope_matches_player`.
#[test]
fn unresolved_parent_object_target_controller_restriction_is_fail_closed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mut p0_spell = scenario.add_spell_to_hand_from_oracle(P0, "P0 Spell", true, "Draw a card.");
    p0_spell.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Blue],
    });
    let p0_spell = p0_spell.id();
    scenario.add_basic_land(P0, ManaColor::Blue);

    let mut runner = scenario.build();

    // Baseline: castable before any restriction exists.
    assert!(
        can_cast_object_now(runner.state(), P0, p0_spell),
        "sanity: P0's spell must be castable before the restriction is stored"
    );

    // Store the hostile UNRESOLVED restriction directly — the exact state the
    // sibling unit test proves `fill_runtime_fields` leaves when there is no
    // object referent (scope stays `ParentObjectTargetController`, never lowered
    // to `SpecificPlayer`).
    runner
        .state_mut()
        .restrictions
        .push(GameRestriction::ProhibitActivity {
            source: ObjectId(9999),
            affected_players: RestrictionPlayerScope::ParentObjectTargetController,
            expiry: RestrictionExpiry::EndOfTurn,
            activity: ProhibitedActivity::CastSpells { spell_filter: None },
        });

    // Fail-closed: the unresolved scope restricts NO ONE and must not panic. This
    // call routes through `restriction_scope_matches_player`'s
    // `ParentObjectTargetController` arm.
    assert!(
        can_cast_object_now(runner.state(), P0, p0_spell),
        "an unresolved ParentObjectTargetController restriction must restrict no one (fail-closed)"
    );
}

/// Put a spell on the stack whose OWNER and CONTROLLER differ: `owner` owns the
/// card (so a counter sends it to `owner`'s graveyard, CR 701.6a) while
/// `controller` controls it on the stack (e.g. a spell cast from another
/// player's zone). Mirrors `put_spell_on_stack` but decouples the two so the
/// "its controller" anaphor can be distinguished from the card's owner.
fn put_spell_on_stack_owner_controller(
    runner: &mut GameRunner,
    owner: PlayerId,
    controller: PlayerId,
) -> ObjectId {
    let spell = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(702),
        owner,
        "Shock".to_string(),
        Zone::Stack,
    );
    if let Some(obj) = runner.state_mut().objects.get_mut(&spell) {
        obj.card_types.core_types = vec![CoreType::Instant];
        // CR 109.4: the spell is controlled by `controller`, distinct from its
        // owner. create_object initialized controller == owner; override it.
        obj.controller = controller;
    }
    runner.state_mut().stack.push_back(StackEntry {
        id: spell,
        source_id: spell,
        controller,
        kind: StackEntryKind::Spell {
            card_id: CardId(702),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    spell
}

/// CR 109.4 + CR 608.2c + CR 608.2h: the owner != controller boundary the sibling
/// fixture leaves untested. The countered spell is OWNED by P0 (also the Render
/// Silent caster) but CONTROLLED by P1. "Its controller can't cast spells this
/// turn" must bind to the CONTROLLER (P1) — not the owner and not the caster,
/// which are BOTH P0. Because those two failure modes collapse onto the same
/// player, a single assertion (P0 stays castable) rules out both at once:
///
/// - Binding to the owner (CR 701.6a graveyard owner = P0) → P0 restricted → fail.
/// - Binding to `self.controller` (the caster fallback = P0) → P0 restricted → fail.
/// - Binding to the countered spell's controller (P1) → only P1 restricted → pass.
///
/// This exercises the exact path called out as unverified: after the counter the
/// spell has left the stack with NO LKI snapshot (LKI is captured only on
/// battlefield/exile exit, not stack exit), so `parent_target_controller` reads
/// the graveyard object's RETAINED controller — which must still be P1 because a
/// stack→graveyard move does not run `reset_for_battlefield_exit`. If a later
/// stack-exit reset or lookup change collapsed that to the owner, this fails.
#[test]
fn render_silent_binds_to_controller_not_owner_when_they_differ() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0: Render Silent + a backup instant. P0 is the caster AND the owner of the
    // countered spell, so its castability after resolution is the discriminator.
    let mut rs = scenario.add_spell_to_hand_from_oracle(P0, "Render Silent", true, RENDER_SILENT);
    rs.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Blue, ManaCostShard::Black],
    });
    let render_silent = rs.id();

    let mut p0_backup =
        scenario.add_spell_to_hand_from_oracle(P0, "P0 Backup", true, "Draw a card.");
    p0_backup.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Blue],
    });
    let p0_backup = p0_backup.id();

    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Black);

    // P1: the controller of the countered spell — the player that must be barred.
    let mut p1_followup =
        scenario.add_spell_to_hand_from_oracle(P1, "P1 Followup", true, "Draw a card.");
    p1_followup.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Blue],
    });
    let p1_followup = p1_followup.id();
    scenario.add_basic_land(P1, ManaColor::Blue);

    let mut runner = scenario.build();

    // The spell being countered — OWNED by P0, CONTROLLED by P1.
    let bait = put_spell_on_stack_owner_controller(&mut runner, P0, P1);

    // (i) Positive reach-guard: BEFORE the restriction exists, P1 can cast.
    assert!(
        can_cast_object_now(runner.state(), P1, p1_followup),
        "reach-guard: P1's follow-up must be castable before Render Silent resolves"
    );

    // (ii) P0 casts Render Silent targeting the spell on the stack.
    runner.cast(render_silent).target_objects(&[bait]).resolve();

    // (iii) The counter sent the spell to its OWNER's (P0's) graveyard (CR 701.6a).
    assert!(
        runner.state().stack.is_empty(),
        "the bait spell must be countered (off the stack)"
    );
    assert_eq!(
        runner.state().objects[&bait].zone,
        Zone::Graveyard,
        "the countered spell goes to its owner's graveyard (CR 701.6a)"
    );
    assert!(
        runner.state().players[P0.0 as usize]
            .graveyard
            .contains(&bait),
        "the countered spell must be in P0's (the owner's) graveyard"
    );

    // (iv) REVERT-FAILING: P1 (the countered spell's CONTROLLER) is barred.
    assert!(
        !can_cast_object_now(runner.state(), P1, p1_followup),
        "P1 (countered spell's controller) must be barred from casting this turn"
    );

    // (v) OWNER/CASTER NOT RESTRICTED: P0 owns the countered spell AND cast Render
    // Silent. Binding that collapsed to the owner or to the caster (both P0) would
    // restrict P0 here — this asserts the restriction bound to the controller (P1).
    assert!(
        can_cast_object_now(runner.state(), P0, p0_backup),
        "P0 (owner of the countered spell AND the caster) must still be able to cast"
    );
}

/// CR 109.4 + CR 701.6a: Render Silent counters the targeted spell (into its
/// controller's graveyard) AND restricts that spell's controller from casting
/// spells for the rest of the turn. The caster is unaffected.
///
/// Reverting the parser scope arm (Step 2) makes the "its controller" clause
/// swallow to Unimplemented, so no restriction is created and assertion (iv)
/// fails. Binding the restriction to `self.controller` (the caster) instead of
/// the countered spell's controller makes (iv) OR (v) fail.
#[test]
fn render_silent_restricts_countered_spells_controller_not_the_caster() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0 (caster): Render Silent + a backup instant to prove the caster is NOT
    // restricted. Ample untapped mana so the backup's castability turns on the
    // restriction, not on mana exhaustion.
    let mut rs = scenario.add_spell_to_hand_from_oracle(P0, "Render Silent", true, RENDER_SILENT);
    rs.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Blue, ManaCostShard::Black],
    });
    let render_silent = rs.id();

    let mut p0_backup =
        scenario.add_spell_to_hand_from_oracle(P0, "P0 Backup", true, "Draw a card.");
    p0_backup.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Blue],
    });
    let p0_backup = p0_backup.id();

    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Black);

    // P1 (opponent): a follow-up instant + mana to prove the restriction blocks
    // this player specifically after the counter.
    let mut p1_followup =
        scenario.add_spell_to_hand_from_oracle(P1, "P1 Followup", true, "Draw a card.");
    p1_followup.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Blue],
    });
    let p1_followup = p1_followup.id();
    scenario.add_basic_land(P1, ManaColor::Blue);

    let mut runner = scenario.build();

    // The spell being countered — controlled and owned by P1.
    let bait = put_spell_on_stack(&mut runner, P1);

    // (i) Positive reach-guard: BEFORE the restriction exists, P1 can cast their
    // follow-up spell. Proves the negative assertion (iv) is not vacuous.
    assert!(
        can_cast_object_now(runner.state(), P1, p1_followup),
        "reach-guard: P1's follow-up must be castable before Render Silent resolves"
    );

    // (ii) P0 casts Render Silent targeting P1's spell on the stack.
    runner.cast(render_silent).target_objects(&[bait]).resolve();

    // (iii) A counter happened: the bait left the stack into P1's graveyard.
    assert!(
        runner.state().stack.is_empty(),
        "the bait spell must be countered (off the stack)"
    );
    assert_eq!(
        runner.state().objects[&bait].zone,
        Zone::Graveyard,
        "the countered spell goes to its owner's graveyard (CR 701.6a)"
    );
    assert!(
        runner.state().players[P1.0 as usize]
            .graveyard
            .contains(&bait),
        "the countered spell must be in P1's graveyard"
    );

    // (iv) REVERT-FAILING: P1 (the countered spell's controller) can no longer
    // cast spells this turn. Drives casting.rs::restriction_scope_matches_player,
    // not merely the parsed AST.
    assert!(
        !can_cast_object_now(runner.state(), P1, p1_followup),
        "P1 (countered spell's controller) must be barred from casting this turn"
    );

    // (v) MULTI-AUTHORITY: P0 (the caster of Render Silent) is NOT restricted —
    // the restriction bound to the countered spell's controller (P1), not to the
    // caster (`self.controller` fallback). Fails if the wrong authority was used.
    assert!(
        can_cast_object_now(runner.state(), P0, p0_backup),
        "P0 (the caster) must still be able to cast — the restriction targets P1, not P0"
    );
}
