//! Runtime regression for the ATTACHMENT-HOST recipient of a durable damage
//! redirection (CR 614.9 + CR 303.4b + CR 301.5a): "All damage that would be
//! dealt to you is dealt to enchanted creature instead." (Pariah, With Great
//! Power . . .) and its Equipment sibling "…to equipped creature instead."
//! (Pariah's Shield).
//!
//! Before the anchored redirection spine these three cards parsed to a bare
//! `ShieldKind::Prevention { All }` with NO redirect destination — a CR 615
//! prevention that DELETED the damage instead of moving it, while
//! `coverage-data.json` reported them fully supported. These tests drive the
//! real `deal_damage` pipeline and fail if either the parser's
//! `TargetFilter::AttachedTo` recipient or the runtime's
//! `DamageRedirectTarget::AttachedToSource` arm is reverted: the host's
//! `damage_marked` stays 0 and the controller's life is untouched (damage
//! vanishing), instead of the host taking it.
//!
//! All Oracle text is verbatim / Scryfall-verified.

use super::rules::damage_ability;
use engine::game::effects::attach::attach_to;
use engine::game::effects::deal_damage;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;

/// Verbatim Pariah (Aura) — the enchanted creature is the recipient.
const PARIAH_TEXT: &str = "Enchant creature\nAll damage that would be dealt to you is dealt to enchanted creature instead.";

/// Verbatim Pariah's Shield (Equipment) — the equipped creature is the recipient.
const PARIAHS_SHIELD_TEXT: &str =
    "All damage that would be dealt to you is dealt to equipped creature instead.\nEquip {3}";

/// Verbatim With Great Power . . . (Aura) — same recipient class as Pariah, used
/// as the second authority in the multi-Aura provenance fixture.
const WITH_GREAT_POWER_TEXT: &str = "Enchant creature you control\nEnchanted creature gets +2/+2 for each Aura and Equipment attached to it.\nAll damage that would be dealt to you is dealt to enchanted creature instead.";

/// Verbatim Empyrial Archangel — the `~`-recipient sibling, used as the negative
/// control proving the new `AttachedToSource` arm did not capture `SelfRef`.
const EMPYRIAL_ARCHANGEL_TEXT: &str =
    "All damage that would be dealt to you is dealt to this creature instead.";

/// Verbatim Treacherous Link — "…is dealt to its controller instead", a recipient
/// the durable grammar deliberately does not support. It must now DECLINE rather
/// than install a global prevention field.
const TREACHEROUS_LINK_TEXT: &str = "Enchant creature\nAll damage that would be dealt to enchanted creature is dealt to its controller instead.";

/// CR 701.3a/b/c + CR 613.7e: Attach through the engine's SINGLE ATTACH
/// AUTHORITY, `effects::attach::attach_to`, rather than hand-wiring
/// `attached_to` / `attachments`.
///
/// That matters here because `attach_to` does three things a hand-wired fixture
/// silently skips: it runs the CR 701.3b legality gate, it bumps the CR 613.7e
/// timestamp on a real host transition, and it detaches from any previous host.
/// A future change that makes redirection depend on attachment legality or
/// timestamp would pass a hand-wired fixture while breaking real games.
///
/// `attach_to`'s return value is deliberately DISCARDED: it yields the PREVIOUS
/// host (`old_target`), not a success flag, so a first attach returns `None`
/// precisely when it succeeded — asserting `.is_some()` on it would fail every
/// fixture here. A refused CR 701.3b gate is instead caught by the wired-state
/// assertions below, which is the only signal that actually discriminates.
/// (Not `attach_as_bestowed_aura` — Pariah is a printed Aura, not a bestowed
/// creature.)
fn attach(runner: &mut GameRunner, attachment: ObjectId, host: ObjectId) {
    attach_to(runner.state_mut(), attachment, host);
    assert_eq!(
        runner.state().objects[&attachment].attached_to,
        Some(AttachTarget::Object(host)),
        "the production attach authority must have wired the host (CR 701.3b gate refused?)"
    );
    assert!(
        runner.state().objects[&host]
            .attachments
            .contains(&attachment),
        "the host must record the attachment"
    );
}

/// CR 614.9 + CR 303.4b: Pariah redirects damage that would be dealt to its
/// controller onto the ENCHANTED CREATURE.
#[test]
fn pariah_redirects_controller_damage_onto_the_enchanted_creature() {
    let mut scenario = GameScenario::new();
    let host = scenario.add_creature(P0, "Wall of Blossoms", 0, 20).id();
    let aura = scenario
        .add_creature(P0, "Pariah", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(PARIAH_TEXT)
        .id();
    let source = scenario.add_creature(P1, "Damage Source", 6, 6).id();
    let mut runner = scenario.build();
    attach(&mut runner, aura, host);

    let p0_life_before = runner.life(P0);

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Player(P0), 6),
        &mut events,
    )
    .expect("damage to Pariah's controller resolves");

    assert_eq!(
        runner.state().objects[&host].damage_marked,
        6,
        "the damage must be redirected onto the enchanted creature, not deleted"
    );
    assert_eq!(
        runner.life(P0),
        p0_life_before,
        "the controller takes none of it — redirected, not dealt twice"
    );
    assert_eq!(
        runner.state().objects[&aura].damage_marked,
        0,
        "the Aura itself is not the recipient"
    );
}

/// CR 614.9 + CR 301.5a: the Equipment sibling. Same arm, "equipped creature".
#[test]
fn pariahs_shield_redirects_controller_damage_onto_the_equipped_creature() {
    let mut scenario = GameScenario::new();
    let host = scenario.add_creature(P0, "Wall of Blossoms", 0, 20).id();
    let equipment = scenario
        .add_creature(P0, "Pariah's Shield", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(PARIAHS_SHIELD_TEXT)
        .id();
    let source = scenario.add_creature(P1, "Damage Source", 4, 4).id();
    let mut runner = scenario.build();
    attach(&mut runner, equipment, host);

    let p0_life_before = runner.life(P0);

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Player(P0), 4),
        &mut events,
    )
    .expect("damage to Pariah's Shield's controller resolves");

    assert_eq!(runner.state().objects[&host].damage_marked, 4);
    assert_eq!(runner.life(P0), p0_life_before);
}

/// NEGATIVE SIBLING: the `~`-recipient class must be unaffected — the new
/// attachment arm must not have captured `TargetFilter::SelfRef`. Empyrial
/// Archangel still marks ITSELF, and a co-existing Pariah in the same fixture
/// still marks its own host.
#[test]
fn self_recipient_sibling_still_redirects_onto_itself() {
    let mut scenario = GameScenario::new();
    let archangel = scenario
        .add_creature_from_oracle(P0, "Empyrial Archangel", 5, 20, EMPYRIAL_ARCHANGEL_TEXT)
        .id();
    let source = scenario.add_creature(P1, "Damage Source", 3, 3).id();
    let mut runner = scenario.build();

    let p0_life_before = runner.life(P0);

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Player(P0), 3),
        &mut events,
    )
    .expect("damage to the Archangel's controller resolves");

    assert_eq!(
        runner.state().objects[&archangel].damage_marked,
        3,
        "a `~` recipient still redirects onto the shield host itself"
    );
    assert_eq!(runner.life(P0), p0_life_before);
}

/// CR 614.9 IDENTITY/PROVENANCE: the recipient is LIVE, not latched at install.
/// Re-pointing the Aura from host A to host B between two damage events must move
/// the redirect with it.
///
/// Revert guard: a latched recipient would mark host A both times.
#[test]
fn attachment_recipient_is_live_not_latched() {
    let mut scenario = GameScenario::new();
    let host_a = scenario.add_creature(P0, "Host A", 0, 20).id();
    let host_b = scenario.add_creature(P0, "Host B", 0, 20).id();
    let aura = scenario
        .add_creature(P0, "Pariah", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(PARIAH_TEXT)
        .id();
    let source = scenario.add_creature(P1, "Damage Source", 2, 2).id();
    let mut runner = scenario.build();
    attach(&mut runner, aura, host_a);

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Player(P0), 2),
        &mut events,
    )
    .expect("first damage event resolves");
    assert_eq!(runner.state().objects[&host_a].damage_marked, 2);

    // Move the Aura. CR 701.3a: `attach_to` performs the detach from the old
    // host itself, so the fixture no longer re-implements it inline.
    attach(&mut runner, aura, host_b);
    assert!(
        !runner.state().objects[&host_a].attachments.contains(&aura),
        "the production attach authority must have detached the old host"
    );

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Player(P0), 5),
        &mut events,
    )
    .expect("second damage event resolves");

    assert_eq!(
        runner.state().objects[&host_b].damage_marked,
        5,
        "the second event must follow the Aura to its new host"
    );
    assert_eq!(
        runner.state().objects[&host_a].damage_marked,
        2,
        "the old host keeps only the first event's damage"
    );
}

/// CR 614.9 + CR 616.1 MULTI-AUTHORITY / PROVENANCE: two Auras of the same class
/// on DIFFERENT hosts, both controlled by you, both applicable to the same
/// player-damage event. The affected player chooses which replacement to apply
/// (`WaitingFor::ReplacementChoice` → `GameAction::ChooseReplacement`), and the
/// recipient must bind to the CHOSEN replacement's own `ReplacementId` source —
/// its own host — not to a per-controller value shared by both shields.
///
/// This is the production `GameAction` route, not a helper call: whichever Aura
/// is selected, ONLY that Aura's host is marked. A per-controller or latched
/// recipient would mark the same host for both selections, flipping one of the
/// two loop iterations.
#[test]
fn chosen_aura_binds_its_own_host_not_a_shared_controller_value() {
    for choose_pariah in [true, false] {
        let mut scenario = GameScenario::new();
        let host_a = scenario.add_creature(P0, "Host A", 0, 20).id();
        let host_b = scenario.add_creature(P0, "Host B", 0, 20).id();
        let pariah = scenario
            .add_creature(P0, "Pariah", 0, 0)
            .as_enchantment()
            .with_subtypes(vec!["Aura"])
            .from_oracle_text(PARIAH_TEXT)
            .id();
        let great_power = scenario
            .add_creature(P0, "With Great Power . . .", 0, 0)
            .as_enchantment()
            .with_subtypes(vec!["Aura"])
            .from_oracle_text(WITH_GREAT_POWER_TEXT)
            .id();
        let source = scenario.add_creature(P1, "Damage Source", 7, 7).id();
        let mut runner = scenario.build();
        attach(&mut runner, pariah, host_a);
        attach(&mut runner, great_power, host_b);

        let p0_life_before = runner.life(P0);

        let mut events = Vec::new();
        deal_damage::resolve(
            runner.state_mut(),
            &damage_ability(source, TargetRef::Player(P0), 7),
            &mut events,
        )
        .expect("damage to the shared controller resolves");

        // Reach guard: two applicable shields really do surface a choice.
        let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
        else {
            panic!(
                "two applicable redirection shields must surface a ReplacementChoice, got {:?}",
                runner.state().waiting_for
            );
        };
        let wanted = if choose_pariah { pariah } else { great_power };
        let index = candidates
            .iter()
            .position(|c| c.source_id == wanted)
            .expect("both Auras must be offered as candidates");

        runner
            .act(GameAction::ChooseReplacement { index })
            .expect("applying the chosen redirection replacement must succeed");

        let (expected_host, other_host) = if choose_pariah {
            (host_a, host_b)
        } else {
            (host_b, host_a)
        };
        assert_eq!(
            runner.state().objects[&expected_host].damage_marked,
            7,
            "the CHOSEN Aura's own host must take the damage (choose_pariah={choose_pariah})"
        );
        assert_eq!(
            runner.state().objects[&other_host].damage_marked,
            0,
            "the other Aura's host must be untouched (choose_pariah={choose_pariah})"
        );
        assert_eq!(
            runner.life(P0),
            p0_life_before,
            "the controller takes none of it"
        );
        // Reach guard: neither Aura marks itself.
        assert_eq!(runner.state().objects[&pariah].damage_marked, 0);
        assert_eq!(runner.state().objects[&great_power].damage_marked, 0);
    }
}

/// CR 614.9: "If one of those permanents is no longer on the battlefield … the
/// effect does nothing." An UNATTACHED Aura has no host at all, so the redirect
/// does nothing and the damage stays on the original recipient — it is neither
/// prevented nor vanished.
#[test]
fn unattached_aura_redirect_does_nothing_and_damage_hits_the_controller() {
    let mut scenario = GameScenario::new();
    let host = scenario.add_creature(P0, "Wall of Blossoms", 0, 20).id();
    let aura = scenario
        .add_creature(P0, "Pariah", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(PARIAH_TEXT)
        .id();
    let source = scenario.add_creature(P1, "Damage Source", 4, 4).id();
    let mut runner = scenario.build();
    // Deliberately NOT attached.

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
        "with no attachment host the redirection does nothing — the damage is NOT prevented"
    );
    assert_eq!(runner.state().objects[&host].damage_marked, 0);
    assert_eq!(runner.state().objects[&aura].damage_marked, 0);
}

/// CR 614.9: "…or is no longer a battle, creature, or planeswalker when the
/// damage would be redirected, the effect does nothing." The host stays on the
/// battlefield but loses its creature core type — an illegal recipient. Mirrors
/// `palisade_giant_illegal_recipient_makes_redirect_do_nothing`.
#[test]
fn illegal_attachment_host_makes_redirect_do_nothing() {
    let mut scenario = GameScenario::new();
    let host = scenario.add_creature(P0, "Wall of Blossoms", 0, 20).id();
    let aura = scenario
        .add_creature(P0, "Pariah", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(PARIAH_TEXT)
        .id();
    let source = scenario.add_creature(P1, "Damage Source", 4, 4).id();
    let mut runner = scenario.build();
    attach(&mut runner, aura, host);
    runner
        .state_mut()
        .objects
        .get_mut(&host)
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
        "an illegal recipient makes the redirection do nothing — damage is not prevented"
    );
    assert_eq!(runner.state().objects[&host].damage_marked, 0);
}

/// The live-bug guard. Treacherous Link's "…is dealt to its controller instead"
/// recipient is unsupported, and the handler used to emit
/// `Prevention { All }` with `damage_target_filter: null` — a shield matching
/// EVERY damage event to every object and player in the game. With the anchored
/// spine it declines outright, so unrelated damage lands normally.
///
/// Revert guard: with the old handler, the first assertion fails (the unrelated
/// player takes no damage at all).
#[test]
fn treacherous_link_no_longer_installs_a_global_damage_prevention_field() {
    let mut scenario = GameScenario::new();
    let host = scenario.add_creature(P1, "Enchanted Bear", 2, 20).id();
    let aura = scenario
        .add_creature(P0, "Treacherous Link", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(TREACHEROUS_LINK_TEXT)
        .id();
    let source = scenario.add_creature(P1, "Damage Source", 3, 3).id();
    let mut runner = scenario.build();
    attach(&mut runner, aura, host);

    let p0_life_before = runner.life(P0);
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Player(P0), 3),
        &mut events,
    )
    .expect("unrelated damage resolves");

    assert_eq!(
        runner.life(P0),
        p0_life_before - 3,
        "an unrelated damage event must still be dealt — no global prevention field"
    );

    // Positive reach-guard: the same board WITHOUT Treacherous Link produces an
    // identical life delta, proving the assertion is not measuring a no-op board.
    let mut scenario = GameScenario::new();
    let source = scenario.add_creature(P1, "Damage Source", 3, 3).id();
    let mut control = scenario.build();
    let control_life_before = control.life(P0);
    let mut events = Vec::new();
    deal_damage::resolve(
        control.state_mut(),
        &damage_ability(source, TargetRef::Player(P0), 3),
        &mut events,
    )
    .expect("control damage resolves");
    assert_eq!(
        control_life_before - control.life(P0),
        p0_life_before - runner.life(P0),
        "the life delta must match the Treacherous-Link-free control board"
    );
}
