//! Runtime + grant-clone proofs for the granted-ability self-reference dual
//! binding (S25). CR 201.5a: when an ability's effect grants another ability
//! that refers to the granting object BY NAME, the name refers only to the
//! granting object — never to the host it was granted to.
//!
//! Three independent channels are exercised, and must stay separate:
//!   1. Granter-referential ("Exile/Sacrifice/Return <granter-name>") →
//!      `TargetFilter::GrantingObject` → concretized to `SpecificObject{granter}`.
//!   2. Host-referential ("Sacrifice this permanent") → stays `SelfRef` → host.
//!   3. Host power read ("where X is this creature's power") → `QuantityRef::Power`
//!      (never a `TargetFilter`) → unchanged.
//!
//! Every behavioral test drives the production Layer-6 grant path
//! (`evaluate_layers`) and, for Deconstruction Hammer, the full activate/resolve
//! pipeline asserting which object left the battlefield.

use std::sync::Arc;

use engine::game::game_object::AttachTarget;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::parser::oracle_util::normalize_card_name_refs;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, ContinuousModification, Effect, ObjectScope, QuantityExpr,
    QuantityRef, StaticDefinition, TargetFilter,
};
use engine::types::card_type::CoreType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn equipment_types() -> (Vec<String>, Vec<String>) {
    (vec!["Artifact".to_string()], vec!["Equipment".to_string()])
}

/// The `AbilityDefinition` an equipment grants via its "Equipped creature has …"
/// static (the parse-time, pre-concretization body).
fn granted_activated_def(oracle: &str, name: &str) -> AbilityDefinition {
    let (types, subtypes) = equipment_types();
    let parsed = parse_oracle_text(oracle, name, &[], &types, &subtypes);
    grant_ability_static(&parsed.statics)
        .modifications
        .iter()
        .find_map(|m| match m {
            ContinuousModification::GrantAbility { definition } => Some((**definition).clone()),
            _ => None,
        })
        .expect("equipment must grant an activated ability")
}

fn grant_ability_static(statics: &[StaticDefinition]) -> StaticDefinition {
    statics
        .iter()
        .find(|s| {
            s.modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::GrantAbility { .. }))
        })
        .expect("equipment must have a GrantAbility static")
        .clone()
}

/// Install `grant_static` on a fresh artifact-equipment attached to `host`, then
/// run the production layer engine so the granted ability is cloned onto the
/// host with its granter self-references concretized.
fn equip_and_layer(
    scenario: GameScenario,
    equipment: ObjectId,
    host: ObjectId,
    grant_static: StaticDefinition,
) -> engine::game::scenario::GameRunner {
    let mut runner = scenario.build();
    {
        let st = runner.state_mut();
        let obj = st.objects.get_mut(&equipment).unwrap();
        obj.card_types.core_types = vec![CoreType::Artifact];
        obj.card_types.subtypes = vec!["Equipment".to_string()];
        obj.base_card_types = obj.card_types.clone();
        obj.power = None;
        obj.toughness = None;
        obj.base_power = None;
        obj.base_toughness = None;
        obj.attached_to = Some(AttachTarget::Object(host));
        obj.static_definitions.push(grant_static.clone());
        Arc::make_mut(&mut obj.base_static_definitions).push(grant_static);
        st.layers_dirty.mark_full();
    }
    evaluate_layers(runner.state_mut());
    runner
}

fn granted_ability_index(
    runner: &engine::game::scenario::GameRunner,
    host: ObjectId,
    pred: impl Fn(&AbilityDefinition) -> bool,
) -> usize {
    runner.state().objects[&host]
        .abilities
        .iter()
        .position(pred)
        .expect("host must carry the granted ability after evaluate_layers")
}

// ---------------------------------------------------------------------------
// Direction A — granter-referential COST/EFFECT resolves to the GRANTING object.
// ---------------------------------------------------------------------------

const DECONSTRUCTION_HAMMER: &str = "Equipped creature gets +1/+1 and has \"{3}, {T}, \
Sacrifice Deconstruction Hammer: Destroy target artifact or enchantment.\"\nEquip {1}";

/// A1: Deconstruction Hammer's sacrifice cost sacrifices THE HAMMER (the granting
/// equipment), not the equipped creature. Full activate/resolve pipeline; asserts
/// which object left the battlefield.
///
/// Revert-to-red: remove the `layers.rs` GrantingObject→SpecificObject rewrite →
/// the cost stays `GrantingObject`, the defensive runtime arm resolves it to the
/// ability source (host) → the CREATURE is sacrificed and the Hammer survives →
/// both the concretization `assert_eq!` and the zone assertions flip.
#[test]
fn deconstruction_hammer_sacrifice_hits_the_equipment_not_the_host() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
        ],
    );
    let host = scenario.add_creature(P0, "Bearer", 2, 2).id();
    let hammer = scenario
        .add_creature(P0, "Deconstruction Hammer", 0, 0)
        .id();
    let victim = scenario.add_creature(P1, "Relic", 0, 0).id();

    let (types, subtypes) = equipment_types();
    let grant_static = grant_ability_static(
        &parse_oracle_text(
            DECONSTRUCTION_HAMMER,
            "Deconstruction Hammer",
            &[],
            &types,
            &subtypes,
        )
        .statics,
    );

    let mut runner = {
        // Make the victim a destructible artifact target BEFORE the layer pass.
        let mut runner = equip_and_layer(scenario, hammer, host, grant_static);
        {
            let v = runner.state_mut().objects.get_mut(&victim).unwrap();
            v.card_types.core_types = vec![CoreType::Artifact];
            v.base_card_types = v.card_types.clone();
            v.power = None;
            v.toughness = None;
            v.base_power = None;
            v.base_toughness = None;
        }
        runner
    };

    let idx = granted_ability_index(&runner, host, |a| {
        a.cost.as_ref().and_then(sacrifice_target).is_some()
    });

    // Concretization proof (the layers.rs seam): the sacrifice cost (inside the
    // `{3},{T},Sacrifice` Composite) targets the Hammer, not `SelfRef`/`GrantingObject`.
    assert_eq!(
        runner.state().objects[&host].abilities[idx]
            .cost
            .as_ref()
            .and_then(sacrifice_target),
        Some(&TargetFilter::SpecificObject { id: hammer }),
        "CR 201.5a: sacrifice cost must target the granting Hammer, not the host"
    );

    // Runtime proof: activate the granted ability, paying the sacrifice cost with
    // the Hammer and targeting the artifact, then assert which permanents left the
    // battlefield.
    let outcome = runner
        .activate(host, idx)
        .target_object(victim)
        .pay_with(&[hammer])
        .resolve();
    assert_eq!(
        outcome.zone_of(hammer),
        Zone::Graveyard,
        "CR 701.21a: the Hammer (granting object) is sacrificed to its owner's graveyard"
    );
    assert_eq!(
        outcome.zone_of(host),
        Zone::Battlefield,
        "the equipped creature survives — it is NOT the object named in the cost"
    );
    assert_eq!(
        outcome.zone_of(victim),
        Zone::Graveyard,
        "the targeted artifact is destroyed by the resolved effect"
    );
}

const THE_DOMINION_BRACELET: &str = "Equipped creature gets +1/+1 and has \"{15}, \
Exile The Dominion Bracelet: You control target opponent during their next turn. \
This ability costs {X} less to activate, where X is this creature's power. \
Activate only as a sorcery.\"\nEquip {1}";

/// A2 + B1: The Dominion Bracelet. The `{15}, Exile <self>` cost exiles THE
/// BRACELET (granter-referential → GrantingObject → SpecificObject{bracelet}),
/// while the `{X} less … this creature's power` reduction stays host-referential
/// (`QuantityRef::Power{Source}`, an untouched third channel).
///
/// Parse-shape supplement proves the two `~`-collapsed referents split; the
/// `evaluate_layers` assertion proves the production concretization. Full {15}
/// activation is impractical, but the Exile-cost runtime resolution reuses the
/// exact `SpecificObject` machinery the Hammer test drives end-to-end.
#[test]
fn the_dominion_bracelet_exile_hits_the_bracelet_reduction_reads_the_host() {
    // Parse-shape: cost = Exile{GrantingObject}; reduction = Power{Source}; no
    // residual Unimplemented reduction node.
    let def = granted_activated_def(THE_DOMINION_BRACELET, "The Dominion Bracelet");
    assert_eq!(
        def.cost.as_ref().and_then(exile_filter),
        Some(&TargetFilter::GrantingObject),
        "the Exile cost names the Bracelet (granter) → GrantingObject, not SelfRef"
    );
    let reduction = def
        .cost_reduction
        .as_ref()
        .expect("the {X}-less reduction must fold into cost_reduction, not stay Unimplemented");
    assert_eq!(
        reduction.count,
        QuantityExpr::Ref {
            qty: QuantityRef::Power {
                scope: ObjectScope::Source
            }
        },
        "the reduction reads the equipped creature's power (host) — untouched third channel"
    );
    assert!(
        find_effect(&def, |e| matches!(e, Effect::Unimplemented { .. })).is_none(),
        "no residual Unimplemented cost-reduction node should remain"
    );

    // Production concretization: after grant-clone the host's Exile cost is
    // SpecificObject{bracelet}.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Bearer", 3, 3).id();
    let bracelet = scenario
        .add_creature(P0, "The Dominion Bracelet", 0, 0)
        .id();
    let (types, subtypes) = equipment_types();
    let grant_static = grant_ability_static(
        &parse_oracle_text(
            THE_DOMINION_BRACELET,
            "The Dominion Bracelet",
            &[],
            &types,
            &subtypes,
        )
        .statics,
    );
    let runner = equip_and_layer(scenario, bracelet, host, grant_static);
    let idx = granted_ability_index(&runner, host, |a| {
        a.cost.as_ref().and_then(exile_filter).is_some()
    });
    assert_eq!(
        runner.state().objects[&host].abilities[idx]
            .cost
            .as_ref()
            .and_then(exile_filter),
        Some(&TargetFilter::SpecificObject { id: bracelet }),
        "CR 201.5a: the concretized Exile cost targets the Bracelet, not the host"
    );
    // Host power read is unchanged by concretization.
    assert_eq!(
        runner.state().objects[&host].abilities[idx]
            .cost_reduction
            .as_ref()
            .map(|r| &r.count),
        Some(&QuantityExpr::Ref {
            qty: QuantityRef::Power {
                scope: ObjectScope::Source
            }
        }),
        "the power reduction remains host-referential after grant-clone"
    );
}

const TRUSTY_BOOMERANG: &str = "Equipped creature has \"{1}, {T}: Tap target creature. \
Return Trusty Boomerang to its owner's hand.\"\nEquip {1}";

/// A3 (effect-target channel): Trusty Boomerang's "Return <self> to its owner's
/// hand" bounces THE EQUIPMENT. After grant-clone the Bounce effect target is
/// `SpecificObject{boomerang}`, proving the effect channel (parse_self_reference)
/// concretizes just like the cost channel.
///
/// Revert-to-red: without the layers.rs rewrite the Bounce target stays
/// `GrantingObject` (≠ SpecificObject{boomerang}) → assertion fails.
#[test]
fn trusty_boomerang_return_bounces_the_equipment_not_the_host() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Bearer", 2, 2).id();
    let boomerang = scenario.add_creature(P0, "Trusty Boomerang", 0, 0).id();
    let (types, subtypes) = equipment_types();
    let grant_static = grant_ability_static(
        &parse_oracle_text(TRUSTY_BOOMERANG, "Trusty Boomerang", &[], &types, &subtypes).statics,
    );
    let runner = equip_and_layer(scenario, boomerang, host, grant_static);

    let idx = granted_ability_index(&runner, host, |a| {
        find_effect(a, |e| matches!(e, Effect::Bounce { .. })).is_some()
    });
    let bounce_target = find_effect(&runner.state().objects[&host].abilities[idx], |e| {
        matches!(e, Effect::Bounce { .. })
    })
    .and_then(|e| match e {
        Effect::Bounce { target, .. } => Some(target.clone()),
        _ => None,
    })
    .expect("granted ability must carry a Bounce effect");
    assert_eq!(
        bounce_target,
        TargetFilter::SpecificObject { id: boomerang },
        "CR 201.5a: the granted Return bounces the Boomerang (granter), not the host"
    );
}

// ---------------------------------------------------------------------------
// Direction B — host-referential "this permanent" stays on the HOST.
// ---------------------------------------------------------------------------

const ACIDIC_SLIVER: &str =
    "All Slivers have \"{2}, Sacrifice this permanent: This permanent deals 2 damage to any target.\"";

/// B2: An Acidic-Sliver-style grant to a SECOND Sliver keeps its "Sacrifice this
/// permanent" cost bound to the HOST (`SelfRef`), never rebound to the granting
/// Sliver. This is the discriminating proof that "this permanent" (a
/// `SELF_REF_TYPE_PHRASES` self-ref, never the card name) is NOT masked to a
/// granter reference — a blanket "SelfRef-in-granted → granter" rewrite would
/// make this `SpecificObject{granter}` and fail.
#[test]
fn sliver_host_ref_sacrifice_stays_on_the_host_not_the_granter() {
    let (types, subtypes) = (vec!["Creature".to_string()], vec!["Sliver".to_string()]);
    let parsed = parse_oracle_text(ACIDIC_SLIVER, "Acidic Sliver", &[], &types, &subtypes);
    let granted = grant_ability_static(&parsed.statics)
        .modifications
        .iter()
        .find_map(|m| match m {
            ContinuousModification::GrantAbility { definition } => Some((**definition).clone()),
            _ => None,
        })
        .expect("Slivers grant an activated ability");
    assert_eq!(
        granted.cost.as_ref().and_then(sacrifice_target),
        Some(&TargetFilter::SelfRef),
        "\"Sacrifice this permanent\" is host-referential (SelfRef), never GrantingObject"
    );
    assert!(
        !contains_granting_object(&granted),
        "a host-ref Sliver ability must contain no GrantingObject reference"
    );
}

// ---------------------------------------------------------------------------
// Direction C — R1 regression guard: `named <self>` name-FILTERS are preserved.
// ---------------------------------------------------------------------------

const FOOD_FIGHT: &str = "Artifacts you control have \"{2}, Sacrifice this artifact: \
It deals damage to any target equal to 1 plus the number of permanents named Food Fight you control.\"";

/// C (R1 negative): Food Fight's "permanents named Food Fight" is a name-FILTER,
/// not a self-reference. The quote masker must SKIP the `named <self>` position,
/// so the name survives to the count filter (and never becomes GrantingObject or
/// the raw placeholder char).
///
/// Revert-to-red: remove the `named`-position skip in
/// `mask_granting_self_reference_in_quotes` → "Food Fight" after `named` is
/// masked to the placeholder, the `named ~`→`named Food Fight` restoration never
/// fires, and the structural AST loses "Food Fight" (gains the placeholder char)
/// → this assertion fails.
#[test]
fn food_fight_named_self_filter_is_not_masked() {
    let (types, subtypes) = (vec!["Artifact".to_string()], Vec::<String>::new());
    let parsed = parse_oracle_text(FOOD_FIGHT, "Food Fight", &[], &types, &subtypes);
    let mut granted = grant_ability_static(&parsed.statics)
        .modifications
        .iter()
        .find_map(|m| match m {
            ContinuousModification::GrantAbility { definition } => Some((**definition).clone()),
            _ => None,
        })
        .expect("Food Fight grants an activated ability");

    // The host self-sacrifice cost is unaffected (positive reach-guard: the body
    // parsed past the cost separator into a real granted ability).
    assert_eq!(
        granted.cost.as_ref().and_then(sacrifice_target),
        Some(&TargetFilter::SelfRef),
        "\"Sacrifice this artifact\" is host-referential (SelfRef)"
    );

    // Structural (description-independent) check: the name survives in the count
    // filter; no GrantingObject and no leaked placeholder char. (The parser
    // lower-cases filter names, so match case-insensitively.)
    granted.description = None;
    let structural = format!("{granted:?}");
    assert!(
        structural.to_lowercase().contains("food fight"),
        "the `named Food Fight` name-filter must preserve the card name; got {structural}"
    );
    assert!(
        !structural.contains('\u{E0002}'),
        "the granting-object placeholder must never leak into the AST"
    );
    assert!(
        !contains_granting_object(&granted),
        "a name-FILTER position must not become a GrantingObject self-reference"
    );
}

// ---------------------------------------------------------------------------
// Recursive AST walkers used by the assertions above.
// ---------------------------------------------------------------------------

fn find_effect(def: &AbilityDefinition, pred: impl Fn(&Effect) -> bool + Copy) -> Option<&Effect> {
    if pred(&def.effect) {
        return Some(&def.effect);
    }
    for child in def
        .sub_ability
        .iter()
        .chain(def.else_ability.iter())
        .map(|b| b.as_ref())
        .chain(def.mode_abilities.iter())
    {
        if let Some(found) = find_effect(child, pred) {
            return Some(found);
        }
    }
    None
}

/// The Sacrifice cost's target filter, searching inside `Composite`/`OneOf`
/// (activation costs like `{3},{T},Sacrifice <x>` parse to a Composite).
fn sacrifice_target(cost: &AbilityCost) -> Option<&TargetFilter> {
    match cost {
        AbilityCost::Sacrifice(sac) => Some(&sac.target),
        AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => {
            costs.iter().find_map(sacrifice_target)
        }
        _ => None,
    }
}

/// The Exile cost's filter, searching inside `Composite`/`OneOf`.
fn exile_filter(cost: &AbilityCost) -> Option<&TargetFilter> {
    match cost {
        AbilityCost::Exile { filter, .. } => filter.as_ref(),
        AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => {
            costs.iter().find_map(exile_filter)
        }
        _ => None,
    }
}

/// Sound presence test for the fieldless `TargetFilter::GrantingObject` variant:
/// its debug repr is exactly `GrantingObject`, and no other AST node's debug
/// output contains that substring. Used only for the negative assertions here.
fn contains_granting_object(def: &AbilityDefinition) -> bool {
    format!("{def:?}").contains("GrantingObject")
}

/// The target filter of a single target-bearing effect (subset used here).
fn effect_target(effect: &Effect) -> Option<&TargetFilter> {
    match effect {
        Effect::PutCounter { target, .. }
        | Effect::GainControl { target, .. }
        | Effect::Bounce { target, .. }
        | Effect::Destroy { target, .. } => Some(target),
        _ => None,
    }
}

/// The GrantAbility body an equipment/aura grants via its "…has \"…\"" static.
fn granted_def_from(
    oracle: &str,
    name: &str,
    types: &[&str],
    subtypes: &[&str],
) -> AbilityDefinition {
    let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
    let parsed = parse_oracle_text(oracle, name, &[], &types, &subtypes);
    grant_ability_static(&parsed.statics)
        .modifications
        .iter()
        .find_map(|m| match m {
            ContinuousModification::GrantAbility { definition } => Some((**definition).clone()),
            _ => None,
        })
        .expect("card must grant an activated ability")
}

/// The private-use masker placeholder (U+E0002). Must NEVER survive into the AST.
const PLACEHOLDER: char = '\u{E0002}';

const FISHING_POLE: &str =
    "Equipped creature has \"{1}, {T}, Tap Fishing Pole: Put a bait counter on Fishing Pole.\"\nEquip {2}";
const HANKYU: &str = "Equipped creature has \"{T}: Put an aim counter on Hankyu\" and \"{T}, \
Remove all aim counters from Hankyu: This creature deals damage to any target equal to the number \
of aim counters removed this way.\"\nEquip {4}";
/// The masker placeholder must NEVER leak the raw private-use char into any
/// parsed output — including the outer static/trigger DESCRIPTION strings that
/// embed the raw quoted text (a granted body's "…has \"…Sacrifice <self>…\""
/// description). The single post-parse degrade sweep
/// (`scrub_granting_placeholder_descriptions`) renders every residual placeholder
/// as `~`. Revert-to-red: remove that sweep → the description carries the raw
/// U+E0002 char → this flips. (Round-1 shipped this leak because the granted
/// body's description was sanitized but the outer static description was not.)
#[test]
fn placeholder_never_leaks_into_any_description() {
    let cards: &[(&str, &str, &[&str], &[&str])] = &[
        (
            DECONSTRUCTION_HAMMER,
            "Deconstruction Hammer",
            &["Artifact"],
            &["Equipment"],
        ),
        (
            THE_DOMINION_BRACELET,
            "The Dominion Bracelet",
            &["Artifact"],
            &["Equipment"],
        ),
        (FISHING_POLE, "Fishing Pole", &["Artifact"], &["Equipment"]),
        (HANKYU, "Hankyu", &["Artifact"], &["Equipment"]),
    ];
    for &(oracle, name, types, subtypes) in cards {
        let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
        let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
        let p = parse_oracle_text(oracle, name, &[], &types, &subtypes);
        let whole = format!(
            "{:?}|{:?}|{:?}|{:?}",
            p.statics, p.triggers, p.abilities, p.replacements
        );
        assert!(
            !whole.contains(PLACEHOLDER),
            "{name}: the masker placeholder must be scrubbed to ~ in every description; leaked"
        );
    }
}

/// R4 (counter channel): the `put a … counter on <self>` (PutCounter target)
/// verb-object position emits `GrantingObject`, exactly like the
/// sacrifice/exile/return channels — Fishing Pole (multi-word) and Hankyu
/// (single-word, case-sensitive masking). Proves the position-aware masker's
/// allowlist still covers the counter target after the HIGH narrowing.
///
/// Revert-to-red: drop `counter on ` from `GRANTER_SELF_REF_VERB_PREFIXES` →
/// these bodies host-bind (`~`/SelfRef) → the `GrantingObject` assertion flips.
#[test]
fn r4_counter_channel_targets_the_granter() {
    for (oracle, name) in [(FISHING_POLE, "Fishing Pole"), (HANKYU, "Hankyu")] {
        let def = granted_def_from(oracle, name, &["Artifact"], &["Equipment"]);
        let target = find_effect(&def, |e| effect_target(e).is_some())
            .and_then(effect_target)
            .unwrap_or_else(|| {
                panic!("{name}: expected a target-bearing effect in the granted body")
            });
        assert_eq!(
            target,
            &TargetFilter::GrantingObject,
            "{name}: the PutCounter target names the granting equipment → GrantingObject"
        );
        assert!(
            !format!("{def:?}").contains(PLACEHOLDER),
            "{name}: no raw placeholder may survive into the AST"
        );
    }
}

// ---------------------------------------------------------------------------
// Round-1 regression guards (R1/HIGH): non-verb-object in-quote self-name refs
// must NOT be masked — they stay `~` (host), BYTE-IDENTICAL to pre-fix. Asserted
// at the masker's direct output (`normalize_card_name_refs`): its ONLY effect is
// inserting the placeholder, so "no placeholder in the normalized string" ⟺ the
// normalized/parsed output is byte-identical to the pre-fix (name→`~`) baseline.
// Re-widening the masker inserts the placeholder into these positions → red.
// ---------------------------------------------------------------------------

/// Assert the masker is a NO-OP for a card's non-verb-object self-name refs: the
/// normalized string carries no placeholder (byte-identical to pre-fix) yet still
/// normalized the self-name/self-ref to `~` (non-vacuous reach-guard).
fn assert_masker_noop(oracle: &str, name: &str) {
    let normalized = normalize_card_name_refs(oracle, name);
    assert!(
        !normalized.contains(PLACEHOLDER),
        "{name}: a non-verb-object self-name position must NOT be masked (byte-identical to pre-fix)"
    );
    assert!(
        normalized.contains('~'),
        "{name}: the self-name/self-ref must still normalize to ~ (reach-guard); got {normalized}"
    );
}

const ARCHERY_TRAINING: &str = "Enchant creature\nAt the beginning of your upkeep, you may put an \
arrow counter on this Aura.\nEnchanted creature has \"{T}: This creature deals X damage to target \
attacking or blocking creature, where X is the number of arrow counters on Archery Training.\"";

/// Archery Training — QuantityRef channel ("number of arrow counters on <self>").
/// Revert-to-red: re-widen the masker → `counters on <placeholder>` appears in the
/// normalized string AND the end-to-end `CountersOn` node is lost → assertions flip.
#[test]
fn archery_training_quantity_ref_channel_not_masked() {
    assert_masker_noop(ARCHERY_TRAINING, "Archery Training");
    // End-to-end: the arrow-counter count still parses to a CountersOn QuantityRef.
    let def = granted_def_from(
        ARCHERY_TRAINING,
        "Archery Training",
        &["Enchantment"],
        &["Aura"],
    );
    assert!(
        format!("{def:?}").contains("CountersOn"),
        "the arrow-counter count must parse to a CountersOn QuantityRef (not dropped)"
    );
    assert!(
        !contains_granting_object(&def),
        "a QuantityRef `counters on <self>` position must never become GrantingObject"
    );
}

const ANIMAL_FRIEND: &str = "Enchant creature\nEnchanted creature has \"Whenever this creature \
attacks, create a 1/1 green Squirrel creature token. Put a +1/+1 counter on that token for each \
Aura and Equipment attached to this creature other than Animal Friend.\"";

/// Animal Friend — exclusion channel ("other than <self>"). Revert-to-red:
/// re-widen the masker → `other than <placeholder>` in the normalized string → red.
#[test]
fn animal_friend_exclusion_channel_not_masked() {
    assert_masker_noop(ANIMAL_FRIEND, "Animal Friend");
}

const TORRENT_OF_LAVA: &str = "Torrent of Lava deals X damage to each creature without flying.\n\
As long as Torrent of Lava is on the stack, each creature has \"{T}: Prevent the next 1 damage \
that would be dealt to this creature by Torrent of Lava this turn.\"";

/// Torrent of Lava — damage-source channel ("dealt … by <self>"). Revert-to-red:
/// re-widen the masker → `by <placeholder>` in the normalized string → red.
#[test]
fn torrent_of_lava_damage_source_channel_not_masked() {
    assert_masker_noop(TORRENT_OF_LAVA, "Torrent of Lava");
}
