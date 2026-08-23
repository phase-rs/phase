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
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::types::ability::{Effect, ShieldKind, TargetFilter, TargetRef};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use super::rules::{damage_ability, run_combat};

/// Verbatim Palisade Giant redirection text (Scryfall-verified). Byte-identical
/// to Ancient Adamantoise's redirection line, so both cards are covered by the
/// same parser arm. The conjunct victim scope ("and other permanents you
/// control") is exercised by
/// `palisade_giant_redirects_damage_dealt_to_your_other_permanents`.
const PALISADE_GIANT_TEXT: &str =
    "All damage that would be dealt to you and other permanents you control is dealt to this creature instead.";

/// Verbatim Weathered Bodyguards redirection text (Scryfall-verified) — combat
/// only, from unblocked creatures, gated on being untapped.
const WEATHERED_BODYGUARDS_TEXT: &str = "As long as this creature is untapped, all combat damage \
    that would be dealt to you by unblocked creatures is dealt to this creature instead.";

/// Structural reach-guard: the redirection line really parsed and really
/// installed a CR 614.9 shield on this object.
///
/// Required by any fixture whose only assertion is a value that would also hold
/// if the code under test were never reached — notably
/// `palisade_giant_self_damage_is_marked_once_not_doubled_or_prevented`, where
/// `damage_marked == 5` is equally true when the line regressed to
/// `Effect::Unimplemented` and no shield exists at all.
fn assert_redirect_shield_installed(runner: &GameRunner, obj: ObjectId, name: &str) {
    let object = &runner.state().objects[&obj];
    assert!(
        !object
            .abilities
            .iter()
            .any(|a| matches!(&*a.effect, Effect::Unimplemented { .. })),
        "{name} must parse with zero Effect::Unimplemented, got {:?}",
        object.abilities
    );
    assert!(
        object.replacement_definitions.iter_unchecked().any(|def| {
            matches!(def.shield_kind, ShieldKind::Prevention { .. })
                && def.redirect_target == Some(TargetFilter::SelfRef)
        }),
        "{name} must install a CR 614.9 self-recipient redirection shield, got {:?}",
        object.replacement_definitions
    );
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

/// CR 614.9 + CR 614.1a: the CONJUNCT victim scope. Palisade Giant's line reads
/// "to you AND OTHER PERMANENTS YOU CONTROL", so damage that would be dealt to
/// another permanent you control redirects onto the Giant too — not just damage
/// aimed at you. Before the `PlayerOrPermanentsControlledBy` victim arm the
/// parser collapsed the conjunct to `Player { Controller }` and the permanent leg
/// was silently unprotected.
///
/// Revert guard: with the victim arm reverted, `bystander.damage_marked` is 3 and
/// the Giant's is 0 — both assertions below flip.
#[test]
fn palisade_giant_redirects_damage_dealt_to_your_other_permanents() {
    let mut scenario = GameScenario::new();
    let giant = scenario
        .add_creature_from_oracle(P0, "Palisade Giant", 2, 20, PALISADE_GIANT_TEXT)
        .id();
    let bystander = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    // Negative control: an opponent's creature is NOT covered by "you control".
    let enemy = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    let source = scenario.add_creature(P1, "Damage Source", 3, 3).id();
    let mut runner = scenario.build();

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Object(bystander), 3),
        &mut events,
    )
    .expect("damage to another permanent you control resolves");

    assert_eq!(
        runner.state().objects[&giant].damage_marked,
        3,
        "damage aimed at another permanent you control must redirect onto Palisade Giant"
    );
    assert_eq!(
        runner.state().objects[&bystander].damage_marked,
        0,
        "the protected permanent takes none of it"
    );

    // Negative: an opponent's creature is outside the victim scope entirely.
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Object(enemy), 3),
        &mut events,
    )
    .expect("damage to an opponent's creature resolves");
    assert_eq!(
        runner.state().objects[&enemy].damage_marked,
        3,
        "damage to a permanent you do NOT control is untouched by the shield"
    );
    assert_eq!(
        runner.state().objects[&giant].damage_marked,
        3,
        "the shield must not have redirected the opponent's creature's damage onto the Giant"
    );
}

/// CR 109.1 + CR 614.5: Damage dealt DIRECTLY to a LONE Palisade Giant is
/// outside its own victim scope — its text says "you and OTHER permanents you
/// control", so the shield host is excluded from the permanent leg. The damage
/// is marked exactly once: neither doubled by re-entry nor deleted.
///
/// With a single shield this outcome is the same either way (CR 614.5 already
/// stops re-entry), which is exactly why the single-shield case cannot pin the
/// exclusion — `two_shields_...` below is the discriminating fixture.
#[test]
fn palisade_giant_self_damage_is_marked_once_not_doubled_or_prevented() {
    let mut scenario = GameScenario::new();
    let giant = scenario
        .add_creature_from_oracle(P0, "Palisade Giant", 2, 20, PALISADE_GIANT_TEXT)
        .id();
    let source = scenario.add_creature(P1, "Damage Source", 5, 5).id();
    let mut runner = scenario.build();

    // Positive reach guard: `damage_marked == 5` below also holds when no shield
    // exists at all, so prove the shield is installed before relying on it.
    assert_redirect_shield_installed(&runner, giant, "Palisade Giant");

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Object(giant), 5),
        &mut events,
    )
    .expect("damage dealt directly to the shield host resolves");

    assert_eq!(
        runner.state().objects[&giant].damage_marked,
        5,
        "self-directed damage is marked exactly once — not 10 (re-entry) and not 0 (prevented)"
    );
}

/// CR 109.1 + CR 614.1a + CR 616.1: THE discriminating fixture for the `"other "`
/// self-exclusion token.
///
/// Two Palisade Giants under the same controller (Ancient Adamantoise carries a
/// byte-identical redirection line). Damage aimed at Giant A is:
/// * OUTSIDE Giant A's own victim scope — A's text protects "you and OTHER
///   permanents you control", and A is not other than A;
/// * INSIDE Giant B's victim scope — A is another permanent that player controls.
///
/// So exactly ONE replacement is applicable to the announced event: no CR 616.1
/// choice is offered. B's shield moves the damage to B; the modified event is
/// then inside A's scope (B is an other permanent), so A's shield — which has not
/// yet had its CR 614.5 opportunity — moves it back to A. Both shields are now
/// spent, and A takes the 3. That ping-pong is the rules-correct outcome, not an
/// engine artifact.
///
/// REVERT GUARD: with the `"other "` token discarded (an `opt()` whose value is
/// thrown away, or a `DamageTargetFilter` with no exclusion axis), BOTH shields
/// are applicable to the ANNOUNCED event, so the affected player is handed a
/// CR 616.1 choice and the pipeline halts in `WaitingFor::ReplacementChoice` with
/// no damage marked anywhere. Every assertion below flips.
#[test]
fn two_shields_exclude_their_own_host_so_no_self_no_op_choice_is_offered() {
    let mut scenario = GameScenario::new();
    let giant_a = scenario
        .add_creature_from_oracle(P0, "Palisade Giant", 2, 20, PALISADE_GIANT_TEXT)
        .id();
    let giant_b = scenario
        .add_creature_from_oracle(P0, "Ancient Adamantoise", 2, 20, PALISADE_GIANT_TEXT)
        .id();
    let source = scenario.add_creature(P1, "Damage Source", 3, 3).id();
    let mut runner = scenario.build();

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Object(giant_a), 3),
        &mut events,
    )
    .expect("damage dealt to one of two shield hosts resolves");

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "only ONE shield may be applicable to the announced event — a self-no-op must not be \
         offered as a CR 616.1 candidate, got {:?}",
        runner.state().waiting_for
    );
    // Reach guard: the event really did travel through the OTHER host's shield
    // rather than never being replaced at all.
    let applied: Vec<ObjectId> = events
        .iter()
        .filter_map(|e| match e {
            engine::types::events::GameEvent::ReplacementApplied { source_id, .. } => {
                Some(*source_id)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        applied,
        vec![giant_b, giant_a],
        "CR 616.1 + CR 614.5: the OTHER host's shield applies first, then the damaged host's \
         shield applies to the modified event"
    );
    assert_eq!(
        runner.state().objects[&giant_a].damage_marked,
        3,
        "after both single-use opportunities the damage lands back on Giant A"
    );
    assert_eq!(
        runner.state().objects[&giant_b].damage_marked,
        0,
        "Giant B only passed the damage along; it does not keep it"
    );

    // Positive reach-guard: the same board DOES offer a CR 616.1 choice when both
    // shields are applicable to the ANNOUNCED event — damage aimed at the
    // controller is inside both victim scopes (the player leg carries no
    // exclusion). This proves the assertion above measures the exclusion axis and
    // not a board where choices never arise.
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Player(P0), 3),
        &mut events,
    )
    .expect("damage to the shared controller resolves");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "two shields applicable to the PLAYER leg must still surface a CR 616.1 choice, got {:?}",
        runner.state().waiting_for
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
