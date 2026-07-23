//! GitHub #6013 — Metamorphic Alteration.
//!
//! ```text
//! Enchant creature
//! As this Aura enters, choose a creature.
//! Enchanted creature is a copy of the chosen creature.
//! ```
//!
//! The Aura latches the CHOSEN creature's copiable values (fixed as the copy
//! effect first starts to apply, CR 707.2c; unaffected by later changes to the
//! chosen object, CR 707.2b) and installs them as a Layer-1 copy on its
//! ENCHANTED HOST — the entering Aura itself never becomes a copy. The install
//! reuses the single copy-values authority (`apply_precomputed_copy_values`),
//! and ends when the Aura leaves the battlefield (CR 400.7 / CR 611.2a).

use engine::game::game_object::{AttachTarget, DisplaySource};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    ChoosePermanentPersist, ChosenAttribute, ContinuousModification, Effect, FilterProp,
    TargetFilter, TypedFilter,
};
use engine::types::card::TokenImageRef;
use engine::types::card_type::CoreType;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const METAMORPHIC_ALTERATION: &str = "Enchant creature\nAs this Aura enters, choose a creature.\nEnchanted creature is a copy of the chosen creature.";

/// Stage Metamorphic Alteration in P0's hand as a {2}{U} Aura carrying its
/// parsed as-enters choice + copy static. Identity (Enchantment / Aura /
/// mana cost) is set BEFORE `from_oracle_text`, which preserves identity fields
/// while installing the parsed abilities/keywords/replacements/statics.
fn stage_metamorphic(scenario: &mut GameScenario) -> ObjectId {
    let aura = scenario
        .add_spell_to_hand(P0, "Metamorphic Alteration", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Blue],
        })
        .from_oracle_text(METAMORPHIC_ALTERATION)
        .id();
    // `add_spell_to_hand(_, is_instant = false)` seeds `CoreType::Sorcery`, and
    // `as_enchantment` only strips `Creature`. CR 205.3h: Aura is an enchantment
    // subtype — drop the stray non-permanent type so the staged card is a clean
    // enchantment spell rather than a Sorcery+Enchantment hybrid.
    let obj = scenario.state_mut().objects.get_mut(&aura).unwrap();
    obj.card_types
        .core_types
        .retain(|t| !matches!(t, CoreType::Instant | CoreType::Sorcery));
    obj.base_card_types = obj.card_types.clone();
    aura
}

fn blue_pool(scenario: &mut GameScenario) {
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(9_990), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_991), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_992), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_993), false, vec![]),
        ],
    );
}

/// CR 707.2c + CR 614.12a + CR 613.1a: casting the Aura on a host, then choosing
/// a donor, turns the ENCHANTED HOST into a copy of the donor — while the Aura
/// itself stays a Metamorphic Alteration Aura attached to that host (it never
/// `BecomeCopy`s). Reverting the install leaves the host as Grizzly Bears 2/2,
/// so every copied-value assertion below flips.
#[test]
fn enchanted_creature_becomes_copy_of_chosen_and_aura_is_unchanged() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let donor = scenario
        .add_creature_from_oracle(P0, "Serra Angel", 4, 4, "Flying")
        .id();
    let host = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let aura = stage_metamorphic(&mut scenario);
    blue_pool(&mut scenario);

    let mut runner = scenario.build();

    runner
        .cast(aura)
        .target_object(host)
        .copy_target(donor)
        .resolve();

    let host_obj = &runner.state().objects[&host];
    assert_eq!(
        host_obj.name, "Serra Angel",
        "CR 707.2c: enchanted host takes on the chosen creature's copiable name"
    );
    assert_eq!(host_obj.power, Some(4), "host copies the donor's power");
    assert_eq!(
        host_obj.toughness,
        Some(4),
        "host copies the donor's toughness"
    );
    assert!(
        host_obj.keywords.contains(&Keyword::Flying),
        "host copies the donor's copiable keywords (CR 707.2)"
    );

    let aura_obj = &runner.state().objects[&aura];
    assert_eq!(
        aura_obj.name, "Metamorphic Alteration",
        "CR 707.4: the Aura's own identity must be unchanged — it never becomes a copy"
    );
    assert!(
        aura_obj
            .card_types
            .core_types
            .contains(&CoreType::Enchantment)
            && !aura_obj.card_types.core_types.contains(&CoreType::Creature),
        "the Aura stays an Enchantment and is never turned into a creature copy"
    );
    assert_eq!(
        aura_obj.attached_to,
        Some(AttachTarget::Object(host)),
        "CR 608.3c: a spell-cast Aura is put onto the battlefield attached to its \
         enchant target, and the copy choice never disturbs that attachment"
    );
}

/// CR 707.2b: the copy is a FROZEN snapshot taken when the effect first started
/// to apply — later changes to the chosen creature never propagate to the host.
/// Mutating the donor's base P/T and re-running the layer engine must leave the
/// host at the originally-copied 4/4. A live re-read of the donor would flip the
/// host to 9/9.
#[test]
fn copied_values_are_a_frozen_snapshot_of_the_chosen_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let donor = scenario
        .add_creature_from_oracle(P0, "Serra Angel", 4, 4, "Flying")
        .id();
    let host = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let aura = stage_metamorphic(&mut scenario);
    blue_pool(&mut scenario);

    let mut runner = scenario.build();
    runner
        .cast(aura)
        .target_object(host)
        .copy_target(donor)
        .resolve();
    assert_eq!(runner.state().objects[&host].power, Some(4));

    {
        let donor_obj = runner.state_mut().objects.get_mut(&donor).unwrap();
        donor_obj.base_power = Some(9);
        donor_obj.base_toughness = Some(9);
    }
    engine::game::layers::evaluate_layers(runner.state_mut());

    let host_obj = &runner.state().objects[&host];
    assert_eq!(
        host_obj.power,
        Some(4),
        "CR 707.2b: the snapshot is frozen — the host keeps the donor's power at choice time"
    );
    assert_eq!(
        host_obj.toughness,
        Some(4),
        "CR 707.2b: later changes to the chosen creature must not propagate to the host"
    );
}

/// CR 400.7 + CR 611.2a: the copy ends when the Aura leaves the battlefield
/// (`Duration::UntilHostLeavesPlay`, pruned on the Aura as the effect's source).
/// After the Aura is gone the host reverts to its own printed identity.
#[test]
fn host_reverts_when_the_aura_leaves_play() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let donor = scenario
        .add_creature_from_oracle(P0, "Serra Angel", 4, 4, "Flying")
        .id();
    let host = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let aura = stage_metamorphic(&mut scenario);
    blue_pool(&mut scenario);

    let mut runner = scenario.build();
    runner
        .cast(aura)
        .target_object(host)
        .copy_target(donor)
        .resolve();
    assert_eq!(runner.state().objects[&host].name, "Serra Angel");

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), aura, Zone::Graveyard, &mut events);
    engine::game::layers::prune_host_left_effects(runner.state_mut(), aura);
    engine::game::layers::evaluate_layers(runner.state_mut());

    let host_obj = &runner.state().objects[&host];
    assert_eq!(
        host_obj.name, "Grizzly Bears",
        "CR 400.7: the copy ends when the source Aura leaves — the host reverts to its own name"
    );
    assert_eq!(host_obj.power, Some(2), "host reverts to its printed power");
    assert_eq!(
        host_obj.toughness,
        Some(2),
        "host reverts to its printed toughness"
    );
    assert!(
        !host_obj.keywords.contains(&Keyword::Flying),
        "the copied Flying keyword ends with the Aura"
    );
}

/// CR 115.10a: "choose a creature" is a CHOICE, not targeting — hexproof (which
/// only protects against opponents' *targeted* spells and abilities) never
/// removes a creature from the copy-choice pool. An opponent's hexproof creature
/// must be a legal donor, and the host becomes a copy of it. Reverting the copy
/// install leaves the host as Grizzly Bears 2/2, so every assertion below flips.
#[test]
fn hexproof_opponent_creature_is_a_legal_copy_donor() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Opponent-controlled donor with Hexproof: a *targeted* effect could never
    // pick it, but this choice can (CR 115.10a).
    let donor = scenario
        .add_creature_from_oracle(P1, "Serra Angel", 4, 4, "Flying")
        .hexproof()
        .id();
    let host = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let aura = stage_metamorphic(&mut scenario);
    blue_pool(&mut scenario);

    let mut runner = scenario.build();
    runner
        .cast(aura)
        .target_object(host)
        .copy_target(donor)
        .resolve();

    let host_obj = &runner.state().objects[&host];
    assert_eq!(
        host_obj.name, "Serra Angel",
        "CR 115.10a: an opponent's hexproof creature is still a legal copy CHOICE — the host copies it"
    );
    assert_eq!(
        host_obj.power,
        Some(4),
        "host copies the hexproof donor's power"
    );
    assert!(
        host_obj.keywords.contains(&Keyword::Flying),
        "host copies the donor's copiable keywords across the choice"
    );
    assert!(
        host_obj.keywords.contains(&Keyword::Hexproof),
        "the copied identity includes the donor's Hexproof (a copiable keyword)"
    );
}

/// CR 609.3 + CR 303.4: the copy-choice pool is every creature matching "a
/// creature", and an Enchant creature Aura guarantees a creature host — so the
/// host itself is always in the pool. Even when the host is the ONLY creature on
/// the battlefield the pool is non-empty and the choice is raised (choosing the
/// host copies its own values — a legal, if inert, pick).
///
/// This is why the empty-pool early-skip in
/// `apply_pending_post_replacement_effect` (the `Effect::ChoosePermanent` arm's
/// `valid_targets.is_empty()` guard, CR 609.3 "do only as much as possible") is
/// UNREACHABLE on the spell-cast Enchant-creature path: `find_copy_targets` over
/// "a creature" always contains the host. A genuinely empty pool (zero creatures
/// anywhere) would also leave the Aura spell with no legal enchant target, so
/// CR 608.2b counters it and it never enters — the "as this Aura enters" choose
/// replacement never fires with an empty pool. We therefore prove pool
/// composition here rather than fabricating an unreachable empty-pool fixture.
#[test]
fn host_is_the_only_creature_and_remains_a_legal_copy_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let host = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let aura = stage_metamorphic(&mut scenario);
    blue_pool(&mut scenario);

    let mut runner = scenario.build();
    runner
        .cast(aura)
        .target_object(host)
        .copy_target(host)
        .resolve();

    // Reach-guard: inert self-copy leaves printed P/T unchanged, so the
    // host-stays-Grizzly assertions below would also pass if CopyTargetChoice
    // were skipped entirely. The snapshot + Layer-1 CopyValues TCE are written
    // only on the answer path — either missing flips if that path is bypassed.
    assert!(
        runner.state().objects[&aura]
            .chosen_attributes
            .iter()
            .any(|a| matches!(a, ChosenAttribute::CopiableSnapshot(_))),
        "CopyTargetChoice must latch ChosenAttribute::CopiableSnapshot on the Aura \
         (absent if the answer path was skipped)"
    );
    assert!(
        runner
            .state()
            .transient_continuous_effects
            .iter()
            .any(|tce| {
                tce.source_id == aura
                    && tce
                        .modifications
                        .iter()
                        .any(|m| matches!(m, ContinuousModification::CopyValues { .. }))
                    && matches!(
                        &tce.affected,
                        TargetFilter::SpecificObject { id } if *id == host
                    )
            }),
        "CopyTargetChoice must install a Layer-1 CopyValues TCE sourced from the Aura \
         on the enchanted host (absent if the answer path was skipped)"
    );

    let host_obj = &runner.state().objects[&host];
    assert_eq!(
        host_obj.name, "Grizzly Bears",
        "choosing the host itself copies its own values — the host stays Grizzly Bears"
    );
    assert_eq!(
        host_obj.power,
        Some(2),
        "self-copy keeps the host's printed power"
    );
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(host)),
        "CR 608.3c: the Aura remains attached to the host it enchanted"
    );
}

/// Two Metamorphic Alterations on two different hosts, each choosing a DIFFERENT
/// donor, resolve independently: each host copies its own Aura's chosen donor.
/// The frozen snapshot is latched per-Aura (CR 707.2c) and installed on that
/// Aura's own enchanted host (CR 303.4 + CR 613.1a), so the two copy effects
/// never bleed. If host B picked up host A's donor's Flying, the independence
/// assertion below flips.
#[test]
fn two_auras_install_independent_copies_on_their_own_hosts() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let donor_a = scenario
        .add_creature_from_oracle(P0, "Serra Angel", 4, 4, "Flying")
        .id();
    let donor_b = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let host_a = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let host_b = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
    let aura_a = stage_metamorphic(&mut scenario);
    let aura_b = stage_metamorphic(&mut scenario);

    // Two casts of {2}{U}: 2 blue + 4 colorless.
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(9_990), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(9_991), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_992), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_993), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_994), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_995), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    runner
        .cast(aura_a)
        .target_object(host_a)
        .copy_target(donor_a)
        .resolve();
    runner
        .cast(aura_b)
        .target_object(host_b)
        .copy_target(donor_b)
        .resolve();

    let ha = &runner.state().objects[&host_a];
    assert_eq!(
        ha.name, "Serra Angel",
        "host A copies Aura A's donor (Serra Angel)"
    );
    assert_eq!(ha.power, Some(4));
    assert!(ha.keywords.contains(&Keyword::Flying));

    let hb = &runner.state().objects[&host_b];
    assert_eq!(
        hb.name, "Hill Giant",
        "host B copies Aura B's donor (Hill Giant)"
    );
    assert_eq!(hb.power, Some(3));
    assert!(
        !hb.keywords.contains(&Keyword::Flying),
        "host B must NOT pick up host A's donor's Flying — the two copies are independent"
    );
}

/// CR 111.1 + CR 707.2: art routing follows the copy. When the chosen donor is a
/// true token, the host's display routing switches to the token art database
/// (`DisplaySource::Token` + the source token's `token_image_ref`) — carried
/// alongside the copiable values, NOT as a copiable value itself. The host is a
/// nontoken; reverting the copy install would reset its display routing to
/// `Card`/`None`, flipping both display assertions.
#[test]
fn host_copying_a_token_donor_routes_token_display() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let donor = scenario.add_creature(P0, "Angel", 4, 4).id();
    // Make the donor a TRUE token so its display routes to the token art db
    // (CR 111.1): `is_token` with no `base_printed_ref` derives
    // `DisplaySource::Token` in the layer engine; its `token_image_ref` is its
    // own art pointer.
    {
        let d = scenario.state_mut().objects.get_mut(&donor).unwrap();
        d.is_token = true;
        d.base_printed_ref = None;
        d.token_image_ref = Some(TokenImageRef {
            scryfall_id: "tok-angel-1".to_string(),
            scryfall_oracle_id: None,
            face_name: None,
            preset_id: "angel_4_4".to_string(),
        });
    }
    let host = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let aura = stage_metamorphic(&mut scenario);
    blue_pool(&mut scenario);

    let mut runner = scenario.build();
    runner
        .cast(aura)
        .target_object(host)
        .copy_target(donor)
        .resolve();

    let host_obj = &runner.state().objects[&host];
    assert_eq!(host_obj.name, "Angel", "host copies the token donor's name");
    assert_eq!(
        host_obj.power,
        Some(4),
        "host copies the token donor's power"
    );
    assert_eq!(
        host_obj.display_source,
        DisplaySource::Token,
        "CR 111.1 + CR 707.2: copying a true token routes the host's art to the token db"
    );
    assert_eq!(
        host_obj
            .token_image_ref
            .as_ref()
            .map(|r| r.preset_id.as_str()),
        Some("angel_4_4"),
        "the source token's token_image_ref rides along with the copy"
    );
}

/// SHAPE: the "As this Aura enters, choose a creature." line parses to an
/// as-enters `Effect::ChoosePermanent { persist: CopiableSnapshot }` over a
/// creature copy-source pool — never a `BecomeCopy` on the entering Aura.
#[test]
fn as_enters_choose_a_creature_parses_to_choose_permanent() {
    let parsed = parse_oracle_text(
        METAMORPHIC_ALTERATION,
        "Metamorphic Alteration",
        &[],
        &["Enchantment".to_string()],
        &["Aura".to_string()],
    );

    let replacement = parsed
        .replacements
        .iter()
        .find(|r| {
            matches!(
                r.execute.as_ref().map(|e| e.effect.as_ref()),
                Some(Effect::ChoosePermanent { .. })
            )
        })
        .expect("as-enters choose-a-creature must lower to an Effect::ChoosePermanent replacement");

    match replacement.execute.as_ref().unwrap().effect.as_ref() {
        Effect::ChoosePermanent { filter, persist } => {
            assert!(
                matches!(persist, ChoosePermanentPersist::CopiableSnapshot),
                "the choice must persist as a copiable snapshot latched onto the Aura"
            );
            assert_eq!(
                filter,
                &TargetFilter::Typed(TypedFilter::creature()),
                "the copy-source pool is 'a creature'"
            );
        }
        other => panic!("expected Effect::ChoosePermanent, got {other:?}"),
    }
}

/// SHAPE: the "Enchanted creature is a copy of the chosen creature." line parses
/// to the `ContinuousModification::CopyChosen` marker affecting the enchanted
/// host (an `EnchantedBy` creature filter). The marker is a Layer-1 no-op; the
/// copy is materialized by the `ChoosePermanent` answer, not this static.
#[test]
fn enchanted_is_a_copy_of_chosen_parses_to_copy_chosen_static() {
    let parsed = parse_oracle_text(
        METAMORPHIC_ALTERATION,
        "Metamorphic Alteration",
        &[],
        &["Enchantment".to_string()],
        &["Aura".to_string()],
    );

    let static_def = parsed
        .statics
        .iter()
        .find(|s| {
            s.modifications
                .contains(&ContinuousModification::CopyChosen)
        })
        .expect("the copy static must lower to ContinuousModification::CopyChosen");

    match static_def
        .affected
        .as_ref()
        .expect("static must be scoped to a host")
    {
        TargetFilter::Typed(tf) => assert!(
            tf.properties.contains(&FilterProp::EnchantedBy),
            "the copy static must affect the enchanted host (CR 303.4 + CR 613.1a)"
        ),
        other => panic!("expected a Typed enchanted-host filter, got {other:?}"),
    }
}
