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
//! Licid stopped being an Aura while STAYING attached, permanently locking the
//! victim under "Enchanted creature can't attack". (CR 704.5p is now
//! implemented — `sba::check_illegal_attachment_unattach` — so the reverted
//! Licid also unattaches; the permanent duration remains the CR 611.2a-correct
//! model, and the SBA is a safety net for an illegal state, not a licence to
//! create one.)
//!
//! All 12 Aura-form Licids share this clause, so the assertions are written
//! against the clause shape rather than one card's flavor text. Three sibling
//! classes that route through the same anaphor helper are pinned as
//! non-regressions: the Equipment-ETB class (Embercleave), the chained-referent
//! class (Aura Graft), and the non-attachable-source class (Stonehewer Giant).
//!
//! The second half of this file covers CR 116.2c — "You may pay {W} to end this
//! effect" — across all THIRTEEN cards in that class (the 12 Aura-form Licids
//! plus Flanking Licid), and CR 704.5p as a card-agnostic building block.

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, TargetFilter};
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

// ── CR 116.2c: "You may pay {W} to end this effect" ──────────────────────────
//
// The Licid cycle grants a LATER special action, not a resolution-time payment
// (CR 116.2c: "Some effects allow a player to take an action at a later time,
// usually to end a continuous effect... Doing so is a special action. A player
// can take such an action any time they have priority, unless that effect
// specifies another timing restriction"). The parser used to lower that clause
// to a mandatory `Effect::PayCost` that force-paid the cost the instant the
// activation resolved; it now rides on the installed continuous effect as
// `Effect::GenericEffect.end_cost` → `TransientContinuousEffect.end_permission`.
//
// Calming Licid's printed ruling (2013-04-15): "Paying the cost to end the
// effect is a special action and can't be responded to" (CR 116.1).

use engine::ai_support::legal_actions_for_viewer;
use engine::game::combat::AttackTarget as CombatAttackTarget;
use engine::game::scenario::GameRunner;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{EndEffectGroupId, GameState};
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::player::PlayerId;
use engine::types::resolved_commands::{
    ResolvedContinuousEffectReplayInvariantError, ResolvedRulesCommand,
};

const CONVULSING_LICID_ORACLE: &str = "{R}, {T}: This creature loses this ability and becomes an Aura enchantment with enchant creature. Attach it to target creature. You may pay {R} to end this effect.\n\
Enchanted creature can't block.";

/// Verbatim from `data/card-data.json` — pre-Oracle-template wording that the
/// Scryfall `oracle:` search for the other twelve does not return.
const FLANKING_LICID_ORACLE: &str = "{R}, {T}: Flanking Licid loses this ability and becomes a creature enchantment that reads \"Enchanted creature gains flanking\" instead of a creature. Move Flanking Licid onto target creature. You may pay {R} to end this effect.";

/// Every shipped card in the CR 116.2c class, verbatim from `data/card-data.json`.
/// Used by V15 to prove none of the thirteen gained a `parse_warnings` entry.
const ALL_LICID_ORACLE: &[(&str, &str, &str)] = &[
    ("Calming Licid", "W", CALMING_LICID_ORACLE),
    ("Convulsing Licid", "R", CONVULSING_LICID_ORACLE),
    ("Corrupting Licid", "B", "{B}, {T}: This creature loses this ability and becomes an Aura enchantment with enchant creature. Attach it to target creature. You may pay {B} to end this effect.\nEnchanted creature has fear. (It can't be blocked except by artifact creatures and/or black creatures.)"),
    ("Dominating Licid", "U", "{1}{U}{U}, {T}: This creature loses this ability and becomes an Aura enchantment with enchant creature. Attach it to target creature. You may pay {U} to end this effect.\nYou control enchanted creature."),
    ("Enraging Licid", "R", "{R}, {T}: This creature loses this ability and becomes an Aura enchantment with enchant creature. Attach it to target creature. You may pay {R} to end this effect.\nEnchanted creature has haste."),
    ("Flanking Licid", "R", FLANKING_LICID_ORACLE),
    ("Gliding Licid", "U", "{U}, {T}: This creature loses this ability and becomes an Aura enchantment with enchant creature. Attach it to target creature. You may pay {U} to end this effect.\nEnchanted creature has flying."),
    ("Leeching Licid", "B", "{B}, {T}: This creature loses this ability and becomes an Aura enchantment with enchant creature. Attach it to target creature. You may pay {B} to end this effect.\nAt the beginning of the upkeep of enchanted creature's controller, this creature deals 1 damage to that player."),
    ("Nurturing Licid", "G", "{G}, {T}: This creature loses this ability and becomes an Aura enchantment with enchant creature. Attach it to target creature. You may pay {G} to end this effect.\n{G}: Regenerate enchanted creature."),
    ("Quickening Licid", "W", "{1}{W}, {T}: This creature loses this ability and becomes an Aura enchantment with enchant creature. Attach it to target creature. You may pay {W} to end this effect.\nEnchanted creature has first strike."),
    ("Stinging Licid", "U", "{1}{U}, {T}: This creature loses this ability and becomes an Aura enchantment with enchant creature. Attach it to target creature. You may pay {U} to end this effect.\nWhenever enchanted creature becomes tapped, this creature deals 2 damage to that creature's controller."),
    ("Tempting Licid", "G", "{G}, {T}: This creature loses this ability and becomes an Aura enchantment with enchant creature. Attach it to target creature. You may pay {G} to end this effect.\nAll creatures able to block enchanted creature do so."),
    ("Transmogrifying Licid", "GENERIC1", "{1}, {T}: This creature loses this ability and becomes an Aura enchantment with enchant creature. Attach it to target creature. You may pay {1} to end this effect.\nEnchanted creature gets +1/+1 and is an artifact in addition to its other types."),
];

fn mana_cost_of(code: &str) -> ManaCost {
    match code {
        "GENERIC1" => ManaCost::Cost {
            shards: vec![],
            generic: 1,
        },
        _ => ManaCost::Cost {
            shards: vec![match code {
                "W" => ManaCostShard::White,
                "U" => ManaCostShard::Blue,
                "B" => ManaCostShard::Black,
                "R" => ManaCostShard::Red,
                "G" => ManaCostShard::Green,
                _ => unreachable!("unmapped test cost code"),
            }],
            generic: 0,
        },
    }
}

/// Every `Effect` in a definition tree, root first.
fn walk_effects(def: &AbilityDefinition, out: &mut Vec<Effect>) {
    out.push((*def.effect).clone());
    if let Some(sub) = def.sub_ability.as_deref() {
        walk_effects(sub, out);
    }
    if let Some(other) = def.else_ability.as_deref() {
        walk_effects(other, out);
    }
}

fn effects_of(def: &AbilityDefinition) -> Vec<Effect> {
    let mut out = Vec::new();
    walk_effects(def, &mut out);
    out
}

fn root_end_cost(def: &AbilityDefinition) -> Option<ManaCost> {
    match def.effect.as_ref() {
        Effect::GenericEffect { end_cost, .. } => end_cost.clone(),
        _ => None,
    }
}

/// The single `EndContinuousEffect` group the engine offers `player`, obtained
/// from the PRODUCTION legal-action path — never hand-constructed. This is what
/// proves registration point `ai_support::candidates` is wired: a missing
/// enumeration entry leaves the action legal but undiscoverable.
fn offered_end_actions(runner: &GameRunner, player: PlayerId) -> Vec<GameAction> {
    let (actions, _, _) = legal_actions_for_viewer(runner.state(), player);
    actions
        .into_iter()
        .filter(|action| matches!(action, GameAction::EndContinuousEffect { .. }))
        .collect()
}

fn offered_end_groups(runner: &GameRunner, player: PlayerId) -> Vec<EndEffectGroupId> {
    offered_end_actions(runner, player)
        .into_iter()
        .map(|action| match action {
            GameAction::EndContinuousEffect { group, .. } => group,
            _ => unreachable!("offered_end_actions keeps only pay-to-end actions"),
        })
        .collect()
}

/// A Licid already animated onto `victim`, with `spare_white` extra untapped
/// Plains left on the battlefield after the `{W},{T}` activation is paid.
///
/// The returned `Vec` holds those spare Plains, so a caller can pin which mana
/// source a later payment actually consumed rather than only reading the pool.
fn animated_calming_licid(spare_white: usize) -> (GameRunner, ObjectId, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, floating_mana(1, ManaType::White));
    let licid = scenario
        .add_creature(P0, "Calming Licid", 2, 2)
        .from_oracle_text(CALMING_LICID_ORACLE)
        .id();
    let victim = scenario.add_creature(P1, "Colossal Dreadmaw", 7, 7).id();
    let spare_plains: Vec<ObjectId> = (0..spare_white)
        .map(|_| {
            scenario
                .add_land_from_oracle(P0, "Plains", "{T}: Add {W}.")
                .id()
        })
        .collect();
    // CR 704.5b: stock both libraries so crossing a turn boundary in the tests
    // below does not end the game by drawing from an empty library.
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    scenario.with_library_top(P1, &["Forest", "Forest", "Forest"]);
    let mut runner = scenario.build();
    runner.activate(licid, 0).target_object(victim).resolve();
    (runner, licid, victim, spare_plains)
}

// ── V1 — parser: the mandatory PayCost is gone; the cost rides on the effect ──

#[test]
fn licid_termination_cost_rides_on_the_effect_not_a_pay_cost_def() {
    let ability = calming_licid_ability();
    let effects = effects_of(&ability);
    assert!(
        !effects.iter().any(|e| matches!(e, Effect::PayCost { .. })),
        "CR 116.2c: the termination clause grants a LATER special action, so it \
         must NOT lower to a resolution-time PayCost def: {effects:?}"
    );
    assert_eq!(
        root_end_cost(&ability),
        Some(mana_cost_of("W")),
        "CR 116.2c: the cost must be stamped on the GenericEffect that installs \
         the continuous effect the payment ends"
    );
}

// ── V2 / V2b — the pay branch, through the real action pipeline ──────────────

#[test]
fn paying_the_termination_cost_ends_the_animation_and_detaches_the_licid() {
    let (mut runner, licid, victim, spare_plains) = animated_calming_licid(1);
    let plains = spare_plains[0];

    let actions = offered_end_actions(&runner, P0);
    assert_eq!(
        actions.len(),
        1,
        "CR 116.2c: exactly one pay-to-end permission must be offered"
    );
    let group = match &actions[0] {
        GameAction::EndContinuousEffect { group, .. } => *group,
        _ => unreachable!("offered_end_actions returned a different action"),
    };

    // Payment pins (paired with the post-action assertions below): the `{W},{T}`
    // activation already emptied the mana pool, so this untapped Plains is the
    // ONLY {W} source on the board. A `pay_end_continuous_effect_cost` that
    // reported success without spending anything would leave the pool at 0 AND
    // this Plains untapped — so the pool total alone proves nothing, and the
    // tapped transition below is what makes the payment assertion discriminating.
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "precondition: the activation consumed the seeded floating {{W}}, so the \
         termination must be paid from a mana source"
    );
    assert!(
        !runner.state().objects[&plains].tapped,
        "precondition: the only {{W}} source is still untapped before the payment"
    );

    // V2b (B1 pin): SBAs must run inside THIS action. `commit_end_continuous_effect`
    // returns `WaitingFor::Priority`, and `apply_action`'s tail runs the CR 704.3
    // post-action pipeline for any arm whose result is Priority. No manual
    // priority advance happens between here and the assertions below.
    let result = runner
        .act(actions[0].clone())
        .expect("the offered pay-to-end special action must be legal");
    assert!(
        matches!(
            result.waiting_for,
            engine::types::game_state::WaitingFor::Priority { .. }
        ),
        "CR 116.2c + CR 116.1: a special action does not use the stack, so \
         priority returns immediately: {:?}",
        result.waiting_for
    );

    let state = runner.state();
    // (a) the continuous effect is gone
    assert!(
        !state
            .transient_continuous_effects
            .iter()
            .any(|t| t.end_permission.as_ref().is_some_and(|p| p.group == group)),
        "CR 116.2c: paying must remove every transient effect in the group"
    );
    // (d) characteristics restored — CR 613.1, emergent from removal
    assert!(
        !state.objects[&licid]
            .card_types
            .subtypes
            .iter()
            .any(|s| s == "Aura"),
        "CR 613.1: removing the effect IS the restoration — the Licid must stop \
         being an Aura: {:?}",
        state.objects[&licid].card_types
    );
    assert!(
        state.objects[&licid]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "CR 613.1: the Licid reverts to its printed Creature type: {:?}",
        state.objects[&licid].card_types
    );
    assert!(
        state.objects[&licid]
            .abilities
            .iter()
            .any(|ability| ability.kind == AbilityKind::Activated),
        "CR 613.1: ending the effect removes its loses-this-ability layer, so the \
         Licid's printed activated ability must be restored"
    );
    // (b) + (c) CR 704.5p sentence 1 unattached it, inside the same action
    assert_eq!(
        state.objects[&licid].attached_to, None,
        "CR 704.5p: a creature attached to an object becomes unattached and \
         remains on the battlefield"
    );
    assert!(
        !state.objects[&victim].attachments.contains(&licid),
        "CR 704.5p: the host's attachment list must drop the ex-Aura too"
    );
    assert_eq!(
        state.objects[&licid].zone,
        engine::types::zones::Zone::Battlefield,
        "CR 704.5p: it becomes unattached and REMAINS ON THE BATTLEFIELD — \
         unlike CR 704.5m, it is not put into its owner's graveyard"
    );
    // (f) the {W} was actually consumed — the sole source auto-tapped for it,
    // and nothing was left floating afterwards.
    assert!(
        state.objects[&plains].tapped,
        "CR 118.1: the termination cost must actually be paid, which means the \
         only {{W}} source on the board must have been tapped for it"
    );
    assert_eq!(
        state.players[P0.0 as usize].mana_pool.total(),
        0,
        "CR 118.1: the mana produced for the cost must be spent, not left \
         floating: {:?}",
        state.players[P0.0 as usize].mana_pool
    );

    // (e) the victim can attack again — driven through the real declaration
    // path, not read off a flag.
    runner.advance_to_end_step();
    runner.advance_to_upkeep();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(victim, CombatAttackTarget::Player(P0))])
        .expect(
            "CR 613.1: with the Licid detached, \"Enchanted creature can't attack\" \
             no longer applies and the victim must be able to attack",
        );
}

// ── V3 — the decline branch: nothing is force-paid ───────────────────────────

#[test]
fn declining_the_termination_leaves_the_animation_and_never_spends_the_mana() {
    // Two white: one consumed by the {W},{T} activation, one left to prove the
    // termination cost was NOT force-paid on resolution (the old PayCost bug).
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, floating_mana(2, ManaType::White));
    let licid = scenario
        .add_creature(P0, "Calming Licid", 2, 2)
        .from_oracle_text(CALMING_LICID_ORACLE)
        .id();
    let victim = scenario.add_creature(P1, "Colossal Dreadmaw", 7, 7).id();
    let mut runner = scenario.build();
    runner.activate(licid, 0).target_object(victim).resolve();

    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        1,
        "CR 116.2c: the termination cost is a LATER special action — resolving \
         the activation must not spend the second {{W}} (regression guard for \
         the mandatory PayCost lowering)"
    );

    // Never submit the action; cross cleanup into the next turn.
    runner.advance_to_end_step();
    runner.advance_to_upkeep();

    let state = runner.state();
    assert!(
        state.objects[&licid]
            .card_types
            .subtypes
            .iter()
            .any(|s| s == "Aura"),
        "CR 611.2a: an unpaid termination leaves the animation in place"
    );
    assert_eq!(
        state.objects[&licid].attached_to,
        Some(AttachTarget::Object(victim)),
        "an unpaid termination leaves the Licid attached"
    );
}

// ── V4 — the action is OFFERED at priority, and only after animating ─────────

#[test]
fn pay_to_end_is_offered_only_once_the_effect_is_installed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, floating_mana(1, ManaType::White));
    let licid = scenario
        .add_creature(P0, "Calming Licid", 2, 2)
        .from_oracle_text(CALMING_LICID_ORACLE)
        .id();
    let victim = scenario.add_creature(P1, "Colossal Dreadmaw", 7, 7).id();
    scenario.add_land_from_oracle(P0, "Plains", "{T}: Add {W}.");
    let mut runner = scenario.build();

    assert!(
        offered_end_groups(&runner, P0).is_empty(),
        "no continuous effect is installed yet, so no CR 116.2c permission exists"
    );

    runner.activate(licid, 0).target_object(victim).resolve();

    assert_eq!(
        offered_end_groups(&runner, P0).len(),
        1,
        "CR 116.2c: the permission must appear in the production legal-action \
         set once the effect is installed"
    );
}

// ── V5 — no timing restriction (CR 116.2c), unlike CR 116.2g's companion ─────

#[test]
fn pay_to_end_is_offered_outside_a_sorcery_speed_window() {
    let (mut runner, _licid, _victim, _plains) = animated_calming_licid(1);

    // A non-main phase. CR 116.2c grants the action "any time they have
    // priority, unless that effect specifies another timing restriction", and
    // no Licid states one — so unlike `CompanionToHand` (CR 116.2g), there is
    // no sorcery-speed gate to fail.
    runner.advance_to_end_step();

    assert_eq!(
        offered_end_groups(&runner, P0).len(),
        1,
        "CR 116.2c: the permission is legal at EVERY priority window"
    );
}

// ── V7 — multi-authority: two Licids, two independent groups ─────────────────

#[test]
fn ending_one_licids_effect_leaves_the_other_licid_untouched() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let calming = scenario
        .add_creature(P0, "Calming Licid", 2, 2)
        .from_oracle_text(CALMING_LICID_ORACLE)
        .id();
    let convulsing = scenario
        .add_creature(P0, "Convulsing Licid", 1, 1)
        .from_oracle_text(CONVULSING_LICID_ORACLE)
        .id();
    let victim_a = scenario.add_creature(P1, "Colossal Dreadmaw", 7, 7).id();
    let victim_b = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    scenario.with_mana_pool(P0, {
        let mut m = floating_mana(2, ManaType::White);
        m.extend(floating_mana(2, ManaType::Red));
        m
    });
    let mut runner = scenario.build();
    runner
        .activate(calming, 0)
        .target_object(victim_a)
        .resolve();
    runner
        .activate(convulsing, 0)
        .target_object(victim_b)
        .resolve();

    let groups = offered_end_groups(&runner, P0);
    assert_eq!(
        groups.len(),
        2,
        "CR 116.2c: each activation installs its OWN continuous effect with its \
         own group, so two independent permissions must be offered: {groups:?}"
    );
    assert_ne!(
        groups[0], groups[1],
        "the two groups must be distinct — a shared group would end both on one \
         payment"
    );

    // End the Calming Licid's effect. Identify its group by source, so the test
    // does not depend on offering order.
    let calming_action = offered_end_actions(&runner, P0)
        .into_iter()
        .find(|action| {
            matches!(
                action,
                GameAction::EndContinuousEffect { source_name, .. }
                    if source_name == "Calming Licid"
            )
        })
        .expect("the Calming Licid's effect must have an engine-authored action");
    runner
        .act(calming_action)
        .expect("ending the Calming Licid's effect must be legal");

    let state = runner.state();
    assert_eq!(
        state.objects[&calming].attached_to, None,
        "the paid-for Licid detaches (CR 704.5p)"
    );
    assert_eq!(
        state.objects[&convulsing].attached_to,
        Some(AttachTarget::Object(victim_b)),
        "CR 116.2c: the OTHER Licid's continuous effect is a different effect \
         and must survive"
    );
    assert!(
        state.objects[&convulsing]
            .card_types
            .subtypes
            .iter()
            .any(|s| s == "Aura"),
        "the other Licid must still be an Aura"
    );
    assert_eq!(
        offered_end_groups(&runner, P0).len(),
        1,
        "the other Licid's permission must still be offered"
    );
}

/// CR 733 replay must advance every identity allocator represented by a
/// continuous-effect install. Otherwise replaying a Licid activation would
/// restore the effect with group N while leaving N available for the next live
/// activation, making one later payment end two unrelated effects.
#[test]
fn pay_to_end_group_allocator_is_replayed_with_the_effect() {
    let (runner, _licid, _victim, _plains) = animated_calming_licid(1);
    let install = runner
        .state()
        .resolved_rules_journal
        .entries()
        .iter()
        .filter_map(|entry| entry.command.as_ref())
        .find_map(|command| match command {
            ResolvedRulesCommand::ContinuousEffectInstall(command)
                if command.effect.end_permission.is_some() =>
            {
                Some(command.as_ref().clone())
            }
            _ => None,
        })
        .expect("the Licid animation must journal its resolved continuous effect");
    let group = install
        .effect
        .end_permission
        .as_ref()
        .expect("the recorded effect must carry its termination group")
        .group;
    assert!(
        group.0 < install.resulting_next_end_effect_group_id,
        "the recorded group must lie below its allocator receipt"
    );

    let mut replay = GameState::new_two_player(17);
    replay
        .apply_resolved_continuous_effect(&install)
        .expect("the recorded install must replay against an empty predecessor");
    assert!(
        replay.next_end_effect_group_id >= install.resulting_next_end_effect_group_id,
        "replay must advance the termination-group allocator past the installed group"
    );

    let mut impossible = install;
    impossible.resulting_next_end_effect_group_id = group.0;
    let mut rejected = GameState::new_two_player(17);
    assert_eq!(
        rejected.apply_resolved_continuous_effect(&impossible),
        Err(
            ResolvedContinuousEffectReplayInvariantError::EndEffectGroupAboveHighWater {
                group: group.0,
                high_water: group.0,
            }
        ),
        "a serialized install whose group was never drawn must fail closed"
    );
    assert!(
        rejected.transient_continuous_effects.is_empty(),
        "a rejected replay command must not mutate the effect list"
    );
}

// ── V8 — an opponent cannot end an effect they do not control ────────────────

#[test]
fn opponent_cannot_end_a_continuous_effect_they_do_not_control() {
    let (mut runner, licid, victim, _plains) = animated_calming_licid(1);
    let action = offered_end_actions(&runner, P0)
        .into_iter()
        .next()
        .expect("the controller must receive the engine-authored action");

    assert!(
        offered_end_groups(&runner, P1).is_empty(),
        "CR 109.5: the activated ability's controller is the player meant by \
         \"you\" and the only player offered its termination"
    );
    assert!(
        engine::game::engine::apply(runner.state_mut(), P1, action,).is_err(),
        "a forged submission from the wrong seat must be rejected"
    );
    assert_eq!(
        runner.state().objects[&licid].attached_to,
        Some(AttachTarget::Object(victim)),
        "a rejected action must mutate nothing"
    );
}

// ── V9 — affordability gates the offer (with a paired positive) ──────────────

#[test]
fn unaffordable_termination_is_not_offered_but_an_affordable_one_is() {
    // No spare white source: the {W},{T} activation consumed the only mana.
    let (runner, _, _, _) = animated_calming_licid(0);
    assert!(
        offered_end_groups(&runner, P0).is_empty(),
        "CR 118.1: an unpayable cost means the special action is not available"
    );

    // Paired positive — the same fixture WITH a white source does offer it, so
    // the absence above is not vacuous.
    let (runner, _, _, _) = animated_calming_licid(1);
    assert_eq!(
        offered_end_groups(&runner, P0).len(),
        1,
        "with a payable source the permission IS offered"
    );
}

// ── V11 — the permission dies with the effect ───────────────────────────────

#[test]
fn permission_disappears_when_the_licid_leaves_the_battlefield() {
    let (mut runner, licid, _victim, _plains) = animated_calming_licid(1);
    assert_eq!(
        offered_end_groups(&runner, P0).len(),
        1,
        "precondition: the permission is offered while the effect is live"
    );

    engine::game::zones::move_to_zone(
        runner.state_mut(),
        licid,
        engine::types::zones::Zone::Graveyard,
        &mut Vec::new(),
    );
    engine::game::layers::flush_layers(runner.state_mut());

    assert!(
        offered_end_groups(&runner, P0).is_empty(),
        "the permission rides on the installed effect, so it dies with it — no \
         dangling CR 116.2c offer"
    );
}

// ── V18 — the payment is visible in the log ─────────────────────────────────

#[test]
fn ending_a_continuous_effect_emits_a_player_visible_event_and_log_line() {
    let (mut runner, licid, _victim, _plains) = animated_calming_licid(1);
    let action = offered_end_actions(&runner, P0)
        .into_iter()
        .next()
        .expect("the controller must receive the engine-authored action");
    let group = match &action {
        GameAction::EndContinuousEffect { group, .. } => *group,
        _ => unreachable!("offered_end_actions returned a different action"),
    };
    let result = runner
        .act(action)
        .expect("the offered action must be legal");

    assert!(
        result.events.iter().any(|e| matches!(
            e,
            GameEvent::ContinuousEffectEnded { group: g, source_id, player }
                if *g == group && *source_id == licid && *player == P0
        )),
        "CR 116.2c: a player-visible special action must emit its event: {:?}",
        result.events
    );
    assert!(
        !result.log_entries.is_empty(),
        "a player-visible special action with no log line is a defect"
    );
}

// ── CR 704.5p as a BUILDING BLOCK, not a Licid special case ─────────────────
//
// > 704.5p If a battle or creature is attached to an object or player, it
// > becomes unattached and remains on the battlefield. Similarly, if any
// > nonbattle, noncreature permanent that's neither an Aura, an Equipment, nor
// > a Fortification is attached to an object or player, it becomes unattached
// > and remains on the battlefield.
//
// `sba::check_illegal_attachment_unattach` generalizes what used to be a
// battle-only fragment. The rows below drive it from a NON-Licid direction so a
// regression cannot be masked by the Licid path, and pin the two engine models
// that correctly suppress creature-ness while attached (reconfigure CR 702.151b,
// bestow CR 702.103b).

/// March of the Machines' own reminder text is this rule restated on a card:
/// "(Equipment that's a creature can't equip a creature.)"
const MARCH_OF_THE_MACHINES_ORACLE: &str = "Each noncreature artifact is an artifact creature with power and toughness each equal to its mana value. (Equipment that's a creature can't equip a creature.)";

/// V12 — CR 704.5p sentence 1 on the genuine Equipment case. Not a Licid.
#[test]
fn animating_an_attached_equipment_unattaches_it_and_keeps_it_on_the_battlefield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bearer = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let equipment = scenario
        .add_creature(P0, "Bonesplitter", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 1,
        })
        .id();
    let march = scenario
        .add_creature(P0, "March of the Machines", 0, 0)
        .as_enchantment()
        .id();
    let mut runner = scenario.build();

    // Attach first, WITHOUT the animation active, so the fixture reaches the
    // widened predicate from a legal starting state rather than a pre-broken one.
    // `attach_to` returns the PRIOR host, so `None` is the normal first-attach
    // result. Legality is asserted by the `attached_to` read below.
    engine::game::effects::attach::attach_to(runner.state_mut(), equipment, bearer);
    assert_eq!(
        runner.state().objects[&equipment].attached_to,
        Some(AttachTarget::Object(bearer)),
        "precondition: the Equipment is legally equipped before March resolves"
    );

    // Now animate it. `from_oracle_text` on the already-built board would not
    // re-run synthesis into the layer pass, so install the static directly and
    // let the layer engine derive the Equipment's Creature type from it.
    let march_static = engine::parser::oracle::parse_oracle_text(
        MARCH_OF_THE_MACHINES_ORACLE,
        "March of the Machines",
        &[],
        &[],
        &[],
    );
    {
        let obj = runner.state_mut().objects.get_mut(&march).unwrap();
        for def in march_static.statics.iter() {
            std::sync::Arc::make_mut(&mut obj.base_static_definitions).push(def.clone());
        }
    }
    runner.state_mut().layers_dirty.mark_full();
    engine::game::layers::flush_layers(runner.state_mut());
    assert!(
        runner.state().objects[&equipment]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "reach guard: March must actually have animated the Equipment before the \
         SBA is asked about it — otherwise this row is vacuous: {:?}",
        runner.state().objects[&equipment].card_types
    );

    let mut events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);

    assert_eq!(
        runner.state().objects[&equipment].attached_to,
        None,
        "CR 704.5p sentence 1: a creature attached to an object becomes unattached"
    );
    assert!(
        !runner.state().objects[&bearer]
            .attachments
            .contains(&equipment),
        "the host's attachment list must drop it too"
    );
    assert_eq!(
        runner.state().objects[&equipment].zone,
        engine::types::zones::Zone::Battlefield,
        "CR 704.5p: it REMAINS ON THE BATTLEFIELD — this is not CR 704.5m"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::Unattached { attachment_id, .. } if *attachment_id == equipment
        )),
        "CR 701.3d: an Equipment that ceases to be attached counts as \"becoming \
         unattached\", so the sweep must emit the event `match_unattach` consumes: \
         {events:?}"
    );
}

/// V13 — the negative sibling: an ordinary Aura and an ordinary Equipment, each
/// legally attached, must NOT be swept. Without this, V12 would pass for a
/// predicate that unattaches everything.
#[test]
fn legally_attached_auras_and_equipment_are_left_alone() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bearer = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let aura = scenario
        .add_creature(P0, "Pacifism", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .id();
    let equipment = scenario
        .add_creature(P0, "Bonesplitter", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .id();
    let mut runner = scenario.build();
    engine::game::effects::attach::attach_to(runner.state_mut(), aura, bearer);
    engine::game::effects::attach::attach_to(runner.state_mut(), equipment, bearer);
    // Reach guards: both must actually be attached before the sweep runs, or
    // the "still attached" assertions below are vacuous.
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(bearer)),
        "precondition: the Aura is legally attached"
    );
    assert_eq!(
        runner.state().objects[&equipment].attached_to,
        Some(AttachTarget::Object(bearer)),
        "precondition: the Equipment is legally attached"
    );

    let mut events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);

    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(bearer)),
        "CR 704.5p sentence 2 explicitly excludes Auras"
    );
    assert_eq!(
        runner.state().objects[&equipment].attached_to,
        Some(AttachTarget::Object(bearer)),
        "CR 704.5p sentence 2 explicitly excludes Equipment"
    );
}

/// V13b — the §5.3 CR-triage table encoded as a test. These two engine models
/// legitimately produce an ATTACHED permanent printed as a creature whose
/// creature-ness is suppressed while attached. If the widened predicate reads
/// printed rather than layer-derived types, or reads them at the wrong time,
/// this row fails — and the fix is the predicate, never the test.
#[test]
fn reconfigure_and_bestow_attachments_survive_the_cr_704_5p_sweep() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host_a = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let host_b = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();

    // CR 702.151b: a reconfigured Equipment "stops being a creature until it
    // becomes unattached". Built through the REAL synthesis path
    // (`database::synthesis::synthesize_reconfigure` from the typed
    // `Keyword::Reconfigure`) rather than a hand-rolled static, so this row
    // exercises the shape production actually produces. A reconfigure Equipment
    // is an Artifact Creature — Equipment while unattached, which is exactly why
    // it is the interesting CR 704.5p case.
    let reconfigured = scenario
        .add_creature(P0, "Kitsune Ace", 2, 2)
        .with_subtypes(vec!["Equipment"])
        .id();

    // CR 702.103b: a bestowed permanent "becomes an Aura enchantment and gains
    // enchant creature" while attached — no Creature type, plus the Aura
    // subtype. Attached below through `GameRunner::attach_as_bestowed_aura`,
    // which routes through the engine's single bestow-form authority.
    let bestowed = scenario.add_creature(P0, "Boon Satyr", 4, 2).id();

    let mut runner = scenario.build();
    {
        let mut face = engine::types::card::CardFace::default();
        face.keywords
            .push(engine::types::keywords::Keyword::Reconfigure(
                ManaCost::Cost {
                    shards: vec![],
                    generic: 2,
                },
            ));
        engine::database::synthesis::synthesize_reconfigure(&mut face);
        let obj = runner.state_mut().objects.get_mut(&reconfigured).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.base_card_types = obj.card_types.clone();
        // Only the BASE list — the Step-1 layer reset rebuilds the live list
        // from it, so pushing to both would apply the static twice.
        obj.base_static_definitions = std::sync::Arc::new(face.static_abilities.clone());
    }
    engine::game::effects::attach::attach_to(runner.state_mut(), reconfigured, host_a);
    runner.attach_as_bestowed_aura(bestowed, host_b);
    runner.state_mut().layers_dirty.mark_full();
    engine::game::layers::flush_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&reconfigured].attached_to,
        Some(AttachTarget::Object(host_a)),
        "precondition: the reconfigured Equipment is attached before the sweep"
    );
    assert_eq!(
        runner.state().objects[&bestowed].attached_to,
        Some(AttachTarget::Object(host_b)),
        "precondition: the bestowed Aura is attached before the sweep"
    );
    // Reach guards: both must actually be non-creatures at SBA time, or the row
    // is testing a shape production never produces.
    assert!(
        !runner.state().objects[&reconfigured]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "CR 702.151b reach guard: the reconfigured Equipment must not be a \
         creature while attached: {:?}",
        runner.state().objects[&reconfigured].card_types
    );
    assert!(
        !runner.state().objects[&bestowed]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "CR 702.103b reach guard: the bestowed permanent must not be a creature \
         while attached: {:?}",
        runner.state().objects[&bestowed].card_types
    );

    let mut events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);

    assert_eq!(
        runner.state().objects[&reconfigured].attached_to,
        Some(AttachTarget::Object(host_a)),
        "CR 702.151b: reconfigure suppresses Creature while attached, so neither \
         CR 704.5p sentence fires"
    );
    assert_eq!(
        runner.state().objects[&bestowed].attached_to,
        Some(AttachTarget::Object(host_b)),
        "CR 702.103b: a bestowed permanent is an Aura and not a creature, so \
         both CR 704.5p sentences must miss"
    );
}

// ── V15 / V15b — parser coverage honesty across all THIRTEEN cards ───────────

/// V15 — the swallow detector must stay quiet on every card in the class. The
/// mandatory `PayCost` def used to be the ONLY AST evidence satisfying
/// `any_ability_is_optional`; removing it without the `end_cost` disjunct on
/// `effect_has_internal_optionality`'s `GenericEffect` arm turns all thirteen
/// yellow.
#[test]
fn no_licid_gains_a_parse_warning() {
    for (name, cost_code, oracle) in ALL_LICID_ORACLE {
        let parsed = parse_oracle_text(oracle, name, &[], &[], &["Licid".to_string()]);
        assert!(
            parsed.parse_warnings.is_empty(),
            "{name} must keep zero parse warnings — the `end_cost` field is the \
             AST evidence for the printed \"you may\": {:?}",
            parsed.parse_warnings
        );
        // Paired reach guard: proves the clause was actually ABSORBED rather
        // than the card simply failing to parse into anything detectable.
        let ability = parsed
            .abilities
            .first()
            .unwrap_or_else(|| panic!("{name} must parse to an activated ability"));
        assert_eq!(
            root_end_cost(ability),
            Some(mana_cost_of(cost_code)),
            "{name}: the termination cost must land on the animating GenericEffect"
        );
        assert!(
            !effects_of(ability)
                .iter()
                .any(|e| matches!(e, Effect::PayCost { .. })),
            "{name}: the mandatory resolution-time PayCost must be gone"
        );
    }
}

/// V15b — Flanking Licid is absorbed by shape, and its coverage status is
/// UNCHANGED. Its intervening clause is an `Unimplemented` "move", not an
/// `Attach`, which is precisely why the reach-back is gated on shape.
#[test]
fn flanking_licid_is_absorbed_and_its_unimplemented_move_clause_survives() {
    let parsed = parse_oracle_text(
        FLANKING_LICID_ORACLE,
        "Flanking Licid",
        &[],
        &[],
        &["Licid".to_string()],
    );
    let ability = parsed
        .abilities
        .first()
        .expect("Flanking Licid must parse to an activated ability");

    // (a) — asserted POSITIVELY first, so (b) below cannot pass vacuously via
    // an upstream parse failure.
    assert_eq!(
        root_end_cost(ability),
        Some(mana_cost_of("R")),
        "CR 116.2c: Flanking Licid is in the class and must be absorbed"
    );
    // (b)
    assert!(
        !effects_of(ability)
            .iter()
            .any(|e| matches!(e, Effect::PayCost { .. })),
        "the mandatory PayCost must be gone for Flanking Licid too"
    );
    // (c) — coverage honesty: this change must not silently "fix" or drop the
    // pre-existing unsupported move clause.
    assert!(
        effects_of(ability).iter().any(|e| matches!(
            e,
            Effect::Unimplemented { name, .. } if name == "move"
        )),
        "the move clause was unsupported before this change and must stay \
         unsupported after it — no Oracle text is newly accepted with deferred \
         semantics: {:?}",
        effects_of(ability)
    );
}

// ── N-rows — the fail-closed boundary and the binding guard ──────────────────

/// N1 — a non-mana termination cost is NOT absorbed (the extractor rejects it),
/// while the broad recognizer still accepts it so the peel blocklist keeps
/// working. This asymmetry is what keeps the change fail-closed.
/// N2 — adjacent grammar is rejected by both.
#[test]
fn only_mana_termination_costs_are_absorbed() {
    let life = "{W}, {T}: This creature loses this ability and becomes an Aura enchantment with enchant creature. Attach it to target creature. You may pay 2 life to end this effect.";
    let parsed = parse_oracle_text(life, "Hypothetical Licid", &[], &[], &["Licid".to_string()]);
    let ability = parsed
        .abilities
        .first()
        .expect("the synthetic card must still parse to an activated ability");
    assert!(
        matches!(ability.effect.as_ref(), Effect::GenericEffect { .. }),
        "reach guard: the animation must still lower to a GenericEffect root: {:?}",
        ability.effect
    );
    assert_eq!(
        root_end_cost(ability),
        None,
        "CR 116.2c: a cost the `SpecialAction` mana-pool contract cannot model \
         must NOT be absorbed — it falls through to its existing lowering rather \
         than being accepted with dropped semantics"
    );
    // The other half of the asymmetry — the BROAD recognizer must still accept
    // this same body so the `YouMayBlocklist::ChunkLoop` peel blocklist keeps
    // firing — is pinned by
    // `clause_shell::tests::broad_recognizer_still_accepts_a_body_the_narrow_extractor_rejects`.
    // It cannot live here: `is_you_may_pay_to_end_effect_phrase` is `pub(crate)`
    // and must stay that way.

    // N2 — adjacent grammar rejected by BOTH detectors.
    let wrong = "{W}, {T}: This creature loses this ability and becomes an Aura enchantment with enchant creature. Attach it to target creature. You may pay {W} to end this turn.";
    let parsed = parse_oracle_text(
        wrong,
        "Hypothetical Licid",
        &[],
        &[],
        &["Licid".to_string()],
    );
    let ability = parsed
        .abilities
        .first()
        .expect("the adjacent-grammar card must still parse to an activated ability");
    assert!(
        matches!(ability.effect.as_ref(), Effect::GenericEffect { .. }),
        "reach guard: the animation must still lower to a GenericEffect root: {:?}",
        ability.effect
    );
    assert_eq!(
        root_end_cost(ability),
        None,
        "\"to end this turn\" is not \"to end this effect\""
    );
}

/// N3 — with no continuous-effect antecedent in the chain, the clause stays an
/// honest orphaned residual rather than being bound to something arbitrary.
#[test]
fn termination_clause_with_no_installing_antecedent_is_not_absorbed() {
    let orphan = "{W}, {T}: Destroy target creature. You may pay {W} to end this effect.";
    let parsed = parse_oracle_text(orphan, "Hypothetical Card", &[], &[], &[]);
    let ability = parsed
        .abilities
        .first()
        .expect("the synthetic card must parse to an activated ability");
    assert_eq!(
        root_end_cost(ability),
        None,
        "the reach-back requires a clause that INSTALLS a continuous effect; a \
         Destroy is not one"
    );
    // Reach guard: the chain really did produce the Destroy, so the negative
    // above is about the reach-back and not about a total parse failure.
    assert!(
        effects_of(ability)
            .iter()
            .any(|e| matches!(e, Effect::Destroy { .. })),
        "reach guard: the synthetic chain must actually contain the Destroy: {:?}",
        effects_of(ability)
    );
}

/// N5 — the reach-back keys on SHAPE, not on `Attach`. A non-`Attach`
/// intervening clause is absorbed just the same. This is Flanking Licid's shape
/// as a building-block test, independent of any one card's data.
#[test]
fn a_non_attach_intervening_clause_still_reaches_back_to_the_animation() {
    let synthetic = "{W}, {T}: This creature loses this ability and becomes an Aura enchantment with enchant creature. Bamboozle the target creature. You may pay {W} to end this effect.";
    let parsed = parse_oracle_text(
        synthetic,
        "Hypothetical Licid",
        &[],
        &[],
        &["Licid".to_string()],
    );
    let ability = parsed
        .abilities
        .first()
        .expect("the synthetic card must parse to an activated ability");
    assert_eq!(
        root_end_cost(ability),
        Some(mana_cost_of("W")),
        "CR 116.2c: the reach-back scans PAST any non-absorbed intervening \
         clause for a continuous-effect-installing antecedent — it is not \
         keyed on `Attach`"
    );
    // Reach guard: the intervening clause is genuinely not an Attach, so this
    // row really does exercise the non-`Attach` path.
    assert!(
        !effects_of(ability)
            .iter()
            .any(|e| matches!(e, Effect::Attach { .. })),
        "reach guard: the intervening clause must NOT be an Attach: {:?}",
        effects_of(ability)
    );
}

// ── V16 / V16b — the auto-pass classifier PAIR ──────────────────────────────
//
// Two sibling classifiers in `ai_support` read the same action list, and this
// change treats them differently ON PURPOSE. The pair exists so a future
// maintainer cannot "fix" one direction without seeing the other fail.
//
//   * `flat_actions_have_meaningful_noncast_priority` gains an explicit
//     `EndContinuousEffect => false` arm, because a pay-to-end permission is a
//     STANDING one — it is legal for as long as the effect lives, so letting it
//     fall through would permanently disable own-upkeep/draw auto-pass.
//   * `flat_actions_have_meaningful_priority` deliberately keeps `_ => true`,
//     because CR 732.4 + CR 104.4b make an optional action a legitimate
//     loop-breaker and the loop-detection firewall reads that primitive. The
//     price is that the permission HOLDS priority at other windows — which is
//     exactly the shipped treatment of `TurnFaceUp` (CR 116.2b), the other
//     standing special action.

/// V16 — auto-pass still fires at the player's own upkeep.
#[test]
fn a_standing_pay_to_end_permission_still_auto_passes_at_own_upkeep() {
    // Dominating Licid on purpose: its activation costs {1}{U}{U} while its
    // termination costs only {U}. With exactly ONE untapped Island left, the
    // pay-to-end permission is affordable but re-activating the Licid is not —
    // so the Licid's own `ActivateAbility` cannot supply the hold and this row
    // is genuinely about the `EndContinuousEffect` arm.
    const DOMINATING_LICID_ORACLE: &str = "{1}{U}{U}, {T}: This creature loses this ability and becomes an Aura enchantment with enchant creature. Attach it to target creature. You may pay {U} to end this effect.\n\
You control enchanted creature.";

    let mut scenario = GameScenario::new();
    // Start directly in P0's OWN upkeep — the window the G2 rung guards.
    scenario.at_phase(Phase::Upkeep);
    scenario.with_mana_pool(P0, {
        let mut m = floating_mana(2, ManaType::Blue);
        m.extend(floating_mana(1, ManaType::Colorless));
        m
    });
    let licid = scenario
        .add_creature(P0, "Dominating Licid", 2, 2)
        .from_oracle_text(DOMINATING_LICID_ORACLE)
        .id();
    let victim = scenario.add_creature(P1, "Colossal Dreadmaw", 7, 7).id();
    scenario.add_land_from_oracle(P0, "Island", "{T}: Add {U}.");
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    scenario.with_library_top(P1, &["Forest", "Forest", "Forest"]);
    let mut runner = scenario.build();
    runner.activate(licid, 0).target_object(victim).resolve();

    assert_eq!(
        runner.state().phase,
        Phase::Upkeep,
        "fixture: this row is about the own-upkeep/draw auto-pass window"
    );
    assert_eq!(
        runner.state().priority_player,
        P0,
        "fixture: this row is about P0's OWN upkeep priority window"
    );

    let (actions, _, _) = legal_actions_for_viewer(runner.state(), P0);
    // Reach guard: the permission must actually be in the list, or this row
    // proves nothing about the classifier arm.
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, GameAction::EndContinuousEffect { .. })),
        "reach guard: the pay-to-end permission must be offered at own upkeep: \
         {actions:?}"
    );
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, GameAction::ActivateAbility { .. })),
        "fixture guard: the Licid's own activation must be unaffordable, or it \
         would supply the hold instead of the permission: {actions:?}"
    );
    assert!(
        engine::ai_support::auto_pass_recommended(runner.state(), &actions),
        "CR 116.2c: a STANDING permission is not a new opportunity arising this \
         turn, so it must not hold the initial upkeep/draw window open — the \
         explicit `EndContinuousEffect => false` arm on \
         `flat_actions_have_meaningful_noncast_priority`"
    );

    // Paired positive: a genuinely meaningful non-cast action in the same list
    // DOES hold the window, so the assertion above is about the new arm and not
    // about auto-pass being unconditionally true here.
    let mut with_meaningful = actions.clone();
    with_meaningful.push(GameAction::CompanionToHand);
    assert!(
        !engine::ai_support::auto_pass_recommended(runner.state(), &with_meaningful),
        "control: a meaningful non-cast action must still hold the upkeep window"
    );
}

/// V16b — the deliberate NON-change: at every other priority window the standing
/// permission HOLDS priority, matching `TurnFaceUp`.
#[test]
fn a_standing_pay_to_end_permission_holds_priority_at_the_opponents_end_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, floating_mana(1, ManaType::White));
    let licid = scenario
        .add_creature(P0, "Calming Licid", 2, 2)
        .from_oracle_text(CALMING_LICID_ORACLE)
        .id();
    let victim = scenario.add_creature(P1, "Colossal Dreadmaw", 7, 7).id();
    // Exactly ONE Plains. `has_feasibly_castable_spell` cannot supply the hold
    // (P0's hand is empty), and the Licid taps for its own activation so its
    // `ActivateAbility` is not re-offered — the permission is the only candidate
    // hold left.
    let plains = scenario
        .add_land_from_oracle(P0, "Plains", "{T}: Add {W}.")
        .id();
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    scenario.with_library_top(P1, &["Forest", "Forest", "Forest"]);
    let mut runner = scenario.build();
    runner.activate(licid, 0).target_object(victim).resolve();
    assert!(
        runner.state().players[P0.0 as usize].hand.is_empty(),
        "fixture: P0's hand must be empty so a castable spell cannot supply the hold"
    );

    // Cross into the opponent's turn and stop at their end step, then let the
    // active player pass so P0 is the one holding priority.
    runner.advance_to_end_step();
    runner.advance_to_upkeep();
    runner.advance_to_end_step();
    let _ = runner.act(GameAction::PassPriority);
    assert_eq!(
        runner.state().priority_player,
        P0,
        "fixture: this row is about P0 holding priority during the OPPONENT's \
         end step"
    );

    let (actions, _, _) = legal_actions_for_viewer(runner.state(), P0);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, GameAction::EndContinuousEffect { .. })),
        "reach guard: the permission must be offered here: {actions:?}"
    );
    assert!(
        !engine::ai_support::auto_pass_recommended(runner.state(), &actions),
        "CR 732.4 + CR 104.4b: `flat_actions_have_meaningful_priority` keeps \
         `_ => true` because the loop-detection firewall needs an optional \
         loop-breaker to count. The consequence — a live permission holds \
         priority outside own upkeep/draw — is deliberate and matches the \
         shipped `TurnFaceUp` (CR 116.2b) treatment"
    );

    // Paired control: with the Plains TAPPED the permission is unaffordable and
    // therefore not offered, so the same call recommends auto-pass. This proves
    // the hold above comes from the permission and nothing else on the board.
    runner.state_mut().objects.get_mut(&plains).unwrap().tapped = true;
    let (actions, _, _) = legal_actions_for_viewer(runner.state(), P0);
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, GameAction::EndContinuousEffect { .. })),
        "control: an unaffordable permission must not be offered: {actions:?}"
    );
    assert!(
        engine::ai_support::auto_pass_recommended(runner.state(), &actions),
        "control: with the permission gone, nothing else on this board holds \
         priority — so the hold above was the permission"
    );
}

// ── V10 — restricted mana is gated by the new SpecialAction variant ─────────

/// CR 116.1 + CR 106.6: paying a continuous effect's termination cost is neither
/// casting a spell nor activating an ability — a special action does not use the
/// stack. Mana restricted to casting spells must therefore be REJECTED here.
///
/// This is the correctness win `SpecialAction::EndContinuousEffect` buys over
/// charging the cost through an unlabelled payment context, so the row drives
/// the production offer and payment pipeline rather than testing the
/// restriction truth table in isolation.
#[test]
fn spell_restricted_mana_cannot_pay_a_termination_cost() {
    use engine::types::mana::ManaRestriction;

    let (mut runner, licid, _victim, _) = animated_calming_licid(0);
    runner.state_mut().players[P0.0 as usize]
        .mana_pool
        .add(ManaUnit::new(
            ManaType::White,
            ObjectId(700),
            false,
            vec![ManaRestriction::OnlyForSpell],
        ));
    assert!(
        offered_end_actions(&runner, P0).is_empty(),
        "CR 116.1 + CR 106.6: spell-only mana must not make the pay-to-end \
         special action affordable"
    );
    assert!(
        runner.state().objects[&licid].attached_to.is_some(),
        "the restricted mana must not end or detach the Licid"
    );

    let pool = &mut runner.state_mut().players[P0.0 as usize].mana_pool;
    pool.clear();
    pool.add(ManaUnit::new(ManaType::White, ObjectId(701), false, vec![]));
    let actions = offered_end_actions(&runner, P0);
    assert_eq!(
        actions.len(),
        1,
        "paired control: unrestricted {{W}} must make the action available"
    );
    runner
        .act(actions[0].clone())
        .expect("the production pay-to-end pipeline must accept unrestricted {W}");
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "the production special-action payment must consume the unrestricted {{W}}"
    );
    assert_eq!(
        runner.state().objects[&licid].attached_to,
        None,
        "the successful special-action payment must end the effect and detach the Licid"
    );
}
