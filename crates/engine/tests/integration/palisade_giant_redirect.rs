//! Runtime regression for continuous damage-redirection statics that parse to a
//! `ShieldKind::Prevention` shield carrying `redirect_target: Some(SelfRef)`
//! (CR 614.9). Palisade Giant, Veteran Bodyguard, and Weathered Bodyguards all
//! route through `game::replacement::damage_done_applier`'s Branch 2
//! (`ShieldKind::Prevention`), which previously never read `redirect_target` —
//! it fully *prevented* the damage instead of *redirecting* it to the intended
//! recipient. These tests drive the real damage-resolution pipeline and would
//! fail if the Branch 2 redirect check were reverted (the damage would vanish
//! and the recipient's `damage_marked` would stay 0).
//!
//! Oracle text under test is verbatim / Scryfall-verified for the redirect line;
//! the dealt-damage trigger in `..._fires_dealt_damage_trigger_on_recipient` is a
//! second, independently real ability template combined onto the test permanent
//! to exercise the "redirected damage flows through ordinary damage/trigger
//! machinery" seam (building-block coverage, not a claim about the real card).

use engine::game::effects::deal_damage;
use engine::game::sba::check_state_based_actions;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use super::rules::run_combat;

/// Verbatim Palisade Giant redirection text (Scryfall-verified). The recipient-
/// list half ("and other permanents you control") is a separate, pre-existing
/// gap and is not exercised here.
const PALISADE_GIANT_TEXT: &str =
    "All damage that would be dealt to you and other permanents you control is dealt to this creature instead.";

/// Verbatim Weathered Bodyguards redirection text (Scryfall-verified) — combat
/// only, from unblocked creatures, gated on being untapped.
const WEATHERED_BODYGUARDS_TEXT: &str = "As long as this creature is untapped, all combat damage \
    that would be dealt to you by unblocked creatures is dealt to this creature instead.";

/// A non-combat damage source dealing `amount` to `target`, controlled by P1
/// (the opponent of the shield's controller in these fixtures).
fn damage_ability(source_id: ObjectId, target: TargetRef, amount: i32) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: amount },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
        vec![target],
        source_id,
        P1,
    )
}

/// CR 614.9: Palisade Giant redirects damage that would be dealt to its
/// controller onto itself — lethal damage marks it and it dies to SBA
/// (CR 704.5g), while the controller takes none of the damage.
///
/// Revert guard: without the Branch 2 redirect check, Palisade Giant's
/// `damage_marked` stays 0 (the damage is merely prevented) and the redirect
/// never happens.
#[test]
fn palisade_giant_redirects_lethal_damage_to_itself_and_dies() {
    let mut scenario = GameScenario::new();
    let giant = scenario
        .add_creature_from_oracle(P0, "Palisade Giant", 2, 6, PALISADE_GIANT_TEXT)
        .id();
    let source = scenario.add_creature(P1, "Damage Source", 6, 6).id();
    let mut runner = scenario.build();

    let p0_life_before = runner.life(P0);

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Player(P0), 6),
        &mut events,
    )
    .expect("damage to Palisade Giant's controller resolves");

    assert_eq!(
        runner.state().objects[&giant].damage_marked,
        6,
        "the 6 damage that would hit the controller must be redirected onto Palisade Giant"
    );
    // Positive reach-guard: the redirect did NOT also hit the controller.
    assert_eq!(
        runner.life(P0),
        p0_life_before,
        "the controller takes no damage — it was redirected, not dealt twice"
    );

    // CR 704.5g: 6 damage marked on a creature with toughness 6 is lethal.
    let mut sba_events = Vec::new();
    check_state_based_actions(runner.state_mut(), &mut sba_events);
    assert_ne!(
        runner.state().objects[&giant].zone,
        Zone::Battlefield,
        "Palisade Giant dies to state-based actions after taking lethal redirected damage"
    );
}

/// CR 614.9 + CR 615.1a: The shield is continuous (never consumed) — it must
/// redirect across multiple separate damage events in the same turn. This
/// proves `consume_after_redirect: false` is actually taking effect; if the
/// shield were wrongly consumed after the first redirect, the second event
/// would bypass it and hit the controller.
#[test]
fn palisade_giant_redirect_survives_multiple_damage_events() {
    let mut scenario = GameScenario::new();
    let giant = scenario
        .add_creature_from_oracle(P0, "Palisade Giant", 2, 6, PALISADE_GIANT_TEXT)
        .id();
    let source = scenario.add_creature(P1, "Damage Source", 2, 2).id();
    let mut runner = scenario.build();

    let p0_life_before = runner.life(P0);

    for _ in 0..2 {
        let mut events = Vec::new();
        deal_damage::resolve(
            runner.state_mut(),
            &damage_ability(source, TargetRef::Player(P0), 2),
            &mut events,
        )
        .expect("damage to the controller resolves");
    }

    assert_eq!(
        runner.state().objects[&giant].damage_marked,
        4,
        "both 2-damage events must redirect onto Palisade Giant (continuous shield, not consumed)"
    );
    assert_eq!(
        runner.life(P0),
        p0_life_before,
        "the controller takes none of either redirected damage instance"
    );
}

/// CR 614.9: Redirected damage flows through the ordinary damage machinery, so a
/// "whenever this creature is dealt damage" trigger on the recipient fires. Uses
/// `TriggerMode::DamageReceived` — a fully wired matcher — to prove the redirect
/// path is not a dead-end that swallows the damage event.
#[test]
fn palisade_giant_redirect_fires_dealt_damage_trigger_on_recipient() {
    let oracle =
        format!("{PALISADE_GIANT_TEXT}\nWhenever this creature is dealt damage, you gain 1 life.");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let giant = scenario
        .add_creature_from_oracle(P0, "Palisade Giant", 2, 6, &oracle)
        .id();
    let source = scenario.add_creature(P1, "Damage Source", 3, 3).id();
    let mut runner = scenario.build();

    // Reach guard: the dealt-damage trigger is actually installed and wired.
    let trigger = runner.state().objects[&giant]
        .trigger_definitions
        .iter_unchecked()
        .find(|t| t.definition.mode == TriggerMode::DamageReceived)
        .expect("Palisade Giant must carry a DamageReceived trigger for this fixture");
    assert_eq!(trigger.definition.valid_card, Some(TargetFilter::SelfRef));

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Player(P0), 3),
        &mut events,
    )
    .expect("damage to the controller resolves");
    process_triggers(runner.state_mut(), &events);

    let queued = runner
        .stack_names()
        .iter()
        .filter(|name| name.contains("Palisade Giant"))
        .count();
    assert_eq!(
        queued, 1,
        "the redirected damage must fire the recipient's DamageReceived trigger exactly once"
    );
}

/// CR 614.9: "If one of those permanents ... is no longer a battle, creature, or
/// planeswalker when the damage would be redirected, the effect does nothing."
/// The host stays ON the battlefield (so it is still a candidate and Branch 2
/// runs) but loses its creature core type, failing `redirect_recipient_is_legal`
/// — the redirect does nothing and the damage proceeds to the original recipient
/// (the controller), neither prevented nor vanished.
#[test]
fn palisade_giant_illegal_recipient_makes_redirect_do_nothing() {
    let mut scenario = GameScenario::new();
    let giant = scenario
        .add_creature_from_oracle(P0, "Palisade Giant", 2, 6, PALISADE_GIANT_TEXT)
        .id();
    let source = scenario.add_creature(P1, "Damage Source", 4, 4).id();
    let mut runner = scenario.build();

    // Strip Palisade Giant's core types while it remains on the battlefield —
    // an illegal redirect recipient per CR 614.9. Mirrors the direct raw-state
    // mutation used by veteran_bodyguard_tap_redirect.rs for `tapped`.
    runner
        .state_mut()
        .objects
        .get_mut(&giant)
        .unwrap()
        .card_types
        .core_types = vec![];

    let p0_life_before = runner.life(P0);

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Player(P0), 4),
        &mut events,
    )
    .expect("damage to the controller resolves");

    assert_eq!(
        runner.life(P0),
        p0_life_before - 4,
        "with an illegal redirect recipient the damage does nothing special and hits the controller"
    );
    assert_eq!(
        runner.state().objects[&giant].damage_marked,
        0,
        "no damage is redirected onto a non-creature recipient"
    );
}

/// CR 614.9 candidate gate (pre-existing, unmodified behavior): once the shield
/// host leaves the battlefield entirely, its replacement definition is filtered
/// out of candidacy upstream of `damage_done_applier` — it neither prevents nor
/// redirects. This guards that this fix does not disturb that boundary.
#[test]
fn destroyed_palisade_giant_neither_prevents_nor_redirects() {
    let mut scenario = GameScenario::new();
    let giant = scenario
        .add_creature_from_oracle(P0, "Palisade Giant", 2, 6, PALISADE_GIANT_TEXT)
        .id();
    let source = scenario.add_creature(P1, "Damage Source", 4, 4).id();
    let mut runner = scenario.build();

    // Move Palisade Giant off the battlefield before any damage is dealt.
    runner.state_mut().objects.get_mut(&giant).unwrap().zone = Zone::Graveyard;

    let p0_life_before = runner.life(P0);

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Player(P0), 4),
        &mut events,
    )
    .expect("damage to the controller resolves");

    assert_eq!(
        runner.life(P0),
        p0_life_before - 4,
        "an off-battlefield shield host must not prevent or redirect — the controller takes the damage"
    );
    assert_eq!(
        runner.state().objects[&giant].damage_marked,
        0,
        "no damage is redirected onto a graveyard-resident former shield host"
    );
}

/// CR 510.2 + CR 614.9: Simultaneous multi-attacker combat damage — two unblocked
/// attackers deal combat damage to the same protected controller in one combat
/// damage step. Every attacker's damage must redirect onto the Bodyguard (summed
/// `damage_marked`), and the controller takes none of it. Uses Weathered
/// Bodyguards deliberately: its "combat damage" qualifier makes it the cleanest
/// combat-only fit for a combat-damage batch (Veteran Bodyguard redirects ALL
/// unblocked-creature damage, combat or not).
///
/// Revert guard: if the redirect path double-counted, dropped a batch event, or
/// bypassed the normal per-event survivor application, the summed `damage_marked`
/// would be wrong.
#[test]
fn weathered_bodyguards_redirects_simultaneous_combat_damage_from_multiple_attackers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // P1 (defending player, "you") controls an untapped Weathered Bodyguards with
    // a large toughness so it survives the combined damage and the assertion is
    // about the summed marked damage, not death.
    let bodyguard = scenario
        .add_creature_from_oracle(P1, "Weathered Bodyguards", 2, 20, WEATHERED_BODYGUARDS_TEXT)
        .id();
    // P0 (active player) attacks P1 with two unblocked creatures.
    let attacker_a = scenario.add_creature(P0, "Charging Bear", 3, 3).id();
    let attacker_b = scenario.add_creature(P0, "Snapping Badger", 2, 2).id();

    let mut runner = scenario.build();
    let p1_life_before = runner.life(P1);

    // Both attackers unblocked (no blocker assignments); run_combat targets P1.
    run_combat(&mut runner, vec![attacker_a, attacker_b], vec![]);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&bodyguard].damage_marked, 5,
        "both attackers' combat damage (3 + 2) must redirect onto Weathered Bodyguards in the same batch"
    );
    assert_eq!(
        runner.life(P1),
        p1_life_before,
        "the protected controller takes none of the simultaneous combat damage"
    );
}
