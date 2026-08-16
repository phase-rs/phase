//! Issue #6437 — Fight Rigging: "Exiles the targeted creature instead of
//! putting a counter on it."
//!
//! Fight Rigging: "At the beginning of combat on your turn, put a +1/+1
//! counter on target creature you control. Then if you control a creature
//! with power 7 or greater, you may play the exiled card without paying its
//! mana cost." Collector's Cage carries the identical shape on an activated
//! ability instead of a trigger. Both chain a TARGETED clause (`PutCounter`)
//! before a linked-exile clause (`CastFromZone { ExiledBySource }`) in the
//! SAME resolution.
//!
//! Root cause: the default target-propagation arm in `resolve_chain_body`
//! (`game/effects/mod.rs`, guarded by a `has_independent_target_slot` check)
//! copies a parent's already-chosen object targets into a following
//! sequential sibling whenever the sibling's own effect carries no
//! recognized "independent" target slot. `TargetFilter::ExiledBySource` is a
//! context-ref (`is_context_ref()`), so `extract_target_filter_from_effect`
//! reports no slot for it, and pre-fix that read as "not independent" —
//! copying the counter's chosen creature into the `CastFromZone` sub's empty
//! `targets`. `cast_from_zone::resolve` then found a non-empty `target_ids`
//! and never reached its own `ExiledBySource` → `exile_links` fallback,
//! instead processing the TARGETED CREATURE as the card to license: since the
//! creature sits on the battlefield (not Hand/Graveyard/Exile),
//! `grant_lingering_permissions` routed it through the exile-delivery batch,
//! moving the targeted creature into exile instead of giving it a counter.
//! `should_propagate_parent_targets` (the sibling authority used by the
//! declined-optional-branch dispatcher and others) carries the matching fix,
//! refined to keep composed filters that ALSO reference `ParentTarget`
//! (Jodah's "put the rest ... except the hit" cleanup) propagating as before.
//!
//! Oracle text is quoted verbatim from Scryfall (`client/public/card-data.json`).

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{CastingPermission, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{ExileLink, ExileLinkKind, WaitingFor};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const FIGHT_RIGGING: &str = "Hideaway 5 (When this enchantment enters, look at the top five cards of your library, exile one face down, then put the rest on the bottom in a random order.)\nAt the beginning of combat on your turn, put a +1/+1 counter on target creature you control. Then if you control a creature with power 7 or greater, you may play the exiled card without paying its mana cost.";

/// Drive the begin-combat trigger to completion: answer the counter's target
/// selection (when more than one legal creature forces an explicit prompt —
/// a lone legal creature auto-binds with no `TriggerTargetSelection` pause),
/// then (if the power-7 condition held) accept the optional free play. Loops
/// `advance_until_stack_empty` (which drains ordinary priority,
/// `OrderTriggers`, and library-bottom `EffectZoneChoice` prompts) against a
/// manual `OptionalEffectChoice` answer, since the driver has no generic
/// "accept every optional offer" policy.
fn resolve_begin_combat_trigger(
    runner: &mut GameRunner,
    counter_target: engine::types::identifiers::ObjectId,
) {
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(counter_target)],
            })
            .expect("select the +1/+1 counter's target creature");
    }

    for _ in 0..10 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ) {
            runner
                .act(GameAction::DecideOptionalEffect { accept: true })
                .expect("accept the optional free play of the hidden card");
            continue;
        }
        if runner.state().stack.is_empty() {
            break;
        }
        runner.advance_until_stack_empty();
    }
}

fn p1p1_counters(runner: &GameRunner, id: engine::types::identifiers::ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .expect("object still present")
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// DISCRIMINATOR (condition met): a controlled 7-power creature satisfies
/// "you control a creature with power 7 or greater", so the granting
/// sub-ability offers the hidden card. Pre-fix this offer landed on the
/// TARGETED creature (Squire) instead of the linked hidden card, and Squire
/// was moved to exile rather than receiving its counter.
#[test]
fn fight_rigging_counters_the_target_and_offers_only_the_hidden_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let fight_rigging = scenario
        .add_enchantment_from_oracle(P0, "Fight Rigging", FIGHT_RIGGING)
        .id();
    // Satisfies "you control a creature with power 7 or greater" without
    // being eligible as the counter's own target choice ambiguity: Squire is
    // deliberately the ONLY creature selected below, proving the resolver
    // doesn't just get lucky by counting the same object twice.
    let big_beater = scenario.add_creature(P0, "Big Beater", 7, 7).id();
    let squire = scenario.add_creature(P0, "Squire", 1, 1).id();
    // Models an earlier turn's already-resolved Hideaway ETB: a card durably
    // linked to Fight Rigging in exile (CR 406.6 + CR 607.2a), independent of
    // this resolution's chosen targets.
    let hidden = scenario.add_creature_to_exile(P0, "Hidden Card", 0, 0).id();

    let mut runner = scenario.build();
    runner.state_mut().exile_links.push(ExileLink {
        exiled_id: hidden,
        source_id: fight_rigging,
        kind: ExileLinkKind::HideawayLookable,
    });

    runner.pass_both_players();
    assert_eq!(runner.state().phase, Phase::BeginCombat);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "reach-guard: begin-combat trigger must prompt for the counter's target, got {:?}",
        runner.state().waiting_for
    );

    resolve_begin_combat_trigger(&mut runner, squire);

    assert_eq!(
        p1p1_counters(&runner, squire),
        1,
        "the TARGETED creature must receive the +1/+1 counter"
    );
    assert_eq!(
        runner.state().objects.get(&squire).map(|o| o.zone),
        Some(Zone::Battlefield),
        "the targeted creature must remain on the battlefield — it must never \
         be routed through the exiled-card's free-cast/exile-delivery path"
    );
    assert!(
        runner.state().objects[&squire]
            .casting_permissions
            .is_empty(),
        "the targeted creature must not receive a casting permission meant for \
         the hidden card, got {:?}",
        runner.state().objects[&squire].casting_permissions
    );

    let hidden_has_permission = runner.state().objects[&hidden]
        .casting_permissions
        .iter()
        .any(|p| matches!(p, CastingPermission::ExileWithAltCost { granted_to: Some(p), .. } if *p == P0));
    assert!(
        hidden_has_permission,
        "the HIDDEN card (linked via exile_links) must receive the free-cast \
         permission, got {:?}",
        runner.state().objects[&hidden].casting_permissions
    );
    assert_eq!(
        runner.state().objects.get(&hidden).map(|o| o.zone),
        Some(Zone::Exile),
        "the hidden card stays in exile — granting the permission is not a zone move"
    );
    assert_eq!(
        runner.state().objects.get(&big_beater).map(|o| o.zone),
        Some(Zone::Battlefield),
        "the power-7 creature that satisfied the condition is not itself a \
         target or subject of either clause and must be untouched"
    );
}

/// CONTROL (condition NOT met): with no power-7 creature, only the counter
/// clause resolves — "nothing else happens" below the threshold. Guards
/// against a fix that accidentally engages the exiled-card path unconditionally.
#[test]
fn fight_rigging_below_power_threshold_only_places_the_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let fight_rigging = scenario
        .add_enchantment_from_oracle(P0, "Fight Rigging", FIGHT_RIGGING)
        .id();
    let squire = scenario.add_creature(P0, "Squire", 1, 1).id();
    let hidden = scenario.add_creature_to_exile(P0, "Hidden Card", 0, 0).id();

    let mut runner = scenario.build();
    runner.state_mut().exile_links.push(ExileLink {
        exiled_id: hidden,
        source_id: fight_rigging,
        kind: ExileLinkKind::HideawayLookable,
    });

    runner.pass_both_players();
    // Squire is the only creature P0 controls, so "target creature you
    // control" auto-binds to it with no `TriggerTargetSelection` pause
    // (unlike the discriminator test above, which has two legal creatures).
    resolve_begin_combat_trigger(&mut runner, squire);

    assert_eq!(
        p1p1_counters(&runner, squire),
        1,
        "the counter must still be placed with no power-7 creature in play"
    );
    assert_eq!(
        runner.state().objects.get(&squire).map(|o| o.zone),
        Some(Zone::Battlefield)
    );
    assert!(
        runner.state().objects[&hidden]
            .casting_permissions
            .is_empty(),
        "below the power threshold the hidden card must receive no permission, got {:?}",
        runner.state().objects[&hidden].casting_permissions
    );
    assert_eq!(
        runner.state().objects.get(&hidden).map(|o| o.zone),
        Some(Zone::Exile)
    );
}

/// Issue #6437 (second root cause) — `capture_linked_exile_snapshot`
/// (`game/zones.rs`) must be kind-agnostic on the REAL leaves-the-battlefield
/// path, not only on the still-on-battlefield path the two tests above
/// exercise. Watcher for Tomorrow: "Hideaway 4 (...) This creature enters
/// tapped. When this creature leaves the battlefield, put the exiled card
/// into its owner's hand." — a pure LTB + `ExiledBySource` shape
/// (`Effect::ChangeZoneAll`, per the parser's
/// `hideaway_ltb_put_exiled_card_binds_exiled_by_source` test), unrelated to
/// the PutCounter target-propagation fix above and untouched by it. Casting a
/// real removal spell destroys Watcher through the actual casting/zone
/// pipeline (`zones::move_to_zone`), which is exactly the leaves-the-
/// battlefield departure `capture_linked_exile_snapshot` snapshots for the
/// trigger's later `ExiledBySource` lookup (`filter.rs`'s `trigger_source.
/// is_some()` branch). Pre-fix, that snapshot kept only `TrackedBySource`
/// links, silently dropping Hideaway's own `HideawayLookable` link and
/// leaving the hidden card stranded in exile instead of reaching the hand.
const WATCHER_FOR_TOMORROW: &str = "Hideaway 4 (When this creature enters, look at the top four cards of your library, exile one face down, then put the rest on the bottom in a random order.)\nThis creature enters tapped.\nWhen this creature leaves the battlefield, put the exiled card into its owner's hand.";

#[test]
fn watcher_for_tomorrow_leaving_the_battlefield_finds_the_hideaway_linked_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let watcher = scenario
        .add_creature_from_oracle(P0, "Watcher for Tomorrow", 2, 1, WATCHER_FOR_TOMORROW)
        .id();
    // Models an earlier turn's already-resolved Hideaway ETB: a card durably
    // linked to Watcher in exile (CR 406.6 + CR 607.2a) via the SAME
    // `HideawayLookable` kind Fight Rigging uses.
    let hidden = scenario.add_creature_to_exile(P0, "Hidden Card", 0, 0).id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Destroy Spell", true, "Destroy target creature.")
        .id();

    let mut runner = scenario.build();
    runner.state_mut().exile_links.push(ExileLink {
        exiled_id: hidden,
        source_id: watcher,
        kind: ExileLinkKind::HideawayLookable,
    });

    // Destroy Watcher through the REAL casting/resolution/zone pipeline —
    // the battlefield departure this drives is exactly the
    // `zones::move_to_zone` leaves-the-battlefield path under test, and the
    // resulting LTB trigger is Watcher's own PARSED printed ability, not a
    // hand-built stand-in.
    let outcome = runner.cast(removal).target_objects(&[watcher]).resolve();

    outcome.assert_zone(&[watcher], Zone::Graveyard);
    assert_eq!(
        outcome.state().objects.get(&hidden).map(|o| o.zone),
        Some(Zone::Hand),
        "the Hideaway-linked hidden card must reach its owner's hand via the \
         leaves-the-battlefield ExiledBySource lookup, got {:?}",
        outcome.state().objects.get(&hidden).map(|o| o.zone)
    );
}
