//! Issue #605 — a Licid activation must attach the Licid itself to the targeted
//! creature, and the animation must not wear off at end of turn.
//!
//! CR 608.2c + CR 303.4: in "This creature loses this ability and becomes an
//! Aura enchantment with enchant creature. Attach **it** to target creature",
//! the earlier clause animates the ability's own SOURCE into an Aura, so the
//! source is the only object the chain has made attachable — the bare pronoun
//! names it. The parser used to fall through to `TargetFilter::ParentTarget`,
//! which `resolve_object_filter` reads as the ability's chosen target: the
//! enchant recipient. The engine then attached the recipient creature to itself,
//! state-based actions silently undid the nonsense attachment, and the card did
//! nothing at all.
//!
//! CR 611.2a: the animation states no duration, so it lasts until the end of the
//! game. Left unstated it defaulted to `UntilEndOfTurn`, and at cleanup the
//! Licid stopped being an Aura while STAYING attached (no state-based action
//! unattaches a non-Aura — CR 704.5p is unimplemented), permanently locking the
//! victim under "Enchanted creature can't attack".
//!
//! All 12 Licids share this clause, so the assertions are written against the
//! clause shape rather than one card's flavor text. Three sibling classes that
//! route through the same anaphor helper are pinned as non-regressions: the
//! Equipment-ETB class (Embercleave), the chained-referent class (Aura Graft),
//! and the non-attachable-source class (Stonehewer Giant).

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityDefinition, Effect, TargetFilter};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const CALMING_LICID_ORACLE: &str = "{W}, {T}: This creature loses this ability and becomes an Aura enchantment with enchant creature. Attach it to target creature. You may pay {W} to end this effect.\n\
Enchanted creature can't attack.";

/// Shipped five-line Oracle text, not just the attach clause — the trigger must
/// keep its binding in the presence of the surrounding static and
/// cost-reduction lines.
const EMBERCLEAVE_ORACLE: &str = "Flash\n\
This spell costs {1} less to cast for each attacking creature you control.\n\
When Embercleave enters, attach it to target creature you control.\n\
Equipped creature gets +1/+1 and has double strike and trample.\n\
Equip {3}";

const AURA_GRAFT_ORACLE: &str = "Gain control of target Aura that's attached to a permanent. Attach it to another permanent it can enchant.";

/// A source that is NOT itself attachable (a Giant creature). Its chain animates
/// nothing, so its "attach it" must keep the pre-existing binding — binding it
/// to the source would attach a creature to a permanent, an illegal state that
/// neither `attachment_illegality` nor the state-based-action sweep cleans up.
const STONEHEWER_GIANT_ORACLE: &str = "Vigilance\n\
{1}{W}, {T}: Search your library for an Equipment card, put it onto the battlefield, attach it to a creature you control, then shuffle.";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

/// The `attachment` filter of the first `Effect::Attach` in an ability chain.
/// Licid activations nest the Attach one link down from a root `GenericEffect`
/// carrying the becomes-an-Aura continuous effect, so a shallow lookup misses it.
fn attach_attachment(def: &AbilityDefinition) -> Option<TargetFilter> {
    if let Effect::Attach { attachment, .. } = def.effect.as_ref() {
        return Some(attachment.clone());
    }
    def.sub_ability
        .as_deref()
        .and_then(attach_attachment)
        .or_else(|| def.else_ability.as_deref().and_then(attach_attachment))
}

fn calming_licid_ability() -> AbilityDefinition {
    let parsed = parse_oracle_text(
        CALMING_LICID_ORACLE,
        "Calming Licid",
        &[],
        &[],
        &["Licid".to_string()],
    );
    assert_eq!(parsed.abilities.len(), 1, "expected one activated ability");
    parsed.abilities.into_iter().next().unwrap()
}

#[test]
fn licid_activation_attaches_the_licid_itself() {
    let attachment = attach_attachment(&calming_licid_ability())
        .expect("licid activation must lower to an Attach effect");
    assert_eq!(
        attachment,
        TargetFilter::SelfRef,
        "CR 608.2c + CR 303.4: an earlier clause animated the SOURCE into an \
         Aura, so bare \"it\" names the source, not the ability's chosen target"
    );
}

/// CR 611.2a: the becomes-an-Aura grant states no duration, so it must be
/// stamped `Duration::Permanent` at parse time. `None` would flip to
/// `UntilEndOfTurn` at the `effect.rs` seam and the Licid would stop being an
/// Aura at cleanup while staying attached.
#[test]
fn licid_animation_is_permanent() {
    let ability = calming_licid_ability();
    let Effect::GenericEffect { duration, .. } = ability.effect.as_ref() else {
        panic!("expected the animation to lower to a GenericEffect: {ability:?}");
    };
    assert_eq!(
        *duration,
        Some(engine::types::ability::Duration::Permanent),
        "CR 611.2a: a continuous effect with no stated duration lasts until the \
         end of the game"
    );
}

#[test]
fn equipment_etb_attach_keeps_its_parent_target_binding() {
    let parsed = parse_oracle_text(
        EMBERCLEAVE_ORACLE,
        "Embercleave",
        &[],
        &[],
        &["Equipment".to_string()],
    );

    let attachment = parsed
        .triggers
        .iter()
        .filter_map(|t| t.execute.as_deref())
        .find_map(attach_attachment)
        .expect("Embercleave's ETB trigger must lower to an Attach effect");
    assert_eq!(
        attachment,
        TargetFilter::ParentTarget,
        "the Equipment-ETB class is intentionally untouched by the issue #605 \
         narrowing: its chain animates nothing, so the parse-time binding stays \
         ParentTarget and is resolved at RUNTIME by \
         `resolve_parent_target_attachment_from_trigger`, which reads the \
         Equipment out of the ZoneChanged trigger context"
    );
}

#[test]
fn chained_referent_attach_still_binds_it_to_the_parent_target() {
    let parsed = parse_oracle_text(AURA_GRAFT_ORACLE, "Aura Graft", &[], &[], &[]);
    let ability = parsed
        .abilities
        .first()
        .expect("Aura Graft must parse to a spell ability");
    assert_eq!(
        attach_attachment(ability).expect("Aura Graft must lower to an Attach effect"),
        TargetFilter::ParentTarget,
        "CR 608.2c: \"it\" after \"gain control of target Aura\" names that \
         earlier chosen target, so ParentTarget must be preserved"
    );
}

/// CR 301.5 + CR 303.4 + CR 704.5p: a source that is neither an Aura nor an
/// Equipment must never become the attachment. Attaching a Giant to a creature
/// is an illegal state the engine cannot currently clean up.
#[test]
fn non_attachable_source_attach_is_never_bound_to_the_source() {
    let parsed = parse_oracle_text(STONEHEWER_GIANT_ORACLE, "Stonehewer Giant", &[], &[], &[]);
    let attachment = parsed
        .abilities
        .iter()
        .find_map(attach_attachment)
        .expect("Stonehewer Giant's activated ability must lower to an Attach effect");
    assert_ne!(
        attachment,
        TargetFilter::SelfRef,
        "Stonehewer Giant is a creature, not an Equipment — its \"attach it\" \
         names the searched-up Equipment, never the source"
    );
}

#[test]
fn calming_licid_becomes_attached_to_the_targeted_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, floating_mana(1, ManaType::White));

    let licid = scenario
        .add_creature(P0, "Calming Licid", 2, 2)
        .from_oracle_text(CALMING_LICID_ORACLE)
        .id();
    let victim = scenario.add_creature(P1, "Colossal Dreadmaw", 7, 7).id();

    let mut runner = scenario.build();
    runner.activate(licid, 0).target_object(victim).resolve();

    {
        let state = runner.state();
        assert_eq!(
            state.objects[&licid].attached_to,
            Some(AttachTarget::Object(victim)),
            "the Licid itself must end up attached to the targeted creature"
        );
        assert!(
            state.objects[&victim].attachments.contains(&licid),
            "the targeted creature must list the Licid among its attachments"
        );
        assert_eq!(
            state.objects[&victim].attached_to, None,
            "the targeted creature must not be attached to anything (issue #605: \
             it was attached to itself, and SBAs then silently undid the \
             activation)"
        );
        assert!(
            state.objects[&licid]
                .card_types
                .subtypes
                .iter()
                .any(|s| s == "Aura"),
            "the Licid must be an Aura while attached: {:?}",
            state.objects[&licid].card_types
        );
    }

    // CR 611.2a + CR 514.2: cross the cleanup step into the next turn. The
    // animation states no duration, so it must survive `prune_end_of_turn_effects`.
    runner.advance_to_end_step();
    runner.advance_to_upkeep();

    let state = runner.state();
    assert!(
        state.objects[&licid]
            .card_types
            .subtypes
            .iter()
            .any(|s| s == "Aura"),
        "CR 611.2a: the becomes-an-Aura grant states no duration, so it must \
         still apply after cleanup: {:?}",
        state.objects[&licid].card_types
    );
    assert_eq!(
        state.objects[&licid].attached_to,
        Some(AttachTarget::Object(victim)),
        "the Licid must still be attached after end of turn"
    );
}
