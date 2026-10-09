//! Runtime + grant-clone proofs for the granted-ability self-reference dual
//! binding (S25). CR 201.5a: when an ability's effect grants another ability
//! that refers to the granting object BY NAME, the name refers only to the
//! granting object — never to the host it was granted to.
//!
//! A granted body's typed AST and its display `description` must name the same
//! object; otherwise the UI would say "sacrifice the Equipment" while the engine
//! sacrificed the creature.

use std::sync::Arc;

use engine::game::game_object::AttachTarget;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::parser::oracle_util::{normalize_card_name_refs, normalize_card_name_refs_reporting};
use engine::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, Comparator, ContinuousModification, Effect,
    ObjectScope, QuantityExpr, QuantityRef, StaticDefinition, TargetFilter,
};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::TargetRef;

fn equipment_types() -> (Vec<String>, Vec<String>) {
    (vec!["Artifact".to_string()], vec!["Equipment".to_string()])
}

/// The `AbilityDefinition` an equipment grants via its "Equipped creature has …"
/// static (the parse-time body).
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
/// host stamped with its granter.
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

/// A1: Deconstruction Hammer's sacrifice cost sacrifices THE HAMMER (the granting
/// equipment), not the equipped creature. Full activate/resolve pipeline; asserts
/// which object left the battlefield.
///
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

    // CR 201.5a: the sacrifice cost names the granter, and the grant is stamped with the Hammer.
    let granted = &runner.state().objects[&host].abilities[idx];
    assert_eq!(
        granted.cost.as_ref().and_then(sacrifice_target),
        Some(&TargetFilter::GrantingObject {
            bound: Some(ObjectIncarnationRef::from_object(
                &runner.state().objects[&hammer]
            ))
        })
    );
    assert_eq!(
        granted.granting_object,
        Some(ObjectIncarnationRef::from_object(
            &runner.state().objects[&hammer]
        ))
    );

    // DISPLAY half of the same seam (matrix rows 1 and 3). This MUST run before
    // the activate below: the Hammer is sacrificed, the grant ends, and
    // `objects[&host].abilities` is empty afterwards (measured: index out of
    // bounds, len 0).
    let desc = runner.state().objects[&host].abilities[idx]
        .description
        .clone()
        .expect("the granted ability carries a display description");
    assert_eq!(
        desc, "{3}, {T}, Sacrifice Deconstruction Hammer: Destroy target artifact or enchantment.",
        "CR 201.5a: the granted body's description must name the GRANTING Hammer, \
         not collapse to the host token `~`"
    );
    // CLIENT PARITY, weaker form. `renderDescription(desc, object.name)` on the
    // host must not put the host's name anywhere in this body. This card's
    // effect half carries no `~`, so this proves only "the host name appears
    // NOWHERE"; the discriminating both-halves fixture is
    // `game::effects::token::tests::catalog_toggo_rock_sacrifice_cost_binds_to_rock_not_host`
    // (Rock's printed body carries a CR 201.5a granter reference in the cost AND
    // a CR 201.5b host `~` in the effect).
    let rendered = desc.replace('~', "Bearer");
    assert!(
        rendered.starts_with("{3}, {T}, Sacrifice Deconstruction Hammer:"),
        "CR 201.5a: a blanket `~`-replace would render `Sacrifice Bearer:`; got {rendered}"
    );
    assert_eq!(
        rendered.matches("Bearer").count(),
        0,
        "the host's name must not appear anywhere in this granted body; got {rendered}"
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

/// A2 + B1: The Dominion Bracelet's `Exile <self>` cost names the granter while
/// its `{X} less … this creature's power` reduction stays host-referential.
#[test]
fn the_dominion_bracelet_exile_hits_the_bracelet_reduction_reads_the_host() {
    // Parse-shape: cost = Exile{GrantingObject}; reduction = Power{Source}; no
    // residual Unimplemented reduction node.
    let def = granted_activated_def(THE_DOMINION_BRACELET, "The Dominion Bracelet");
    assert_eq!(
        def.cost.as_ref().and_then(exile_filter),
        Some(&TargetFilter::GrantingObject { bound: None }),
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
}

/// A3 (effect-target channel): Trusty Boomerang's "Return <self> to its owner's
/// hand" names the granter, and the grant is stamped with the Boomerang.
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
        TargetFilter::GrantingObject {
            bound: Some(ObjectIncarnationRef::from_object(
                &runner.state().objects[&boomerang]
            ))
        }
    );
    assert_eq!(
        runner.state().objects[&host].abilities[idx].granting_object,
        Some(ObjectIncarnationRef::from_object(
            &runner.state().objects[&boomerang]
        ))
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

    // DISPLAY half. CR 201.5b: a host reference stays the host token `~` and must
    // NOT gain the granting card's name — the render is sentinel-driven, not a
    // blanket name substitution. Reach-guard: the `SelfRef` assertion above proves
    // this body really is the host-referential shape.
    let desc = granted
        .description
        .as_deref()
        .expect("the granted Sliver ability carries a display description");
    assert!(
        desc.contains('~'),
        "CR 201.5b: the host reference must stay `~`; got {desc}"
    );
    assert!(
        !desc.contains("Acidic Sliver"),
        "CR 201.5b: a host reference must never render as the GRANTER's name; got {desc}"
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
    let json = serde_json::to_string(&granted).expect("the granted definition serializes");
    assert!(
        !json.contains(PLACEHOLDER),
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

/// The private-use masker placeholder (U+E0004). Must NEVER survive into the AST.
const PLACEHOLDER: char = '\u{E0004}';

// ---------------------------------------------------------------------------
// CR 201.5a class corpus: exported cards whose quoted granted body names the
// card itself in a `GRANTER_SELF_REF_VERB_PREFIXES` position and whose parse
// carries a granter symbol. Every text is the verbatim Oracle text, reminder text
// and all, because a paraphrase can take a different parser branch.
//
// The predefined token Rock reaches the parser through
// `game::effects::token::catalog_rules_text_abilities`; its arm of this corpus
// property lives in
// `game::effects::token::tests::catalog_rules_text_abilities_never_leaks_the_placeholder`.
// ---------------------------------------------------------------------------

const BLAZING_TORCH: &str =
    "Equipped creature can't be blocked by Vampires or Zombies.\nEquipped creature has \"{T}, Sacrifice Blazing Torch: Blazing Torch deals 2 damage to any target.\"\nEquip {1} ({1}: Attach to target creature you control. Equip only as a sorcery.)";
const CITIZENS_CROWBAR: &str =
    "When this Equipment enters, create a 1/1 green and white Citizen creature token, then attach this Equipment to it.\nEquipped creature gets +1/+1 and has \"{W}, {T}, Sacrifice Citizen's Crowbar: Destroy target artifact or enchantment.\"\nEquip {2} ({2}: Attach to target creature you control. Equip only as a sorcery.)";
const DECONSTRUCTION_HAMMER: &str =
    "Equipped creature gets +1/+1 and has \"{3}, {T}, Sacrifice Deconstruction Hammer: Destroy target artifact or enchantment.\"\nEquip {1} ({1}: Attach to target creature you control. Equip only as a sorcery.)";
const FISHING_POLE: &str =
    "Equipped creature has \"{1}, {T}, Tap Fishing Pole: Put a bait counter on Fishing Pole.\"\nWhenever equipped creature becomes untapped, remove a bait counter from this Equipment. If you do, create a 1/1 blue Fish creature token.\nEquip {2} ({2}: Attach to target creature you control. Equip only as a sorcery.)";
const KROVIKAN_PLAGUE: &str =
    "Enchant non-Wall creature you control\nWhen this Aura enters, draw a card at the beginning of the next turn's upkeep.\nTap enchanted creature: This Aura deals 1 damage to any target. Put a -0/-1 counter on enchanted creature. Activate only if enchanted creature is untapped.";
const HANKYU: &str =
    "Equipped creature has \"{T}: Put an aim counter on Hankyu\" and \"{T}, Remove all aim counters from Hankyu: This creature deals damage to any target equal to the number of aim counters removed this way.\"\nEquip {4} ({4}: Attach to target creature you control. Equip only as a sorcery.)";
const MEANDERED_TOWERSHELL: &str =
    "Enchant creature\nEnchanted creature has islandwalk and \"Whenever this creature attacks, exile it and Meandered Towershell. Return it to the battlefield under your control tapped and attacking at the beginning of the declare attackers step on your next turn, then return Meandered Towershell to the battlefield under its owner's control attached to that creature.\"";
const NINJAS_KUNAI: &str =
    "Equipped creature has \"{1}, {T}, Sacrifice Ninja's Kunai: Ninja's Kunai deals 3 damage to any target.\"\nEquip {1} ({1}: Attach to target creature you control. Equip only as a sorcery.)";
const RAKDOS_RITEKNIFE: &str =
    "Equipped creature gets +1/+0 for each blood counter on this Equipment and has \"{T}, Sacrifice a creature: Put a blood counter on Rakdos Riteknife.\"\n{B}{R}, Sacrifice this Equipment: Target player sacrifices a permanent of their choice for each blood counter on this Equipment.\nEquip {2}";
const RAZOR_BOOMERANG: &str =
    "Equipped creature has \"{T}, Unattach Razor Boomerang: It deals 1 damage to any target. Return Razor Boomerang to its owner's hand.\"\nEquip {2}";
const SAKASHIMA_THE_IMPOSTOR: &str =
    "You may have Sakashima the Impostor enter as a copy of any creature on the battlefield, except its name is Sakashima the Impostor, it's legendary in addition to its other types, and it has \"{2}{U}{U}: Return Sakashima the Impostor to its owner's hand at the beginning of the next end step.\"";
const SPARE_DAGGER: &str =
    "Equipped creature gets +1/+0 and has \"Whenever this creature attacks, you may sacrifice Spare Dagger. When you do, this creature deals 1 damage to any target.\"\nEquip {1} ({1}: Attach to target creature you control. Equip only as a sorcery.)";
const SUNFIRE_TORCH: &str =
    "Equipped creature gets +1/+0 and has \"Whenever this creature attacks, you may sacrifice Sunfire Torch. When you do, this creature deals 2 damage to any target.\"\nEquip {1} ({1}: Attach to target creature you control. Equip only as a sorcery.)";
const THE_DOMINION_BRACELET: &str =
    "Equipped creature gets +1/+1 and has \"{15}, Exile The Dominion Bracelet: You control target opponent during their next turn. This ability costs {X} less to activate, where X is this creature's power. Activate only as a sorcery.\" (You see all cards that player could see and make all decisions for them.)\nEquip {1}";
const TORALFS_HAMMER: &str =
    "Equipped creature has \"{1}{R}, {T}, Unattach Toralf's Hammer: It deals 3 damage to any target. Return Toralf's Hammer to its owner's hand.\"\nEquipped creature gets +3/+0 as long as it's legendary.\nEquip {1}{R}";
const TRICKSTERS_TALISMAN: &str =
    "Invoke Duplicity \u{2014} Equipped creature gets +1/+1 and has \"Whenever this creature deals combat damage to a player, you may sacrifice Trickster's Talisman. If you do, create a token that's a copy of this creature.\"\nEquip {2}";
const TRUSTY_BOOMERANG: &str =
    "Equipped creature has \"{1}, {T}: Tap target creature. Return Trusty Boomerang to its owner's hand.\"\nEquip {1} ({1}: Attach to target creature you control. Equip only as a sorcery.)";
const GUTTER_GRIME: &str = "Whenever a nontoken creature you control dies, put a slime \
counter on this enchantment, then create a green Ooze creature token with \"This token's power \
and toughness are each equal to the number of slime counters on Gutter Grime.\"";
const DIRE_BLUNDERBUSS: &str = "Equipped creature gets +3/+0 and has \"Whenever this creature \
attacks, you may sacrifice an artifact other than Dire Blunderbuss. When you do, this creature \
deals damage equal to its power to target creature.\"\nEquip {1}";
const NETTLEVINE_BLIGHT: &str = "Enchant creature or land\nEnchanted permanent has \"At the \
beginning of your end step, sacrifice this permanent and attach Nettlevine Blight to a creature \
or land you control.\"";
const HELIODS_PUNISHMENT: &str = "Enchant creature\nThis Aura enters with four task counters \
on it.\nEnchanted creature can't attack or block. It loses all abilities and has \"{T}: Remove a \
task counter from Heliod's Punishment. Then if it has no task counters on it, destroy Heliod's \
Punishment.\"";
const SAPROLING_BURST: &str = "Fading 7 (This enchantment enters with seven fade counters on it. \
At the beginning of your upkeep, remove a fade counter from it. If you can't, sacrifice it.)\n\
Remove a fade counter from this enchantment: Create a green Saproling creature token. It has \
\"This token's power and toughness are each equal to the number of fade counters on Saproling \
Burst.\"\nWhen this enchantment leaves the battlefield, destroy all tokens created with this \
enchantment. They can't be regenerated.";
const GROTHAMA: &str = "Other creatures have \"Whenever this creature attacks, you may have it \
fight Grothama, All-Devouring.\"\nWhen Grothama leaves the battlefield, each player draws cards \
equal to the amount of damage dealt to Grothama this turn by sources they controlled.";
const THE_AETHERSPARK: &str = "As long as The Aetherspark is attached to a creature, The \
Aetherspark can't be attacked and has \"Whenever equipped creature deals combat damage during \
your turn, put that many loyalty counters on The Aetherspark.\"\n[+1]: Attach The Aetherspark to \
up to one target creature you control. Put a +1/+1 counter on that creature.\n[\u{2212}5]: Draw \
two cards.\n[\u{2212}10]: Add ten mana of any one color.";
const SHIFTING_SHADOW: &str = "Enchant creature\nEnchanted creature has haste and \"At the \
beginning of your upkeep, destroy this creature. Reveal cards from the top of your library until \
you reveal a creature card. Put that card onto the battlefield and attach Shifting Shadow to it, \
then put all other cards revealed this way on the bottom of your library in a random order.\"";

const HELLISH_REBUKE: &str = "Until end of turn, permanents your opponents control gain \"When \
this permanent deals damage to the player who cast Hellish Rebuke, sacrifice this permanent. You \
lose 2 life.\"";

/// `(oracle text, printed name, core types, subtypes)` for the exported class
/// members.
const CLASS_CORPUS: &[(&str, &str, &[&str], &[&str])] = &[
    (
        CITIZENS_CROWBAR,
        "Citizen's Crowbar",
        &["Artifact"],
        &["Equipment"],
    ),
    (
        DECONSTRUCTION_HAMMER,
        "Deconstruction Hammer",
        &["Artifact"],
        &["Equipment"],
    ),
    (FISHING_POLE, "Fishing Pole", &["Artifact"], &["Equipment"]),
    (HANKYU, "Hankyu", &["Artifact"], &["Equipment"]),
    (
        RAKDOS_RITEKNIFE,
        "Rakdos Riteknife",
        &["Artifact"],
        &["Equipment"],
    ),
    (
        SAKASHIMA_THE_IMPOSTOR,
        "Sakashima the Impostor",
        &["Creature"],
        &["Human", "Rogue"],
    ),
    (SPARE_DAGGER, "Spare Dagger", &["Artifact"], &["Equipment"]),
    (
        SUNFIRE_TORCH,
        "Sunfire Torch",
        &["Artifact"],
        &["Equipment"],
    ),
    (
        THE_DOMINION_BRACELET,
        "The Dominion Bracelet",
        &["Artifact"],
        &["Equipment"],
    ),
    (
        TRICKSTERS_TALISMAN,
        "Trickster's Talisman",
        &["Artifact"],
        &["Equipment"],
    ),
    (
        TRUSTY_BOOMERANG,
        "Trusty Boomerang",
        &["Artifact"],
        &["Equipment"],
    ),
    (
        ARCHERY_TRAINING,
        "Archery Training",
        &["Enchantment"],
        &["Aura"],
    ),
    (GUTTER_GRIME, "Gutter Grime", &["Enchantment"], &[]),
    (SAPROLING_BURST, "Saproling Burst", &["Enchantment"], &[]),
    (
        DIRE_BLUNDERBUSS,
        "Dire Blunderbuss",
        &["Artifact"],
        &["Equipment"],
    ),
    (
        THE_AETHERSPARK,
        "The Aetherspark",
        &["Artifact", "Planeswalker"],
        &["Equipment"],
    ),
    (
        HELIODS_PUNISHMENT,
        "Heliod's Punishment",
        &["Enchantment"],
        &["Aura"],
    ),
    (
        GROTHAMA,
        "Grothama, All-Devouring",
        &["Creature"],
        &["Wurm"],
    ),
    (
        NETTLEVINE_BLIGHT,
        "Nettlevine Blight",
        &["Enchantment"],
        &["Aura"],
    ),
    (HELLISH_REBUKE, "Hellish Rebuke", &["Instant"], &[]),
];

const HEARTSEEKER: &str = "Equipped creature gets +2/+1 and has \"{T}, Unattach Heartseeker: \
Destroy target creature.\"\nEquip {5} ({5}: Attach to target creature you control. Equip only as a \
sorcery.)";
const TIBALT_COSMIC_IMPOSTOR: &str = "As Tibalt enters, you get an emblem with \"You may play \
cards exiled with Tibalt, Cosmic Impostor, and you may spend mana as though it were mana of any \
color to cast those spells.\"\n[+2]: Exile the top card of each player's library.\n[\u{2212}3]: \
Exile target artifact or creature.\n[\u{2212}8]: Exile all graveyards. Add {R}{R}{R}.";

/// Exported faces whose quoted granted body names the card where the masker refuses it, so
/// that name would read the host.
const REFUSED_CORPUS: &[(&str, &str, &[&str], &[&str])] = &[
    (
        BLAZING_TORCH,
        "Blazing Torch",
        &["Artifact"],
        &["Equipment"],
    ),
    (HEARTSEEKER, "Heartseeker", &["Artifact"], &["Equipment"]),
    (
        MEANDERED_TOWERSHELL,
        "Meandered Towershell",
        &["Enchantment"],
        &["Aura"],
    ),
    (NINJAS_KUNAI, "Ninja's Kunai", &["Artifact"], &["Equipment"]),
    (
        RAZOR_BOOMERANG,
        "Razor Boomerang",
        &["Artifact"],
        &["Equipment"],
    ),
    (
        TORALFS_HAMMER,
        "Toralf's Hammer",
        &["Artifact"],
        &["Equipment"],
    ),
];

/// CR 201.5a: no raw U+E0004 may survive into ANY string reachable from
/// `ParsedAbilities`' four top-level vectors through the render net's descend
/// set — including the outer static/trigger DESCRIPTION strings that embed the
/// raw quoted text (a granted body's "…has \"…Sacrifice <self>…\"" description).
/// `parser::oracle::render_granting_self_descriptions` renders every residual
/// marker to the granting card's printed name.
///
/// TWO REPAIRS to the round-1 form of this guard, both of which were measured
/// vacuous:
///
/// 1. **`serde_json`, not `format!("{:?}")`.** `Debug` ESCAPES the raw
///    private-use char to the literal text `\u{e0004}`, so searching a `Debug`
///    dump for the real character was ALWAYS false — the guard could not fail.
///    `serde_json` emits it raw, at every `String`, at every depth, which is
///    strictly stronger than any hand-written `visit_*` walk.
/// 2. **The whole measured class, not four constants.** Four cards cannot see a
///    copy-family regression; Sakashima is the only shipped card whose granted
///    description lives inside an `Effect::BecomeCopy` payload.
///
/// SCOPE NOTE: the serde ORACLE is WIDER than the net's REPAIR. It serializes
/// `def.cost` too, so a cost-borne marker would red here even though the net
/// deliberately does not walk the `AbilityCost` axis (the named excluded axis —
/// see `parser::oracle::tests::granted_cost_axis_is_not_walked_and_no_parse_shape_reaches_it`).
/// That is the correct polarity: this guard should red if a marker ever reaches
/// a cost, because nothing downstream would render it.
///
/// Non-vacuity is proved by `placeholder_leak_guard_reports_a_planted_marker`.
///
/// Revert-to-red: remove the render net from `parse_oracle_text` → every card's
/// outer static description carries the raw U+E0004 char.
#[test]
fn placeholder_never_leaks_into_any_description() {
    for &(oracle, name, types, subtypes) in CLASS_CORPUS {
        let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
        let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
        let p = parse_oracle_text(oracle, name, &[], &types, &subtypes);
        let json = serde_json::to_string(&p).expect("ParsedAbilities serializes");
        // PER-CARD POSITIVE REACH-GUARD: this card must actually be a class
        // member in the parsed tree — the masker fired and the typed channel
        // consumed the marker as `TargetFilter::GrantingObject`. Without it, a
        // card that silently stopped parsing its granted body would pass the
        // negative below on an empty tree.
        assert!(
            json.contains("GrantingObject"),
            "reach-guard: {name} must carry a granter self-reference in the typed \
             channel, or its leak assertion below is vacuous"
        );
        assert!(
            !json.contains(PLACEHOLDER),
            "{name}: the masker placeholder must render to the granting card's \
             printed name in every description; a raw U+E0004 leaked"
        );
    }
}

/// CR 201.5a: the masker marked every quoted granter name of each class member, and the
/// typed binder reaches every mark, so the parse demotes none of its abilities.
#[test]
fn class_corpus_has_no_unreached_granter_reference() {
    for &(oracle, name, types, subtypes) in CLASS_CORPUS {
        assert!(
            normalize_card_name_refs_reporting(oracle, name)
                .1
                .is_empty(),
            "{name}: the masker left a quoted granter name as the host"
        );
        let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
        let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
        let parsed = parse_oracle_text(oracle, name, &[], &types, &subtypes);
        let json = serde_json::to_string(&parsed).expect("ParsedAbilities serializes");
        assert!(
            json.contains("\"type\":\"GrantingObject"),
            "reach-guard: {name}"
        );
        assert!(!json.contains("granter_reference_unreached"), "{name}");
    }
}

fn granter_residuals(parsed: &engine::parser::oracle::ParsedAbilities) -> Vec<&str> {
    parsed
        .abilities
        .iter()
        .filter(|def| {
            matches!(&*def.effect, Effect::Unimplemented { name, .. } if name == "granter_reference_unreached")
        })
        .filter_map(|def| def.description.as_deref())
        .collect()
}

/// CR 201.5a: a granted body that names its granter where the masker refuses the name is
/// strictly unsupported, whatever the body's other granter references bind.
#[test]
fn refused_corpus_demotes_every_affected_grant() {
    for &(oracle, name, types, subtypes) in REFUSED_CORPUS {
        assert!(
            !normalize_card_name_refs_reporting(oracle, name)
                .1
                .is_empty(),
            "reach-guard: {name}"
        );
        let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
        let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
        let parsed = parse_oracle_text(oracle, name, &[], &types, &subtypes);
        let residuals = granter_residuals(&parsed);
        assert_eq!(residuals.len(), 1, "{name}: {parsed:#?}");
        assert!(
            residuals[0].contains('"'),
            "{name}: the residual is the grant line"
        );
        let json = serde_json::to_string(&parsed).expect("ParsedAbilities serializes");
        assert!(!json.contains(PLACEHOLDER), "{name}");
        assert!(!json.contains("\"GrantAbility\""), "{name}");
        assert!(!json.contains("\"GrantTrigger\""), "{name}");
    }
}

/// CR 201.5a — NON-VACUITY PROOF for `placeholder_never_leaks_into_any_description`.
///
/// A negative assertion is only worth what its ability to fail is worth. This
/// plants a marker into a real parsed tree AFTER the net has run and asserts the
/// same `serde_json` oracle DOES report it.
///
/// Revert-to-red: delete the injection — the guard passes on a clean tree and
/// this test's own assertion flips, which is the point.
#[test]
fn placeholder_leak_guard_reports_a_planted_marker() {
    let (types, subtypes) = equipment_types();
    let mut p = parse_oracle_text(
        DECONSTRUCTION_HAMMER,
        "Deconstruction Hammer",
        &[],
        &types,
        &subtypes,
    );
    assert!(
        !serde_json::to_string(&p)
            .expect("ParsedAbilities serializes")
            .contains(PLACEHOLDER),
        "reach-guard: the tree must be clean BEFORE the injection, or this test \
         proves nothing about the guard's sensitivity"
    );
    p.statics[0].description = Some(format!("x{PLACEHOLDER}y"));
    assert!(
        serde_json::to_string(&p)
            .expect("ParsedAbilities serializes")
            .contains(PLACEHOLDER),
        "the `serde_json` leak oracle must REPORT a planted marker — if it cannot \
         fail, `placeholder_never_leaks_into_any_description` is vacuous (which is \
         exactly what the round-1 `format!(\"{{:?}}\")` form was)"
    );
}

/// CR 201.5a: "exile it and Meandered Towershell" names the Aura where the masker refuses
/// the name, so the granted trigger is demoted even though its later return operand binds.
#[test]
fn meandered_towershell_refused_operand_demotes_its_grant() {
    let parsed = parse_oracle_text(
        MEANDERED_TOWERSHELL,
        "Meandered Towershell",
        &["Enchant".to_string()],
        &["Enchantment".to_string()],
        &["Aura".to_string()],
    );
    assert!(
        parsed
            .extracted_keywords
            .iter()
            .any(|k| matches!(k, engine::types::keywords::Keyword::Enchant(_))),
        "reach-guard: the card's other line still parses: {parsed:#?}"
    );
    assert_eq!(
        granter_residuals(&parsed),
        vec![
            "Enchanted creature has islandwalk and \"Whenever ~ attacks, exile it and ~. Return it \
             to the battlefield under your control tapped and attacking at the beginning of the \
             declare attackers step on your next turn, then return Meandered Towershell to the \
             battlefield under its owner's control attached to that creature.\""
        ]
    );
    assert!(
        parsed.statics.is_empty() && parsed.triggers.is_empty(),
        "{parsed:#?}"
    );
}

/// CR 201.5a: a self-granted body's refused name still names the granter once the granted
/// ability is copied onto a new object, so the grant line is demoted.
#[test]
fn self_grant_refused_name_demotes_its_grant() {
    for (oracle, name, types, residual) in [
        (
            IRON_FIST,
            "Iron Fist, Living Weapon",
            "Creature",
            "Whenever you cast a spell that targets a creature you control, ~ gains \"{T}: ~ \
             deals damage equal to his power to any other target\" until end of turn.",
        ),
        (
            MS_MARVEL,
            "Ms. Marvel, Kamala Khan",
            "Creature",
            "Whenever you cast a spell that targets a creature you control, draw a card. Until \
             end of turn, ~ gains \"~'s base power is equal to the number of cards in your hand.\"",
        ),
        (
            NECROMANCY,
            "Necromancy",
            "Enchantment",
            "When ~ enters, if it's on the battlefield, it becomes an Aura with \"enchant \
             creature put onto the battlefield with ~.\" Put target creature card from a graveyard \
             onto the battlefield under your control and attach ~ to it. When ~ leaves the \
             battlefield, that creature's controller sacrifices it.",
        ),
    ] {
        assert!(
            !normalize_card_name_refs_reporting(oracle, name)
                .1
                .is_empty(),
            "reach-guard: {name}"
        );
        let parsed = parse_oracle_text(oracle, name, &[], &[types.to_string()], &[]);
        assert_eq!(
            granter_residuals(&parsed),
            vec![residual],
            "{name}: {parsed:#?}"
        );
    }
}

/// CR 201.5a: Quicksilver Elemental copying Iron Fist's granted ability must not deal damage
/// from Quicksilver, so the refused self-grant installs no ability to copy.
#[test]
fn quicksilver_copies_no_ability_from_a_refused_iron_fist_grant() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let iron_fist = scenario
        .add_creature_from_oracle(P0, "Iron Fist, Living Weapon", 4, 4, IRON_FIST)
        .id();
    let quicksilver = scenario
        .add_creature_from_oracle(P0, "Quicksilver Elemental", 2, 2, QUICKSILVER_ELEMENTAL)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Untap Probe", true, "Untap target creature.")
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();
    let p1_life = runner.life(P1);
    let targeted_by = |events: &[GameEvent], source: ObjectId| {
        events.iter().any(|e| {
            matches!(e, GameEvent::BecomesTarget { target: TargetRef::Object(t), source_id, .. }
                if *t == iron_fist && *source_id == source)
        })
    };

    let cast = runner.cast(spell).target_object(iron_fist).resolve();
    assert!(
        targeted_by(cast.events(), spell),
        "reach-guard: a spell targeting Iron Fist was cast"
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Graveyard,
        "reach-guard: the spell resolved"
    );

    let gain = runner.state().objects[&quicksilver]
        .abilities
        .iter()
        .position(|a| matches!(*a.effect, Effect::GainActivatedAbilitiesOfTarget { .. }))
        .expect("Quicksilver's {U} ability");
    let copied = runner
        .activate(quicksilver, gain)
        .target_object(iron_fist)
        .resolve();
    assert!(
        targeted_by(copied.events(), quicksilver),
        "reach-guard: Quicksilver's {{U}} targeted Iron Fist"
    );
    assert!(
        copied
            .events()
            .iter()
            .any(|e| matches!(e, GameEvent::StackResolved { .. })),
        "reach-guard: Quicksilver's {{U}} resolved"
    );
    runner.advance_until_stack_empty();

    let copied_damage = runner.state().objects[&quicksilver]
        .abilities
        .iter()
        .position(|a| matches!(*a.effect, Effect::DealDamage { .. }));
    let mut events = Vec::new();
    if let Some(index) = copied_damage {
        let out = runner
            .activate(quicksilver, index)
            .target_player(P1)
            .resolve();
        events.extend_from_slice(out.events());
    }
    assert_eq!(runner.life(P1), p1_life);
    assert!(!events.iter().any(
        |e| matches!(e, GameEvent::DamageDealt { source_id, .. } if *source_id == quicksilver)
    ));
    assert_eq!(copied_damage, None, "Quicksilver gained a damage ability");
}

const QUICKSILVER_ELEMENTAL: &str = "{U}: This creature gains all activated abilities of target \
creature until end of turn. (If any of the abilities use that creature's name, use this \
creature's name instead.)\nYou may spend blue mana as though it were mana of any color to pay the \
activation costs of this creature's abilities.";
const NECROMANCY: &str = "You may cast this spell as though it had flash. If you cast it any \
time a sorcery couldn't have been cast, the controller of the permanent it becomes sacrifices it \
at the beginning of the next cleanup step.\nWhen this enchantment enters, if it's on the \
battlefield, it becomes an Aura with \"enchant creature put onto the battlefield with \
Necromancy.\" Put target creature card from a graveyard onto the battlefield under your control \
and attach this enchantment to it. When this enchantment leaves the battlefield, that creature's \
controller sacrifices it.";
const IRON_FIST: &str = "Whenever you cast a spell that targets a creature you control, Iron Fist \
gains \"{T}: Iron Fist deals damage equal to his power to any other target\" until end of turn.";
const MS_MARVEL: &str = "Reach, vigilance\nYou have no maximum hand size.\nEmbiggen Fist \u{2014} \
Whenever you cast a spell that targets a creature you control, draw a card. Until end of turn, Ms. \
Marvel gains \"Ms. Marvel's base power is equal to the number of cards in your hand.\"";

/// CR 201.5a (last sentence: "This is also true if the second ability is copied
/// onto a new object") + CR 707.2 — HOSTILE FIXTURE: granter == host, via
/// copy-except.
///
/// Sakashima the Impostor is the ONLY shipped card whose granted description
/// lives inside an `Effect::BecomeCopy` payload. `types::ability_visit` treats
/// `BecomeCopy`/`CopySpell`/`CopyTokenOf` as LEAVES, so no walker in the tree
/// reaches this description without the render net's copy-family arm — which is
/// why this card is a load-bearing structural fixture, not a footnote. (Its
/// sibling `SetName` modification means the rendered output is the same before
/// and after this change; the STRUCTURAL claim is what this test pins.)
///
/// Revert-to-red: delete the `BecomeCopy | CopySpell | CopyTokenOf` arm from
/// `render_effect_descriptions` — the nested description retains the raw marker.
#[test]
fn sakashima_copy_except_grant_description_renders_the_granter() {
    let parsed = parse_oracle_text(
        SAKASHIMA_THE_IMPOSTOR,
        "Sakashima the Impostor",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Rogue".to_string()],
    );
    let json = serde_json::to_string(&parsed).expect("ParsedAbilities serializes");
    // POSITIVE REACH-GUARD: the self-grant's `Return <self> to its owner's hand`
    // really reached the typed channel as a granter reference.
    assert!(
        json.contains("GrantingObject"),
        "reach-guard: the copy-except self-grant must reach the typed channel"
    );
    assert!(
        !json.contains(PLACEHOLDER),
        "a raw CR 201.5a marker survived inside an `Effect::BecomeCopy` payload — \
         the copy-family descend arm is missing"
    );
    assert!(
        json.contains("Sakashima the Impostor to its owner"),
        "CR 201.5a: the granted body nested in the copy payload must name the \
         granting object: {json}"
    );
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
            &TargetFilter::GrantingObject { bound: None },
            "{name}: the PutCounter target names the granting equipment → GrantingObject"
        );
        // `serde_json`, not `format!("{:?}")`: `Debug` ESCAPES the raw private-use
        // char to the literal text `\u{e0004}`, so a Debug search for the real
        // character is always false and this negative would be vacuous.
        assert!(
            !serde_json::to_string(&def)
                .expect("the granted definition serializes")
                .contains(PLACEHOLDER),
            "{name}: no raw placeholder may survive into the AST"
        );
    }
}

fn granted_modifications(
    oracle: &str,
    name: &str,
    types: &[&str],
    subtypes: &[&str],
) -> Vec<ContinuousModification> {
    let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
    parse_oracle_text(oracle, name, &[], &types, &subtypes)
        .statics
        .into_iter()
        .flat_map(|s| s.modifications)
        .collect()
}

fn granted_trigger_effect(oracle: &str, name: &str, types: &[&str], subtypes: &[&str]) -> Effect {
    granted_modifications(oracle, name, types, subtypes)
        .into_iter()
        .find_map(|m| match m {
            ContinuousModification::GrantTrigger { trigger } => trigger.execute.map(|e| *e.effect),
            _ => None,
        })
        .expect("a granted trigger body")
}

fn cost_parts(cost: &AbilityCost) -> Vec<&AbilityCost> {
    match cost {
        AbilityCost::Composite { costs } => costs.iter().collect(),
        other => vec![other],
    }
}

/// CR 201.5a: the token's granted CDA reads the granting Saproling Burst.
#[test]
fn saproling_burst_granted_cda_reads_the_granter() {
    let parsed = parse_oracle_text(
        SAPROLING_BURST,
        "Saproling Burst",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    let activated = parsed
        .abilities
        .iter()
        .find(|a| matches!(a.cost, Some(AbilityCost::RemoveCounter { .. })))
        .expect("the fade-counter ability");
    assert!(matches!(*activated.effect, Effect::Unimplemented { .. }));
    let grant = match activated.sub_ability.as_deref().map(|s| s.effect.as_ref()) {
        Some(Effect::GenericEffect {
            static_abilities, ..
        }) => static_abilities[0]
            .modifications
            .iter()
            .find_map(|m| match m {
                ContinuousModification::GrantStaticAbility { definition } => Some(definition),
                _ => None,
            })
            .expect("GrantStaticAbility"),
        other => panic!("expected a GenericEffect grant, got {other:?}"),
    };
    let fade = QuantityExpr::Ref {
        qty: QuantityRef::CountersOn {
            scope: ObjectScope::GrantingObject,
            counter_type: Some(CounterType::Fade),
        },
    };
    assert_eq!(
        grant.modifications,
        vec![
            ContinuousModification::SetDynamicPower {
                value: fade.clone()
            },
            ContinuousModification::SetDynamicToughness { value: fade },
        ]
    );
}

/// CR 201.5a: "Remove all aim counters from Hankyu" is a cost on Hankyu.
#[test]
fn hankyu_remove_all_cost_names_the_granter() {
    let mods = granted_modifications(HANKYU, "Hankyu", &["Artifact"], &["Equipment"]);
    let defs: Vec<&AbilityDefinition> = mods
        .iter()
        .filter_map(|m| match m {
            ContinuousModification::GrantAbility { definition } => Some(definition.as_ref()),
            _ => None,
        })
        .collect();
    assert_eq!(defs.len(), 2);
    assert!(matches!(
        defs[0].effect.as_ref(),
        Effect::PutCounter {
            target: TargetFilter::GrantingObject { .. },
            ..
        }
    ));
    let remove = cost_parts(defs[1].cost.as_ref().expect("a cost"))
        .into_iter()
        .find_map(|c| match c {
            AbilityCost::RemoveCounter { target, .. } => Some(target.clone()),
            _ => None,
        })
        .expect("a remove-counter cost");
    assert_eq!(remove, Some(TargetFilter::GrantingObject { bound: None }));
}

/// CR 201.5a: the granted "fight Grothama" fights the granting Grothama.
#[test]
fn grothama_granted_fight_names_the_granter() {
    match granted_trigger_effect(
        GROTHAMA,
        "Grothama, All-Devouring",
        &["Creature"],
        &["Wurm"],
    ) {
        Effect::Fight { target, .. } => {
            assert_eq!(target, TargetFilter::GrantingObject { bound: None })
        }
        other => panic!("expected Fight, got {other:?}"),
    }
}

/// CR 201.5a: "Tap Fishing Pole" is a cost on Fishing Pole.
#[test]
fn fishing_pole_tap_cost_names_the_granter() {
    let def = granted_def_from(FISHING_POLE, "Fishing Pole", &["Artifact"], &["Equipment"]);
    assert!(matches!(
        def.effect.as_ref(),
        Effect::PutCounter {
            target: TargetFilter::GrantingObject { .. },
            ..
        }
    ));
    let tap = cost_parts(def.cost.as_ref().expect("a cost"))
        .into_iter()
        .find_map(|c| match c {
            AbilityCost::EffectCost { effect } => match effect.as_ref() {
                Effect::SetTapState { target, .. } => Some(target.clone()),
                _ => None,
            },
            _ => None,
        })
        .expect("a tap effect cost");
    assert_eq!(tap, TargetFilter::GrantingObject { bound: None });
}

fn foo_bar_body_condition(body: &str) -> (AbilityDefinition, AbilityCondition) {
    let oracle = format!("Enchant creature\nEnchanted creature has \"{body}\"");
    let def = granted_def_from(&oracle, "Foo Bar", &["Enchantment"], &["Aura"]);
    let sub = def.sub_ability.as_deref().expect("the destroy clause");
    assert!(matches!(
        sub.effect.as_ref(),
        Effect::Destroy {
            target: TargetFilter::GrantingObject { .. },
            ..
        }
    ));
    let condition = sub.condition.clone().expect("the counter gate");
    (def, condition)
}

fn task_gate(scope: ObjectScope) -> AbilityCondition {
    AbilityCondition::QuantityCheck {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::CountersOn {
                scope,
                counter_type: Some(CounterType::Generic("task".to_string())),
            },
        },
        comparator: Comparator::EQ,
        rhs: QuantityExpr::Fixed { value: 0 },
    }
}

/// CR 608.2c + CR 201.5a: the bare "it" of a leading counter gate reads the
/// granter only when the prior clause names the granter, conditioned or not.
#[test]
fn counter_gate_pronoun_follows_its_antecedent() {
    let (_, host) = foo_bar_body_condition(
        "{T}: Remove a task counter from this creature. Then if it has no task counters on it, destroy Foo Bar.",
    );
    assert_eq!(host, task_gate(ObjectScope::Source));

    let (def, granter) = foo_bar_body_condition(
        "{T}: If you control an artifact, remove a task counter from Foo Bar. Then if it has no task counters on it, destroy Foo Bar.",
    );
    assert!(def.condition.is_some());
    assert_eq!(granter, task_gate(ObjectScope::GrantingObject));
}

/// CR 201.5a: Heliod's Punishment's body removes from, counts and destroys the
/// granting Aura.
#[test]
fn heliods_punishment_parse_reads_the_granter() {
    let def = granted_def_from(
        HELIODS_PUNISHMENT,
        "Heliod's Punishment",
        &["Enchantment"],
        &["Aura"],
    );
    assert!(matches!(
        def.effect.as_ref(),
        Effect::RemoveCounter {
            target: TargetFilter::GrantingObject { .. },
            ..
        }
    ));
    let sub = def.sub_ability.as_deref().expect("the destroy clause");
    assert!(matches!(
        sub.effect.as_ref(),
        Effect::Destroy {
            target: TargetFilter::GrantingObject { .. },
            ..
        }
    ));
    assert_eq!(sub.condition, Some(task_gate(ObjectScope::GrantingObject)));
}

// ---------------------------------------------------------------------------
// CR 201.5a masker positions: an allowlisted position masks the granter name, a
// refused one (`by `) stays `~`.
// ---------------------------------------------------------------------------

/// Assert the masker leaves a refused position unmasked while the name still
/// normalizes to `~`.
fn assert_masker_noop(oracle: &str, name: &str, reach: &str) {
    let normalized = normalize_card_name_refs(oracle, name);
    assert!(
        !normalized.contains(PLACEHOLDER),
        "{name}: a refused self-name position must NOT be masked"
    );
    assert!(
        normalized.contains(reach),
        "{name}: the refused position must still normalize to ~ (reach-guard); got {normalized}"
    );
}

const ARCHERY_TRAINING: &str = "Enchant creature\nAt the beginning of your upkeep, you may put an \
arrow counter on this Aura.\nEnchanted creature has \"{T}: This creature deals X damage to target \
attacking or blocking creature, where X is the number of arrow counters on Archery Training.\"";

/// CR 201.5a: Archery Training's "number of arrow counters on <self>" reads the
/// granter, in both the typed and the display channel.
#[test]
fn archery_training_quantity_ref_channel_binds_the_granter() {
    assert!(normalize_card_name_refs(ARCHERY_TRAINING, "Archery Training").contains(PLACEHOLDER));
    let def = granted_def_from(
        ARCHERY_TRAINING,
        "Archery Training",
        &["Enchantment"],
        &["Aura"],
    );
    match def.effect.as_ref() {
        Effect::DealDamage { amount, .. } => assert_eq!(
            *amount,
            QuantityExpr::Ref {
                qty: QuantityRef::CountersOn {
                    scope: ObjectScope::GrantingObject,
                    counter_type: Some(CounterType::Generic("arrow".to_string())),
                },
            }
        ),
        other => panic!("expected DealDamage, got {other:?}"),
    }
    let desc = def
        .description
        .as_deref()
        .expect("the granted Archery Training ability carries a display description");
    assert!(
        desc.contains("arrow counters on Archery Training"),
        "{desc}"
    );
}

const ANIMAL_FRIEND: &str = "Enchant creature\nEnchanted creature has \"Whenever this creature \
attacks, create a 1/1 green Squirrel creature token. Put a +1/+1 counter on that token for each \
Aura and Equipment attached to this creature other than Animal Friend.\"";

/// CR 201.5a: Animal Friend's "other than <self>" and Shifting Shadow's
/// "attach <self>" are masked, and no placeholder survives their dropped clauses.
#[test]
fn animal_friend_exclusion_channel_masks_without_leaking() {
    for (oracle, name) in [
        (ANIMAL_FRIEND, "Animal Friend"),
        (SHIFTING_SHADOW, "Shifting Shadow"),
    ] {
        assert!(
            normalize_card_name_refs(oracle, name).contains(PLACEHOLDER),
            "{name}"
        );
        let parsed = parse_oracle_text(
            oracle,
            name,
            &[],
            &["Enchantment".to_string()],
            &["Aura".to_string()],
        );
        assert!(
            parsed
                .statics
                .iter()
                .flat_map(|s| s.modifications.iter())
                .any(|m| matches!(m, ContinuousModification::GrantTrigger { .. })),
            "{name}"
        );
        assert!(
            !serde_json::to_string(&parsed)
                .expect("ParsedAbilities serializes")
                .contains(PLACEHOLDER),
            "{name}"
        );
    }
}

const TORRENT_OF_LAVA: &str = "Torrent of Lava deals X damage to each creature without flying.\n\
As long as Torrent of Lava is on the stack, each creature has \"{T}: Prevent the next 1 damage \
that would be dealt to this creature by Torrent of Lava this turn.\"";

/// Torrent of Lava — damage-source channel ("dealt … by <self>"). Revert-to-red:
/// re-widen the masker → `by <placeholder>` in the normalized string → red.
#[test]
fn torrent_of_lava_damage_source_channel_not_masked() {
    assert_masker_noop(TORRENT_OF_LAVA, "Torrent of Lava", "by ~ this turn");
}

/// CR 201.5a + CR 400.7 + CR 608.2h: `ObjectScope::SpecificObject` reads one exact
/// object incarnation; the unbound `ObjectScope::GrantingObject` reads as `Source` in
/// counter reads.
mod object_scope_reads {
    use engine::game::layers::evaluate_layers;
    use engine::game::quantity::{resolve_quantity, resolve_quantity_with_targets};
    use engine::game::scenario::{GameScenario, P0};
    use engine::game::zones::move_to_zone;
    use engine::types::ability::{
        ContinuousModification, Effect, ObjectScope, QuantityExpr, QuantityRef, ResolvedAbility,
        StaticDefinition, TargetFilter,
    };
    use engine::types::counter::CounterType;
    use engine::types::game_state::GameState;
    use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
    use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
    use engine::types::zones::Zone;

    fn slime() -> CounterType {
        CounterType::Generic("slime".to_string())
    }

    fn qty(qty: QuantityRef) -> QuantityExpr {
        QuantityExpr::Ref { qty }
    }

    fn counters(scope: ObjectScope) -> QuantityExpr {
        qty(QuantityRef::CountersOn {
            scope,
            counter_type: Some(slime()),
        })
    }

    fn bound(object: ObjectIncarnationRef) -> ObjectScope {
        ObjectScope::SpecificObject { object }
    }

    /// Host 1/1 {1}{W} white with 1 slime; granter 2/2 {2}{G}{U} green-blue with 3
    /// slime. With `bind_cda`, the host's P/T is a CDA counting slime on the granter.
    fn setup(bind_cda: bool) -> (GameState, ObjectId, ObjectId) {
        let mut scenario = GameScenario::new();
        let granter = {
            let mut b = scenario.add_creature(P0, "Granter", 2, 2);
            b.with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Green, ManaCostShard::Blue],
                generic: 2,
            })
            .with_color(vec![ManaColor::Green, ManaColor::Blue]);
            b.id()
        };
        let host = {
            let mut b = scenario.add_creature(P0, "Host", 1, 1);
            b.with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 1,
            })
            .with_color(vec![ManaColor::White]);
            b.id()
        };
        scenario.with_counter(host, slime(), 1);
        scenario.with_counter(granter, slime(), 3);
        let mut state = scenario.build().state().clone();
        if bind_cda {
            let value = counters(bound(ObjectIncarnationRef::from_object(
                &state.objects[&granter],
            )));
            let def = StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .cda()
                .modifications(vec![
                    ContinuousModification::SetDynamicPower {
                        value: value.clone(),
                    },
                    ContinuousModification::SetDynamicToughness { value },
                ]);
            let obj = state.objects.get_mut(&host).unwrap();
            obj.static_definitions.push(def.clone());
            std::sync::Arc::make_mut(&mut obj.base_static_definitions).push(def);
        }
        recompute(&mut state);
        (state, host, granter)
    }

    fn recompute(state: &mut GameState) {
        state.layers_dirty.mark_full();
        evaluate_layers(state);
    }

    fn resolving(host: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::unimplemented("granted", "granted ability read"),
            vec![],
            host,
            P0,
        )
    }

    fn host_pt(state: &GameState, host: ObjectId) -> (Option<i32>, Option<i32>) {
        let obj = &state.objects[&host];
        (obj.power, obj.toughness)
    }

    fn current(state: &GameState, id: ObjectId) -> ObjectScope {
        bound(ObjectIncarnationRef::from_object(&state.objects[&id]))
    }

    #[test]
    fn bound_object_scope_cda_reads_the_granter_not_the_host() {
        let (state, host, granter) = setup(true);
        let g0 = current(&state, granter);

        assert_eq!(host_pt(&state, host), (Some(3), Some(3)));
        let read = |q| resolve_quantity(&state, &qty(q), P0, host);
        assert_eq!(read(QuantityRef::Power { scope: g0 }), 2);
        assert_eq!(read(QuantityRef::ObjectManaValue { scope: g0 }), 4);
        assert_eq!(read(QuantityRef::ObjectColorCount { scope: g0 }), 2);
        assert_eq!(
            read(QuantityRef::ManaSymbolsInManaCost {
                scope: g0,
                color: None
            }),
            2
        );
    }

    #[test]
    fn bound_object_scope_departed_reads_zero_statically_lki_when_resolving() {
        let (mut state, host, granter) = setup(true);
        let g0 = current(&state, granter);
        move_to_zone(&mut state, granter, Zone::Graveyard, &mut Vec::new());
        recompute(&mut state);

        assert_eq!(host_pt(&state, host), (Some(0), Some(0)));
        assert_eq!(resolve_quantity(&state, &counters(g0), P0, host), 0);

        let ability = resolving(host);
        assert_eq!(
            resolve_quantity_with_targets(&state, &counters(g0), &ability),
            3
        );
        assert_eq!(
            resolve_quantity_with_targets(&state, &qty(QuantityRef::Power { scope: g0 }), &ability),
            2
        );

        let g_gy = current(&state, granter);
        assert_eq!(
            resolve_quantity(&state, &qty(QuantityRef::Power { scope: g_gy }), P0, host),
            2
        );
    }

    #[test]
    fn bound_object_scope_blinked_granter_is_a_new_object() {
        let (mut state, host, granter) = setup(true);
        let g0 = current(&state, granter);
        move_to_zone(&mut state, granter, Zone::Exile, &mut Vec::new());
        move_to_zone(&mut state, granter, Zone::Battlefield, &mut Vec::new());
        state
            .objects
            .get_mut(&granter)
            .unwrap()
            .counters
            .insert(slime(), 5);
        recompute(&mut state);
        let returned = current(&state, granter);
        let color_count = |scope| qty(QuantityRef::ObjectColorCount { scope });

        assert_eq!(resolve_quantity(&state, &counters(returned), P0, host), 5);
        assert_eq!(
            resolve_quantity(&state, &color_count(returned), P0, host),
            2
        );

        assert_eq!(host_pt(&state, host), (Some(0), Some(0)));
        assert_eq!(resolve_quantity(&state, &counters(g0), P0, host), 0);
        assert_eq!(
            resolve_quantity_with_targets(&state, &counters(g0), &resolving(host)),
            3
        );
        assert_eq!(resolve_quantity(&state, &color_count(g0), P0, host), 0);
        let pips = qty(QuantityRef::ManaSymbolsInManaCost {
            scope: g0,
            color: None,
        });
        assert_eq!(resolve_quantity(&state, &pips, P0, host), 0);
    }

    #[test]
    fn unbound_granting_object_counters_read_the_source() {
        let (mut state, host, _granter) = setup(false);
        let unbound = counters(ObjectScope::GrantingObject);

        assert_eq!(resolve_quantity(&state, &unbound, P0, host), 1);
        assert_eq!(
            resolve_quantity_with_targets(&state, &unbound, &resolving(host)),
            1
        );

        move_to_zone(&mut state, host, Zone::Graveyard, &mut Vec::new());
        assert_eq!(
            resolve_quantity_with_targets(&state, &unbound, &resolving(host)),
            1
        );
    }

    #[test]
    fn object_scope_granter_values_round_trip() {
        let g0 = bound(ObjectIncarnationRef::of(ObjectId(7), 2));
        let json = serde_json::to_string(&g0).unwrap();
        assert!(json.contains("\"SpecificObject\""), "{json}");
        assert_eq!(serde_json::from_str::<ObjectScope>(&json).unwrap(), g0);

        let json = serde_json::to_string(&ObjectScope::GrantingObject).unwrap();
        assert_eq!(
            serde_json::from_str::<ObjectScope>(&json).unwrap(),
            ObjectScope::GrantingObject
        );
    }
}

// ---------------------------------------------------------------------------
// CR 201.5a: every channel through which a granted body names its granter, read
// through the stamp each attachment seam (Layer-6 grants, token creation) sets.
// Card fixtures use verbatim Oracle text.
// ---------------------------------------------------------------------------

mod concretizer_seams {
    use std::sync::Arc;

    use engine::game::combat::{can_block_pair, AttackTarget};
    use engine::game::effects::attach::attach_to;
    use engine::game::effects::resolve_ability_chain;
    use engine::game::filter::{matches_target_filter, FilterContext};
    use engine::game::game_object::AttachTarget;
    use engine::game::layers::evaluate_layers;
    use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
    use engine::game::zones::move_to_zone;
    use engine::parser::oracle::parse_oracle_text;
    use engine::types::ability::{
        AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, Comparator,
        ContinuousModification, Effect, EffectKind, FilterProp, ObjectScope, PtValue, QuantityExpr,
        QuantityRef, ResolvedAbility, StaticDefinition, TargetFilter, TargetRef, TypeFilter,
        TypedFilter,
    };
    use engine::types::actions::GameAction;
    use engine::types::card_type::CoreType;
    use engine::types::counter::CounterType;
    use engine::types::events::GameEvent;
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
    use engine::types::keywords::Keyword;
    use engine::types::mana::{ManaCost, ManaType, ManaUnit};
    use engine::types::phase::Phase;
    use engine::types::statics::StaticMode;
    use engine::types::zones::Zone;

    use super::{
        ARCHERY_TRAINING, DIRE_BLUNDERBUSS, FISHING_POLE, GROTHAMA, GUTTER_GRIME, HANKYU,
        HELIODS_PUNISHMENT, KROVIKAN_PLAGUE, NETTLEVINE_BLIGHT, SPARE_DAGGER, THE_AETHERSPARK,
        TRUSTY_BOOMERANG,
    };

    fn counter(kind: &str) -> CounterType {
        CounterType::Generic(kind.to_string())
    }

    fn counters_on(scope: ObjectScope, kind: &str) -> QuantityExpr {
        QuantityExpr::Ref {
            qty: QuantityRef::CountersOn {
                scope,
                counter_type: Some(counter(kind)),
            },
        }
    }

    fn incarnation(state: &GameState, id: ObjectId) -> ObjectIncarnationRef {
        ObjectIncarnationRef::from_object(&state.objects[&id])
    }

    fn bound(state: &GameState, id: ObjectId) -> ObjectScope {
        ObjectScope::SpecificObject {
            object: incarnation(state, id),
        }
    }

    fn grant_static(oracle: &str, name: &str, core: &str, subtype: &str) -> StaticDefinition {
        parse_oracle_text(
            oracle,
            name,
            &[],
            &[core.to_string()],
            &[subtype.to_string()],
        )
        .statics
        .into_iter()
        .find(|s| {
            s.modifications.iter().any(|m| {
                matches!(
                    m,
                    ContinuousModification::GrantAbility { .. }
                        | ContinuousModification::GrantTrigger { .. }
                )
            })
        })
        .expect("a static granting an ability or trigger")
    }

    fn granted_ability(grant: &mut StaticDefinition) -> &mut AbilityDefinition {
        grant
            .modifications
            .iter_mut()
            .find_map(|m| match m {
                ContinuousModification::GrantAbility { definition } => Some(definition.as_mut()),
                _ => None,
            })
            .expect("GrantAbility")
    }

    fn granted_execute(grant: &mut StaticDefinition) -> &mut AbilityDefinition {
        grant
            .modifications
            .iter_mut()
            .find_map(|m| match m {
                ContinuousModification::GrantTrigger { trigger } => trigger.execute.as_deref_mut(),
                _ => None,
            })
            .expect("GrantTrigger execute")
    }

    fn relayer(state: &mut GameState) {
        state.layers_dirty.mark_full();
        evaluate_layers(state);
    }

    /// Makes `granter` a `core` `subtype` attached to `host` that carries `grant`.
    fn attach(
        runner: &mut GameRunner,
        granter: ObjectId,
        host: ObjectId,
        core: CoreType,
        subtype: &str,
        grant: StaticDefinition,
    ) {
        let st = runner.state_mut();
        let obj = st.objects.get_mut(&granter).unwrap();
        obj.card_types.core_types = vec![core];
        obj.card_types.subtypes = vec![subtype.to_string()];
        obj.base_card_types = obj.card_types.clone();
        obj.power = None;
        obj.toughness = None;
        obj.base_power = None;
        obj.base_toughness = None;
        obj.static_definitions.push(grant.clone());
        Arc::make_mut(&mut obj.base_static_definitions).push(grant);
        attach_to(st, granter, host);
        relayer(st);
    }

    fn make_artifact(state: &mut GameState, id: ObjectId) {
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types = vec![CoreType::Artifact];
        obj.base_card_types = obj.card_types.clone();
        obj.power = None;
        obj.toughness = None;
        obj.base_power = None;
        obj.base_toughness = None;
    }

    fn pass_to_declare_attackers(runner: &mut GameRunner) {
        for _ in 0..8 {
            if matches!(
                runner.state().waiting_for,
                WaitingFor::DeclareAttackers { .. }
            ) {
                return;
            }
            runner.act(GameAction::PassPriority).unwrap();
        }
        panic!(
            "never reached DeclareAttackers: {:?}",
            runner.state().waiting_for
        );
    }

    fn attack_with(runner: &mut GameRunner, attacker: ObjectId) {
        pass_to_declare_attackers(runner);
        runner
            .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
            .unwrap();
    }

    /// Drives priority, "you may" prompts and trigger ordering until the engine asks
    /// for anything else, answering a trigger target with `target`.
    fn drive(runner: &mut GameRunner, target: Option<TargetRef>) {
        for _ in 0..40 {
            let action = match &runner.state().waiting_for {
                WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                    GameAction::PassPriority
                }
                WaitingFor::OptionalEffectChoice { .. } => {
                    GameAction::DecideOptionalEffect { accept: true }
                }
                WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                    order: (0..triggers.len()).collect(),
                },
                WaitingFor::TriggerTargetSelection { .. } if target.is_some() => {
                    GameAction::ChooseTarget {
                        target: target.clone(),
                    }
                }
                _ => return,
            };
            runner.act(action).unwrap();
        }
        panic!("drive did not settle: {:?}", runner.state().waiting_for);
    }

    fn activate(runner: &mut GameRunner, host: ObjectId, index: usize, target: Option<ObjectId>) {
        runner
            .act(GameAction::ActivateAbility {
                source_id: host,
                ability_index: index,
            })
            .unwrap();
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ) {
            runner
                .act(GameAction::SelectTargets {
                    targets: target.into_iter().map(TargetRef::Object).collect(),
                })
                .unwrap();
        }
        if let Some(t) = target {
            let top = runner.state().stack.last().and_then(|e| e.ability());
            assert_eq!(
                top.map(|a| a.targets.clone()),
                Some(vec![TargetRef::Object(t)])
            );
        }
    }

    /// Resolves the whole stack, returning every event it produced.
    fn resolve_stack(runner: &mut GameRunner) -> Vec<GameEvent> {
        let mut events = Vec::new();
        for _ in 0..40 {
            if runner.state().stack.is_empty() {
                return events;
            }
            events.extend(runner.act(GameAction::PassPriority).unwrap().events);
        }
        panic!("stack never emptied: {:?}", runner.state().waiting_for);
    }

    /// Host carries 1 arrow counter; each Archery Training carries `arrows[i]`.
    fn archery_training(arrows: &[u32]) -> (GameRunner, ObjectId, Vec<ObjectId>, ObjectId) {
        let mut grant = grant_static(ARCHERY_TRAINING, "Archery Training", "Enchantment", "Aura");
        match granted_ability(&mut grant).effect.as_ref() {
            Effect::DealDamage { amount, .. } => {
                assert_eq!(*amount, counters_on(ObjectScope::GrantingObject, "arrow"))
            }
            other => panic!("expected DealDamage, got {other:?}"),
        }
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let host = scenario.add_creature(P0, "Bearer", 2, 2).id();
        let raider = scenario.add_creature(P0, "Raider", 1, 20).id();
        scenario.with_counter(host, counter("arrow"), 1);
        let trainings: Vec<ObjectId> = arrows
            .iter()
            .map(|&n| {
                let id = scenario.add_creature(P0, "Archery Training", 0, 0).id();
                scenario.with_counter(id, counter("arrow"), n);
                id
            })
            .collect();
        let mut runner = scenario.build();
        for &at in &trainings {
            attach(
                &mut runner,
                at,
                host,
                CoreType::Enchantment,
                "Aura",
                grant.clone(),
            );
        }
        attack_with(&mut runner, raider);
        drive(&mut runner, None);
        (runner, host, trainings, raider)
    }

    fn granted_damage_indices(runner: &GameRunner, host: ObjectId) -> Vec<usize> {
        runner.state().objects[&host]
            .abilities
            .iter()
            .enumerate()
            .filter(|(_, a)| matches!(*a.effect, Effect::DealDamage { .. }))
            .map(|(i, _)| i)
            .collect()
    }

    #[test]
    fn archery_training_damage_reads_the_granter_counters() {
        let (mut runner, host, trainings, raider) = archery_training(&[2]);
        let idx = granted_damage_indices(&runner, host);
        assert_eq!(idx.len(), 1);
        match runner.state().objects[&host].abilities[idx[0]]
            .effect
            .as_ref()
        {
            Effect::DealDamage { amount, .. } => {
                assert_eq!(
                    *amount,
                    counters_on(bound(runner.state(), trainings[0]), "arrow")
                )
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            runner.state().objects[&host].abilities[idx[0]].granting_object,
            Some(incarnation(runner.state(), trainings[0]))
        );
        assert_ne!(trainings[0], host);
        activate(&mut runner, host, idx[0], Some(raider));
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&raider].damage_marked, 2);
    }

    #[test]
    fn archery_training_two_granters_each_read_their_own() {
        let (mut runner, host, _trainings, raider) = archery_training(&[3, 2]);
        let idx = granted_damage_indices(&runner, host);
        assert_eq!(idx.len(), 2, "one granted ability per granter");
        let mut dealt = Vec::new();
        for i in idx {
            let before = runner.state().objects[&raider].damage_marked;
            runner.state_mut().objects.get_mut(&host).unwrap().tapped = false;
            activate(&mut runner, host, i, Some(raider));
            runner.advance_until_stack_empty();
            dealt.push(runner.state().objects[&raider].damage_marked - before);
        }
        dealt.sort_unstable();
        assert_eq!(dealt, vec![2, 3]);
    }

    #[test]
    fn archery_training_removed_in_response_uses_last_known_counters() {
        let (mut runner, host, trainings, raider) = archery_training(&[2]);
        let idx = granted_damage_indices(&runner, host);
        activate(&mut runner, host, idx[0], Some(raider));
        assert_eq!(runner.state().stack.len(), 1);
        move_to_zone(
            runner.state_mut(),
            trainings[0],
            Zone::Graveyard,
            &mut Vec::new(),
        );
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&raider].damage_marked, 2);
    }

    fn add_gutter_grime(scenario: &mut GameScenario, slime: u32) -> ObjectId {
        let id = scenario
            .add_enchantment_from_oracle(P0, "Gutter Grime", GUTTER_GRIME)
            .id();
        scenario.with_counter(id, counter("slime"), slime);
        id
    }

    #[test]
    fn gutter_grime_parse_reads_the_granter() {
        let trigger = parse_oracle_text(
            GUTTER_GRIME,
            "Gutter Grime",
            &[],
            &["Enchantment".to_string()],
            &[],
        )
        .triggers
        .remove(0);
        let slime = counters_on(ObjectScope::GrantingObject, "slime");
        match trigger
            .execute
            .as_ref()
            .and_then(|e| e.sub_ability.as_ref())
            .map(|s| s.effect.as_ref())
        {
            Some(Effect::Token {
                power,
                toughness,
                static_abilities,
                ..
            }) => {
                assert_eq!(*power, PtValue::Quantity(slime.clone()));
                assert_eq!(*toughness, PtValue::Quantity(slime.clone()));
                assert_eq!(
                    static_abilities[0].modifications,
                    vec![
                        ContinuousModification::SetDynamicPower {
                            value: slime.clone()
                        },
                        ContinuousModification::SetDynamicToughness { value: slime },
                    ]
                );
            }
            other => panic!("expected Token, got {other:?}"),
        }
    }

    fn oozes(state: &GameState) -> Vec<ObjectId> {
        state
            .battlefield
            .iter()
            .copied()
            .filter(|id| state.objects[id].name == "Ooze")
            .collect()
    }

    /// Kills a nontoken creature and stops with the Gutter Grime trigger(s) on the stack.
    fn kill_victim_to_triggers(
        scenario: GameScenario,
        victim: ObjectId,
        bolt: ObjectId,
    ) -> GameRunner {
        let mut runner = scenario.build();
        {
            let _cast = runner.cast(bolt).free_cast().target_object(victim).commit();
        }
        for _ in 0..8 {
            if let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for {
                let order = (0..triggers.len()).collect();
                runner.act(GameAction::OrderTriggers { order }).unwrap();
            }
            if runner.state().objects[&victim].zone == Zone::Graveyard
                && !runner.state().stack.is_empty()
            {
                return runner;
            }
            runner.act(GameAction::PassPriority).unwrap();
        }
        panic!(
            "trigger never reached the stack: {:?}",
            runner.state().waiting_for
        );
    }

    #[test]
    fn gutter_grime_token_survives_at_the_granters_count() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let gg = add_gutter_grime(&mut scenario, 2);
        let victim = scenario.add_creature(P0, "Victim", 1, 1).id();
        let bolt = scenario.add_bolt_to_hand(P0);
        let mut runner = kill_victim_to_triggers(scenario, victim, bolt);
        runner.advance_until_stack_empty();
        let tokens = oozes(runner.state());
        assert_eq!(tokens.len(), 1);
        let token = &runner.state().objects[&tokens[0]];
        assert_eq!((token.power, token.toughness), (Some(3), Some(3)));
        assert_ne!(tokens[0], gg);
    }

    /// CR 201.5a + CR 707.2: a copy of a Gutter Grime Ooze still names the Gutter Grime that created the original.
    #[test]
    fn gutter_grime_ooze_copy_reads_the_original_granter() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let gg = add_gutter_grime(&mut scenario, 2);
        let populator = scenario.add_creature(P0, "Populator", 1, 1).id();
        let victim = scenario.add_creature(P0, "Victim", 1, 1).id();
        let bolt = scenario.add_bolt_to_hand(P0);
        let mut runner = kill_victim_to_triggers(scenario, victim, bolt);
        runner.advance_until_stack_empty();
        let populate = ResolvedAbility::new(Effect::Populate, vec![], populator, P0);
        resolve_ability_chain(runner.state_mut(), &populate, &mut Vec::new(), 0).unwrap();
        relayer(runner.state_mut());
        let tokens = oozes(runner.state());
        assert_eq!(tokens.len(), 2);
        let granter = incarnation(runner.state(), gg);
        for &t in &tokens {
            let st = runner.state();
            assert_eq!(
                (st.objects[&t].power, st.objects[&t].toughness),
                (Some(3), Some(3))
            );
            assert!(st.objects[&t]
                .static_definitions
                .as_slice()
                .iter()
                .any(|sd| sd.granting_object == Some(granter)));
        }
        runner
            .state_mut()
            .objects
            .get_mut(&gg)
            .unwrap()
            .counters
            .insert(counter("slime"), 4);
        relayer(runner.state_mut());
        for &t in &tokens {
            assert_eq!(runner.state().objects[&t].power, Some(4));
        }
    }

    #[test]
    fn gutter_grime_two_granters_each_latch_their_own() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        add_gutter_grime(&mut scenario, 2);
        add_gutter_grime(&mut scenario, 5);
        let victim = scenario.add_creature(P0, "Victim", 1, 1).id();
        let bolt = scenario.add_bolt_to_hand(P0);
        let mut runner = kill_victim_to_triggers(scenario, victim, bolt);
        runner.advance_until_stack_empty();
        let mut sizes: Vec<_> = oozes(runner.state())
            .iter()
            .map(|id| runner.state().objects[id].power)
            .collect();
        sizes.sort();
        assert_eq!(sizes, vec![Some(3), Some(6)]);
    }

    #[test]
    fn gutter_grime_blinked_before_resolution_latches_the_old_object() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let gg = add_gutter_grime(&mut scenario, 2);
        let victim = scenario.add_creature(P0, "Victim", 1, 1).id();
        let bolt = scenario.add_bolt_to_hand(P0);
        let mut runner = kill_victim_to_triggers(scenario, victim, bolt);
        {
            let st = runner.state_mut();
            move_to_zone(st, gg, Zone::Exile, &mut Vec::new());
            move_to_zone(st, gg, Zone::Battlefield, &mut Vec::new());
            st.objects
                .get_mut(&gg)
                .unwrap()
                .counters
                .insert(counter("slime"), 4);
        }
        let events = resolve_stack(&mut runner);
        let token = events
            .iter()
            .find_map(|e| match e {
                GameEvent::TokenCreated {
                    object_id, name, ..
                } if name == "Ooze" => Some(*object_id),
                _ => None,
            })
            .expect("the trigger created the token");
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), .. } if *object_id == token
        )));
        assert!(oozes(runner.state()).is_empty());
    }

    #[test]
    fn gutter_grime_ooze_stays_at_its_granters_count() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let gg = add_gutter_grime(&mut scenario, 2);
        let victim = scenario.add_creature(P0, "Victim", 1, 1).id();
        let bolt = scenario.add_bolt_to_hand(P0);
        let mut runner = kill_victim_to_triggers(scenario, victim, bolt);
        runner.advance_until_stack_empty();
        let tokens = oozes(runner.state());
        assert_eq!(tokens.len(), 1);
        assert_ne!(tokens[0], gg);
        let token = &runner.state().objects[&tokens[0]];
        assert_eq!((token.power, token.toughness), (Some(3), Some(3)));
        runner
            .state_mut()
            .objects
            .get_mut(&gg)
            .unwrap()
            .counters
            .insert(counter("slime"), 4);
        relayer(runner.state_mut());
        let token = &runner.state().objects[&tokens[0]];
        assert_eq!((token.power, token.toughness), (Some(4), Some(4)));
    }

    #[test]
    fn gutter_grime_existing_token_drops_to_zero_power_when_its_creator_is_blinked() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let gg = add_gutter_grime(&mut scenario, 2);
        let victim = scenario.add_creature(P0, "Victim", 1, 1).id();
        let bolt = scenario.add_bolt_to_hand(P0);
        let mut runner = kill_victim_to_triggers(scenario, victim, bolt);
        runner.advance_until_stack_empty();
        let token = oozes(runner.state())[0];
        assert_eq!(runner.state().objects[&token].power, Some(3));
        {
            let st = runner.state_mut();
            move_to_zone(st, gg, Zone::Exile, &mut Vec::new());
            move_to_zone(st, gg, Zone::Battlefield, &mut Vec::new());
            st.objects
                .get_mut(&gg)
                .unwrap()
                .counters
                .insert(counter("slime"), 4);
        }
        relayer(runner.state_mut());
        assert_eq!(runner.state().objects[&token].power, Some(0));
    }

    #[test]
    fn gutter_grime_leaving_makes_its_token_zero() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let gg = add_gutter_grime(&mut scenario, 2);
        let victim = scenario.add_creature(P0, "Victim", 1, 1).id();
        let bolt = scenario.add_bolt_to_hand(P0);
        let mut runner = kill_victim_to_triggers(scenario, victim, bolt);
        runner.advance_until_stack_empty();
        let token = oozes(runner.state())[0];
        assert_eq!(runner.state().objects[&token].power, Some(3));
        move_to_zone(runner.state_mut(), gg, Zone::Graveyard, &mut Vec::new());
        relayer(runner.state_mut());
        assert_eq!(runner.state().objects[&token].power, Some(0));
    }

    fn charge_power_grant() -> StaticDefinition {
        let inner = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::SetDynamicPower {
                value: counters_on(ObjectScope::GrantingObject, "charge"),
            }]);
        StaticDefinition::continuous()
            .affected(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EquippedBy]),
            ))
            .modifications(vec![ContinuousModification::GrantStaticAbility {
                definition: Box::new(inner),
            }])
    }

    #[test]
    fn granted_static_reads_its_granters_counters() {
        let mut scenario = GameScenario::new();
        let host = scenario.add_creature(P0, "Bearer", 2, 2).id();
        let other_host = scenario.add_creature(P0, "Other Bearer", 2, 2).id();
        let granter = scenario.add_creature(P0, "Charger", 0, 0).id();
        let other = scenario.add_creature(P0, "Other Charger", 0, 0).id();
        scenario.with_counter(host, counter("charge"), 1);
        scenario.with_counter(granter, counter("charge"), 3);
        scenario.with_counter(other, counter("charge"), 5);
        let mut runner = scenario.build();
        attach(
            &mut runner,
            granter,
            host,
            CoreType::Artifact,
            "Equipment",
            charge_power_grant(),
        );
        attach(
            &mut runner,
            other,
            other_host,
            CoreType::Artifact,
            "Equipment",
            charge_power_grant(),
        );
        let st = runner.state();
        assert_eq!(st.objects[&host].power, Some(3));
        assert_eq!(st.objects[&other_host].power, Some(5));
        let (installed, stamp) = st.objects[&host]
            .static_definitions
            .as_slice()
            .iter()
            .find_map(|s| {
                s.modifications.iter().find_map(|m| match m {
                    ContinuousModification::SetDynamicPower { value } => {
                        Some((value.clone(), s.granting_object))
                    }
                    _ => None,
                })
            })
            .expect("the granted static is installed on the host");
        // CR 201.5a: the installed carrier names the granter's current incarnation.
        assert_eq!(
            installed,
            counters_on(
                ObjectScope::SpecificObject {
                    object: incarnation(st, granter)
                },
                "charge"
            )
        );
        assert_eq!(stamp, Some(incarnation(st, granter)));
    }

    fn blunderbuss_grant() -> StaticDefinition {
        let mut grant = grant_static(
            DIRE_BLUNDERBUSS,
            "Dire Blunderbuss",
            "Artifact",
            "Equipment",
        );
        match granted_execute(&mut grant).effect.as_ref() {
            Effect::Sacrifice {
                target: TargetFilter::Typed(typed),
                ..
            } => {
                assert!(typed.properties.contains(&FilterProp::DistinctFrom {
                    reference: Box::new(TargetFilter::GrantingObject { bound: None }),
                }));
                assert!(!typed.properties.contains(&FilterProp::Another));
            }
            other => panic!("expected a typed Sacrifice, got {other:?}"),
        }
        grant
    }

    #[test]
    fn dire_blunderbuss_cannot_sacrifice_itself() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let host = scenario.add_creature(P0, "Bearer", 2, 2).id();
        let db = scenario.add_creature(P0, "Dire Blunderbuss", 0, 0).id();
        let relic = scenario.add_creature(P0, "Relic", 0, 0).id();
        let idol = scenario.add_creature(P0, "Idol", 0, 0).id();
        let mut runner = scenario.build();
        make_artifact(runner.state_mut(), relic);
        make_artifact(runner.state_mut(), idol);
        attach(
            &mut runner,
            db,
            host,
            CoreType::Artifact,
            "Equipment",
            blunderbuss_grant(),
        );
        attack_with(&mut runner, host);
        drive(&mut runner, None);
        let choices = match &runner.state().waiting_for {
            WaitingFor::EffectZoneChoice {
                cards,
                effect_kind: EffectKind::Sacrifice,
                ..
            } => cards.clone(),
            other => panic!("expected a sacrifice choice, got {other:?}"),
        };
        assert!(
            choices.contains(&relic) && choices.contains(&idol),
            "{choices:?}"
        );
        assert!(!choices.contains(&db), "{choices:?}");
    }

    #[test]
    fn distinct_from_a_bound_granter_excludes_it_across_trigger_batches() {
        let mut scenario = GameScenario::new();
        let host = scenario.add_creature(P0, "Host", 2, 2).id();
        let granter = scenario.add_creature(P0, "Granter", 0, 0).id();
        let other = scenario.add_creature(P0, "Other", 0, 0).id();
        let mut runner = scenario.build();
        make_artifact(runner.state_mut(), granter);
        make_artifact(runner.state_mut(), other);
        let st = runner.state_mut();
        st.current_trigger_events = vec![
            GameEvent::PermanentTapped {
                object_id: host,
                caused_by: None,
                incarnation: None,
            },
            GameEvent::PermanentTapped {
                object_id: other,
                caused_by: None,
                incarnation: None,
            },
        ];
        let st = runner.state();
        let specific = Box::new(TargetFilter::SpecificObject { id: granter });
        let distinct =
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact).properties(vec![
                FilterProp::DistinctFrom {
                    reference: specific.clone(),
                },
            ]));
        let control = TargetFilter::And {
            filters: vec![
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                TargetFilter::Not { filter: specific },
            ],
        };
        let ctx = FilterContext::from_source(st, host);
        for filter in [&distinct, &control] {
            assert!(!matches_target_filter(st, granter, filter, &ctx));
            assert!(matches_target_filter(st, other, filter, &ctx));
        }
    }

    #[test]
    fn nettlevine_blight_attach_moves_the_granter() {
        let mut grant = grant_static(
            NETTLEVINE_BLIGHT,
            "Nettlevine Blight",
            "Enchantment",
            "Aura",
        );
        match granted_execute(&mut grant)
            .sub_ability
            .as_mut()
            .map(|s| s.effect.as_mut())
        {
            Some(Effect::Attach { attachment, .. }) => {
                assert_eq!(*attachment, TargetFilter::GrantingObject { bound: None });
            }
            other => panic!("expected Attach, got {other:?}"),
        }
        let mut scenario = GameScenario::new();
        let host = scenario.add_creature(P0, "Bearer", 2, 2).id();
        let blight = scenario.add_creature(P0, "Nettlevine Blight", 0, 0).id();
        let mut runner = scenario.build();
        attach(
            &mut runner,
            blight,
            host,
            CoreType::Enchantment,
            "Aura",
            grant,
        );
        let (attachment, stamp) = runner.state().objects[&host]
            .trigger_definitions
            .as_slice()
            .iter()
            .find_map(|t| {
                match t
                    .definition
                    .execute
                    .as_ref()?
                    .sub_ability
                    .as_ref()?
                    .effect
                    .as_ref()
                {
                    Effect::Attach { attachment, .. } => {
                        Some((attachment.clone(), t.definition.granting_object))
                    }
                    _ => None,
                }
            })
            .expect("the granted trigger is on the host");
        assert_eq!(
            attachment,
            TargetFilter::GrantingObject {
                bound: Some(incarnation(runner.state(), blight))
            }
        );
        assert_eq!(stamp, Some(incarnation(runner.state(), blight)));
        assert_ne!(blight, host);
    }

    fn heliods_punishment_grant() -> StaticDefinition {
        grant_static(
            HELIODS_PUNISHMENT,
            "Heliod's Punishment",
            "Enchantment",
            "Aura",
        )
    }

    #[test]
    fn heliods_punishment_counts_and_destroys_the_granter() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let host = scenario.add_creature(P0, "Bearer", 2, 2).id();
        let hp = scenario.add_creature(P0, "Heliod's Punishment", 0, 0).id();
        scenario.with_counter(hp, counter("task"), 2);
        scenario.with_counter(host, counter("task"), 1);
        let mut runner = scenario.build();
        attach(
            &mut runner,
            hp,
            host,
            CoreType::Enchantment,
            "Aura",
            heliods_punishment_grant(),
        );
        let index = runner.state().objects[&host].abilities.len() - 1;

        activate(&mut runner, host, index, None);
        runner.advance_until_stack_empty();
        let st = runner.state();
        assert_eq!(st.objects[&hp].counters.get(&counter("task")), Some(&1));
        assert_eq!(st.objects[&host].counters.get(&counter("task")), Some(&1));
        assert_eq!(st.objects[&hp].zone, Zone::Battlefield);

        runner.state_mut().objects.get_mut(&host).unwrap().tapped = false;
        activate(&mut runner, host, index, None);
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&hp].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&host].zone, Zone::Battlefield);
    }

    #[test]
    fn bound_condition_reads_the_granter_not_the_host() {
        for (granter_n, host_n, bound_runs, source_runs) in
            [(0, 2, true, false), (1, 0, false, true)]
        {
            let mut scenario = GameScenario::new();
            let host = scenario.add_creature(P0, "Host", 2, 2).id();
            let granter = scenario.add_creature(P0, "Granter", 0, 3).id();
            scenario.with_counter(host, counter("task"), host_n);
            scenario.with_counter(granter, counter("task"), granter_n);
            let mut runner = scenario.build();
            let granter_scope = bound(runner.state(), granter);
            for (scope, runs) in [
                (granter_scope, bound_runs),
                (ObjectScope::Source, source_runs),
            ] {
                let mut st = runner.state_mut().clone();
                let life = st.players[0].life;
                let gain = |n| Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: n },
                    player: TargetFilter::Controller,
                };
                let mut sub = ResolvedAbility::new(gain(1), vec![], host, P0);
                sub.condition = Some(AbilityCondition::QuantityCheck {
                    lhs: counters_on(scope, "task"),
                    comparator: Comparator::EQ,
                    rhs: QuantityExpr::Fixed { value: 0 },
                });
                let mut root = ResolvedAbility::new(gain(10), vec![], host, P0);
                root.sub_ability = Some(Box::new(sub));
                resolve_ability_chain(&mut st, &root, &mut Vec::new(), 0).unwrap();
                assert_eq!(
                    st.players[0].life - life,
                    if runs { 11 } else { 10 },
                    "{scope:?}"
                );
            }
        }
    }

    const UPKEEP_GAIN: &str =
        "Equipped creature has \"At the beginning of your upkeep, you gain 1 life.\"";

    fn upkeep_grant(granter_counters: bool) -> StaticDefinition {
        let mut grant = grant_static(UPKEEP_GAIN, "Charger", "Artifact", "Equipment");
        if granter_counters {
            match granted_execute(&mut grant).effect.as_mut() {
                Effect::GainLife { amount, .. } => {
                    *amount = counters_on(ObjectScope::GrantingObject, "charge")
                }
                other => panic!("expected GainLife, got {other:?}"),
            }
        }
        grant
    }

    fn upkeep_runner(pairs: usize, granter_counters: bool) -> GameRunner {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::Untap);
        let pairs: Vec<(ObjectId, ObjectId)> = (0..pairs)
            .map(|i| {
                let host = scenario.add_creature(P0, "Bearer", 2, 2).id();
                let granter = scenario.add_creature(P0, "Charger", 0, 0).id();
                scenario.with_counter(host, counter("charge"), 1);
                scenario.with_counter(granter, counter("charge"), 2 + i as u32);
                (host, granter)
            })
            .collect();
        let mut runner = scenario.build();
        for (host, granter) in pairs {
            attach(
                &mut runner,
                granter,
                host,
                CoreType::Artifact,
                "Equipment",
                upkeep_grant(granter_counters),
            );
        }
        runner.advance_to_upkeep();
        runner
    }

    const CHARGE_ANTHEM: &str = "Creatures you control have \"At the beginning of your upkeep, put a charge counter on each artifact you control. Then put a +1/+1 counter on this creature.\"";

    /// P0's two creatures each carry one artifact granter's upkeep trigger, whose
    /// second effect becomes `second` (the parsed +1/+1 counter when `None`).
    fn anthem_runner(second: Option<Effect>) -> GameRunner {
        let mut grant = grant_static(CHARGE_ANTHEM, "Charger", "Artifact", "Equipment");
        if let Some(effect) = second {
            *granted_execute(&mut grant)
                .sub_ability
                .as_mut()
                .unwrap()
                .effect = effect;
        }
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::Untap);
        scenario.add_creature(P0, "Bearer", 2, 2);
        scenario.add_creature(P0, "Bearer", 2, 2);
        let charger = scenario.add_creature(P0, "Charger", 0, 0).id();
        let mut runner = scenario.build();
        let st = runner.state_mut();
        make_artifact(st, charger);
        let obj = st.objects.get_mut(&charger).unwrap();
        obj.static_definitions.push(grant.clone());
        Arc::make_mut(&mut obj.base_static_definitions).push(grant);
        relayer(st);
        runner.advance_to_upkeep();
        runner
    }

    fn granter_charge() -> PtValue {
        PtValue::Quantity(counters_on(ObjectScope::GrantingObject, "charge"))
    }

    /// CR 603.3b: `second` reads the granter's charge counters, which each sibling's
    /// first effect raises, so the two triggers' order is observable.
    fn assert_shared_granter_reads_need_ordering(second: Effect) {
        let runner = anthem_runner(Some(second));
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::OrderTriggers { .. }
        ));
    }

    #[test]
    fn granted_counter_placement_reading_the_shared_granter_needs_ordering() {
        assert_shared_granter_reads_need_ordering(Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: counters_on(ObjectScope::GrantingObject, "charge"),
            target: TargetFilter::SelfRef,
        });
    }

    #[test]
    fn granted_pump_by_the_shared_granters_counters_needs_ordering() {
        assert_shared_granter_reads_need_ordering(Effect::Pump {
            power: granter_charge(),
            toughness: PtValue::Fixed(0),
            target: TargetFilter::SelfRef,
        });
    }

    #[test]
    fn granted_animate_to_the_shared_granters_counters_needs_ordering() {
        assert_shared_granter_reads_need_ordering(Effect::Animate {
            power: Some(granter_charge()),
            toughness: Some(granter_charge()),
            types: vec![],
            remove_types: vec![],
            target: TargetFilter::SelfRef,
            keywords: vec![],
        });
    }

    /// CR 603.3b: with no granter read the two triggers commute and are auto-ordered.
    #[test]
    fn granted_triggers_not_reading_the_shared_granter_are_auto_ordered() {
        let runner = anthem_runner(None);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert_eq!(runner.state().stack.len(), 2);
    }

    #[test]
    fn granted_trigger_gains_the_granters_counters() {
        let mut runner = upkeep_runner(1, true);
        let life = runner.state().players[0].life;
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().players[0].life - life, 2);
    }

    #[test]
    fn granted_triggers_bound_to_distinct_granters_need_ordering() {
        let mut runner = upkeep_runner(2, true);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::OrderTriggers { .. }
        ));
        let life = runner.state().players[0].life;
        drive(&mut runner, None);
        runner.advance_until_stack_empty();
        // Each trigger gains its own granter's 2 and 3 charge counters, not the hosts' 1.
        assert_eq!(runner.state().players[0].life - life, 5);
        let control = upkeep_runner(2, false);
        assert!(matches!(
            control.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert_eq!(control.state().stack.len(), 2);
    }

    /// Two "Grower"s with one and two charge counters, each with its own unstamped
    /// upkeep trigger putting `count` charge counters on each creature P0 controls.
    fn own_upkeep_charge_runner(count: QuantityExpr) -> GameRunner {
        let mut trigger = parse_oracle_text(
            "At the beginning of your upkeep, put a charge counter on each creature you control.",
            "Grower",
            &[],
            &["Creature".to_string()],
            &[],
        )
        .triggers
        .remove(0);
        match trigger.execute.as_mut().map(|e| e.effect.as_mut()) {
            Some(Effect::PutCounterAll { count: c, .. }) => *c = count,
            other => panic!("expected PutCounterAll, got {other:?}"),
        }
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::Untap);
        for n in 1..=2 {
            let id = scenario
                .add_creature(P0, "Grower", 1, 1)
                .with_trigger_definition(trigger.clone())
                .id();
            scenario.with_counter(id, counter("charge"), n);
        }
        let mut runner = scenario.build();
        runner.advance_to_upkeep();
        runner
    }

    /// CR 603.3b: unstamped, each trigger counts its own source's charge counters,
    /// which the other trigger raises, so the order is observable.
    #[test]
    fn unstamped_granter_reads_on_distinct_sources_need_ordering() {
        let runner = own_upkeep_charge_runner(counters_on(ObjectScope::GrantingObject, "charge"));
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::OrderTriggers { .. }
        ));
        let control = own_upkeep_charge_runner(QuantityExpr::Fixed { value: 1 });
        assert!(matches!(
            control.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert_eq!(control.state().stack.len(), 2);
    }

    #[test]
    fn spare_dagger_sacrifices_the_dagger_and_deals_damage() {
        let grant = grant_static(SPARE_DAGGER, "Spare Dagger", "Artifact", "Equipment");
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let host = scenario.add_creature(P0, "Bearer", 2, 2).id();
        let dagger = scenario.add_creature(P0, "Spare Dagger", 0, 0).id();
        let mut runner = scenario.build();
        attach(
            &mut runner,
            dagger,
            host,
            CoreType::Artifact,
            "Equipment",
            grant,
        );
        let life = runner.state().players[1].life;
        attack_with(&mut runner, host);
        drive(&mut runner, Some(TargetRef::Player(P1)));
        let st = runner.state();
        assert_eq!(st.objects[&dagger].zone, Zone::Graveyard);
        assert_eq!(st.objects[&host].zone, Zone::Battlefield);
        assert_eq!(st.players[1].life, life - 1);
    }

    /// CR 201.5a: every condition shape reads its `GrantingObject` quantity from the stamped granter.
    #[test]
    fn every_condition_and_quantity_arm_binds_the_granter() {
        let g = || counters_on(ObjectScope::GrantingObject, "charge");
        let fixed = |value| QuantityExpr::Fixed { value };
        let at_least = |lhs, rhs| AbilityCondition::QuantityCheck {
            lhs,
            comparator: Comparator::GE,
            rhs,
        };
        // Each arm holds iff the charge count it reads is 3.
        let arms = [
            at_least(
                QuantityExpr::Difference {
                    left: Box::new(QuantityExpr::Offset {
                        inner: Box::new(g()),
                        offset: 1,
                    }),
                    right: Box::new(QuantityExpr::Sum {
                        exprs: vec![fixed(1)],
                    }),
                },
                fixed(3),
            ),
            AbilityCondition::And {
                conditions: vec![at_least(g(), fixed(3))],
            },
            AbilityCondition::Or {
                conditions: vec![AbilityCondition::Not {
                    condition: Box::new(at_least(fixed(2), g())),
                }],
            },
            AbilityCondition::ConditionInstead {
                inner: Box::new(at_least(g(), fixed(3))),
            },
            AbilityCondition::Not {
                condition: Box::new(AbilityCondition::PreviousEffectAmount {
                    comparator: Comparator::GE,
                    rhs: g(),
                    channel: Default::default(),
                }),
            },
        ];
        for (index, arm) in arms.into_iter().enumerate() {
            let mut gained = Vec::new();
            for (granter_n, host_n) in [(3, 0), (0, 3)] {
                let mut def = AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::GainLife {
                        amount: fixed(1),
                        player: TargetFilter::Controller,
                    },
                )
                .cost(AbilityCost::Tap);
                def.condition = Some(arm.clone());
                let grant = StaticDefinition::continuous()
                    .affected(TargetFilter::Typed(
                        TypedFilter::creature().properties(vec![FilterProp::EquippedBy]),
                    ))
                    .modifications(vec![ContinuousModification::GrantAbility {
                        definition: Box::new(def),
                    }]);
                let mut scenario = GameScenario::new();
                scenario.at_phase(Phase::PreCombatMain);
                let host = scenario.add_creature(P0, "Bearer", 2, 2).id();
                let granter = scenario.add_creature(P0, "Charger", 0, 0).id();
                scenario.with_counter(host, counter("charge"), host_n);
                scenario.with_counter(granter, counter("charge"), granter_n);
                let mut runner = scenario.build();
                attach(
                    &mut runner,
                    granter,
                    host,
                    CoreType::Artifact,
                    "Equipment",
                    grant,
                );
                let index_on_host = runner.state().objects[&host].abilities.len() - 1;
                assert_eq!(
                    runner.state().objects[&host].abilities[index_on_host].granting_object,
                    Some(incarnation(runner.state(), granter))
                );
                let life = runner.state().players[0].life;
                activate(&mut runner, host, index_on_host, None);
                runner.advance_until_stack_empty();
                gained.push(runner.state().players[0].life - life);
            }
            assert_eq!(gained, vec![1, 0], "arm {index}");
        }
    }

    #[test]
    fn trusty_boomerang_taps_the_target_and_returns_itself() {
        let grant = grant_static(
            TRUSTY_BOOMERANG,
            "Trusty Boomerang",
            "Artifact",
            "Equipment",
        );
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(
                ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            )],
        );
        let host = scenario.add_creature(P0, "Bearer", 2, 2).id();
        let boomerang = scenario.add_creature(P0, "Trusty Boomerang", 0, 0).id();
        let victim = scenario.add_creature(P1, "Victim", 2, 2).id();
        let mut runner = scenario.build();
        attach(
            &mut runner,
            boomerang,
            host,
            CoreType::Artifact,
            "Equipment",
            grant,
        );
        runner
            .state_mut()
            .objects
            .get_mut(&boomerang)
            .unwrap()
            .keywords
            .push(Keyword::Shroud);
        let index = runner.state().objects[&host].abilities.len() - 1;
        activate(&mut runner, host, index, Some(victim));
        runner.advance_until_stack_empty();
        let st = runner.state();
        assert!(st.objects[&victim].tapped);
        assert_eq!(st.objects[&boomerang].zone, Zone::Hand);
        assert_eq!(st.objects[&host].zone, Zone::Battlefield);
    }

    /// CR 400.7: a Trusty Boomerang blinked in response is a new object, so its granted ability does not return it.
    #[test]
    fn trusty_boomerang_blinked_in_response_stays() {
        let grant = grant_static(
            TRUSTY_BOOMERANG,
            "Trusty Boomerang",
            "Artifact",
            "Equipment",
        );
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(
                ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            )],
        );
        let host = scenario.add_creature(P0, "Bearer", 2, 2).id();
        let boomerang = scenario.add_creature(P0, "Trusty Boomerang", 0, 0).id();
        let victim = scenario.add_creature(P1, "Victim", 2, 2).id();
        let mut runner = scenario.build();
        attach(
            &mut runner,
            boomerang,
            host,
            CoreType::Artifact,
            "Equipment",
            grant,
        );
        let index = runner.state().objects[&host].abilities.len() - 1;
        activate(&mut runner, host, index, Some(victim));
        let st = runner.state_mut();
        move_to_zone(st, boomerang, Zone::Exile, &mut Vec::new());
        move_to_zone(st, boomerang, Zone::Battlefield, &mut Vec::new());
        runner.advance_until_stack_empty();
        let st = runner.state();
        assert!(st.objects[&victim].tapped);
        assert_eq!(st.objects[&boomerang].zone, Zone::Battlefield);
        assert_eq!(st.objects[&host].zone, Zone::Battlefield);
    }

    #[test]
    fn heliods_punishment_cast_enters_with_four_and_destroys_itself_on_the_fourth() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let host = scenario.add_creature(P0, "Bearer", 2, 2).id();
        let hp = scenario
            .add_spell_to_hand(P0, "Heliod's Punishment", false)
            .as_enchantment()
            .with_subtypes(vec!["Aura"])
            .from_oracle_text(HELIODS_PUNISHMENT)
            .with_keyword(Keyword::Enchant(TargetFilter::Typed(
                TypedFilter::creature(),
            )))
            .with_mana_cost(ManaCost::generic(0))
            .id();
        let mut runner = scenario.build();
        runner.cast(hp).target_object(host).resolve();
        let st = runner.state();
        assert_eq!(st.objects[&hp].zone, Zone::Battlefield);
        assert_eq!(
            st.objects[&hp].attached_to,
            Some(AttachTarget::Object(host))
        );
        assert_eq!(st.objects[&hp].counters.get(&counter("task")), Some(&4));
        for remaining in [3, 2, 1] {
            runner.state_mut().objects.get_mut(&host).unwrap().tapped = false;
            let index = runner.state().objects[&host].abilities.len() - 1;
            activate(&mut runner, host, index, None);
            runner.advance_until_stack_empty();
            let st = runner.state();
            assert_eq!(
                st.objects[&hp].counters.get(&counter("task")),
                Some(&remaining)
            );
            assert_eq!(st.objects[&hp].zone, Zone::Battlefield);
            assert_eq!(st.objects[&host].zone, Zone::Battlefield);
        }
        runner.state_mut().objects.get_mut(&host).unwrap().tapped = false;
        let index = runner.state().objects[&host].abilities.len() - 1;
        activate(&mut runner, host, index, None);
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&hp].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&host].zone, Zone::Battlefield);
    }

    /// CR 201.5a + CR 601.2h: the cost removes Hankyu's counters, not the host's.
    #[test]
    fn hankyu_remove_all_reads_the_granter() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let host = scenario.add_creature(P0, "Bearer", 2, 2).id();
        scenario.with_counter(host, counter("aim"), 1);
        let hk = scenario
            .add_artifact_from_oracle(P0, "Hankyu", HANKYU)
            .with_subtypes(vec!["Equipment"])
            .id();
        scenario.with_counter(hk, counter("aim"), 2);
        let mut runner = scenario.build();
        assert_ne!(hk, host);
        attach_to(runner.state_mut(), hk, host);
        relayer(runner.state_mut());
        let index = runner.state().objects[&host].abilities.len() - 1;
        runner
            .act(GameAction::ActivateAbility {
                source_id: host,
                ability_index: index,
            })
            .unwrap();
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ) {
            runner
                .act(GameAction::SelectTargets {
                    targets: vec![TargetRef::Player(P1)],
                })
                .unwrap();
        }
        assert!(
            !matches!(runner.state().waiting_for, WaitingFor::PayCost { .. }),
            "{:?}",
            runner.state().waiting_for
        );
        runner.advance_until_stack_empty();
        let st = runner.state();
        assert_eq!(st.objects[&hk].counters.get(&counter("aim")), None);
        assert_eq!(st.objects[&host].counters.get(&counter("aim")), Some(&1));
        assert!(st.objects[&host].tapped);
        assert!(st.stack.is_empty());
    }

    /// CR 201.5a + CR 118.3: "Tap Fishing Pole" taps the granter, and can't be paid while it is tapped.
    #[test]
    fn fishing_pole_tap_cost_taps_the_granter() {
        for pole_tapped in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            scenario.with_mana_pool(
                P0,
                vec![ManaUnit::new(
                    ManaType::Colorless,
                    ObjectId(0),
                    false,
                    vec![],
                )],
            );
            let host = scenario.add_creature(P0, "Bearer", 2, 2).id();
            let pole = scenario
                .add_artifact_from_oracle(P0, "Fishing Pole", FISHING_POLE)
                .with_subtypes(vec!["Equipment"])
                .id();
            let mut runner = scenario.build();
            attach_to(runner.state_mut(), pole, host);
            relayer(runner.state_mut());
            runner.state_mut().objects.get_mut(&pole).unwrap().tapped = pole_tapped;
            let index = runner.state().objects[&host].abilities.len() - 1;
            let result = runner.act(GameAction::ActivateAbility {
                source_id: host,
                ability_index: index,
            });
            if pole_tapped {
                assert!(result.is_err(), "{:?}", runner.state().waiting_for);
                assert!(!runner.state().objects[&host].tapped);
                continue;
            }
            result.unwrap();
            runner.advance_until_stack_empty();
            let st = runner.state();
            assert!(st.objects[&pole].tapped);
            assert!(st.objects[&host].tapped);
            assert_eq!(st.objects[&pole].counters.get(&counter("bait")), Some(&1));
            assert_eq!(st.objects[&host].counters.get(&counter("bait")), None);
        }
    }

    /// CR 118.3: "Tap enchanted creature" taps the Aura's host, not the Aura.
    #[test]
    fn krovikan_plague_tap_cost_taps_the_enchanted_creature() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let host = scenario.add_creature(P0, "Bearer", 2, 2).id();
        let plague = scenario
            .add_enchantment_from_oracle(P0, "Krovikan Plague", KROVIKAN_PLAGUE)
            .with_subtypes(vec!["Aura"])
            .id();
        let mut runner = scenario.build();
        attach_to(runner.state_mut(), plague, host);
        relayer(runner.state_mut());
        let life = runner.state().players[1].life;
        let index = runner.state().objects[&plague]
            .abilities
            .iter()
            .position(|a| a.kind == AbilityKind::Activated)
            .unwrap();
        runner
            .act(GameAction::ActivateAbility {
                source_id: plague,
                ability_index: index,
            })
            .unwrap();
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ) {
            runner
                .act(GameAction::SelectTargets {
                    targets: vec![TargetRef::Player(P1)],
                })
                .unwrap();
        }
        runner.advance_until_stack_empty();
        let st = runner.state();
        assert!(st.objects[&host].tapped);
        assert!(!st.objects[&plague].tapped);
        assert_eq!(st.players[1].life, life - 1);
    }

    #[test]
    fn the_aetherspark_loyalty_lands_on_itself() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let host = scenario.add_creature(P0, "Bearer", 2, 2).id();
        let sp = scenario
            .add_artifact_from_oracle(P0, "The Aetherspark", THE_AETHERSPARK)
            .with_subtypes(vec!["Equipment"])
            .id();
        scenario.with_counter(sp, CounterType::Loyalty, 1);
        let mut runner = scenario.build();
        attach_to(runner.state_mut(), sp, host);
        relayer(runner.state_mut());
        let targets: Vec<(TargetFilter, Option<ObjectIncarnationRef>)> = runner.state().objects
            [&sp]
            .trigger_definitions
            .as_slice()
            .iter()
            .filter_map(|t| t.definition.execute.as_deref())
            .filter_map(|d| match d.effect.as_ref() {
                Effect::PutCounter { target, .. } => Some((target.clone(), d.granting_object)),
                _ => None,
            })
            .collect();
        assert_eq!(
            targets,
            vec![(
                TargetFilter::GrantingObject {
                    bound: Some(incarnation(runner.state(), sp))
                },
                Some(incarnation(runner.state(), sp))
            )]
        );
        let life = runner.state().players[1].life;
        attack_with(&mut runner, host);
        runner.combat_damage();
        runner.advance_until_stack_empty();
        let st = runner.state();
        assert_eq!(st.players[1].life, life - 2);
        assert_eq!(
            st.objects[&sp].counters.get(&CounterType::Loyalty),
            Some(&3)
        );
        assert_eq!(st.objects[&host].counters.get(&CounterType::Loyalty), None);
    }

    #[test]
    fn grothama_granted_fight_fights_grothama() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let attacker = scenario.add_creature(P0, "Bearer", 3, 5).id();
        let grothama = scenario
            .add_creature_from_oracle(P0, "Grothama, All-Devouring", 10, 8, GROTHAMA)
            .as_legendary()
            .id();
        let mut runner = scenario.build();
        relayer(runner.state_mut());
        attack_with(&mut runner, attacker);
        drive(&mut runner, None);
        let st = runner.state();
        assert_ne!(grothama, attacker);
        assert_eq!(st.objects[&grothama].damage_marked, 3);
        assert_eq!(st.objects[&attacker].damage_marked, 10);
        assert_eq!(st.objects[&attacker].zone, Zone::Graveyard);
    }

    /// CR 115.10a + CR 608.2c + CR 608.2d: the untargeted attach choice is made while the
    /// trigger resolves, after the host is sacrificed.
    #[test]
    fn nettlevine_blight_chooses_its_new_host_after_the_sacrifice() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PostCombatMain);
        let host = scenario.add_creature(P0, "Bearer", 2, 2).id();
        let other = scenario.add_creature(P0, "Other", 1, 1).id();
        let shrouded = scenario
            .add_creature(P0, "Shrouded", 1, 1)
            .with_keyword(Keyword::Shroud)
            .id();
        let blight = scenario
            .add_enchantment_from_oracle(P0, "Nettlevine Blight", NETTLEVINE_BLIGHT)
            .with_subtypes(vec!["Aura"])
            .id();
        let mut runner = scenario.build();
        attach_to(runner.state_mut(), blight, host);
        relayer(runner.state_mut());
        runner.advance_to_end_step();
        drive(&mut runner, None);
        let st = runner.state();
        let offered = match &st.waiting_for {
            WaitingFor::EffectZoneChoice { cards, .. } => cards.clone(),
            other => panic!("expected the resolution-time attach choice, got {other:?}"),
        };
        assert_eq!(st.objects[&host].zone, Zone::Graveyard);
        assert!(!offered.contains(&host), "{offered:?}");
        assert!(offered.contains(&shrouded), "{offered:?}");
        assert!(offered.contains(&other), "{offered:?}");
        runner
            .act(GameAction::SelectCards {
                cards: vec![shrouded],
            })
            .unwrap();
        runner.advance_until_stack_empty();
        let st = runner.state();
        assert_eq!(st.objects[&blight].zone, Zone::Battlefield);
        assert_eq!(
            st.objects[&blight].attached_to,
            Some(AttachTarget::Object(shrouded))
        );
    }

    /// CR 201.5a: a grant a granted copy effect adds names the original granter.
    #[test]
    fn granted_copy_riders_grant_reads_the_granter() {
        let mut grant = grant_static(
            "Equipped creature has \"{T}: Draw a card.\"\nEquip {1}",
            "Foo Bar",
            "Artifact",
            "Equipment",
        );
        let rider = ContinuousModification::GrantAbility {
            definition: Box::new(AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: counters_on(ObjectScope::GrantingObject, "charge"),
                    player: TargetFilter::Controller,
                },
            )),
        };
        *granted_ability(&mut grant).effect = Effect::BecomeCopy {
            target: TargetFilter::Typed(TypedFilter::creature()),
            recipient: engine::types::ability::CopyRecipient::Source,
            duration: None,
            mana_value_limit: None,
            additional_modifications: vec![rider],
        };
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let host = scenario.add_creature(P0, "Bearer", 2, 2).id();
        scenario.with_counter(host, counter("charge"), 1);
        let fb = scenario.add_creature(P0, "Foo Bar", 0, 0).id();
        scenario.with_counter(fb, counter("charge"), 3);
        let model = scenario.add_creature(P0, "Model", 1, 1).id();
        let mut runner = scenario.build();
        attach(
            &mut runner,
            fb,
            host,
            CoreType::Artifact,
            "Equipment",
            grant,
        );
        let copy = runner.state().objects[&host]
            .abilities
            .iter()
            .position(|a| matches!(&*a.effect, Effect::BecomeCopy { .. }))
            .expect("the granted copy ability is on the host");
        activate(&mut runner, host, copy, Some(model));
        drive(&mut runner, None);
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&host].name, "Model");
        let gain = runner.state().objects[&host]
            .abilities
            .iter()
            .position(|a| matches!(&*a.effect, Effect::GainLife { .. }))
            .expect("the copy rider's grant is on the host");
        runner.state_mut().objects.get_mut(&host).unwrap().tapped = false;
        let life = runner.state().players[0].life;
        activate(&mut runner, host, gain, None);
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().players[0].life - life, 3);
    }

    /// CR 201.5a + CR 114.4 + CR 509.1b: an emblem a granted ability creates names the
    /// granter, so its "can't block" restriction binds the granter, not the host.
    #[test]
    fn granted_emblems_cant_block_names_the_granter() {
        let mut grant = grant_static(
            "Other creatures you control have \"{T}: Draw a card.\"",
            "Foo Bar",
            "Creature",
            "Human",
        );
        *granted_ability(&mut grant).effect = Effect::CreateEmblem {
            statics: vec![StaticDefinition::new(StaticMode::CantBlock)
                .affected(TargetFilter::GrantingObject { bound: None })],
            triggers: vec![],
        };
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let host = scenario.add_creature(P0, "Bearer", 2, 2).id();
        let fb = scenario.add_creature(P0, "Foo Bar", 2, 2).id();
        let raider = scenario.add_creature(P1, "Raider", 2, 2).id();
        let mut runner = scenario.build();
        let st = runner.state_mut();
        let obj = st.objects.get_mut(&fb).unwrap();
        obj.static_definitions.push(grant.clone());
        Arc::make_mut(&mut obj.base_static_definitions).push(grant);
        relayer(st);
        assert!(can_block_pair(runner.state(), fb, raider));
        let index = runner.state().objects[&host]
            .abilities
            .iter()
            .position(|a| matches!(&*a.effect, Effect::CreateEmblem { .. }))
            .expect("the granted emblem ability is on the host");
        activate(&mut runner, host, index, None);
        runner.advance_until_stack_empty();
        relayer(runner.state_mut());
        assert!(
            runner
                .state()
                .objects
                .values()
                .any(|o| o.zone == Zone::Command
                    && o.static_definitions
                        .as_slice()
                        .iter()
                        .any(|s| s.mode == StaticMode::CantBlock)),
            "reach-guard: the emblem is in the command zone"
        );
        assert!(!can_block_pair(runner.state(), fb, raider));
        assert!(can_block_pair(runner.state(), host, raider));
    }

    /// CR 201.5a + CR 603.2c: a batched trigger counts its subjects through the granter
    /// it admitted them with.
    #[test]
    fn batched_granted_trigger_counts_subjects_against_the_granter() {
        let mut grant = grant_static(
            "Equipped creature has \"Whenever one or more other creatures you control leave the battlefield, draw a card.\"\nEquip {1}",
            "Foo Bar",
            "Artifact",
            "Equipment",
        );
        let trigger = grant
            .modifications
            .iter_mut()
            .find_map(|m| match m {
                ContinuousModification::GrantTrigger { trigger } => Some(trigger),
                _ => None,
            })
            .expect("GrantTrigger");
        assert!(trigger.batched);
        trigger.valid_card = Some(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Artifact).properties(vec![FilterProp::Cmc {
                comparator: Comparator::LT,
                value: counters_on(ObjectScope::GrantingObject, "charge"),
            }]),
        ));
        *trigger.execute.as_mut().unwrap().effect = Effect::Draw {
            count: QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            target: TargetFilter::Controller,
        };
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let host = scenario.add_creature(P0, "Bearer", 2, 2).id();
        let fb = scenario.add_creature(P0, "Foo Bar", 0, 0).id();
        scenario.with_counter(fb, counter("charge"), 3);
        let other = scenario.add_creature(P0, "Other", 0, 0).id();
        let killer = scenario
            .add_creature_from_oracle(P0, "Killer", 3, 3, "{T}: Exile target artifact.")
            .id();
        scenario.with_library_top(P0, &["L1", "L2", "L3"]);
        let mut runner = scenario.build();
        make_artifact(runner.state_mut(), other);
        attach(
            &mut runner,
            fb,
            host,
            CoreType::Artifact,
            "Equipment",
            grant,
        );
        runner
            .state_mut()
            .objects
            .get_mut(&killer)
            .unwrap()
            .summoning_sick = false;
        let hand = runner.state().players[0].hand.len();
        activate(&mut runner, killer, 0, Some(other));
        let mut fired = false;
        while !runner.state().stack.is_empty() {
            fired |= runner.state().stack.iter().any(|e| e.source_id == host);
            runner.act(GameAction::PassPriority).unwrap();
        }
        assert!(fired, "the granted trigger fired");
        drive(&mut runner, None);
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&other].zone, Zone::Exile);
        assert_eq!(runner.state().players[0].hand.len() - hand, 1);
    }
}

// ---------------------------------------------------------------------------
// CR 201.5a granter stamp: every definition node of a granted body carries its
// granter's incarnation, and resolution-time readers bind GrantingObject to it.
// ---------------------------------------------------------------------------

mod granter_stamp {
    use engine::game::contraptions::resolve as resolve_contraptions;
    use engine::game::deck_loading::create_contraption_deck_card;
    use engine::game::effects::attach::attach_to;
    use engine::game::effects::resolve_ability_chain;
    use engine::game::filter::{matches_target_filter, FilterContext};
    use engine::game::layers::evaluate_layers;
    use engine::game::mana_abilities::can_activate_mana_ability_now;
    use engine::game::quantity::resolve_quantity_with_targets;
    use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
    use engine::game::targeting::resolved_targets;
    use engine::game::zones::move_to_zone;
    use engine::types::ability::{
        AbilityCondition, AbilityDefinition, AbilityKind, CardPlayMode, CastingPermission,
        Comparator, ContinuousModification, Effect, EffectScope, ObjectScope,
        PlayFromExileProvenance, PlayerFilter, PlayerRelation, PlayerScope, PtValue, QuantityExpr,
        QuantityModification, QuantityRef, ReplacementDefinition, ResolvedAbility, SpellContext,
        TapStateChange, TargetFilter, TargetRef, TriggerDefinition,
    };
    use engine::types::actions::GameAction;
    use engine::types::card::CardFace;
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::counter::CounterType;
    use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
    use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
    use engine::types::mana::{ManaCost, ManaType};
    use engine::types::phase::Phase;
    use engine::types::replacements::ReplacementEvent;
    use engine::types::statics::CastFrequency;
    use engine::types::triggers::TriggerMode;
    use engine::types::zones::{EtbTapState, Zone};
    use std::sync::Arc;

    const EXCLUSION_COUNT: &str =
        "{T}: Put a +1/+1 counter on this creature for each artifact you control other than Foo Bar.";
    const COUNTERS_PUMP: &str = "{T}: This creature gets +X/+0 until end of turn, where X is the number of +1/+1 counters on Foo Bar.";

    struct Board {
        runner: GameRunner,
        host: ObjectId,
        granters: Vec<ObjectId>,
        other: Option<ObjectId>,
    }

    fn relayer(state: &mut GameState) {
        state.layers_dirty.mark_full();
        evaluate_layers(state);
    }

    /// P0's 2/2 host with one +1/+1 counter, equipped by one "Foo Bar" (MV 5) per
    /// entry of `granter_counters` granting `body`; `other` adds a MV-2 artifact.
    fn board_with(body: &str, granter_counters: &[u32], other: bool) -> Board {
        board_full(body, granter_counters, other.then_some(""), "")
    }

    /// `board_with`, where `other` is the other artifact's Oracle text and
    /// `granter_extra` adds lines to Foo Bar's; P1 controls a 2/2 "Victim".
    fn board_full(
        body: &str,
        granter_counters: &[u32],
        other: Option<&str>,
        granter_extra: &str,
    ) -> Board {
        board_built(body, granter_counters, other, granter_extra, |_| {})
    }

    /// `board_full`, where `add` places further objects before the build.
    fn board_built(
        body: &str,
        granter_counters: &[u32],
        other: Option<&str>,
        granter_extra: &str,
        add: impl FnOnce(&mut GameScenario),
    ) -> Board {
        let text = format!("Equipped creature has \"{body}\"\n{granter_extra}Equip {{1}}");
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let host = scenario.add_creature(P0, "Bearer", 2, 2).id();
        let granters: Vec<ObjectId> = granter_counters
            .iter()
            .map(|&n| {
                let fb = scenario
                    .add_artifact_from_oracle(P0, "Foo Bar", &text)
                    .with_subtypes(vec!["Equipment"])
                    .with_mana_cost(ManaCost::generic(5))
                    .id();
                scenario.with_counter(fb, CounterType::Plus1Plus1, n);
                fb
            })
            .collect();
        scenario.with_library_top(P0, &["L1", "L2"]);
        let other = other.map(|text| {
            scenario
                .add_artifact_from_oracle(P0, "Other", text)
                .with_mana_cost(ManaCost::generic(2))
                .id()
        });
        scenario.add_creature(engine::game::scenario::P1, "Victim", 2, 2);
        add(&mut scenario);
        let mut runner = scenario.build();
        let st = runner.state_mut();
        st.objects
            .get_mut(&host)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);
        for &fb in &granters {
            attach_to(st, fb, host);
        }
        relayer(st);
        Board {
            runner,
            host,
            granters,
            other,
        }
    }

    fn board(body: &str, other: bool) -> Board {
        board_with(body, &[3], other)
    }

    fn last_ability(b: &Board) -> usize {
        b.runner.state().objects[&b.host].abilities.len() - 1
    }

    fn activate(b: &mut Board, index: usize) {
        b.runner
            .act(GameAction::ActivateAbility {
                source_id: b.host,
                ability_index: index,
            })
            .unwrap();
    }

    fn activate_last(b: &mut Board) {
        let index = last_ability(b);
        activate(b, index);
    }

    fn p1p1(b: &Board, id: ObjectId) -> u32 {
        b.runner.state().objects[&id]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0)
    }

    fn power(b: &Board) -> i32 {
        b.runner.state().objects[&b.host].power.unwrap()
    }

    fn hand(b: &Board) -> usize {
        b.runner.state().players[0].hand.len()
    }

    #[test]
    fn exclusion_count_excludes_the_granter() {
        let mut b = board(EXCLUSION_COUNT, true);
        activate_last(&mut b);
        b.runner.advance_until_stack_empty();
        assert_eq!(p1p1(&b, b.host), 2);
    }

    #[test]
    fn exclusion_names_the_granter_incarnation_not_its_id() {
        let mut b = board(EXCLUSION_COUNT, true);
        let fb = b.granters[0];
        let st = b.runner.state();
        assert_eq!(
            st.objects[&b.host].abilities[last_ability(&b)].granting_object,
            Some(ObjectIncarnationRef::from_object(&st.objects[&fb]))
        );
        activate_last(&mut b);
        let st = b.runner.state_mut();
        move_to_zone(st, fb, Zone::Exile, &mut Vec::new());
        move_to_zone(st, fb, Zone::Battlefield, &mut Vec::new());
        b.runner.advance_until_stack_empty();
        assert_eq!(b.runner.state().objects[&fb].zone, Zone::Battlefield);
        assert_eq!(p1p1(&b, b.host), 3);
    }

    #[test]
    fn aggregate_excludes_the_granter() {
        let mut b = board(
            "{T}: You gain X life, where X is the greatest mana value among artifacts you control other than Foo Bar.",
            true,
        );
        activate_last(&mut b);
        b.runner.advance_until_stack_empty();
        assert_eq!(b.runner.state().players[0].life, 22);
    }

    #[test]
    fn counters_read_the_granter() {
        let mut b = board(COUNTERS_PUMP, false);
        activate_last(&mut b);
        b.runner.advance_until_stack_empty();
        assert_eq!(power(&b), 6);
    }

    #[test]
    fn condition_exclusion_reads_the_granter() {
        for (other, drawn) in [(false, 0), (true, 1)] {
            let mut b = board(
                "{T}: Draw a card if you control an artifact other than Foo Bar.",
                other,
            );
            activate_last(&mut b);
            b.runner.advance_until_stack_empty();
            assert_eq!(hand(&b), drawn, "other={other}");
        }
    }

    fn attack(b: &mut Board) {
        for _ in 0..8 {
            if matches!(
                b.runner.state().waiting_for,
                engine::types::game_state::WaitingFor::DeclareAttackers { .. }
            ) {
                break;
            }
            b.runner.act(GameAction::PassPriority).unwrap();
        }
        b.runner
            .declare_attackers(&[(
                b.host,
                engine::game::combat::AttackTarget::Player(engine::game::scenario::P1),
            )])
            .unwrap();
        b.runner.advance_until_stack_empty();
    }

    #[test]
    fn granted_trigger_bodies_read_the_granter() {
        let mut b = board(
            "Whenever this creature attacks, put a +1/+1 counter on it for each artifact you control other than Foo Bar.",
            true,
        );
        attack(&mut b);
        assert_eq!(p1p1(&b, b.host), 2);

        let mut b = board(
            "Whenever this creature attacks, it gets +X/+0 until end of turn, where X is the number of +1/+1 counters on Foo Bar.",
            false,
        );
        attack(&mut b);
        assert_eq!(power(&b), 6);
    }

    #[test]
    fn delayed_payload_carries_its_own_stamp() {
        let mut b = board(
            "{T}: At the beginning of the next end step, this creature gets +X/+0 until end of turn, where X is the number of +1/+1 counters on Foo Bar.",
            false,
        );
        activate_last(&mut b);
        b.runner.advance_until_stack_empty();
        assert_eq!(power(&b), 3);
        b.runner.advance_to_end_step();
        b.runner.advance_until_stack_empty();
        assert_eq!(power(&b), 6);
    }

    #[test]
    fn two_granters_grant_two_abilities() {
        let mut b = board_with(COUNTERS_PUMP, &[3, 1], false);
        let n = b.runner.state().objects[&b.host].abilities.len();
        let first = n - 2;
        let stamps: Vec<_> = b.runner.state().objects[&b.host].abilities[first..]
            .iter()
            .map(|a| a.granting_object.map(|g| g.object_id))
            .collect();
        assert_eq!(stamps.len(), 2);
        assert_ne!(stamps[0], stamps[1]);
        let mut gains = Vec::new();
        for index in [first, first + 1] {
            let before = power(&b);
            activate(&mut b, index);
            b.runner.advance_until_stack_empty();
            gains.push(power(&b) - before);
            b.runner
                .state_mut()
                .objects
                .get_mut(&b.host)
                .unwrap()
                .tapped = false;
        }
        gains.sort_unstable();
        assert_eq!(gains, vec![1, 3]);
    }

    #[test]
    fn host_self_references_stay_on_the_host() {
        let mut b = board(
            "{T}: Put a +1/+1 counter on this creature for each artifact you control other than Foo Bar. You gain life equal to the number of +1/+1 counters on this creature.",
            true,
        );
        activate_last(&mut b);
        b.runner.advance_until_stack_empty();
        assert_eq!(p1p1(&b, b.host), 2);
        assert_eq!(p1p1(&b, b.granters[0]), 3);
        assert_eq!(b.runner.state().players[0].life, 22);
    }

    #[test]
    fn stamp_is_omitted_when_absent_and_round_trips_when_present() {
        let mut def = AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp);
        let json = serde_json::to_string(&def).unwrap();
        assert!(!json.contains("granting_object"), "{json}");
        let mut trigger = TriggerDefinition::new(TriggerMode::Attacks);
        let json = serde_json::to_string(&trigger).unwrap();
        assert!(!json.contains("granting_object"), "{json}");
        let mut context = SpellContext::default();
        let json = serde_json::to_string(&context).unwrap();
        assert!(!json.contains("granting_object"), "{json}");

        let stamp = Some(ObjectIncarnationRef::of(ObjectId(7), 2));
        def.granting_object = stamp;
        let json = serde_json::to_string(&def).unwrap();
        assert!(json.contains("granting_object"), "{json}");
        assert_eq!(
            serde_json::from_str::<AbilityDefinition>(&json).unwrap(),
            def
        );
        trigger.granting_object = stamp;
        let json = serde_json::to_string(&trigger).unwrap();
        assert!(json.contains("granting_object"), "{json}");
        assert_eq!(
            serde_json::from_str::<TriggerDefinition>(&json).unwrap(),
            trigger
        );
        context.granting_object = stamp;
        let json = serde_json::to_string(&context).unwrap();
        assert!(json.contains("granting_object"), "{json}");
        assert_eq!(
            serde_json::from_str::<SpellContext>(&json).unwrap(),
            context
        );
    }

    /// Building-block reads with the `ResolvedAbility` in scope: host 2/2 (one +1/+1
    /// counter), granter "Foo Bar" 4/4 of MV 5 with three +1/+1 counters.
    #[test]
    fn stamped_granter_is_what_every_resolution_reader_names() {
        let mut scenario = GameScenario::new();
        let host = scenario.add_creature(P0, "Bearer", 2, 2).id();
        let fb = scenario
            .add_creature(P0, "Foo Bar", 4, 4)
            .with_mana_cost(ManaCost::generic(5))
            .id();
        scenario.with_counter(fb, CounterType::Plus1Plus1, 3);
        scenario.with_counter(host, CounterType::Plus1Plus1, 1);
        let mut state = scenario.build().state().clone();
        relayer(&mut state);
        let stamp = ObjectIncarnationRef::from_object(&state.objects[&fb]);
        let ability = |granter: Option<ObjectIncarnationRef>| {
            let mut a = ResolvedAbility::new(Effect::NoOp, vec![], host, P0);
            a.context.granting_object = granter;
            a
        };
        let q = |qty| QuantityExpr::Ref { qty };
        let counters = q(QuantityRef::CountersOn {
            scope: ObjectScope::GrantingObject,
            counter_type: Some(CounterType::Plus1Plus1),
        });
        let power = q(QuantityRef::Power {
            scope: ObjectScope::GrantingObject,
        });
        let mana_value = q(QuantityRef::ObjectManaValue {
            scope: ObjectScope::GrantingObject,
        });
        let read = |st: &GameState, e: &QuantityExpr, a: &ResolvedAbility| {
            resolve_quantity_with_targets(st, e, a)
        };
        let matches = |st: &GameState, id: ObjectId, a: &ResolvedAbility| {
            matches_target_filter(
                st,
                id,
                &TargetFilter::GrantingObject { bound: None },
                &FilterContext::from_ability(a),
            )
        };

        let targets = |st: &GameState, a: &ResolvedAbility| {
            resolved_targets(a, &TargetFilter::GrantingObject { bound: None }, st)
        };
        let gated_gain = |st: &GameState, a: &ResolvedAbility| {
            let mut st = st.clone();
            let gain = |n| Effect::GainLife {
                amount: QuantityExpr::Fixed { value: n },
                player: TargetFilter::Controller,
            };
            let mut sub = ResolvedAbility::new(gain(1), vec![], host, P0);
            sub.condition = Some(AbilityCondition::QuantityCheck {
                lhs: mana_value.clone(),
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 5 },
            });
            let mut root = a.clone();
            root.effect = gain(10);
            root.sub_ability = Some(Box::new(sub));
            let life = st.players[0].life;
            resolve_ability_chain(&mut st, &root, &mut Vec::new(), 0).unwrap();
            st.players[0].life - life
        };

        let stamped = ability(Some(stamp));
        assert_eq!(targets(&state, &stamped), vec![TargetRef::Object(fb)]);
        assert_eq!(gated_gain(&state, &stamped), 11);
        assert_eq!(read(&state, &counters, &stamped), 3);
        assert_eq!(read(&state, &power, &stamped), 7);
        assert_eq!(read(&state, &mana_value, &stamped), 5);
        assert!(matches(&state, fb, &stamped));
        assert!(!matches(&state, host, &stamped));

        let unbound = ability(None);
        assert_eq!(targets(&state, &unbound), vec![TargetRef::Object(host)]);
        assert_eq!(gated_gain(&state, &unbound), 10);
        assert_eq!(read(&state, &counters, &unbound), 1);
        assert_eq!(read(&state, &power, &unbound), 0);
        assert!(!matches(&state, host, &unbound));
        assert!(!matches(&state, fb, &unbound));

        move_to_zone(&mut state, fb, Zone::Exile, &mut Vec::new());
        move_to_zone(&mut state, fb, Zone::Battlefield, &mut Vec::new());
        relayer(&mut state);
        assert!(!matches(&state, fb, &stamped));
        assert!(targets(&state, &stamped).is_empty());
        assert_eq!(read(&state, &counters, &stamped), 3);
    }

    /// CR 201.5a + CR 115.10a: an effect naming the granter acts on the stamped granter, not the host.
    #[test]
    fn effect_naming_the_granter_acts_on_the_granter() {
        type Check = fn(&Board, ObjectId) -> bool;
        let zone = |b: &Board, id: ObjectId| b.runner.state().objects[&id].zone;
        let cases: [(&str, Check); 6] = [
            ("{T}: Put a +1/+1 counter on Foo Bar.", |b, fb| {
                p1p1(b, fb) == 4 && p1p1(b, b.host) == 1
            }),
            ("{T}: Remove a +1/+1 counter from Foo Bar.", |b, fb| {
                p1p1(b, fb) == 2 && p1p1(b, b.host) == 1
            }),
            ("{T}: Destroy Foo Bar.", |b, fb| {
                b.runner.state().objects[&fb].zone == Zone::Graveyard
            }),
            ("{T}: Return Foo Bar to its owner's hand.", |b, fb| {
                b.runner.state().objects[&fb].zone == Zone::Hand
            }),
            ("{T}: Sacrifice Foo Bar.", |b, fb| {
                b.runner.state().objects[&fb].zone == Zone::Graveyard
            }),
            ("{T}: Exile Foo Bar.", |b, fb| {
                b.runner.state().objects[&fb].zone == Zone::Exile
            }),
        ];
        for (body, check) in cases {
            let mut b = board(body, false);
            let fb = b.granters[0];
            assert_eq!(
                b.runner.state().objects[&b.host].abilities[last_ability(&b)].granting_object,
                Some(ObjectIncarnationRef::from_object(
                    &b.runner.state().objects[&fb]
                )),
                "{body}"
            );
            activate_last(&mut b);
            b.runner.advance_until_stack_empty();
            assert!(check(&b, fb), "{body}");
            assert_eq!(zone(&b, b.host), Zone::Battlefield, "{body}");
        }
    }

    /// CR 201.5a + CR 603.7c: a delayed return naming the granter returns the stamped granter.
    #[test]
    fn delayed_return_naming_the_granter_returns_the_granter() {
        let mut b = board(
            "{T}: Return Foo Bar to its owner's hand at the beginning of the next end step.",
            false,
        );
        let fb = b.granters[0];
        activate_last(&mut b);
        b.runner.advance_until_stack_empty();
        assert_eq!(b.runner.state().objects[&fb].zone, Zone::Battlefield);
        b.runner.advance_to_end_step();
        b.runner.advance_until_stack_empty();
        assert_eq!(b.runner.state().objects[&fb].zone, Zone::Hand);
        assert_eq!(b.runner.state().objects[&b.host].zone, Zone::Battlefield);
    }

    fn object_named(b: &Board, name: &str) -> ObjectId {
        let st = b.runner.state();
        *st.battlefield
            .iter()
            .find(|id| st.objects[id].name == name)
            .unwrap()
    }

    fn try_activate_last(b: &mut Board) -> bool {
        let index = last_ability(b);
        try_activate(b, index)
    }

    fn try_activate(b: &mut Board, index: usize) -> bool {
        b.runner
            .act(GameAction::ActivateAbility {
                source_id: b.host,
                ability_index: index,
            })
            .is_ok()
    }

    /// CR 601.2c via CR 602.2b: a target slot's threshold reads the announcing ability's granter.
    #[test]
    fn target_slot_threshold_reads_the_granter() {
        let mut b = board(
            "{T}: Destroy target creature with power less than the number of +1/+1 counters on Foo Bar.",
            false,
        );
        let victim = object_named(&b, "Victim");
        assert!(try_activate_last(&mut b));
        if matches!(
            b.runner.state().waiting_for,
            engine::types::game_state::WaitingFor::TargetSelection { .. }
        ) {
            b.runner
                .act(GameAction::SelectTargets {
                    targets: vec![TargetRef::Object(victim)],
                })
                .unwrap();
        }
        b.runner.advance_until_stack_empty();
        assert_eq!(b.runner.state().objects[&victim].zone, Zone::Graveyard);
        assert_eq!(b.runner.state().objects[&b.host].zone, Zone::Battlefield);
    }

    /// CR 602.5: an activation restriction reads the activated ability's granter.
    #[test]
    fn activation_restriction_reads_the_granter() {
        for (other, allowed) in [(false, false), (true, true)] {
            let mut b = board(
                "{T}: Draw a card. Activate only if you control an artifact other than Foo Bar.",
                other,
            );
            assert_eq!(try_activate_last(&mut b), allowed, "other={other}");
            b.runner.advance_until_stack_empty();
            assert_eq!(hand(&b), usize::from(allowed), "other={other}");
        }
    }

    /// The index of the host's ability stamped by `granter`.
    fn ability_for_granter(b: &Board, granter: ObjectId) -> usize {
        let st = b.runner.state();
        let stamp = Some(ObjectIncarnationRef::from_object(&st.objects[&granter]));
        st.objects[&b.host]
            .abilities
            .iter()
            .position(|a| a.granting_object == stamp)
            .unwrap()
    }

    const THRESHOLD_RESTRICTION: &str = "{T}: Draw a card. Activate only if you control an artifact with mana value less than the number of +1/+1 counters on Foo Bar.";

    /// CR 602.5: a threshold inside an activation restriction's filter reads the granter.
    #[test]
    fn activation_restriction_threshold_reads_the_granter() {
        for (other, allowed) in [(true, true), (false, false)] {
            let mut b = board(THRESHOLD_RESTRICTION, other);
            assert_eq!(try_activate_last(&mut b), allowed, "other={other}");
        }
        // CR 201.5a + CR 602.5c: each acquired copy reads the granter it was acquired from.
        for (granter, allowed) in [(0, true), (1, false)] {
            let mut b = board_with(THRESHOLD_RESTRICTION, &[3, 1], true);
            let index = ability_for_granter(&b, b.granters[granter]);
            assert_eq!(try_activate(&mut b, index), allowed, "granter={granter}");
        }
    }

    const TAP: &str = "{T}: You gain 1 life.\n";
    const TAPPED_EXCEPT: &str = "Whenever another artifact other than Foo Bar you control becomes tapped, put a +1/+1 counter on this creature.";

    /// CR 603.2: a granted trigger's event filter excludes its granter.
    #[test]
    fn trigger_event_filter_excludes_the_granter() {
        for (tapper, counters) in [("Foo Bar", 1), ("Other", 2)] {
            let mut b = board_full(TAPPED_EXCEPT, &[3], Some(TAP), TAP);
            activate_named(&mut b, tapper, |e| matches!(e, Effect::GainLife { .. }));
            b.runner.advance_until_stack_empty();
            assert_eq!(p1p1(&b, b.host), counters, "{tapper}");
        }
        // CR 201.5a: each granted copy excludes only its own granter.
        for granter in [0, 1] {
            let mut b = board_full(TAPPED_EXCEPT, &[3, 1], Some(TAP), TAP);
            let tapper = b.granters[granter];
            activate_tapper(&mut b, tapper, |e| matches!(e, Effect::GainLife { .. }));
            b.runner.advance_until_stack_empty();
            assert_eq!(p1p1(&b, b.host), 2, "granter={granter}");
        }
    }

    const ATTACK_IF: &str =
        "Whenever this creature attacks, if you control an artifact other than Foo Bar, draw a card.";

    /// Declares the host as an attacker and reports whether its trigger reached the stack.
    fn declare_attack(b: &mut Board) -> bool {
        for _ in 0..8 {
            if matches!(
                b.runner.state().waiting_for,
                engine::types::game_state::WaitingFor::DeclareAttackers { .. }
            ) {
                break;
            }
            b.runner.act(GameAction::PassPriority).unwrap();
        }
        b.runner
            .declare_attackers(&[(
                b.host,
                engine::game::combat::AttackTarget::Player(engine::game::scenario::P1),
            )])
            .unwrap();
        let host = b.host;
        b.runner.state().stack.iter().any(|entry| {
            entry.source_id == host
                && matches!(
                    entry.kind,
                    engine::types::game_state::StackEntryKind::TriggeredAbility { .. }
                )
        })
    }

    /// CR 603.4: the trigger-time intervening-if reads the granter.
    #[test]
    fn intervening_if_at_trigger_time_reads_the_granter() {
        for (other, triggered) in [(false, false), (true, true)] {
            let mut b = board(ATTACK_IF, other);
            assert_eq!(declare_attack(&mut b), triggered, "other={other}");
            b.runner.advance_until_stack_empty();
            assert_eq!(hand(&b), usize::from(triggered), "other={other}");
        }
    }

    /// CR 603.4: the resolution recheck reads the granter the instantiated trigger carries.
    #[test]
    fn intervening_if_recheck_reads_the_granter() {
        for (remove_other, drawn) in [(true, 0), (false, 1)] {
            let mut b = board(ATTACK_IF, true);
            assert!(declare_attack(&mut b));
            if remove_other {
                let other = b.other.unwrap();
                move_to_zone(
                    b.runner.state_mut(),
                    other,
                    Zone::Graveyard,
                    &mut Vec::new(),
                );
            }
            b.runner.advance_until_stack_empty();
            assert_eq!(hand(&b), drawn, "remove_other={remove_other}");
        }
    }

    /// Activates `tapper`'s first ability whose effect `is_tap_ability` accepts.
    fn activate_named(b: &mut Board, tapper: &str, is_tap_ability: fn(&Effect) -> bool) {
        let source = object_named(b, tapper);
        activate_tapper(b, source, is_tap_ability);
    }

    /// Activates `source`'s first ability whose effect `is_tap_ability` accepts.
    fn activate_tapper(b: &mut Board, source: ObjectId, is_tap_ability: fn(&Effect) -> bool) {
        let index = b.runner.state().objects[&source]
            .abilities
            .iter()
            .position(|a| is_tap_ability(&a.effect))
            .unwrap();
        b.runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: index,
            })
            .unwrap();
        assert!(b.runner.state().objects[&source].tapped, "{source:?}");
    }

    /// CR 603.8: a granted state trigger's condition excludes its granter.
    #[test]
    fn state_trigger_condition_excludes_the_granter() {
        for (other, zone) in [(false, Zone::Graveyard), (true, Zone::Battlefield)] {
            let mut b = board(
                "When you control no artifacts other than Foo Bar, sacrifice this creature.",
                other,
            );
            b.runner.act(GameAction::PassPriority).unwrap();
            b.runner.advance_until_stack_empty();
            assert_eq!(
                b.runner.state().objects[&b.host].zone,
                zone,
                "other={other}"
            );
        }
    }

    /// CR 603.4 + CR 603.10a: a batched leaves-the-battlefield trigger's intervening-if reads
    /// the granter when the trigger event occurs.
    #[test]
    fn batched_zone_intervening_if_at_trigger_time_reads_the_granter() {
        for other in [false, true] {
            let mut b = board_built(
                "Whenever one or more other creatures you control leave the battlefield, if you control an artifact other than Foo Bar, draw a card.",
                &[3],
                other.then_some(""),
                "",
                |scenario| {
                    scenario.add_creature(P0, "Fodder", 1, 1);
                    scenario.add_creature_from_oracle(
                        P0,
                        "Killer",
                        3,
                        3,
                        "{T}: Exile target creature.",
                    );
                },
            );
            let fodder = object_named(&b, "Fodder");
            let killer = object_named(&b, "Killer");
            b.runner
                .state_mut()
                .objects
                .get_mut(&killer)
                .unwrap()
                .summoning_sick = false;
            b.runner
                .act(GameAction::ActivateAbility {
                    source_id: killer,
                    ability_index: 0,
                })
                .unwrap();
            if matches!(
                b.runner.state().waiting_for,
                engine::types::game_state::WaitingFor::TargetSelection { .. }
            ) {
                b.runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(fodder)],
                    })
                    .unwrap();
            }
            for _ in 0..6 {
                if b.runner.state().objects[&fodder].zone == Zone::Exile {
                    break;
                }
                b.runner.act(GameAction::PassPriority).unwrap();
            }
            assert_eq!(b.runner.state().objects[&fodder].zone, Zone::Exile);
            let host = b.host;
            let triggered = b.runner.state().stack.iter().any(|entry| {
                entry.source_id == host
                    && matches!(
                        entry.kind,
                        engine::types::game_state::StackEntryKind::TriggeredAbility { .. }
                    )
            });
            assert_eq!(triggered, other, "other={other}");
            b.runner.advance_until_stack_empty();
            assert_eq!(hand(&b), usize::from(other), "other={other}");
        }
    }

    /// CR 603.2 + CR 605.1b: a granted mana trigger's event filter excludes its granter.
    #[test]
    fn mana_trigger_event_filter_excludes_the_granter() {
        for (tapper, pool) in [("Foo Bar", 1), ("Other", 2)] {
            let mut b = board_full(
                "Whenever an artifact other than Foo Bar you control is tapped for mana, add {C}.",
                &[3],
                Some("{T}: Add {C}."),
                "{T}: Add {C}.\n",
            );
            activate_named(&mut b, tapper, |e| matches!(e, Effect::Mana { .. }));
            assert_eq!(
                b.runner.state().players[0].mana_pool.total(),
                pool,
                "{tapper}"
            );
        }
    }

    /// CR 201.5a + CR 613.4c: a granted static's quantity excludes its granter.
    #[test]
    fn granted_static_quantity_excludes_the_granter() {
        for (other, host_power) in [(true, 5), (false, 3)] {
            let b = board(
                "This creature gets +X/+0, where X is the greatest mana value among artifacts you control other than Foo Bar.",
                other,
            );
            assert_eq!(power(&b), host_power, "other={other}");
            assert_eq!(
                installed_stamps(&b),
                vec![Some(stamp_of(&b, b.granters[0]))]
            );
        }
        let b = board("This creature gets +1/+0.", true);
        assert_eq!(power(&b), 4);
        assert_eq!(installed_stamps(&b), vec![None]);
    }

    fn stamp_of(b: &Board, id: ObjectId) -> ObjectIncarnationRef {
        ObjectIncarnationRef::from_object(&b.runner.state().objects[&id])
    }

    /// The stamps on the statics granted to the host.
    fn installed_stamps(b: &Board) -> Vec<Option<ObjectIncarnationRef>> {
        let st = b.runner.state();
        let host = &st.objects[&b.host];
        let base = host.base_static_definitions.len();
        host.static_definitions.as_slice()[base..]
            .iter()
            .map(|s| s.granting_object)
            .collect()
    }

    /// Foo Bar (beside `others` "Other" artifacts) creates `token`; returns Foo Bar and the Ooze.
    fn create_token(token: &str, others: usize) -> (GameRunner, ObjectId, ObjectId) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let fb = scenario
            .add_artifact_from_oracle(P0, "Foo Bar", &format!("{{T}}: Create {token}"))
            .id();
        for _ in 0..others {
            scenario.add_artifact_from_oracle(P0, "Other", "");
        }
        let mut runner = scenario.build();
        runner
            .act(GameAction::ActivateAbility {
                source_id: fb,
                ability_index: 0,
            })
            .unwrap();
        runner.advance_until_stack_empty();
        let st = runner.state();
        let ooze = *st
            .battlefield
            .iter()
            .find(|id| st.objects[id].name == "Ooze")
            .expect("the Ooze token");
        (runner, fb, ooze)
    }

    fn token_pt(runner: &GameRunner, ooze: ObjectId) -> (Option<i32>, Option<i32>) {
        let token = &runner.state().objects[&ooze];
        (token.power, token.toughness)
    }

    fn token_stamps(runner: &GameRunner, ooze: ObjectId) -> Vec<Option<ObjectIncarnationRef>> {
        runner.state().objects[&ooze]
            .base_static_definitions
            .iter()
            .map(|s| s.granting_object)
            .collect()
    }

    /// CR 201.5a + CR 604.3: a token's P/T CDA excludes the object that created it.
    #[test]
    fn token_cda_excludes_its_creator() {
        let (mut runner, fb, ooze) = create_token(
            "a green Ooze creature token with \"This token's power and toughness are each equal to the number of artifacts you control other than Foo Bar.\"",
            1,
        );
        let creator = ObjectIncarnationRef::from_object(&runner.state().objects[&fb]);
        assert_eq!(token_pt(&runner, ooze), (Some(1), Some(1)));
        assert_eq!(token_stamps(&runner, ooze), vec![Some(creator)]);
        // CR 400.7: the returned Foo Bar is a new object, so the token counts it.
        let st = runner.state_mut();
        move_to_zone(st, fb, Zone::Exile, &mut Vec::new());
        move_to_zone(st, fb, Zone::Battlefield, &mut Vec::new());
        relayer(st);
        assert_eq!(token_pt(&runner, ooze), (Some(2), Some(2)));

        let (runner, _, ooze) = create_token(
            "a 1/1 green Ooze creature token with \"This token can't block.\"",
            1,
        );
        assert_eq!(token_pt(&runner, ooze), (Some(1), Some(1)));
        assert_eq!(token_stamps(&runner, ooze), vec![None]);
    }

    const TOKEN_OTHERS: &str = "a 2/2 green Ooze creature token with \"This creature gets +2/+2 as long as you control two or more artifacts other than Foo Bar.\"";
    const TOKEN_CHARGE: &str = "a 2/2 green Ooze creature token with \"This creature gets +2/+2 as long as there are three or more charge counters on Foo Bar.\"";

    fn set_charge(runner: &mut GameRunner, id: ObjectId, n: u32) {
        let st = runner.state_mut();
        st.objects
            .get_mut(&id)
            .unwrap()
            .counters
            .insert(CounterType::Generic("charge".to_string()), n);
        relayer(st);
    }

    /// CR 201.5a + CR 611.3a: a token static's condition excludes the object that created it.
    #[test]
    fn token_static_condition_excludes_its_creator() {
        let power = [1, 2].map(|others| {
            let (runner, _, ooze) = create_token(TOKEN_OTHERS, others);
            token_pt(&runner, ooze).0
        });
        assert_eq!(power, [Some(2), Some(4)]);
    }

    /// CR 122.1 + CR 201.5a: a token static's condition reads counters on its creator.
    #[test]
    fn token_static_counter_condition_reads_its_creator() {
        let power = [(3, 0), (0, 3)].map(|(creator, token)| {
            let (mut runner, fb, ooze) = create_token(TOKEN_CHARGE, 1);
            set_charge(&mut runner, fb, creator);
            set_charge(&mut runner, ooze, token);
            token_pt(&runner, ooze).0
        });
        assert_eq!(power, [Some(4), Some(2)]);
    }

    /// CR 400.7: the token's ability names the creator's old incarnation, which no longer exists.
    #[test]
    fn token_static_counter_condition_does_not_follow_a_blinked_creator() {
        let (mut runner, fb, ooze) = create_token(TOKEN_CHARGE, 1);
        set_charge(&mut runner, fb, 3);
        set_charge(&mut runner, ooze, 3);
        assert_eq!(token_pt(&runner, ooze), (Some(4), Some(4)));
        let st = runner.state_mut();
        move_to_zone(st, fb, Zone::Exile, &mut Vec::new());
        move_to_zone(st, fb, Zone::Battlefield, &mut Vec::new());
        set_charge(&mut runner, fb, 3);
        assert_eq!(token_pt(&runner, ooze), (Some(2), Some(2)));
    }

    /// `create_token`, then a new permanent "Mimic" becomes a copy of the Ooze (CR 707.2).
    fn copy_token(token: &str, others: usize) -> (GameRunner, ObjectId, ObjectId) {
        let (mut runner, fb, ooze) = create_token(token, others);
        let st = runner.state_mut();
        let mimic = engine::game::zones::create_object(
            st,
            engine::types::identifiers::CardId(st.next_object_id),
            P0,
            "Mimic".to_string(),
            Zone::Battlefield,
        );
        let copy = ResolvedAbility::new(
            Effect::BecomeCopy {
                target: TargetFilter::Any,
                recipient: engine::types::ability::CopyRecipient::Source,
                duration: None,
                mana_value_limit: None,
                additional_modifications: Vec::new(),
            },
            vec![TargetRef::Object(ooze)],
            mimic,
            P0,
        );
        resolve_ability_chain(st, &copy, &mut Vec::new(), 0).unwrap();
        relayer(st);
        (runner, fb, mimic)
    }

    /// CR 201.5a + CR 707.2: a copy of the token still excludes the token's creator.
    #[test]
    fn token_copy_static_condition_excludes_its_creator() {
        let power = [1, 2].map(|others| {
            let (runner, _, mimic) = copy_token(TOKEN_OTHERS, others);
            assert_eq!(runner.state().objects[&mimic].name, "Ooze");
            token_pt(&runner, mimic).0
        });
        assert_eq!(power, [Some(2), Some(4)]);
    }

    /// CR 201.5a + CR 400.7: a copy of the token reads its creator's counters, and not a
    /// blinked creator's new incarnation.
    #[test]
    fn token_copy_counter_condition_reads_its_creator() {
        let (mut runner, fb, mimic) = copy_token(TOKEN_CHARGE, 1);
        set_charge(&mut runner, fb, 3);
        assert_eq!(token_pt(&runner, mimic), (Some(4), Some(4)));
        set_charge(&mut runner, mimic, 3);
        assert_eq!(token_pt(&runner, mimic), (Some(4), Some(4)));
        let st = runner.state_mut();
        move_to_zone(st, fb, Zone::Exile, &mut Vec::new());
        move_to_zone(st, fb, Zone::Battlefield, &mut Vec::new());
        set_charge(&mut runner, fb, 3);
        assert_eq!(token_pt(&runner, mimic), (Some(2), Some(2)));
    }

    /// Adds to each Foo Bar a grant of `replacement` to its equipped creature.
    fn grant_replacement(b: &mut Board, replacement: ReplacementDefinition) {
        grant_modification(
            b,
            ContinuousModification::GrantReplacement {
                replacement: Box::new(replacement),
            },
        );
    }

    /// Adds to each Foo Bar a grant `modification` to its equipped creature.
    fn grant_modification(b: &mut Board, modification: ContinuousModification) {
        let st = b.runner.state_mut();
        for fb in &b.granters {
            let fb = st.objects.get_mut(fb).unwrap();
            let mut grant = super::grant_ability_static(&fb.base_static_definitions);
            grant.modifications = vec![modification.clone()];
            fb.static_definitions.push(grant.clone());
            Arc::make_mut(&mut fb.base_static_definitions).push(grant);
        }
        relayer(st);
    }

    /// Taps `id`, then untaps it with an effect; returns whether it untapped.
    fn untap(b: &mut Board, id: ObjectId) -> bool {
        let st = b.runner.state_mut();
        st.objects.get_mut(&id).unwrap().tapped = true;
        let untap = ResolvedAbility::new(
            Effect::SetTapState {
                target: TargetFilter::Any,
                scope: EffectScope::Single,
                state: TapStateChange::Untap,
            },
            vec![TargetRef::Object(id)],
            id,
            P0,
        );
        resolve_ability_chain(st, &untap, &mut Vec::new(), 0).unwrap();
        !b.runner.state().objects[&id].tapped
    }

    /// CR 201.5a + CR 614.1: a granted replacement's `valid_card` names its granter.
    #[test]
    fn granted_replacement_applies_to_the_granters_event() {
        let mut b = board_with("{T}: Draw a card.", &[3, 1], false);
        grant_replacement(
            &mut b,
            ReplacementDefinition::new(ReplacementEvent::Untap)
                .valid_card(TargetFilter::GrantingObject { bound: None }),
        );
        let (host, granters) = (b.host, b.granters.clone());
        assert_eq!(
            installed_replacement_stamps(&b),
            granters
                .iter()
                .map(|&fb| Some(stamp_of(&b, fb)))
                .collect::<Vec<_>>()
        );
        for fb in granters {
            assert!(!untap(&mut b, fb), "each granter's untap is replaced");
        }
        assert!(untap(&mut b, host), "the host's untap is not");

        let mut b = board("{T}: Draw a card.", false);
        grant_replacement(
            &mut b,
            ReplacementDefinition::new(ReplacementEvent::Untap).valid_card(TargetFilter::SelfRef),
        );
        let (host, fb) = (b.host, b.granters[0]);
        assert_eq!(installed_replacement_stamps(&b), vec![None]);
        assert!(untap(&mut b, fb));
        assert!(!untap(&mut b, host));
    }

    /// The stamps on the replacements granted to the host.
    fn installed_replacement_stamps(b: &Board) -> Vec<Option<ObjectIncarnationRef>> {
        let host = &b.runner.state().objects[&b.host];
        let base = host.base_replacement_definitions.len();
        host.replacement_definitions.as_slice()[base..]
            .iter()
            .map(|r| r.granting_object)
            .collect()
    }

    /// CR 201.5a + CR 614.6: a granted replacement's execute reads its granter.
    #[test]
    fn granted_replacement_execute_reads_the_granter() {
        let mut b = board("{T}: Draw a card.", false);
        grant_replacement(
            &mut b,
            ReplacementDefinition::new(ReplacementEvent::Untap)
                .valid_card(TargetFilter::SelfRef)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::PutCounter {
                        counter_type: CounterType::Plus1Plus1,
                        count: QuantityExpr::Ref {
                            qty: QuantityRef::CountersOn {
                                scope: ObjectScope::GrantingObject,
                                counter_type: Some(CounterType::Plus1Plus1),
                            },
                        },
                        target: TargetFilter::SelfRef,
                    },
                )),
        );
        let host = b.host;
        assert!(!untap(&mut b, host));
        assert_eq!(p1p1(&b, host), 4);
    }

    /// A creature token named `name` with power and toughness `pt`.
    fn creature_token(name: &str, pt: PtValue) -> Effect {
        Effect::Token {
            name: name.to_string(),
            power: pt.clone(),
            toughness: pt,
            types: vec!["Creature".to_string(), name.to_string()],
            colors: Vec::new(),
            keywords: Vec::new(),
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            supertypes: Vec::new(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
        }
    }

    /// CR 201.5a + CR 614.1a: a granted token substitution reads its granter.
    #[test]
    fn granted_token_substitution_reads_the_granter() {
        let mut b = board("{T}: Draw a card.", false);
        let counters_on_granter = PtValue::Quantity(QuantityExpr::Ref {
            qty: QuantityRef::CountersOn {
                scope: ObjectScope::GrantingObject,
                counter_type: Some(CounterType::Plus1Plus1),
            },
        });
        grant_replacement(
            &mut b,
            ReplacementDefinition::new(ReplacementEvent::CreateToken).execute(
                AbilityDefinition::new(
                    AbilityKind::Spell,
                    creature_token("Angel", counters_on_granter),
                ),
            ),
        );
        let create = ResolvedAbility::new(
            creature_token("Spirit", PtValue::Fixed(1)),
            Vec::new(),
            b.host,
            P0,
        );
        let st = b.runner.state_mut();
        resolve_ability_chain(st, &create, &mut Vec::new(), 0).unwrap();
        let tokens: Vec<_> = st
            .battlefield
            .iter()
            .map(|id| &st.objects[id])
            .filter(|o| o.name == "Angel" || o.name == "Spirit")
            .map(|o| (o.name.clone(), o.power, o.toughness))
            .collect();
        assert_eq!(tokens, vec![("Angel".to_string(), Some(3), Some(3))]);
    }

    /// How many Contraptions `source` assembles when told to assemble one.
    fn assembles(b: &mut Board, source: ObjectId) -> u32 {
        let st = b.runner.state_mut();
        for name in ["Cog", "Gear"] {
            let face = CardFace {
                name: name.to_string(),
                card_type: CardType {
                    supertypes: Vec::new(),
                    core_types: vec![CoreType::Artifact],
                    subtypes: vec!["Contraption".to_string()],
                },
                ..CardFace::default()
            };
            create_contraption_deck_card(st, &face, P0);
        }
        let assemble = ResolvedAbility::new(
            Effect::AssembleContraptions {
                count: QuantityExpr::Fixed { value: 1 },
            },
            Vec::new(),
            source,
            P0,
        );
        resolve_contraptions(st, &assemble, &mut Vec::new()).unwrap();
        let WaitingFor::ChooseOneOfBranch { branches, .. } = &st.waiting_for else {
            panic!("expected the sprocket choice, got {:?}", st.waiting_for);
        };
        let Effect::AssembleContraptionOnSprocket { remaining, .. } = &*branches[0].effect else {
            panic!("expected an assemble branch");
        };
        remaining + 1
    }

    /// CR 201.5a + CR 701.45a: a granted assemble replacement's `valid_card` names its granter.
    #[test]
    fn granted_assemble_replacement_applies_to_the_granters_assemble() {
        let doubling = ReplacementDefinition::new(ReplacementEvent::AssembleContraption)
            .valid_card(TargetFilter::GrantingObject { bound: None })
            .quantity_modification(QuantityModification::Times { factor: 2 });
        let mut b = board("{T}: Draw a card.", false);
        grant_replacement(&mut b, doubling.clone());
        let fb = b.granters[0];
        assert_eq!(assembles(&mut b, fb), 2);

        let mut b = board("{T}: Draw a card.", false);
        grant_replacement(&mut b, doubling);
        let host = b.host;
        assert_eq!(assembles(&mut b, host), 1);
    }

    const SACRIFICE_THRESHOLD: &str = "Sacrifice a creature with power less than the number of +1/+1 counters on Foo Bar: Draw a card.";

    /// CR 201.5a + CR 118.3 via CR 602.2b: a cost's eligibility threshold reads the
    /// paying ability's granter, so two granters' copies on one host differ.
    #[test]
    fn cost_eligibility_threshold_reads_the_granter() {
        for (granter, allowed) in [(0, true), (1, false)] {
            let mut b = board_built(SACRIFICE_THRESHOLD, &[3, 1], None, "", |s| {
                s.add_creature(P0, "Fodder", 2, 2);
            });
            let fodder = object_named(&b, "Fodder");
            let index = ability_for_granter(&b, b.granters[granter]);
            assert_eq!(try_activate(&mut b, index), allowed, "granter={granter}");
            if allowed {
                let WaitingFor::PayCost { choices, .. } = &b.runner.state().waiting_for else {
                    panic!("expected the sacrifice choice");
                };
                assert_eq!(choices, &vec![fodder]);
                b.runner
                    .act(GameAction::SelectCards {
                        cards: vec![fodder],
                    })
                    .unwrap();
            }
            b.runner.advance_until_stack_empty();
            let expected = if allowed {
                Zone::Graveyard
            } else {
                Zone::Battlefield
            };
            assert_eq!(
                b.runner.state().objects[&fodder].zone,
                expected,
                "granter={granter}"
            );
            assert_eq!(hand(&b), usize::from(allowed), "granter={granter}");
        }
    }

    const REVEAL_THRESHOLD: &str = "Reveal a creature card with mana value less than the number of +1/+1 counters on Foo Bar from your hand: Draw a card.";

    /// CR 201.5a + CR 118.3 + CR 701.20a via CR 602.2b: a reveal cost's filter
    /// reads the paying ability's granter, so two granters' copies on one host differ.
    #[test]
    fn reveal_cost_threshold_reads_the_granter() {
        for (granter, allowed) in [(0, true), (1, false)] {
            let mut revealed = None;
            let mut b = board_built(REVEAL_THRESHOLD, &[3, 1], None, "", |s| {
                let id = s
                    .add_creature_to_hand(P0, "Revealed", 1, 1)
                    .with_mana_cost(ManaCost::generic(2))
                    .id();
                revealed = Some(id);
            });
            let revealed = revealed.unwrap();
            let index = ability_for_granter(&b, b.granters[granter]);
            assert_eq!(try_activate(&mut b, index), allowed, "granter={granter}");
            if allowed {
                let WaitingFor::PayCost { choices, .. } = &b.runner.state().waiting_for else {
                    panic!("expected the reveal choice");
                };
                assert_eq!(choices, &vec![revealed]);
                b.runner
                    .act(GameAction::SelectCards {
                        cards: vec![revealed],
                    })
                    .unwrap();
            }
            b.runner.advance_until_stack_empty();
            assert_eq!(hand(&b), 1 + usize::from(allowed), "granter={granter}");
        }
    }

    /// CR 201.5a + CR 701.13a: "Exile The Dominion Bracelet" exiles the granter
    /// from the battlefield, whoever controls it (CR 301.5d).
    #[test]
    fn exile_cost_naming_the_granter_exiles_the_granter() {
        for bracelet_controller in [P0, P1] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let host = scenario.add_creature(P0, "Bearer", 14, 14).id();
            let bracelet = scenario
                .add_artifact_from_oracle(P0, "The Dominion Bracelet", super::THE_DOMINION_BRACELET)
                .with_subtypes(vec!["Equipment"])
                .id();
            let mut runner = scenario.build();
            let st = runner.state_mut();
            attach_to(st, bracelet, host);
            st.objects.get_mut(&bracelet).unwrap().base_controller = Some(bracelet_controller);
            relayer(st);
            let index = runner.state().objects[&host].abilities.len() - 1;
            runner
                .act(GameAction::ActivateAbility {
                    source_id: host,
                    ability_index: index,
                })
                .unwrap_or_else(|e| panic!("controller={bracelet_controller:?}: {e:?}"));
            let WaitingFor::PayCost { choices, .. } = &runner.state().waiting_for else {
                panic!("expected the exile choice, controller={bracelet_controller:?}");
            };
            assert_eq!(choices, &vec![bracelet]);
            runner
                .act(GameAction::SelectCards {
                    cards: vec![bracelet],
                })
                .unwrap_or_else(|e| panic!("controller={bracelet_controller:?}: {e:?}"));
            runner.advance_until_stack_empty();
            assert_eq!(runner.state().objects[&bracelet].zone, Zone::Exile);
            assert_eq!(runner.state().objects[&host].zone, Zone::Battlefield);
        }
    }

    /// CR 109.5 + CR 601.2a: a spell's "exile a creature you control" cost reads the
    /// caster, not the owner of the card cast from another player's exile.
    #[test]
    fn exile_cost_you_control_reads_the_payer() {
        const NECROTIC_FUMES: &str = "As an additional cost to cast this spell, exile a creature you control.\nExile target creature or planeswalker.";

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let mine = scenario.add_vanilla(P0, 2, 2);
        let theirs = scenario.add_vanilla(P1, 2, 2);
        let victim = scenario.add_vanilla(P1, 3, 3);
        let fumes = {
            let mut b = scenario.add_spell_to_exile(P1, "Necrotic Fumes", false);
            b.from_oracle_text(NECROTIC_FUMES);
            b.with_mana_cost(ManaCost::default());
            b.id()
        };
        let mut runner = scenario.build();
        runner
            .state_mut()
            .objects
            .get_mut(&fumes)
            .unwrap()
            .casting_permissions
            .push(CastingPermission::PlayFromExile {
                provenance: PlayFromExileProvenance::Impulse,
                duration: Duration::UntilEndOfTurn,
                granted_to: P0,
                mode: CardPlayMode::Play,
                frequency: CastFrequency::Unlimited,
                source_id: None,
                invalidation: None,
                exiled_by_ability_controller: None,
                mana_spend_permission: None,
                card_filter: None,
                single_use_group: None,
                single_use: false,
                cast_cost_modifier: None,
                alt_ability_cost: None,
                land_enter_tapped: EtbTapState::Unspecified,
            });
        let card_id = runner.state().objects[&fumes].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: fumes,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(victim)),
            })
            .unwrap();
        let WaitingFor::PayCost { choices, .. } = &runner.state().waiting_for else {
            panic!("expected the exile choice");
        };
        assert_eq!(choices, &vec![mine]);
        runner
            .act(GameAction::SelectCards { cards: vec![mine] })
            .unwrap();
        assert_eq!(runner.state().objects[&mine].zone, Zone::Exile);
        assert_eq!(runner.state().objects[&theirs].zone, Zone::Battlefield);
    }

    /// CR 201.5a + CR 605.1a: a granted mana ability whose cost names its granter
    /// pays that cost with the granter.
    fn granted_mana_cost_pays_with_the_granter(verb: &str, zone: Zone) {
        let mut b = board_with(&format!("{{T}}, {verb} Foo Bar: Add {{C}}."), &[0], false);
        let granter = b.granters[0];
        let index = last_ability(&b);
        let def = b.runner.state().objects[&b.host].abilities[index].clone();
        assert!(can_activate_mana_ability_now(
            b.runner.state(),
            P0,
            b.host,
            index,
            &def
        ));
        b.runner
            .act(GameAction::ActivateAbility {
                source_id: b.host,
                ability_index: index,
            })
            .unwrap();
        let WaitingFor::PayCost { choices, .. } = &b.runner.state().waiting_for else {
            panic!(
                "expected the cost choice, got {:?}",
                b.runner.state().waiting_for
            );
        };
        assert_eq!(choices, &vec![granter]);
        b.runner
            .act(GameAction::SelectCards {
                cards: vec![granter],
            })
            .unwrap();
        let st = b.runner.state();
        assert_eq!(st.objects[&granter].zone, zone);
        assert_eq!(st.objects[&b.host].zone, Zone::Battlefield);
        assert_eq!(st.players[0].mana_pool.count_color(ManaType::Colorless), 1);
    }

    #[test]
    fn granted_mana_ability_sacrifice_cost_sacrifices_the_granter() {
        granted_mana_cost_pays_with_the_granter("Sacrifice", Zone::Graveyard);
    }

    #[test]
    fn granted_mana_ability_exile_cost_exiles_the_granter() {
        granted_mana_cost_pays_with_the_granter("Exile", Zone::Exile);
    }

    // CR 201.5a + CR 400.7 + CR 613.1f: a carrier rebuilt each layer pass names its
    // granter's current incarnation in its own filters, conditions and quantities.

    use engine::game::combat::can_block_pair;
    use engine::types::ability::{
        ControllerRef, FilterProp, ReplacementCondition, StaticDefinition, TypeFilter, TypedFilter,
    };
    use engine::types::keywords::Keyword;
    use engine::types::mana::{ManaColor, ManaUnit};

    const TWO_OTHERS: &str = "As long as you control two or more artifacts other than Foo Bar, ";

    /// `board_built` with `others` extra artifacts beside the one Foo Bar.
    fn board_others(body: &str, others: usize) -> Board {
        board_built(body, &[3], None, "", |s| {
            for i in 0..others {
                s.add_artifact_from_oracle(P0, &format!("Other{i}"), "");
            }
        })
    }

    #[test]
    fn granted_static_condition_excludes_the_granter() {
        for (others, host_power) in [(1, 3), (2, 5)] {
            let b = board_others(&format!("{TWO_OTHERS}this creature gets +2/+2."), others);
            assert_eq!(power(&b), host_power, "others={others}");
        }
    }

    fn grant_static_def(b: &mut Board, definition: StaticDefinition) {
        grant_modification(
            b,
            ContinuousModification::GrantStaticAbility {
                definition: Box::new(definition),
            },
        );
    }

    #[test]
    fn granted_static_affected_filter_excludes_the_granter() {
        let mut b = board_others("{T}: Draw a card.", 1);
        grant_static_def(
            &mut b,
            StaticDefinition::continuous()
                .affected(artifacts_other_than_granter())
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Hexproof,
                }]),
        );
        let st = b.runner.state();
        assert!(!st.objects[&b.granters[0]].has_keyword(&Keyword::Hexproof));
        assert!(st.objects[&object_named(&b, "Other0")].has_keyword(&Keyword::Hexproof));
    }

    #[test]
    fn granted_restriction_condition_excludes_the_granter() {
        for (others, can_block) in [(1, true), (2, false)] {
            let b = board_others(&format!("{TWO_OTHERS}this creature can't block."), others);
            let victim = object_named(&b, "Victim");
            assert_eq!(
                can_block_pair(b.runner.state(), b.host, victim),
                can_block,
                "others={others}"
            );
        }
    }

    /// Reaches `zones.rs`'s entry restriction through the granted static's own condition.
    #[test]
    fn granted_entry_restriction_condition_excludes_the_granter() {
        for (others, enters) in [(1, true), (2, false)] {
            let mut corpse = None;
            let mut b = board_built(
                &format!("{TWO_OTHERS}creature cards in graveyards can't enter the battlefield."),
                &[3],
                None,
                "",
                |s| {
                    for i in 0..others {
                        s.add_artifact_from_oracle(P0, &format!("Other{i}"), "");
                    }
                    corpse = Some(s.add_creature_to_graveyard(P0, "Corpse", 1, 1).id());
                },
            );
            let corpse = corpse.unwrap();
            move_to_zone(
                b.runner.state_mut(),
                corpse,
                Zone::Battlefield,
                &mut Vec::new(),
            );
            assert_eq!(
                b.runner.state().objects[&corpse].zone == Zone::Battlefield,
                enters,
                "others={others}"
            );
        }
    }

    fn artifacts_other_than_granter() -> TargetFilter {
        TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Artifact)
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::DistinctFrom {
                    reference: Box::new(TargetFilter::GrantingObject { bound: None }),
                }]),
        )
    }

    fn untap_unless_two_others() -> ReplacementDefinition {
        ReplacementDefinition::new(ReplacementEvent::Untap)
            .valid_card(TargetFilter::SelfRef)
            .condition(ReplacementCondition::UnlessControlsCountMatching {
                minimum: 2,
                filter: artifacts_other_than_granter(),
            })
    }

    #[test]
    fn granted_replacement_condition_excludes_the_granter() {
        for (others, untaps) in [(1, false), (2, true)] {
            let mut b = board_others("{T}: Draw a card.", others);
            grant_replacement(&mut b, untap_unless_two_others());
            let host = b.host;
            assert_eq!(untap(&mut b, host), untaps, "others={others}");
        }
    }

    /// A Foo Bar that grants `body` to each creature P0 controls, beside one Other artifact.
    fn anthem_board(body: &str) -> Board {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let host = scenario.add_creature(P0, "Bearer", 2, 2).id();
        let fb = scenario
            .add_artifact_from_oracle(
                P0,
                "Foo Bar",
                &format!("Creatures you control have \"{body}\""),
            )
            .id();
        let other = scenario.add_artifact_from_oracle(P0, "Other", "").id();
        let mut runner = scenario.build();
        relayer(runner.state_mut());
        Board {
            runner,
            host,
            granters: vec![fb],
            other: Some(other),
        }
    }

    /// CR 400.7: returns Foo Bar as a new object.
    fn blink(b: &mut Board, id: ObjectId) -> ObjectIncarnationRef {
        let st = b.runner.state_mut();
        move_to_zone(st, id, Zone::Exile, &mut Vec::new());
        move_to_zone(st, id, Zone::Battlefield, &mut Vec::new());
        relayer(st);
        ObjectIncarnationRef::from_object(&b.runner.state().objects[&id])
    }

    #[test]
    fn blinked_granter_rebinds_the_granted_static() {
        let mut b = anthem_board(&format!("{TWO_OTHERS}this creature gets +2/+2."));
        let fb = b.granters[0];
        let before = stamp_of(&b, fb);
        let after = blink(&mut b, fb);
        assert_ne!(before, after);
        assert_eq!(installed_stamps(&b), vec![Some(after)]);
        assert_eq!(b.runner.state().objects[&b.host].power, Some(2));
    }

    #[test]
    fn blinked_granter_rebinds_the_granted_replacement() {
        let mut b = anthem_board("{T}: Draw a card.");
        let fb = b.granters[0];
        {
            let st = b.runner.state_mut();
            let o = st.objects.get_mut(&fb).unwrap();
            let mut grant = o.base_static_definitions[0].clone();
            grant.modifications = vec![ContinuousModification::GrantReplacement {
                replacement: Box::new(untap_unless_two_others()),
            }];
            o.static_definitions.push(grant.clone());
            Arc::make_mut(&mut o.base_static_definitions).push(grant);
            relayer(st);
        }
        let after = blink(&mut b, fb);
        assert!(installed_replacement_stamps(&b).contains(&Some(after)));
        let host = b.host;
        assert!(!untap(&mut b, host));
    }

    const CANT_BE_BLOCKED: &str =
        "{T}: This creature can't be blocked this turn except by artifact creatures other than Foo Bar.";

    /// P1's artifact creatures Foo Bar and Other; Foo Bar grants P0's host `CANT_BE_BLOCKED`,
    /// which the host activates.
    fn evasion_board() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let host = scenario.add_creature(P0, "Bearer", 2, 2).id();
        let fb = scenario
            .add_creature_from_oracle(
                P1,
                "Foo Bar",
                2,
                2,
                &format!("Creatures your opponents control have \"{CANT_BE_BLOCKED}\""),
            )
            .as_artifact()
            .as_creature()
            .id();
        let other = scenario
            .add_creature(P1, "Other", 2, 2)
            .as_artifact()
            .as_creature()
            .id();
        let mut runner = scenario.build();
        relayer(runner.state_mut());
        let index = runner.state().objects[&host].abilities.len() - 1;
        runner
            .act(GameAction::ActivateAbility {
                source_id: host,
                ability_index: index,
            })
            .unwrap();
        runner.advance_until_stack_empty();
        (runner, host, fb, other)
    }

    #[test]
    fn transient_evasion_excludes_the_granter() {
        let (runner, host, fb, other) = evasion_board();
        assert!(!can_block_pair(runner.state(), fb, host));
        assert!(can_block_pair(runner.state(), other, host));
    }

    /// CR 400.7: a latched stamp whose granter changed zones names no object.
    #[test]
    fn transient_evasion_does_not_honor_a_blinked_granter() {
        let (mut runner, host, fb, _) = evasion_board();
        let st = runner.state_mut();
        move_to_zone(st, fb, Zone::Exile, &mut Vec::new());
        move_to_zone(st, fb, Zone::Battlefield, &mut Vec::new());
        relayer(st);
        assert!(can_block_pair(runner.state(), fb, host));
    }

    use engine::types::ability::{AbilityCost, Duration, StaticCondition};
    use engine::types::statics::StaticMode;

    fn three_charge_on_granter() -> (QuantityExpr, Comparator, QuantityExpr) {
        (
            QuantityExpr::Ref {
                qty: QuantityRef::CountersOn {
                    scope: ObjectScope::GrantingObject,
                    counter_type: Some(CounterType::Generic("charge".to_string())),
                },
            },
            Comparator::GE,
            QuantityExpr::Fixed { value: 3 },
        )
    }

    fn pump_while_three_charge_on_granter() -> StaticDefinition {
        let (lhs, comparator, rhs) = three_charge_on_granter();
        StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![
                ContinuousModification::AddPower { value: 2 },
                ContinuousModification::AddToughness { value: 2 },
            ])
            .condition(StaticCondition::QuantityComparison {
                lhs,
                comparator,
                rhs,
            })
    }

    /// Foo Bar grants "{T}: Until end of turn, this creature <`grant`>", activated with three
    /// charge counters on Foo Bar and on the host.
    fn transient_board(grant: StaticDefinition) -> Board {
        let mut b = board_others("{T}: Draw a card.", 0);
        grant_modification(
            &mut b,
            ContinuousModification::GrantAbility {
                definition: Box::new(
                    AbilityDefinition::new(
                        AbilityKind::Activated,
                        Effect::GenericEffect {
                            static_abilities: vec![grant.affected(TargetFilter::SelfRef)],
                            duration: Some(Duration::UntilEndOfTurn),
                            target: None,
                            end_cost: None,
                        },
                    )
                    .cost(AbilityCost::Tap),
                ),
            },
        );
        let (fb, host) = (b.granters[0], b.host);
        set_charge(&mut b.runner, fb, 3);
        set_charge(&mut b.runner, host, 3);
        activate_last(&mut b);
        b.runner.advance_until_stack_empty();
        b
    }

    /// CR 400.7: returns Foo Bar as a new object with three charge counters.
    fn blink_charged_granter(b: &mut Board) {
        let fb = b.granters[0];
        blink(b, fb);
        set_charge(&mut b.runner, fb, 3);
    }

    /// CR 400.7: a transient evasion's counter read names the blinked granter's old incarnation.
    #[test]
    fn transient_evasion_count_does_not_follow_a_blinked_granter() {
        let (count, _, _) = three_charge_on_granter();
        let mut b = transient_board(StaticDefinition::continuous().modifications(vec![
            ContinuousModification::AddStaticMode {
                mode: StaticMode::CantBeBlockedBy {
                    filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature).properties(
                        vec![FilterProp::Cmc {
                            comparator: Comparator::LT,
                            value: count,
                        }],
                    )),
                },
            },
        ]));
        let victim = object_named(&b, "Victim");
        assert!(!can_block_pair(b.runner.state(), victim, b.host));
        blink_charged_granter(&mut b);
        assert!(can_block_pair(b.runner.state(), victim, b.host));
    }

    /// CR 400.7: a static granted until end of turn names the blinked granter's old incarnation.
    #[test]
    fn transient_granted_static_does_not_follow_a_blinked_granter() {
        let mut b = transient_board(StaticDefinition::continuous().modifications(vec![
            ContinuousModification::GrantStaticAbility {
                definition: Box::new(pump_while_three_charge_on_granter()),
            },
        ]));
        assert_eq!(power(&b), 5);
        blink_charged_granter(&mut b);
        assert_eq!(power(&b), 3);
    }

    /// CR 400.7: a replacement granted until end of turn names the blinked granter's old incarnation.
    #[test]
    fn transient_granted_replacement_does_not_follow_a_blinked_granter() {
        let (lhs, comparator, rhs) = three_charge_on_granter();
        let mut b = transient_board(StaticDefinition::continuous().modifications(vec![
            ContinuousModification::GrantReplacement {
                replacement: Box::new(
                    ReplacementDefinition::new(ReplacementEvent::Untap)
                        .valid_card(TargetFilter::SelfRef)
                        .condition(ReplacementCondition::OnlyIfQuantity {
                            lhs,
                            comparator,
                            rhs,
                            active_player_req: None,
                        }),
                ),
            },
        ]));
        let host = b.host;
        assert!(!untap(&mut b, host));
        blink_charged_granter(&mut b);
        assert!(untap(&mut b, host));
    }

    #[test]
    fn transient_cost_reduction_excludes_the_granter() {
        for (payer, left) in [("Foo Bar", 0), ("Other", 1)] {
            let mut b = board_built(
                "{T}: Until end of turn, activated abilities of artifacts other than Foo Bar cost {1} less to activate.",
                &[3],
                Some("{2}: You gain 1 life."),
                "{2}: You gain 1 life.\n",
                |s| {
                    s.with_mana_pool(
                        P0,
                        (0..2)
                            .map(|_| ManaUnit::new(ManaColor::White.into(), ObjectId(0), false, Vec::new()))
                            .collect(),
                    );
                },
            );
            activate_last(&mut b);
            b.runner.advance_until_stack_empty();
            let source = object_named(&b, payer);
            let index = b.runner.state().objects[&source]
                .abilities
                .iter()
                .position(|a| matches!(*a.effect, Effect::GainLife { .. }))
                .unwrap();
            b.runner.activate(source, index).resolve();
            let pool = b.runner.state().players[0].mana_pool.total();
            assert_eq!(pool, left, "payer={payer}");
        }
    }

    /// CR 122.1 + CR 201.5a: counters named on the granter are read from the granter.
    fn charge_board(body: &str, granter: u32, host: u32) -> Board {
        let mut b = board_others(body, 0);
        let st = b.runner.state_mut();
        st.objects
            .get_mut(&b.granters[0])
            .unwrap()
            .counters
            .insert(CounterType::Generic("charge".to_string()), granter);
        st.objects
            .get_mut(&b.host)
            .unwrap()
            .counters
            .insert(CounterType::Generic("charge".to_string()), host);
        relayer(st);
        b
    }

    #[test]
    fn granted_static_counter_condition_reads_the_granter() {
        let body = "As long as there are three or more charge counters on Foo Bar, this creature gets +2/+2.";
        for (granter, host, host_power) in [(3, 1, 5), (1, 3, 3)] {
            let b = charge_board(body, granter, host);
            assert_eq!(power(&b), host_power, "granter={granter} host={host}");
        }
    }

    #[test]
    fn granted_trigger_counter_condition_reads_the_granter() {
        let body = "Whenever this creature attacks, if there are three or more charge counters on Foo Bar, draw a card.";
        for (granter, host, triggered) in [(3, 1, true), (1, 3, false)] {
            let mut b = charge_board(body, granter, host);
            assert_eq!(
                declare_attack(&mut b),
                triggered,
                "granter={granter} host={host}"
            );
        }
    }

    #[test]
    fn granted_state_trigger_fires_on_the_granters_counters() {
        let body = "When there are no charge counters on Foo Bar, draw a card.";
        for (granter, host, fires) in [(0, 1, true), (1, 0, false)] {
            let mut b = charge_board(body, granter, host);
            b.runner.act(GameAction::PassPriority).unwrap();
            let host_id = b.host;
            assert_eq!(
                b.runner
                    .state()
                    .stack
                    .iter()
                    .any(|entry| entry.source_id == host_id),
                fires,
                "granter={granter} host={host}"
            );
        }
    }

    const CHARGE_DURATION: &str =
        "{T}: This creature gets +2/+0 for as long as there are three or more charge counters on Foo Bar.";

    #[test]
    fn granted_duration_reads_the_granters_counters() {
        for (granter, host, host_power) in [(3, 1, 5), (1, 3, 3)] {
            let mut b = charge_board(CHARGE_DURATION, granter, host);
            activate_last(&mut b);
            b.runner.advance_until_stack_empty();
            assert_eq!(power(&b), host_power, "granter={granter} host={host}");
        }
    }

    /// CR 400.7 + CR 611.2b: the duration names the incarnation that was the granter.
    #[test]
    fn granted_duration_does_not_follow_a_blinked_granter() {
        let mut b = charge_board(CHARGE_DURATION, 3, 3);
        activate_last(&mut b);
        b.runner.advance_until_stack_empty();
        assert_eq!(power(&b), 5);
        let fb = b.granters[0];
        blink(&mut b, fb);
        let st = b.runner.state_mut();
        st.objects
            .get_mut(&fb)
            .unwrap()
            .counters
            .insert(CounterType::Generic("charge".to_string()), 3);
        relayer(st);
        assert_eq!(power(&b), 3);
    }

    /// CR 611.2b: an effect-owned duration reads the granter too.
    #[test]
    fn granted_copy_duration_reads_the_granters_counters() {
        let body = "{T}: This creature becomes a copy of target creature for as long as there are three or more charge counters on Foo Bar.";
        for (granter, host, name) in [(3, 1, "Victim"), (1, 3, "Bearer")] {
            let mut b = charge_board(body, granter, host);
            let victim = object_named(&b, "Victim");
            let (source, index) = (b.host, last_ability(&b));
            b.runner
                .activate(source, index)
                .target_object(victim)
                .resolve();
            relayer(b.runner.state_mut());
            assert_eq!(
                b.runner.state().objects[&b.host].name,
                name,
                "granter={granter} host={host}"
            );
        }
    }

    /// CR 611.2b: a play permission's duration names the granter's incarnation.
    #[test]
    fn granted_play_permission_duration_names_the_granter() {
        let body = "{T}: Exile the top card of your library. You may play that card for as long as there are three or more charge counters on Foo Bar.";
        let mut b = charge_board(body, 3, 1);
        let card = b.runner.state().players[0].library[0];
        let (source, index) = (b.host, last_ability(&b));
        b.runner.activate(source, index).resolve();
        let granter = serde_json::to_string(&ObjectScope::SpecificObject {
            object: stamp_of(&b, b.granters[0]),
        })
        .unwrap();
        let permissions =
            serde_json::to_string(&b.runner.state().objects[&card].casting_permissions).unwrap();
        assert!(permissions.contains("ForAsLongAs"), "{permissions}");
        assert!(
            permissions.contains(&granter) && !permissions.contains("GrantingObject"),
            "{permissions}"
        );
    }

    #[test]
    fn granted_state_trigger_counter_condition_reads_the_granter() {
        let parsed = engine::parser::oracle::parse_oracle_text(
            "Equipped creature has \"When there are no charge counters on Foo Bar, draw a card.\"\nEquip {1}",
            "Foo Bar",
            &[],
            &["Artifact".to_string()],
            &["Equipment".to_string()],
        );
        let json = serde_json::to_string(&parsed).unwrap();
        assert!(!json.contains(super::PLACEHOLDER), "{json}");
        let trigger = parsed
            .statics
            .iter()
            .flat_map(|s| s.modifications.iter())
            .find_map(|m| match m {
                ContinuousModification::GrantTrigger { trigger } => Some(trigger),
                _ => None,
            })
            .expect("the granted state trigger");
        assert_eq!(trigger.mode, TriggerMode::StateCondition);
        assert!(serde_json::to_string(&trigger.condition)
            .unwrap()
            .contains("GrantingObject"));
    }

    mod bound_granter {
        use super::*;
        use engine::types::game_state::{AutoMayChoice, MayTriggerAutoChoiceScope};
        use engine::types::keywords::KeywordKind;

        fn set_tapped(b: &mut Board, id: ObjectId, tapped: bool) {
            let st = b.runner.state_mut();
            st.objects.get_mut(&id).unwrap().tapped = tapped;
            relayer(st);
        }

        fn granter_tapped() -> StaticCondition {
            StaticCondition::IsTapped {
                scope: ObjectScope::GrantingObject,
            }
        }

        fn tap_gated_board(condition: StaticCondition) -> Board {
            transient_board(StaticDefinition::continuous().modifications(vec![
                ContinuousModification::GrantStaticAbility {
                    definition: Box::new(
                        StaticDefinition::continuous()
                            .affected(TargetFilter::SelfRef)
                            .modifications(vec![
                                ContinuousModification::AddPower { value: 2 },
                                ContinuousModification::AddToughness { value: 2 },
                            ])
                            .condition(condition),
                    ),
                },
            ]))
        }

        #[test]
        fn tap_gate_reads_the_current_granter() {
            let mut b = tap_gated_board(granter_tapped());
            let fb = b.granters[0];
            assert_eq!(power(&b), 3);
            set_tapped(&mut b, fb, true);
            assert_eq!(power(&b), 5);
        }

        #[test]
        fn tap_gate_ignores_a_blinked_granter() {
            let mut b = tap_gated_board(granter_tapped());
            let fb = b.granters[0];
            blink(&mut b, fb);
            set_tapped(&mut b, fb, true);
            assert_eq!(power(&b), 3);
        }

        #[test]
        fn untapped_gate_reads_the_current_granter() {
            let mut b = tap_gated_board(StaticCondition::Not {
                condition: Box::new(granter_tapped()),
            });
            let fb = b.granters[0];
            assert_eq!(power(&b), 5);
            set_tapped(&mut b, fb, true);
            assert_eq!(power(&b), 3);
        }

        #[test]
        fn damage_all_player_threshold_reads_the_stamped_granter() {
            let mut scenario = GameScenario::new();
            let host = scenario.add_creature(P0, "Bearer", 2, 2).id();
            let fb = scenario.add_creature(P0, "Foo Bar", 4, 4).id();
            scenario.with_counter(fb, CounterType::Plus1Plus1, 3);
            scenario.with_counter(host, CounterType::Plus1Plus1, 1);
            let mut state = scenario.build().state().clone();
            relayer(&mut state);
            state.players[1].life = 2;
            let stamp = ObjectIncarnationRef::from_object(&state.objects[&fb]);
            let effect = Effect::DamageAll {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::None,
                player_filter: Some(PlayerFilter::PlayerAttribute {
                    relation: PlayerRelation::All,
                    attr: Box::new(QuantityRef::LifeTotal {
                        player: PlayerScope::ScopedPlayer,
                    }),
                    comparator: Comparator::LE,
                    value: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::CountersOn {
                            scope: ObjectScope::GrantingObject,
                            counter_type: Some(CounterType::Plus1Plus1),
                        },
                    }),
                }),
                damage_source: None,
            };
            let lives = |granter: Option<ObjectIncarnationRef>| {
                let mut st = state.clone();
                let mut a = ResolvedAbility::new(effect.clone(), vec![], host, P0);
                a.context.granting_object = granter;
                resolve_ability_chain(&mut st, &a, &mut Vec::new(), 0).unwrap();
                (st.players[0].life, st.players[1].life)
            };
            assert_eq!(lives(Some(stamp)), (20, 1));
            assert_eq!(lives(None), (20, 2));
        }

        const RECALL_GRANT: &str =
            "Until end of turn, target creature gains \"{T}: Exile Recall Grant.\"";

        #[test]
        fn spell_grant_names_the_resolved_spell() {
            let mut sc = GameScenario::new();
            sc.at_phase(Phase::PreCombatMain);
            let spell = sc
                .add_spell_to_hand_from_oracle(P0, "Recall Grant", true, RECALL_GRANT)
                .id();
            let host = sc.add_creature(P0, "Host", 2, 2).id();
            let mut runner = sc.build();
            runner.cast(spell).target_object(host).resolve();
            let st = runner.state();
            let index = st.objects[&host].abilities.len() - 1;
            assert!(matches!(
                *st.objects[&host].abilities[index].effect,
                Effect::ChangeZone {
                    target: TargetFilter::GrantingObject { .. },
                    ..
                }
            ));
            runner.activate(host, index).resolve();
            assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        }

        /// CR 113.7 + CR 201.5a: a resolving spell's token static names the spell as it is on the stack.
        #[test]
        fn spell_token_static_names_the_spell() {
            let mut sc = GameScenario::new();
            let spell = sc
                .add_spell_to_hand_from_oracle(P0, "Foo Bar", true, "Create a 2/2 green Ooze creature token with \"This creature gets +2/+2 as long as there are three or more charge counters on Foo Bar.\"")
                .id();
            let mut state = sc.build().state().clone();
            move_to_zone(&mut state, spell, Zone::Stack, &mut Vec::new());
            let on_stack = ObjectIncarnationRef::from_object(&state.objects[&spell]);
            let effect = (*state.objects[&spell].abilities[0].effect).clone();
            assert!(
                matches!(&effect, Effect::Token { static_abilities, .. } if static_abilities.len() == 1),
                "reach-guard: the spell's effect creates a token with one static: {effect:?}"
            );
            let ability = ResolvedAbility::new(effect, vec![], spell, P0);
            resolve_ability_chain(&mut state, &ability, &mut Vec::new(), 0).unwrap();
            let ooze = *state
                .battlefield
                .iter()
                .find(|id| state.objects[id].name == "Ooze")
                .expect("the Ooze token");
            let stamps: Vec<_> = state.objects[&ooze]
                .base_static_definitions
                .iter()
                .map(|s| s.granting_object)
                .collect();
            assert_eq!(stamps, vec![Some(on_stack)]);
        }

        const PRINTED_PT_GRANT: &str = "{T}: Target creature gains \"This creature's power and toughness are each equal to the number of charge counters on Foo Bar.\" until end of turn.";
        const PRINTED_DAMAGE_GRANT: &str = "{T}: Target creature gains \"{T}: This creature deals damage equal to the number of charge counters on Foo Bar to target player.\" until end of turn.";

        fn charge() -> CounterType {
            CounterType::Generic("charge".into())
        }

        fn printed_grant_board(text: &str) -> (GameRunner, ObjectId, ObjectId) {
            let mut sc = GameScenario::new();
            sc.at_phase(Phase::PreCombatMain);
            let src = sc.add_artifact_from_oracle(P0, "Foo Bar", text).id();
            sc.with_counter(src, charge(), 3);
            let host = sc.add_creature(P0, "Host", 1, 1).id();
            let mut runner = sc.build();
            runner.activate(src, 0).target_object(host).resolve();
            relayer(runner.state_mut());
            (runner, src, host)
        }

        fn blink_with_charge(runner: &mut GameRunner, id: ObjectId, n: u32) {
            let st = runner.state_mut();
            move_to_zone(st, id, Zone::Exile, &mut Vec::new());
            move_to_zone(st, id, Zone::Battlefield, &mut Vec::new());
            st.objects
                .get_mut(&id)
                .unwrap()
                .counters
                .insert(charge(), n);
            relayer(st);
        }

        #[test]
        fn printed_static_grant_does_not_follow_a_blinked_source() {
            let (mut runner, src, host) = printed_grant_board(PRINTED_PT_GRANT);
            assert_eq!(runner.state().objects[&host].power, Some(3));
            blink_with_charge(&mut runner, src, 5);
            assert_eq!(runner.state().objects[&host].power, Some(0));
        }

        #[test]
        fn printed_ability_grant_reads_the_departed_source() {
            let (mut runner, src, host) = printed_grant_board(PRINTED_DAMAGE_GRANT);
            blink_with_charge(&mut runner, src, 5);
            let index = runner.state().objects[&host].abilities.len() - 1;
            runner.activate(host, index).target_player(P1).resolve();
            assert_eq!(runner.state().players[1].life, 17);
        }

        const DRAW_BY_GRANTER: &str =
            "{T}: Draw cards equal to the number of +1/+1 counters on Foo Bar.";
        const GAIN_BY_GRANTER: &str =
            "{T}: You gain life equal to the number of +1/+1 counters on Foo Bar.";

        fn foo_bar_gaps(body: &str) -> Vec<String> {
            let oracle = format!("Equipped creature has \"{body}\"");
            let types = ["Artifact".to_string()];
            let subtypes = ["Equipment".to_string()];
            let parsed = engine::parser::oracle::parse_oracle_text(
                &oracle,
                "Foo Bar",
                &[],
                &types,
                &subtypes,
            );
            engine::game::coverage::card_face_gaps(&CardFace {
                name: "Foo Bar".to_string(),
                oracle_text: Some(oracle),
                abilities: parsed.abilities,
                triggers: parsed.triggers,
                static_abilities: parsed.statics,
                replacements: parsed.replacements,
                ..Default::default()
            })
        }

        #[test]
        fn unreached_granter_reference_is_unsupported_and_grants_nothing() {
            let reached = board(GAIN_BY_GRANTER, false);
            let st = reached.runner.state();
            assert_eq!(
                st.objects[&reached.host]
                    .abilities
                    .last()
                    .and_then(|def| def.granting_object),
                Some(ObjectIncarnationRef::from_object(
                    &st.objects[&reached.granters[0]]
                ))
            );
            assert_eq!(foo_bar_gaps(GAIN_BY_GRANTER), Vec::<String>::new());

            assert_eq!(
                foo_bar_gaps(DRAW_BY_GRANTER),
                vec!["Effect:granter_reference_unreached".to_string()]
            );
            let unreached = board(DRAW_BY_GRANTER, false);
            assert!(unreached.runner.state().objects[&unreached.host]
                .abilities
                .is_empty());
        }

        /// `(printed shape, core type)` of a grant whose body draws by the granter's counters.
        const EVERY_PRINTED_KIND: &[(&str, &str)] = &[
            ("Creatures you control have \"{T}: BODY\"", "Artifact"),
            ("{T}: Target creature gains \"{T}: BODY\" until end of turn.", "Artifact"),
            (
                "At the beginning of your upkeep, target creature gains \"{T}: BODY\" until end of turn.",
                "Artifact",
            ),
            (
                "You may have Foo Bar enter as a copy of any creature on the battlefield, except it has \"{T}: BODY\"",
                "Creature",
            ),
        ];

        fn printed_kind_parse(shape: &str, core: &str, body: &str) -> (Vec<String>, String) {
            let oracle = shape.replace("BODY", body);
            let parsed = engine::parser::oracle::parse_oracle_text(
                &oracle,
                "Foo Bar",
                &[],
                &[core.to_string()],
                &[],
            );
            let json = serde_json::to_string(&parsed).unwrap();
            let gaps = engine::game::coverage::card_face_gaps(&CardFace {
                name: "Foo Bar".to_string(),
                oracle_text: Some(oracle),
                abilities: parsed.abilities,
                triggers: parsed.triggers,
                static_abilities: parsed.statics,
                replacements: parsed.replacements,
                ..Default::default()
            });
            (gaps, json)
        }

        #[test]
        fn every_printed_kind_with_an_unreached_reference_is_demoted_whole() {
            const UNREACHED: &str = "Effect:granter_reference_unreached";
            let mut wrong = Vec::new();
            for &(shape, core) in EVERY_PRINTED_KIND {
                let (gaps, json) = printed_kind_parse(
                    shape,
                    core,
                    "You gain life equal to the number of charge counters on Foo Bar.",
                );
                if gaps.iter().any(|g| g == UNREACHED)
                    || !json.contains("\"type\":\"GrantingObject")
                {
                    wrong.push(format!("reach-guard {shape}: {gaps:?}"));
                }
                let (gaps, json) = printed_kind_parse(
                    shape,
                    core,
                    "Draw cards equal to the number of charge counters on Foo Bar.",
                );
                if !gaps.iter().any(|g| g == UNREACHED)
                    || json.contains("\"type\":\"GrantingObject")
                {
                    wrong.push(format!("{shape}: {gaps:?}"));
                }
            }
            assert_eq!(wrong, Vec::<String>::new());
        }

        /// CR 201.5a + CR 613.4b: a granted Animate's dynamic base P/T counts the
        /// granter's counters (5), not the host's (1); CR 613.4c then adds the host's counter.
        #[test]
        fn granted_animate_pt_reads_the_granter() {
            let mut b = board_with("{T}: Draw a card.", &[5], false);
            let granter_counters = PtValue::Quantity(QuantityExpr::Ref {
                qty: QuantityRef::CountersOn {
                    scope: ObjectScope::GrantingObject,
                    counter_type: Some(CounterType::Plus1Plus1),
                },
            });
            grant_modification(
                &mut b,
                ContinuousModification::GrantAbility {
                    definition: Box::new(
                        AbilityDefinition::new(
                            AbilityKind::Activated,
                            Effect::Animate {
                                power: Some(granter_counters.clone()),
                                toughness: Some(granter_counters),
                                types: vec![],
                                remove_types: vec![],
                                target: TargetFilter::None,
                                keywords: vec![],
                            },
                        )
                        .cost(AbilityCost::Tap),
                    ),
                },
            );
            let index = last_ability(&b);
            assert!(matches!(
                *b.runner.state().objects[&b.host].abilities[index].effect,
                Effect::Animate { .. }
            ));
            assert_eq!(power(&b), 3);
            activate(&mut b, index);
            b.runner.advance_until_stack_empty();
            assert!(
                b.runner
                    .state()
                    .transient_continuous_effects
                    .iter()
                    .any(|tce| tce.affected == TargetFilter::SpecificObject { id: b.host }),
                "reach-guard: the Animate installed its effect on the host"
            );
            assert_eq!(power(&b), 6);
        }

        const UNREACHED: &str = "Effect:granter_reference_unreached";

        fn keyword_counters(b: &Board, id: ObjectId, kind: KeywordKind) -> u32 {
            b.runner.state().objects[&id]
                .counters
                .get(&CounterType::Keyword(kind))
                .copied()
                .unwrap_or(0)
        }

        /// Resolves the host's last granted ability, accepting each "you may".
        fn resolve_accepting(b: &mut Board) {
            activate_last(b);
            for _ in 0..8 {
                match b.runner.state().waiting_for {
                    WaitingFor::OptionalEffectChoice { .. } => {
                        b.runner
                            .act(GameAction::DecideOptionalEffect { accept: true })
                            .unwrap();
                    }
                    _ => b.runner.advance_until_stack_empty(),
                }
            }
        }

        /// CR 201.5a + CR 115.10a: an unprompted tap of the granter, or a counter list headed
        /// by it, is refused; the prompted forms and the other granter recipients are not.
        #[test]
        fn granter_read_from_empty_targets_is_refused() {
            let shape = "Creatures you control have \"BODY\"";
            for refused in [
                "{1}: Tap Foo Bar.",
                "{T}: Put a flying counter and a vigilance counter on Foo Bar.",
                "{T}: Put a +1/+1 counter, a flying counter and a vigilance counter on Foo Bar.",
                "Whenever this creature attacks, tap Foo Bar.",
                "{T}: Draw a card. Put a flying counter and a vigilance counter on Foo Bar.",
            ] {
                let (gaps, json) = printed_kind_parse(shape, "Artifact", refused);
                assert_eq!(gaps, [UNREACHED], "{refused}");
                assert!(!json.contains("\"GrantingObject\""), "{refused}: {json}");
            }
            for served in [
                "{1}: You may tap Foo Bar.",
                "{T}: You may put a flying counter and a vigilance counter on Foo Bar.",
                "{T}: Put a flying counter on Foo Bar.",
                "{T}: Remove a +1/+1 counter from Foo Bar.",
                "{T}: Sacrifice Foo Bar.",
                "{T}: Exile Foo Bar.",
                "{T}: Return Foo Bar to its owner's hand.",
                "{T}: Destroy Foo Bar.",
                "Whenever this creature attacks, you may have it fight Foo Bar.",
            ] {
                let (gaps, json) = printed_kind_parse(shape, "Artifact", served);
                assert!(json.contains("\"GrantingObject\""), "reach-guard: {served}");
                assert_eq!(gaps, Vec::<String>::new(), "{served}");
            }
        }

        /// CR 201.5a: a refused body grants its host nothing.
        #[test]
        fn refused_granter_read_grants_nothing() {
            let granted = |body: &str| {
                let b = board(body, false);
                b.runner.state().objects[&b.host].abilities.len()
            };
            assert_eq!(
                granted("{T}: Put a flying counter on Foo Bar."),
                1,
                "reach-guard"
            );
            for body in [
                "{T}: Tap Foo Bar.",
                "{T}: Put a flying counter and a vigilance counter on Foo Bar.",
            ] {
                assert_eq!(granted(body), 0, "{body}");
            }
        }

        /// CR 608.2d + CR 201.5a: the "you may" prompt hands the granter to the tap and to the
        /// counter list's later entries.
        #[test]
        fn prompted_granter_tap_and_counter_list_act_on_the_granter() {
            let mut b = board("{T}: You may tap Foo Bar.", false);
            let fb = b.granters[0];
            resolve_accepting(&mut b);
            assert!(b.runner.state().objects[&fb].tapped);
            assert!(
                !b.runner.state().objects[&b.host].abilities.is_empty(),
                "reach-guard"
            );

            let mut b = board(
                "{T}: You may put a flying counter and a vigilance counter on Foo Bar.",
                false,
            );
            let fb = b.granters[0];
            resolve_accepting(&mut b);
            for (id, expected) in [(fb, 1), (b.host, 0)] {
                assert_eq!(keyword_counters(&b, id, KeywordKind::Flying), expected);
                assert_eq!(keyword_counters(&b, id, KeywordKind::Vigilance), expected);
            }
        }

        /// The own-host list names the host for every entry.
        #[test]
        fn own_host_counter_list_puts_every_counter_on_the_host() {
            let mut b = board(
                "{T}: Put a flying counter and a vigilance counter on this creature.",
                false,
            );
            let fb = b.granters[0];
            activate_last(&mut b);
            b.runner.advance_until_stack_empty();
            for (id, expected) in [(b.host, 1), (fb, 0)] {
                assert_eq!(keyword_counters(&b, id, KeywordKind::Flying), expected);
                assert_eq!(keyword_counters(&b, id, KeywordKind::Vigilance), expected);
            }
        }

        /// CR 603.5 + CR 201.5a: a remembered "you may" answer hands the reader the granter the
        /// prompt would.
        #[test]
        fn remembered_answer_hands_the_granter_to_a_target_slot_reader() {
            let mut b = board_full(
                "Whenever you gain life, you may tap Foo Bar.",
                &[3],
                None,
                "{0}: You gain 1 life.\n",
            );
            let fb = b.granters[0];
            for prompted in [true, false] {
                b.runner.state_mut().objects.get_mut(&fb).unwrap().tapped = false;
                b.runner
                    .act(GameAction::ActivateAbility {
                        source_id: fb,
                        ability_index: 0,
                    })
                    .unwrap();
                assert_eq!(remember_accept(&mut b.runner), prompted, "reach-guard");
                assert!(b.runner.state().objects[&fb].tapped, "prompted={prompted}");
            }
        }

        /// Drives to rest, answering each "you may" with a remembered yes; whether it asked.
        fn remember_accept(runner: &mut GameRunner) -> bool {
            let mut asked = false;
            for _ in 0..8 {
                match runner.state().waiting_for {
                    WaitingFor::OptionalEffectChoice { .. } => {
                        asked = true;
                        runner
                            .act(GameAction::DecideOptionalEffectAndRemember {
                                choice: AutoMayChoice::Accept,
                                scope: MayTriggerAutoChoiceScope::ExactInstance,
                            })
                            .unwrap();
                    }
                    _ => runner.advance_until_stack_empty(),
                }
            }
            asked
        }

        /// CR 603.5 + CR 701.14a: Grothama's form, "you may have it fight <granter>", fights the
        /// granter on a remembered answer too.
        #[test]
        fn remembered_answer_fights_the_granter() {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let host = scenario
                .add_creature_from_oracle(P0, "Bearer", 2, 9, "{0}: You gain 1 life.")
                .id();
            let fb = scenario
                .add_creature_from_oracle(
                    P0,
                    "Foo Bar",
                    1,
                    9,
                    "Other creatures you control have \"Whenever you gain life, you may have \
                     this creature fight Foo Bar.\"",
                )
                .id();
            let mut runner = scenario.build();
            relayer(runner.state_mut());
            for (prompted, damage) in [(true, (2, 1)), (false, (4, 2))] {
                runner
                    .act(GameAction::ActivateAbility {
                        source_id: host,
                        ability_index: 0,
                    })
                    .unwrap();
                assert_eq!(remember_accept(&mut runner), prompted, "reach-guard");
                let st = runner.state();
                assert_eq!(
                    (
                        st.objects[&fb].damage_marked,
                        st.objects[&host].damage_marked
                    ),
                    damage,
                    "prompted={prompted}"
                );
            }
        }
    }
}

/// CR 601.2a + CR 201.5a: "the player who cast <granter>" is latched to the spell's caster when
/// the grant is installed; a copy that was not cast has no caster (CR 707.10).
mod granted_caster_reference {
    use super::*;
    use engine::game::scenario::GameRunner;
    use engine::types::ability::{PlayerFilter, TargetRef};
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;

    const TWINCAST: &str =
        "Copy target instant or sorcery spell. You may choose new targets for the copy.";
    const PINGER: &str = "{T}: This creature deals 1 damage to any target.";

    fn granted_recipient(oracle: &str) -> (Option<TargetFilter>, String) {
        let parsed =
            parse_oracle_text(oracle, "Hellish Rebuke", &[], &["Instant".to_string()], &[]);
        let json = serde_json::to_string(&parsed).expect("ParsedAbilities serializes");
        let recipient = parsed
            .abilities
            .iter()
            .find_map(|ability| match &*ability.effect {
                Effect::GenericEffect {
                    static_abilities, ..
                } => static_abilities
                    .iter()
                    .flat_map(|s| &s.modifications)
                    .find_map(|m| match m {
                        ContinuousModification::GrantTrigger { trigger } => {
                            Some(trigger.valid_target.clone())
                        }
                        _ => None,
                    }),
                _ => None,
            })
            .expect("reach-guard: the spell grants a trigger");
        (recipient, json)
    }

    #[test]
    fn caster_reference_parses_as_the_granted_triggers_recipient() {
        let (recipient, json) = granted_recipient(HELLISH_REBUKE);
        assert_eq!(
            recipient,
            Some(TargetFilter::PlayerMatching {
                player: Box::new(PlayerFilter::GrantingObjectCaster),
            })
        );
        assert!(!json.contains("Unimplemented"), "{json}");
        assert!(!json.contains(PLACEHOLDER), "{json}");

        let pronoun = HELLISH_REBUKE.replace("who cast Hellish Rebuke", "who cast it");
        let (_, json) = granted_recipient(&pronoun);
        assert!(!json.contains("GrantingObjectCaster"));

        let any_player = HELLISH_REBUKE.replace("the player who cast Hellish Rebuke", "a player");
        assert_eq!(granted_recipient(&any_player).0, Some(TargetFilter::Player));
    }

    fn rebuke_board() -> (GameRunner, [ObjectId; 2], ObjectId, ObjectId) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let pingers = [
            scenario
                .add_creature_from_oracle(P1, "Pinger A", 1, 1, PINGER)
                .id(),
            scenario
                .add_creature_from_oracle(P1, "Pinger B", 1, 1, PINGER)
                .id(),
        ];
        let rebuke = scenario
            .add_spell_to_hand_from_oracle(P0, "Hellish Rebuke", true, HELLISH_REBUKE)
            .with_mana_cost(ManaCost::zero())
            .id();
        let twincast = scenario
            .add_spell_to_hand_from_oracle(P0, "Twincast", true, TWINCAST)
            .with_mana_cost(ManaCost::zero())
            .id();
        (scenario.build(), pingers, rebuke, twincast)
    }

    /// P1 activates `pinger` at `player` and only that activation resolves.
    fn ping(runner: &mut GameRunner, pinger: ObjectId, player: PlayerId) {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0) {
            runner.act(GameAction::PassPriority).expect("P0 passes");
        }
        runner
            .act(GameAction::ActivateAbility {
                source_id: pinger,
                ability_index: 0,
            })
            .expect("the pinger activates");
        while matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ) {
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(player)),
                })
                .expect("the player is a legal target");
        }
        runner.resolve_top();
    }

    fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
        runner.state().objects[&id].zone
    }

    #[test]
    fn granted_trigger_fires_only_on_damage_to_the_caster() {
        let (mut runner, [pinger, _], rebuke, _) = rebuke_board();
        runner.cast(rebuke).resolve();
        ping(&mut runner, pinger, P0);
        runner.advance_until_stack_empty();
        assert_eq!(zone(&runner, pinger), Zone::Graveyard, "reach-guard");
        assert_eq!(runner.state().players[1].life, 18);

        let (mut runner, [pinger, _], rebuke, _) = rebuke_board();
        runner.cast(rebuke).resolve();
        ping(&mut runner, pinger, P1);
        runner.advance_until_stack_empty();
        assert_eq!(zone(&runner, pinger), Zone::Battlefield);
        assert_eq!(runner.state().players[1].life, 19);
    }

    #[test]
    fn uncast_copy_grants_a_trigger_that_never_fires() {
        let (mut runner, [first, second], rebuke, twincast) = rebuke_board();
        runner
            .cast(rebuke)
            .commit()
            .cast(twincast)
            .target_object(rebuke)
            .commit();
        // Resolves Twincast and its copy of Hellish Rebuke; the original stays on the stack.
        runner.resolve_top();
        assert_eq!(zone(&runner, rebuke), Zone::Stack);
        ping(&mut runner, first, P0);
        assert_eq!(zone(&runner, first), Zone::Battlefield);
        assert_eq!(runner.state().players[1].life, 20);

        runner.advance_until_stack_empty();
        ping(&mut runner, second, P0);
        runner.advance_until_stack_empty();
        assert_eq!(zone(&runner, second), Zone::Graveyard, "reach-guard");
        assert_eq!(runner.state().players[1].life, 18);
    }

    const BODY: &str =
        "When this creature deals damage to the player who cast Foo Bar, draw a card.";
    const ANY_PLAYER: &str = "When this creature deals damage to a player, draw a card.";

    fn foo_bar_gaps(oracle: &str, card_type: &str) -> (Vec<String>, String) {
        let parsed = parse_oracle_text(oracle, "Foo Bar", &[], &[card_type.to_string()], &[]);
        let json = serde_json::to_string(&parsed).expect("ParsedAbilities serializes");
        let gaps = engine::game::coverage::card_face_gaps(&engine::types::card::CardFace {
            name: "Foo Bar".to_string(),
            oracle_text: Some(oracle.to_string()),
            abilities: parsed.abilities,
            triggers: parsed.triggers,
            static_abilities: parsed.statics,
            replacements: parsed.replacements,
            ..Default::default()
        });
        (gaps, json)
    }

    /// Carriers of a quoted `body` that no cast reaches, with the card type each is printed on.
    fn uncast_carriers(body: &str) -> [(String, &'static str); 8] {
        let inner = body.replace('"', "'");
        [
            (format!("Creatures you control have \"{body}\""), "Creature"),
            (
                format!("Create a 1/1 white Spirit creature token with \"{body}\""),
                "Instant",
            ),
            (
                format!(
                    "When Foo Bar enters, create a 1/1 white Spirit creature token with \"{body}\""
                ),
                "Creature",
            ),
            (
                format!(
                    "When this enchantment enters, you get an emblem with \"Creatures you \
                     control have '{inner}'\""
                ),
                "Enchantment",
            ),
            (
                format!(
                    "When Foo Bar enters, creatures you control gain \"{body}\" until end of turn."
                ),
                "Creature",
            ),
            (
                format!("{{T}}: Target creature gains \"{body}\" until end of turn."),
                "Creature",
            ),
            (
                format!(
                    "At the beginning of the next end step, creatures you control gain \"{body}\" \
                     until end of turn."
                ),
                "Instant",
            ),
            (
                format!(
                    "Flip a coin. If you win the flip, creatures you control gain \"{body}\" \
                     until end of turn."
                ),
                "Instant",
            ),
        ]
    }

    /// CR 601.2i + CR 707.10: off a spell's own instructions there is no cast to name.
    #[test]
    fn caster_reference_no_cast_reaches_is_refused() {
        let rows = |body: &str, expected: &[&str]| {
            let carriers = uncast_carriers(body);
            let actual: Vec<(String, Vec<String>)> = carriers
                .iter()
                .map(|(oracle, card_type)| (oracle.clone(), foo_bar_gaps(oracle, card_type).0))
                .collect();
            let expected: Vec<(String, Vec<String>)> = carriers
                .into_iter()
                .map(|(oracle, _)| (oracle, expected.iter().map(|g| g.to_string()).collect()))
                .collect();
            (actual, expected)
        };
        let (actual, expected) = rows(BODY, &["Effect:granter_reference_unreached"]);
        assert_eq!(actual, expected);
        let (actual, expected) = rows(ANY_PLAYER, &[]);
        assert_eq!(actual, expected);
    }

    /// CR 601.2i: a grant anywhere on the cast spell's chain names that spell's caster.
    #[test]
    fn caster_reference_on_the_cast_spells_chain_stays_supported() {
        let q = format!("\"{BODY}\"");
        for oracle in [
            format!("Draw a card. Creatures you control gain {q} until end of turn."),
            format!("Draw a card, then creatures you control gain {q} until end of turn."),
            format!(
                "If you control an artifact, draw a card. Otherwise, creatures you control gain \
                 {q} until end of turn."
            ),
            format!("If you control a creature, creatures you control gain {q} until end of turn."),
            format!(
                "Choose one \u{2014}\n\u{2022} Creatures you control gain {q} until end of \
                 turn.\n\u{2022} Draw a card."
            ),
            format!(
                "Kicker {{2}}\nDraw a card. If this spell was kicked, creatures you control gain \
                 {q} until end of turn."
            ),
            format!(
                "You may draw a card. If you do, creatures you control gain {q} until end of turn."
            ),
            format!(
                "Spree\n+ {{1}} \u{2014} Draw a card.\n+ {{1}} \u{2014} Creatures you control \
                 gain {q} until end of turn."
            ),
            format!("Until end of turn, creatures you control gain {q}."),
            format!("Target creature gains {q} until end of turn. Draw a card."),
        ] {
            let (gaps, json) = foo_bar_gaps(&oracle, "Instant");
            assert!(
                json.contains("GrantingObjectCaster"),
                "reach-guard: {oracle}: {json}"
            );
            assert_eq!(gaps, Vec::<String>::new(), "{oracle}");
        }
    }

    /// P0 casts the Foo Bar `add_foo_bar` puts in hand, then P0's pinger deals 1 damage to P0.
    fn cast_then_ping_self(
        add_foo_bar: impl FnOnce(&mut GameScenario) -> ObjectId,
    ) -> (GameRunner, ObjectId, ObjectId, usize) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Card A", "Card B", "Card C"]);
        let pinger = scenario
            .add_creature_from_oracle(P0, "Pinger", 1, 1, PINGER)
            .id();
        let foo_bar = add_foo_bar(&mut scenario);
        let mut runner = scenario.build();
        runner.cast(foo_bar).resolve();
        runner.advance_until_stack_empty();
        let before = runner.state().players[0].hand.len();
        runner
            .act(GameAction::ActivateAbility {
                source_id: pinger,
                ability_index: 0,
            })
            .expect("the pinger activates");
        while matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ) {
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(P0)),
                })
                .expect("P0 is a legal target");
        }
        runner.advance_until_stack_empty();
        assert_eq!(
            runner.state().players[0].life,
            19,
            "reach-guard: the ping resolved"
        );
        let drawn = runner.state().players[0].hand.len() - before;
        (runner, pinger, foo_bar, drawn)
    }

    fn creature_foo_bar(oracle: String) -> impl FnOnce(&mut GameScenario) -> ObjectId {
        move |scenario| {
            scenario
                .add_creature_to_hand_from_oracle(P0, "Foo Bar", 1, 1, &oracle)
                .with_mana_cost(ManaCost::zero())
                .id()
        }
    }

    #[test]
    fn refused_static_caster_grant_installs_nothing() {
        let (runner, pinger, foo_bar, drawn) = cast_then_ping_self(creature_foo_bar(format!(
            "Creatures you control have \"{BODY}\""
        )));
        assert_eq!(zone(&runner, foo_bar), Zone::Battlefield, "reach-guard");
        let granted = serde_json::to_string(&runner.state().objects[&pinger].trigger_definitions)
            .expect("triggers serialize");
        assert!(
            !granted.contains("GrantingObjectCaster"),
            "a refused static must grant no caster trigger: {granted}"
        );
        assert_eq!(drawn, 0);

        let (_, _, _, drawn) = cast_then_ping_self(creature_foo_bar(format!(
            "Creatures you control have \"{ANY_PLAYER}\""
        )));
        assert_eq!(drawn, 1);
    }

    #[test]
    fn spell_chain_caster_grant_draws_on_damage_to_the_caster() {
        let oracle =
            format!("Draw a card. Creatures you control gain \"{BODY}\" until end of turn.");
        let (_, _, _, drawn) = cast_then_ping_self(|scenario| {
            scenario
                .add_spell_to_hand_from_oracle(P0, "Foo Bar", true, &oracle)
                .with_mana_cost(ManaCost::zero())
                .id()
        });
        assert_eq!(drawn, 1);
    }
}

/// CR 111.1 + CR 114.1 + CR 201.5a: a token or an emblem is another object, so a granter
/// name the masker refused in its body would read that object; on an equipped creature an
/// "exiled with" name would read the creature's own exiles.
#[test]
fn refused_name_on_a_created_or_equipped_host_is_demoted() {
    for (oracle, types, subtypes) in [
        (
            "Whenever a creature you control dies, create a 1/1 green Ooze creature token with \
             \"{T}: Foo Bar deals 1 damage to any target.\"",
            "Enchantment",
            None,
        ),
        (
            "When this enchantment enters, you get an emblem with \"Whenever a creature you \
             control dies, Foo Bar deals 1 damage to any target.\"",
            "Enchantment",
            None,
        ),
        (
            "Equipped creature has \"You may play cards exiled with Foo Bar.\"\nEquip {1}",
            "Artifact",
            Some("Equipment"),
        ),
    ] {
        assert!(
            !normalize_card_name_refs_reporting(oracle, "Foo Bar")
                .1
                .is_empty(),
            "reach-guard: {oracle}"
        );
        let subtypes: Vec<String> = subtypes.into_iter().map(str::to_string).collect();
        let parsed = parse_oracle_text(oracle, "Foo Bar", &[], &[types.to_string()], &subtypes);
        assert_eq!(granter_residuals(&parsed).len(), 1, "{oracle}: {parsed:#?}");
    }
}

/// CR 607.1d: Tibalt's emblem latches Tibalt, so its "exiled with Tibalt" is no granter
/// reference and the emblem stays supported.
#[test]
fn emblem_linked_exile_name_is_not_refused() {
    assert!(
        normalize_card_name_refs_reporting(TIBALT_COSMIC_IMPOSTOR, "Tibalt, Cosmic Impostor")
            .1
            .is_empty()
    );
    let parsed = parse_oracle_text(
        TIBALT_COSMIC_IMPOSTOR,
        "Tibalt, Cosmic Impostor",
        &[],
        &["Planeswalker".to_string()],
        &["Tibalt".to_string()],
    );
    assert!(granter_residuals(&parsed).is_empty(), "{parsed:#?}");
    let json = serde_json::to_string(&parsed.replacements).expect("replacements serialize");
    assert!(
        json.contains("\"CreateEmblem\"") && json.contains("ExileCastPermission"),
        "{json}"
    );
}
