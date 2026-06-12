use super::anthem::*;
use super::keyword_grant::*;
use super::prelude::*;
use super::restriction::*;
use super::support::*;
use super::*;
use crate::types::ability::{
    ActivationRestriction, AggregateFunction, CardTypeSetSource, CountScope, DamageKindFilter,
    Duration, Effect, FilterProp, ObjectProperty, ObjectScope, PlayerFilter, PlayerScope, PtStat,
    PtValueScope, QuantityExpr, QuantityRef, SharedQuality, SharedQualityRelation, TypeFilter,
    ZoneRef,
};
use crate::types::counter::CounterType;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaCost;
use crate::types::statics::{CrewAction, CrewContributionKind};

/// CR 702.16 + CR 609.6: Serra's Emissary's compound-subject keyword grant
/// "You and creatures you control have protection from the chosen card
/// type." must decompose into exactly TWO `StaticDefinition`s:
///   - object-half: `Continuous` / `AddKeyword(Protection(ChosenCardType))`
///     with a controller-You creatures filter;
///   - player-half: `PlayerProtection(ChosenCardType)` with controller-You.
#[test]
fn compound_subject_keyword_static_splits_serras_emissary() {
    use crate::types::keywords::{Keyword, ProtectionTarget};

    let defs = parse_static_line_multi(
        "You and creatures you control have protection from the chosen card type.",
    );
    assert_eq!(
        defs.len(),
        2,
        "expected exactly two StaticDefinitions, got {defs:?}"
    );

    // Object-half.
    let object_def = &defs[0];
    assert_eq!(object_def.mode, StaticMode::Continuous);
    match &object_def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "object-half must affect creatures, got {:?}",
                tf.type_filters
            );
        }
        other => {
            panic!("object-half affected must be Typed(creatures you control), got {other:?}")
        }
    }
    assert!(
        object_def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Protection(ProtectionTarget::ChosenCardType),
            }),
        "object-half must grant Protection(ChosenCardType), got {:?}",
        object_def.modifications
    );

    // Player-half.
    let player_def = &defs[1];
    assert_eq!(
        player_def.mode,
        StaticMode::PlayerProtection(ProtectionTarget::ChosenCardType)
    );
    assert_eq!(
        player_def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You)
        )),
        "player-half must affect the controller"
    );
}

/// CR 509.1b: Brave the Sands — "Creatures you control have vigilance and can
/// block an additional creature each combat." must decompose into BOTH the
/// vigilance grant AND an `ExtraBlockers` grant affecting creatures you control.
/// Previously the trailing extra-block clause was dropped entirely (the ability
/// did nothing).
#[test]
fn extra_blockers_static_splits_from_keyword_grant() {
    let defs = parse_static_line_multi(
        "Creatures you control have vigilance and can block an additional creature each combat.",
    );
    assert!(
        defs.len() >= 2,
        "expected vigilance + extra-block defs, got {:?}",
        defs.iter().map(|d| &d.mode).collect::<Vec<_>>()
    );
    let extra = defs
        .iter()
        .find(|d| matches!(d.mode, StaticMode::ExtraBlockers { .. }))
        .expect("expected an ExtraBlockers static def");
    assert_eq!(extra.mode, StaticMode::ExtraBlockers { count: Some(1) });
    match &extra.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "extra-block grant must affect creatures, got {:?}",
                tf.type_filters
            );
        }
        other => panic!("ExtraBlockers must affect creatures you control, got {other:?}"),
    }
}

/// CR 509.1b: A self-referential standalone extra-block grant ("~ can block an
/// additional creature", e.g. Palace Guard) keeps the grant on the source.
#[test]
fn extra_blockers_static_self_reference_stays_selfref() {
    let def = parse_static_line("~ can block an additional creature.").expect("static def");
    assert_eq!(def.mode, StaticMode::ExtraBlockers { count: Some(1) });
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
}

/// CR 502.3 + CR 113.6: the self-referential Seedborn-class untap permission —
/// "Untap this artifact during each other player's untap step" (Bender's
/// Waterskin) must lower to `UntapsDuringEachOtherPlayersUntapStep` with the
/// affected filter being the source itself (`SelfRef`), so its controller
/// untaps it during every other player's untap step. Before this, the line was
/// classified as a one-shot `Effect::Untap` and never reached this static path.
#[test]
fn self_untap_during_each_other_untap_step_bender_waterskin() {
    for subject in [
        "this artifact",
        "this creature",
        "this permanent",
        "this card",
        "this attraction",
        "itself",
        "~",
    ] {
        let def = parse_static_line(&format!(
            "Untap {subject} during each other player's untap step."
        ))
        .unwrap_or_else(|| panic!("static def for subject {subject:?}"));
        assert_eq!(def.mode, StaticMode::UntapsDuringEachOtherPlayersUntapStep);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }
}

/// CR 509.1b: Copper Carapace — "Equipped creature gets +2/+2 and can't block."
/// must decompose into BOTH the P/T grant AND a `CantBlock` static affecting the
/// equipped creature. Previously the "can't block" drawback was dropped, making
/// the card a strictly-better-than-printed pure pump.
#[test]
fn cant_block_static_splits_from_pump() {
    let defs = parse_static_line_multi("Equipped creature gets +2/+2 and can't block.");
    assert!(
        defs.iter().any(|d| d.mode == StaticMode::CantBlock),
        "expected a CantBlock static, got {:?}",
        defs.iter().map(|d| &d.mode).collect::<Vec<_>>()
    );
    assert!(
        defs.iter()
            .any(|d| matches!(d.mode, StaticMode::Continuous)),
        "the +2/+2 grant must be preserved"
    );
}

/// CR 509.1b: A self-referential pump ("This creature gets +2/+2 and can't
/// block.", e.g. Threshold downside creatures) also splits, with the `CantBlock`
/// static affecting the source.
#[test]
fn cant_block_static_splits_self_reference() {
    let defs = parse_static_line_multi("This creature gets +2/+2 and can't block.");
    let cant = defs
        .iter()
        .find(|d| d.mode == StaticMode::CantBlock)
        .expect("expected a CantBlock static");
    assert_eq!(cant.affected, Some(TargetFilter::SelfRef));
}

/// CR 502.3: Flood the Engine — "Enchanted permanent loses all abilities and
/// doesn't untap during its controller's untap step." must decompose into BOTH
/// the loses-all-abilities grant AND a `CantUntap` static affecting the
/// enchanted permanent. Previously the untap restriction was dropped, so the
/// permanent untapped normally and the lock did nothing.
#[test]
fn doesnt_untap_static_splits_from_grant() {
    let defs = parse_static_line_multi(
        "Enchanted permanent loses all abilities and doesn't untap during its controller's untap step.",
    );
    assert!(
        defs.iter().any(|d| d.mode == StaticMode::CantUntap),
        "expected a CantUntap static, got {:?}",
        defs.iter().map(|d| &d.mode).collect::<Vec<_>>()
    );
    assert!(
        defs.iter()
            .any(|d| matches!(d.mode, StaticMode::Continuous)),
        "the loses-all-abilities grant must be preserved"
    );
}

/// CR 502.3: A P/T pump compounded with the untap restriction also splits, with
/// the `CantUntap` static affecting the same subject.
#[test]
fn doesnt_untap_static_splits_from_pump() {
    let defs = parse_static_line_multi(
        "Enchanted creature gets +1/+1 and doesn't untap during its controller's untap step.",
    );
    let cant = defs
        .iter()
        .find(|d| d.mode == StaticMode::CantUntap)
        .expect("expected a CantUntap static");
    assert!(
        cant.affected.is_some(),
        "CantUntap companion must share the grant's affected set"
    );
    assert!(
        defs.iter()
            .any(|d| matches!(d.mode, StaticMode::Continuous)),
        "the +1/+1 grant must be preserved"
    );
}

/// CR 502.3: A trailing "as long as …" rider on the untap clause attaches to
/// the `CantUntap` companion, not the grant.
#[test]
fn doesnt_untap_static_split_keeps_trailing_condition() {
    let defs = parse_static_line_multi(
        "Enchanted creature gets +1/+1 and doesn't untap during its controller's untap step as long as you control a Swamp.",
    );
    let cant = defs
        .iter()
        .find(|d| d.mode == StaticMode::CantUntap)
        .expect("expected a CantUntap static");
    assert!(
        cant.condition.is_some(),
        "the 'as long as …' rider must attach to the CantUntap companion"
    );
}

/// CR 502.3: The split declines (rather than silently dropping a clause) when
/// an unrecognized clause trails the untap-step phrase — parity with the
/// sibling `try_split_and_cant_block` terminal guard.
#[test]
fn doesnt_untap_static_split_declines_unknown_tail() {
    let defs = parse_static_line_multi(
        "Enchanted creature gets +1/+1 and doesn't untap during its controller's untap step, then its controller loses 1 life.",
    );
    assert!(
        !defs.iter().any(|d| d.mode == StaticMode::CantUntap),
        "unrecognized trailing clause must make the split decline, got {:?}",
        defs.iter().map(|d| &d.mode).collect::<Vec<_>>()
    );
}

/// CR 508.1c: Cagemail — "Enchanted creature gets +2/+2 and can't attack." must
/// decompose into BOTH the P/T grant AND a `CantAttack` static affecting the
/// enchanted creature. Previously the "can't attack" drawback was dropped,
/// making the Aura a strictly-better-than-printed pure pump.
#[test]
fn cant_attack_static_splits_from_pump() {
    let defs = parse_static_line_multi("Enchanted creature gets +2/+2 and can't attack.");
    assert!(
        defs.iter().any(|d| d.mode == StaticMode::CantAttack),
        "expected a CantAttack static, got {:?}",
        defs.iter().map(|d| &d.mode).collect::<Vec<_>>()
    );
    assert!(
        defs.iter()
            .any(|d| matches!(d.mode, StaticMode::Continuous)),
        "the +2/+2 grant must be preserved"
    );
}

/// CR 508.1c: A self-referential pump ("This creature gets +1/+1 and can't
/// attack.") also splits, with the `CantAttack` static affecting the source.
#[test]
fn cant_attack_static_splits_self_reference() {
    let defs = parse_static_line_multi("This creature gets +1/+1 and can't attack.");
    let cant = defs
        .iter()
        .find(|d| d.mode == StaticMode::CantAttack)
        .expect("expected a CantAttack static");
    assert_eq!(cant.affected, Some(TargetFilter::SelfRef));
}

/// CR 508.1c: The terminal-phrase guard must NOT mis-split scoped attack
/// restrictions. "can't attack alone" (Sightless Brawler) is owned by a
/// different branch; the Vow cycle's "can't attack you or planeswalkers you
/// control" must be parsed as a defended-scope `CantAttack`, not a blanket one.
#[test]
fn cant_attack_split_declines_scoped_restrictions() {
    let alone = parse_static_line_multi("Enchanted creature gets +3/+2 and can't attack alone.");
    assert!(
        !alone.iter().any(|d| d.mode == StaticMode::CantAttack),
        "\"can't attack alone\" must not become a plain CantAttack, got {:?}",
        alone.iter().map(|d| &d.mode).collect::<Vec<_>>()
    );

    let scoped = parse_static_line_multi(
        "Enchanted creature gets +2/+2, has vigilance, and can't attack you or planeswalkers you control.",
    );
    let scoped_lockout = scoped
        .iter()
        .find(|d| d.mode == StaticMode::CantAttack)
        .unwrap_or_else(|| panic!("expected scoped CantAttack, got {:?}", scoped));
    assert_eq!(
        scoped_lockout.attack_defended,
        Some(crate::types::triggers::AttackTargetFilter::PlayerOrPlaneswalker),
        "scoped \"can't attack you ...\" must not become a blanket CantAttack"
    );
}

/// CR 508.1c / CR 509.1b: A grant followed by "and can't attack or block"
/// (Immovable Rod, Fog on the Barrow-Downs) must produce both the leading
/// clause's static(s) and a companion `CantAttackOrBlock` static sharing the
/// same affected set.
#[test]
fn cant_attack_or_block_splits_from_loses_all_abilities() {
    let defs = parse_static_line_multi(
        "Enchanted creature loses all abilities and can't attack or block.",
    );
    assert!(
        defs.iter().any(|d| d.mode == StaticMode::CantAttackOrBlock),
        "expected CantAttackOrBlock, got {:?}",
        defs.iter().map(|d| &d.mode).collect::<Vec<_>>()
    );
    assert!(
        defs.iter()
            .any(|d| matches!(d.mode, StaticMode::Continuous)),
        "leading loses-all-abilities clause must be preserved"
    );
}

/// CR 508.1c / CR 509.1b: A type-change "is a [type]" followed by "and can't
/// attack or block" also splits, with the CantAttackOrBlock companion sharing
/// the enchanted-creature subject.
#[test]
fn cant_attack_or_block_splits_from_type_change() {
    let defs = parse_static_line_multi("Enchanted creature is a Frog and can't attack or block.");
    assert!(
        defs.iter().any(|d| d.mode == StaticMode::CantAttackOrBlock),
        "expected CantAttackOrBlock, got {:?}",
        defs.iter().map(|d| &d.mode).collect::<Vec<_>>()
    );
    // The companion must target the same enchanted-creature subject.
    let lockout = defs
        .iter()
        .find(|d| d.mode == StaticMode::CantAttackOrBlock)
        .unwrap();
    assert!(
        lockout.affected.is_some(),
        "CantAttackOrBlock companion must carry an affected filter"
    );
}

/// CR 508.1c / CR 509.1b: The bare "can't attack" and "can't block" split
/// functions must still fire unaffected — the new "attack or block" handler
/// must not over-match and suppress them.
#[test]
fn cant_attack_or_block_split_does_not_suppress_siblings() {
    let cant_attack = parse_static_line_multi("Enchanted creature gets +2/+2 and can't attack.");
    assert!(
        cant_attack.iter().any(|d| d.mode == StaticMode::CantAttack),
        "bare can't-attack split must still produce CantAttack"
    );
    assert!(
        !cant_attack
            .iter()
            .any(|d| d.mode == StaticMode::CantAttackOrBlock),
        "bare can't-attack split must NOT produce CantAttackOrBlock"
    );

    let cant_block = parse_static_line_multi("Enchanted creature gets +2/+2 and can't block.");
    assert!(
        cant_block.iter().any(|d| d.mode == StaticMode::CantBlock),
        "bare can't-block split must still produce CantBlock"
    );
    assert!(
        !cant_block
            .iter()
            .any(|d| d.mode == StaticMode::CantAttackOrBlock),
        "bare can't-block split must NOT produce CantAttackOrBlock"
    );
}

/// CR 508.1b + CR 508.1c: Vow of Lightning — "Enchanted creature gets +2/+2, has first
/// strike, and can't attack you or planeswalkers you control." must produce the
/// +2/+2 and first-strike grant AND a companion `CantAttack` scoped to
/// `PlayerOrPlaneswalker`. Previously the attack restriction was silently
/// dropped, so the enchanted creature could freely attack the Aura controller.
#[test]
fn cant_attack_scoped_splits_from_pt_and_keyword_grant() {
    let defs = parse_static_line_multi(
        "Enchanted creature gets +2/+2, has first strike, and can't attack you or planeswalkers you control.",
    );
    let lockout = defs
        .iter()
        .find(|d| d.mode == StaticMode::CantAttack)
        .unwrap_or_else(|| panic!("expected a CantAttack companion, got {:?}", defs));
    assert_eq!(
        lockout.attack_defended,
        Some(crate::types::triggers::AttackTargetFilter::PlayerOrPlaneswalker),
        "companion must be scoped to PlayerOrPlaneswalker, got {:?}",
        lockout.attack_defended
    );
    assert!(
        lockout.affected.is_some(),
        "CantAttack companion must carry an affected filter"
    );
    // The leading grant must also be preserved.
    assert!(
        defs.iter().any(|d| d.mode == StaticMode::Continuous),
        "leading +2/+2 and first-strike grant must be preserved"
    );
}

/// CR 508.1b + CR 508.1c: "and can't attack you" (Player scope only, without
/// planeswalkers) also splits into a `CantAttack` with `defended = Player`.
#[test]
fn cant_attack_scoped_player_only_variant() {
    let defs = parse_static_line_multi("Enchanted creature gets +0/+3 and can't attack you.");
    let lockout = defs
        .iter()
        .find(|d| d.mode == StaticMode::CantAttack)
        .expect("expected a CantAttack companion for 'can't attack you'");
    assert_eq!(
        lockout.attack_defended,
        Some(crate::types::triggers::AttackTargetFilter::Player),
        "companion must be scoped to Player, got {:?}",
        lockout.attack_defended
    );
}

/// CR 508.1b + CR 508.1c: The scoped-attack splitter must not suppress the
/// existing bare-attack and cant-attack-or-block splitters.
#[test]
fn cant_attack_scoped_split_does_not_suppress_siblings() {
    // Bare "can't attack" must still route to plain CantAttack with no defended scope.
    let bare = parse_static_line_multi("Enchanted creature gets +2/+2 and can't attack.");
    let bare_lockout = bare
        .iter()
        .find(|d| d.mode == StaticMode::CantAttack)
        .expect("bare cant-attack splitter must still produce CantAttack");
    assert_eq!(
        bare_lockout.attack_defended, None,
        "bare CantAttack must have no defended scope"
    );

    // "can't attack or block" must still route to CantAttackOrBlock.
    let aob = parse_static_line_multi(
        "Enchanted creature loses all abilities and can't attack or block.",
    );
    assert!(
        aob.iter().any(|d| d.mode == StaticMode::CantAttackOrBlock),
        "cant-attack-or-block splitter must still produce CantAttackOrBlock"
    );
}

/// CR 701.21: Assault Suit — "Equipped creature gets +2/+2, has haste, can't
/// attack you or planeswalkers you control, and can't be sacrificed." must
/// include an `Other("CantBeSacrificed")` static alongside the +2/+2 grant.
/// Previously the sacrifice prohibition was dropped, so the equipped creature
/// could still be sacrificed.
#[test]
fn cant_be_sacrificed_static_splits_from_grant() {
    let defs = parse_static_line_multi(
        "Equipped creature gets +2/+2, has haste, can't attack you or planeswalkers you control, and can't be sacrificed.",
    );
    let continuous = defs
        .iter()
        .find(|d| matches!(d.mode, StaticMode::Continuous))
        .expect("expected the +2/+2 and haste grant to be preserved");
    assert!(
        continuous
            .modifications
            .contains(&ContinuousModification::AddPower { value: 2 }),
        "expected +2 power grant, got {:?}",
        continuous.modifications
    );
    assert!(
        continuous
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 2 }),
        "expected +2 toughness grant, got {:?}",
        continuous.modifications
    );
    assert!(
        continuous
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Haste,
            }),
        "expected haste grant, got {:?}",
        continuous.modifications
    );
    let cant = defs
        .iter()
        .find(|d| d.mode == StaticMode::Other("CantBeSacrificed".to_string()))
        .expect("expected a CantBeSacrificed static");
    assert_eq!(
        cant.affected, continuous.affected,
        "CantBeSacrificed companion must share the grant's affected set"
    );
    assert!(
        cant.affected.is_some(),
        "CantBeSacrificed companion must have an affected set"
    );
}

/// CR 701.21: A simple pump compounded with the sacrifice prohibition also
/// splits, with `CantBeSacrificed` sharing the grant's affected set.
#[test]
fn cant_be_sacrificed_static_splits_from_pump() {
    let defs = parse_static_line_multi("Enchanted creature gets +1/+1 and can't be sacrificed.");
    let cant = defs
        .iter()
        .find(|d| d.mode == StaticMode::Other("CantBeSacrificed".to_string()))
        .expect("expected a CantBeSacrificed static");
    assert!(
        cant.affected.is_some(),
        "CantBeSacrificed companion must share the grant's affected set"
    );
    assert!(
        defs.iter()
            .any(|d| matches!(d.mode, StaticMode::Continuous)),
        "the +1/+1 grant must be preserved"
    );
}

/// CR 701.21: A qualified "can't be sacrificed unless …" tail must not be
/// mis-split into a plain `CantBeSacrificed` — the terminal guard declines.
#[test]
fn cant_be_sacrificed_split_declines_qualified_tail() {
    let defs = parse_static_line_multi(
        "Enchanted creature gets +1/+1 and can't be sacrificed unless you pay 2 life.",
    );
    assert!(
        !defs
            .iter()
            .any(|d| d.mode == StaticMode::Other("CantBeSacrificed".to_string())),
        "qualified \"unless …\" tail must make the split decline, got {:?}",
        defs.iter().map(|d| &d.mode).collect::<Vec<_>>()
    );
}

/// CR 702.5: Consecrate Land — "Enchanted land has indestructible and can't be
/// enchanted by other Auras." must decompose into BOTH the indestructible grant
/// AND a `CantBeEnchanted` static affecting the enchanted permanent. Previously
/// the attach prohibition was dropped, so other Auras could still be attached.
#[test]
fn cant_be_attached_static_splits_from_grant() {
    let defs = parse_static_line_multi(
        "Enchanted land has indestructible and can't be enchanted by other Auras.",
    );
    assert!(
        defs.iter()
            .any(|d| d.mode == StaticMode::Other("CantBeEnchanted".to_string())),
        "expected a CantBeEnchanted static, got {:?}",
        defs.iter().map(|d| &d.mode).collect::<Vec<_>>()
    );
    assert!(
        defs.iter()
            .any(|d| matches!(d.mode, StaticMode::Continuous)),
        "the indestructible grant must be preserved"
    );
}

/// CR 702.5: Anti-Magic Aura — "Enchanted creature can't be the target of spells
/// and can't be enchanted by other Auras." splits into BOTH the targeting
/// restriction AND a `CantBeEnchanted` static.
#[test]
fn cant_be_attached_static_splits_from_restriction() {
    let defs = parse_static_line_multi(
        "Enchanted creature can't be the target of spells and can't be enchanted by other Auras.",
    );
    let cant = defs
        .iter()
        .find(|d| d.mode == StaticMode::Other("CantBeEnchanted".to_string()))
        .expect("expected a CantBeEnchanted static");
    assert!(
        cant.affected.is_some(),
        "CantBeEnchanted companion must share the first clause's affected set"
    );
    assert!(
        defs.len() >= 2,
        "the first clause must be preserved alongside CantBeEnchanted, got {:?}",
        defs.iter().map(|d| &d.mode).collect::<Vec<_>>()
    );
}

/// CR 702.5 + CR 702.6: A compound "can't be equipped or enchanted" tail yields
/// BOTH attach prohibitions (equipped-first, matching the standalone dispatch).
#[test]
fn cant_be_attached_static_splits_both_prohibitions() {
    let defs = parse_static_line_multi(
        "Enchanted creature gets +1/+1 and can't be equipped or enchanted.",
    );
    assert!(
        defs.iter()
            .any(|d| d.mode == StaticMode::Other("CantBeEquipped".to_string())),
        "expected CantBeEquipped, got {:?}",
        defs.iter().map(|d| &d.mode).collect::<Vec<_>>()
    );
    assert!(
        defs.iter()
            .any(|d| d.mode == StaticMode::Other("CantBeEnchanted".to_string())),
        "expected CantBeEnchanted, got {:?}",
        defs.iter().map(|d| &d.mode).collect::<Vec<_>>()
    );
}

/// CR 702.18a: Spectral Shield — "Enchanted creature gets +0/+2 and can't be the
/// target of spells." must decompose into BOTH the +0/+2 grant AND a
/// `CantBeTargeted` static sharing the first conjunct's `affected` set.
/// Previously the targeting restriction was dropped, so the enchanted creature
/// could still be targeted — the Aura's entire protection was lost.
#[test]
fn cant_be_targeted_static_splits_from_grant() {
    let defs =
        parse_static_line_multi("Enchanted creature gets +0/+2 and can't be the target of spells.");
    let targeting = defs
        .iter()
        .find(|d| d.mode == StaticMode::CantBeTargeted)
        .expect("expected a CantBeTargeted static");
    assert!(
        targeting.affected.is_some(),
        "CantBeTargeted companion must share the first clause's affected set"
    );
    assert!(
        defs.iter()
            .any(|d| matches!(d.mode, StaticMode::Continuous)),
        "the +0/+2 grant must be preserved, got {:?}",
        defs.iter().map(|d| &d.mode).collect::<Vec<_>>()
    );
}

/// CR 702.11a: the opponents-only targeting scope in a compound static must
/// become Hexproof (controller can still target), NOT blanket Shroud — mirroring
/// the standalone dispatch so the "your opponents control" qualifier is not lost.
#[test]
fn cant_be_targeted_opponents_scope_splits_as_hexproof() {
    let defs = parse_static_line_multi(
        "Enchanted creature gets +1/+1 and can't be the target of spells your opponents control.",
    );
    assert!(
        defs.iter().any(|d| matches!(d.mode, StaticMode::Continuous)
            && d.modifications
                .contains(&ContinuousModification::AddKeyword {
                    keyword: Keyword::Hexproof
                })),
        "expected a Hexproof grant (opponents-only scope), got {:?}",
        defs.iter()
            .map(|d| (&d.mode, &d.modifications))
            .collect::<Vec<_>>()
    );
    assert!(
        !defs.iter().any(|d| d.mode == StaticMode::CantBeTargeted),
        "opponents-only scope must NOT collapse into blanket Shroud"
    );
}

/// CR 602.5: Viper's Kiss — "Enchanted creature gets -1/-1, and its activated
/// abilities can't be activated." must decompose into BOTH the -1/-1 grant AND a
/// `CantBeActivated` static. Previously the activation prohibition was dropped,
/// so the enchanted creature's activated abilities still worked.
#[test]
fn cant_activate_abilities_static_splits_from_grant() {
    let defs = parse_static_line_multi(
        "Enchanted creature gets -1/-1, and its activated abilities can't be activated.",
    );
    assert!(
        defs.iter()
            .any(|d| matches!(d.mode, StaticMode::CantBeActivated { .. })),
        "expected a CantBeActivated static, got {:?}",
        defs.iter().map(|d| &d.mode).collect::<Vec<_>>()
    );
    assert!(
        defs.iter()
            .any(|d| matches!(d.mode, StaticMode::Continuous)),
        "the -1/-1 grant must be preserved"
    );
}

/// CR 602.5 + CR 603.2a: The split `CantBeActivated` prohibits every player from
/// activating the ENCHANTED creature's abilities. Because the static lives on the
/// Aura and `is_blocked_by_cant_be_activated` matches `source_filter` from the
/// Aura source (ignoring `affected`, and with no re-homing for this static mode),
/// the companion's `source_filter` must be the host filter (`EnchantedBy`) — NOT
/// `SelfRef`, which would resolve to the Aura itself (a silent runtime no-op).
#[test]
fn cant_activate_abilities_static_targets_enchanted_creature() {
    let defs = parse_static_line_multi(
        "Enchanted creature gets -1/-1, and its activated abilities can't be activated.",
    );
    let prohibition = defs
        .iter()
        .find(|d| matches!(d.mode, StaticMode::CantBeActivated { .. }))
        .expect("expected a CantBeActivated static");
    let StaticMode::CantBeActivated {
        who, source_filter, ..
    } = &prohibition.mode
    else {
        unreachable!("matched CantBeActivated above");
    };
    assert_eq!(
        *who,
        ProhibitionScope::AllPlayers,
        "every player is prohibited"
    );
    assert!(
        matches!(
            source_filter,
            TargetFilter::Typed(tf) if tf.properties.contains(&FilterProp::EnchantedBy)
        ),
        "source_filter must be the host (EnchantedBy) filter so it resolves to the \
         enchanted creature from the Aura source, got {source_filter:?}"
    );
}

/// CR 509.1b: Madcap Skills — "Enchanted creature gets +3/+0 and can't be
/// blocked by more than one creature." must decompose into BOTH the P/T grant
/// AND a `CantBeBlockedByMoreThan { max: 1 }` static affecting the enchanted
/// creature. Previously the evasion clause was dropped entirely.
#[test]
fn cant_be_blocked_static_splits_from_keyword_grant() {
    let defs = parse_static_line_multi(
        "Enchanted creature gets +3/+0 and can't be blocked by more than one creature.",
    );
    assert!(
        defs.len() >= 2,
        "expected P/T + evasion defs, got {:?}",
        defs.iter().map(|d| &d.mode).collect::<Vec<_>>()
    );
    assert!(
        defs.iter()
            .any(|d| d.mode == StaticMode::CantBeBlockedByMoreThan { max: 1 }
                && matches!(
                    d.affected,
                    Some(TargetFilter::Typed(TypedFilter { ref properties, .. }))
                        if properties.contains(&FilterProp::EnchantedBy)
                )),
        "expected enchanted-creature CantBeBlockedByMoreThan {{ max: 1 }}, got {:?}",
        defs.iter().map(|d| &d.mode).collect::<Vec<_>>()
    );
    // The P/T grant is preserved (and remains a continuous modification).
    assert!(
        defs.iter()
            .any(|d| matches!(d.mode, StaticMode::Continuous)),
        "P/T grant must be preserved"
    );
}

/// CR 509.1b: The compound split must also match the typographic U+2019
/// apostrophe ("can'​t"), matching the standalone evasion branches — MTGJSON
/// Oracle text uses U+2019, and the static path doesn't normalize apostrophes.
#[test]
fn cant_be_blocked_static_splits_with_typographic_apostrophe() {
    let defs = parse_static_line_multi(
        "Enchanted creature gets +3/+0 and can\u{2019}t be blocked by more than one creature.",
    );
    assert!(
        defs.iter()
            .any(|d| d.mode == StaticMode::CantBeBlockedByMoreThan { max: 1 }),
        "U+2019 form must still split off the evasion grant, got {:?}",
        defs.iter().map(|d| &d.mode).collect::<Vec<_>>()
    );
}

/// CR 509.1b: The bare compound form "… and can't be blocked" yields a plain
/// `CantBeBlocked` static alongside the keyword grant.
#[test]
fn cant_be_blocked_static_splits_bare_form() {
    let defs = parse_static_line_multi("Enchanted creature gets +2/+2 and can't be blocked.");
    assert!(
        defs.iter().any(|d| d.mode == StaticMode::CantBeBlocked),
        "expected CantBeBlocked, got {:?}",
        defs.iter().map(|d| &d.mode).collect::<Vec<_>>()
    );
}

/// CR 105.4 + CR 509.1b + CR 608.2c: The split path must use the chosen-color
/// qualifier parser before the generic type parser, matching the standalone
/// "can't be blocked by <filter>" path.
#[test]
fn cant_be_blocked_static_split_keeps_chosen_color_filter() {
    let defs = parse_static_line_multi(
        "Enchanted creature gets +2/+2 and can't be blocked by creatures of that color.",
    );
    let filter = defs
        .iter()
        .find_map(|d| match &d.mode {
            StaticMode::CantBeBlockedBy { filter } => Some(filter),
            _ => None,
        })
        .expect("expected split CantBeBlockedBy static");

    assert!(
        matches!(
            filter,
            TargetFilter::Typed(tf)
                if tf.properties
                    .iter()
                    .any(|prop| matches!(prop, FilterProp::IsChosenColor))
        ),
        "expected IsChosenColor blocker filter, got {filter:?}"
    );
}

/// CR 509.1b: A trailing evasion condition belongs on the split
/// `CantBeBlocked` companion, not only on standalone attached-subject forms.
#[test]
fn cant_be_blocked_static_split_keeps_trailing_condition() {
    let defs = parse_static_line_multi(
        "Enchanted creature gets +2/+2 and can't be blocked as long as you control a Gate.",
    );
    let condition = defs
        .iter()
        .find_map(|d| {
            (d.mode == StaticMode::CantBeBlocked)
                .then_some(d.condition.as_ref())
                .flatten()
        })
        .expect("expected split CantBeBlocked static with condition");

    assert!(
        matches!(
            condition,
            StaticCondition::IsPresent {
                filter: Some(TargetFilter::Typed(tf))
            } if tf.get_subtype() == Some("Gate")
        ),
        "expected Gate condition, got {condition:?}"
    );
}

/// CR 118.9: Rooftop Storm grants {0} as an alternative MANA cost for Zombie
/// creature spells the controller casts.
#[test]
fn alt_cost_rooftop_storm_zombie_creature_zero() {
    let def = parse_spells_alternative_cost(
        "You may pay {0} rather than pay the mana cost for Zombie creature spells you cast.",
    )
    .expect("Rooftop Storm must parse to a CastWithAlternativeCost static");
    match &def.mode {
        StaticMode::CastWithAlternativeCost { cost, .. } => {
            assert_eq!(
                *cost,
                AbilityCost::Mana {
                    cost: crate::types::mana::ManaCost::zero()
                }
            );
        }
        other => panic!("expected CastWithAlternativeCost, got {other:?}"),
    }
    // Affected: Zombie creature spells you cast.
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "expected Creature type filter, got {:?}",
                tf.type_filters
            );
            assert_eq!(
                tf.get_subtype(),
                Some("Zombie"),
                "expected Zombie subtype, got {:?}",
                tf.type_filters
            );
        }
        other => panic!("expected Typed(Zombie creature you cast), got {other:?}"),
    }
    assert_eq!(def.active_zones, vec![Zone::Battlefield]);
}

/// CR 118.9: Fist of Suns grants {WUBRG} as an alternative cost for ANY
/// spell the controller casts (no type prefix → any-card filter).
#[test]
fn alt_cost_fist_of_suns_any_spell_wubrg() {
    let def = parse_spells_alternative_cost(
        "You may pay {W}{U}{B}{R}{G} rather than pay the mana cost for spells you cast.",
    )
    .expect("Fist of Suns must parse to a CastWithAlternativeCost static");
    match &def.mode {
        StaticMode::CastWithAlternativeCost { cost, .. } => {
            use crate::types::mana::{ManaCost, ManaCostShard};
            assert_eq!(
                *cost,
                AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        shards: vec![
                            ManaCostShard::White,
                            ManaCostShard::Blue,
                            ManaCostShard::Black,
                            ManaCostShard::Red,
                            ManaCostShard::Green,
                        ],
                        generic: 0,
                    }
                }
            );
        }
        other => panic!("expected CastWithAlternativeCost, got {other:?}"),
    }
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
        }
        other => panic!("expected Typed(any spell you cast), got {other:?}"),
    }
}

/// CR 118.9 + CR 107.14 + CR 702.8a: Primal Prayers grants {E} as an
/// alternative cost for creature spells with MV ≤ 3, with flash tied to that
/// alternative-cost path.
#[test]
fn alt_cost_primal_prayers_energy_creature_mv_leq_3() {
    use crate::parser::oracle_static::cost_mod::parse_cast_spells_alternative_cost_multi;
    use crate::types::ability::{CastTimingPermission, Comparator, QuantityExpr};

    let text = "You may cast creature spells with mana value 3 or less by paying {E} \
                rather than paying their mana costs. If you cast a spell this way, \
                you may cast it as though it had flash.";
    let defs = parse_cast_spells_alternative_cost_multi(text);
    assert_eq!(
        defs.len(),
        1,
        "expected one alt-cost static with timing permission, got {defs:?}"
    );

    match &defs[0].mode {
        StaticMode::CastWithAlternativeCost {
            cost,
            timing_permission,
        } => {
            assert_eq!(
                *cost,
                AbilityCost::PayEnergy {
                    amount: QuantityExpr::Fixed { value: 1 }
                }
            );
            assert_eq!(
                *timing_permission,
                Some(CastTimingPermission::AsThoughHadFlash)
            );
        }
        other => panic!("expected CastWithAlternativeCost, got {other:?}"),
    }
    match &defs[0].affected {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "expected Creature filter, got {:?}",
                tf.type_filters
            );
            assert!(
                tf.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::Cmc {
                        comparator: Comparator::LE,
                        value: QuantityExpr::Fixed { value: 3 }
                    }
                )),
                "expected Cmc LE 3, got {:?}",
                tf.properties
            );
        }
        other => panic!("expected Typed(creature MV<=3 you cast), got {other:?}"),
    }
}

/// CR 118.9: a Jodah-style MV qualifier — "spells you cast with mana value 5
/// or greater" — either parses cleanly into a Cmc filter or strict-fails to
/// None. This test pins whichever behavior the parser actually produces so
/// the deferral decision is explicit.
#[test]
fn alt_cost_jodah_mv_qualifier_behavior() {
    let result = parse_spells_alternative_cost(
            "You may pay {W}{U}{B}{R}{G} rather than pay the mana cost for spells you cast with mana value 5 or greater.",
        );
    match result {
        Some(def) => {
            // If it parses, the MV qualifier must be attached as a Cmc prop.
            match &def.affected {
                Some(TargetFilter::Typed(tf)) => {
                    assert!(
                        tf.properties
                            .iter()
                            .any(|p| matches!(p, FilterProp::Cmc { .. })),
                        "MV qualifier must produce a Cmc filter prop, got {:?}",
                        tf.properties
                    );
                }
                other => panic!("expected Typed with Cmc prop, got {other:?}"),
            }
        }
        None => {
            // Deferral is acceptable per the plan — the MV qualifier did not
            // parse cleanly, so the static is not produced (not misparsed).
        }
    }
}

/// Strict-fail: non-mana payment shapes must NOT misparse into the static.
/// Bolas's Citadel ("pay life equal to ...") and Dream Halls ("discard a
/// card ...") defer to None rather than producing a wrong CastWithAlternativeCost.
#[test]
fn alt_cost_non_mana_payment_defers_to_none() {
    // Bolas's Citadel-style life payment.
    assert!(
            parse_spells_alternative_cost(
                "You may pay life equal to its mana value rather than pay the mana cost for spells you cast.",
            )
            .is_none(),
            "life payment must defer to None"
        );
    // Dream Halls-style discard payment.
    assert!(
            parse_spells_alternative_cost(
                "You may discard a card that shares a color with that spell rather than pay the mana cost for spells you cast.",
            )
            .is_none(),
            "discard payment must defer to None"
        );
}

/// CR 118.9: full-dispatcher regression — Fist of Suns must route through
/// the new Priority 6c-altcost branch into a CastWithAlternativeCost static
/// with NO free-floating Effect::PayCost ability (the prior misparse), and
/// the deferred non-mana classes (Bolas's Citadel, Dream Halls, As Foretold,
/// Conspiracy Unraveler) must NOT be newly misparsed into this static.
#[test]
fn full_dispatch_alt_cost_routing_and_deferrals() {
    use crate::parser::oracle::parse_oracle_text;
    use crate::types::ability::Effect;

    // Fist of Suns: routes to the static, no PayCost ability.
    let parsed = parse_oracle_text(
        "You may pay {W}{U}{B}{R}{G} rather than pay the mana cost for spells you cast.",
        "Fist of Suns",
        &[],
        &["Artifact".to_string()],
        &[],
    );
    assert!(
        parsed
            .statics
            .iter()
            .any(|d| matches!(d.mode, StaticMode::CastWithAlternativeCost { .. })),
        "Fist of Suns must produce a CastWithAlternativeCost static, got {:?}",
        parsed.statics
    );
    assert!(
        !parsed
            .abilities
            .iter()
            .any(|a| matches!(*a.effect, Effect::PayCost { .. })),
        "Fist of Suns must NOT produce a free-floating PayCost ability, got {:?}",
        parsed.abilities
    );

    // Deferred non-mana payment classes: must NOT produce the new static.
    let deferred = [
            (
                "Bolas's Citadel",
                "You may pay life equal to a spell's mana value rather than pay its mana cost.",
            ),
            (
                "Dream Halls",
                "Rather than pay the mana cost for a spell, its controller may discard a card that shares a color with that spell.",
            ),
        ];
    for (name, text) in deferred {
        let parsed = parse_oracle_text(text, name, &[], &["Enchantment".to_string()], &[]);
        assert!(
            !parsed
                .statics
                .iter()
                .any(|d| matches!(d.mode, StaticMode::CastWithAlternativeCost { .. })),
            "{name} must NOT be misparsed into CastWithAlternativeCost, got {:?}",
            parsed.statics
        );
    }
}

/// CR 205.1a + CR 205.2 + CR 205.3 + CR 613.1c: "becomes a [subtype]*
/// [core-type]+ in addition to its other types" must decompose into
/// typed `AddType`/`AddSubtype` modifications. Jump Scare regression.
#[test]
fn continuous_mods_decompose_becomes_compound_type_phrase() {
    let mods = parse_continuous_modifications(
            "get +2/+2, gains flying, and becomes a Horror enchantment creature in addition to its other types",
        );
    assert!(
        mods.contains(&ContinuousModification::AddSubtype {
            subtype: "Horror".into()
        }),
        "expected AddSubtype(Horror) in {mods:?}"
    );
    assert!(
        mods.contains(&ContinuousModification::AddType {
            core_type: CoreType::Enchantment
        }),
        "expected AddType(Enchantment) in {mods:?}"
    );
    assert!(
        mods.contains(&ContinuousModification::AddType {
            core_type: CoreType::Creature
        }),
        "expected AddType(Creature) in {mods:?}"
    );
    // Must not regress to the single-string whole-phrase subtype.
    assert!(
        !mods.contains(&ContinuousModification::AddSubtype {
            subtype: "Horror enchantment creature".into()
        }),
        "must not emit whole-phrase AddSubtype"
    );
}

#[test]
fn continuous_mods_replace_creature_subtypes_for_bare_becomes_clause() {
    let mods = parse_continuous_modifications("gets +3/+3 and becomes a Bear Berserker");
    assert!(mods.contains(&ContinuousModification::AddPower { value: 3 }));
    assert!(mods.contains(&ContinuousModification::AddToughness { value: 3 }));
    assert!(mods.contains(&ContinuousModification::RemoveAllSubtypes {
        set: crate::types::card_type::SubtypeSet::Creature,
    }));
    assert!(mods.contains(&ContinuousModification::AddSubtype {
        subtype: "Bear".to_string(),
    }));
    assert!(mods.contains(&ContinuousModification::AddSubtype {
        subtype: "Berserker".to_string(),
    }));
}

#[test]
fn continuous_mods_replace_creature_subtypes_with_color_and_core_type_tail() {
    let mods = parse_continuous_modifications(
        "becomes a white and green Bear Berserker creature with trample",
    );
    assert!(mods.contains(&ContinuousModification::SetColor {
        colors: vec![ManaColor::White, ManaColor::Green],
    }));
    assert!(mods.contains(&ContinuousModification::RemoveAllSubtypes {
        set: crate::types::card_type::SubtypeSet::Creature,
    }));
    assert!(mods.contains(&ContinuousModification::AddSubtype {
        subtype: "Bear".to_string(),
    }));
    assert!(mods.contains(&ContinuousModification::AddSubtype {
        subtype: "Berserker".to_string(),
    }));
    assert!(mods.contains(&ContinuousModification::SetCardTypes {
        core_types: vec![CoreType::Creature],
    }));
    assert!(mods.contains(&ContinuousModification::AddKeyword {
        keyword: Keyword::Trample,
    }));
}

#[test]
fn continuous_mods_preserve_additive_artifact_creature_exception() {
    let mods = parse_continuous_modifications("becomes an artifact creature");
    assert!(mods.contains(&ContinuousModification::AddType {
        core_type: CoreType::Artifact,
    }));
    assert!(mods.contains(&ContinuousModification::AddType {
        core_type: CoreType::Creature,
    }));
    assert!(
        !mods.iter().any(|modification| matches!(
            modification,
            ContinuousModification::SetCardTypes { .. }
        )),
        "artifact creature exception must retain previous card types: {mods:?}"
    );
}

#[test]
fn continuous_mods_preserve_still_type_retention_clause() {
    let mods = parse_continuous_modifications(
        "becomes a 0/0 Elemental creature with vigilance and haste that's still a land",
    );
    assert!(mods.contains(&ContinuousModification::SetPower { value: 0 }));
    assert!(mods.contains(&ContinuousModification::SetToughness { value: 0 }));
    assert!(mods.contains(&ContinuousModification::AddSubtype {
        subtype: "Elemental".to_string(),
    }));
    assert!(mods.contains(&ContinuousModification::AddType {
        core_type: CoreType::Creature,
    }));
    assert!(mods.contains(&ContinuousModification::AddType {
        core_type: CoreType::Land,
    }));
    assert!(
        !mods.iter().any(|modification| matches!(
            modification,
            ContinuousModification::SetCardTypes { .. }
                | ContinuousModification::RemoveAllSubtypes { .. }
        )),
        "still-retained types must stay additive under CR 205.1b: {mods:?}"
    );
}

#[test]
fn continuous_mods_replace_noncreature_subtype_set_for_bare_becomes_clause() {
    let mods = parse_continuous_modifications("becomes a Treasure artifact");
    assert!(mods.contains(&ContinuousModification::SetCardTypes {
        core_types: vec![CoreType::Artifact],
    }));
    assert!(mods.contains(&ContinuousModification::RemoveAllSubtypes {
        set: SubtypeSet::Artifact,
    }));
    assert!(mods.contains(&ContinuousModification::AddSubtype {
        subtype: "Treasure".to_string(),
    }));
}

#[test]
fn static_merfolk_lord() {
    let def = parse_static_line("Other Merfolk you control get +1/+1.").unwrap();
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: 1 }));
}

/// CR 113.1 + CR 113.3 + CR 604.1: Muraganda Petroglyphs' battlefield-wide anthem
/// "Creatures with no abilities get +2/+2." — a `Continuous` static (CR 604.1: always
/// true, re-evaluated each time) affecting every player's creatures (no controller
/// restriction) that have none of the four ability categories, granting +2/+2.
#[test]
fn static_muraganda_petroglyphs() {
    let def = parse_static_line("Creatures with no abilities get +2/+2.").expect("static def");
    assert_eq!(def.mode, StaticMode::Continuous);
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "must affect creatures, got {:?}",
                tf.type_filters
            );
            assert!(
                tf.properties.contains(&FilterProp::HasNoAbilities),
                "must filter on no abilities, got {:?}",
                tf.properties
            );
            // CR 604.1: no controller restriction → applies to ALL players' creatures.
            assert_eq!(
                tf.controller, None,
                "anthem must affect every player's creatures, got {:?}",
                tf.controller
            );
        }
        other => panic!("expected Typed(creatures with no abilities), got {other:?}"),
    }
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: 2 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddToughness { value: 2 }));
}

/// CR 509.1b + CR 609.4 + CR 702.14c: Ur-Drago's landwalk canceller produces
/// `StaticMode::IgnoreLandwalkForBlocking { qualifier: Some("Swamp") }`.
#[test]
fn ignore_landwalk_for_blocking_parses_ur_drago_swampwalk() {
    let def = parse_static_line(
        "Creatures with swampwalk can be blocked as though they didn't have swampwalk.",
    )
    .expect("ur-drago line must parse");
    assert_eq!(
        def.mode,
        StaticMode::IgnoreLandwalkForBlocking {
            qualifier: Some("Swamp".to_string()),
        }
    );
}

/// CR 702.14a: All five basic-land qualifiers parse to the canonical
/// capitalized form (verified for islandwalk here).
#[test]
fn ignore_landwalk_for_blocking_parses_islandwalk() {
    let def = parse_static_line(
        "Creatures with islandwalk can be blocked as though they didn't have islandwalk.",
    )
    .expect("islandwalk line must parse");
    assert_eq!(
        def.mode,
        StaticMode::IgnoreLandwalkForBlocking {
            qualifier: Some("Island".to_string()),
        }
    );
}

/// CR 702.14d: cross-qualifier sentences are not landwalk cancellations
/// (different landwalks don't cancel each other). The parser must reject.
#[test]
fn ignore_landwalk_for_blocking_rejects_cross_qualifier() {
    let result = parse_static_line(
        "Creatures with swampwalk can be blocked as though they didn't have islandwalk.",
    );
    // Must not produce IgnoreLandwalkForBlocking. Other parsers may produce
    // something else, but the qualifier-mismatch path must not match.
    if let Some(def) = result {
        assert!(
            !matches!(def.mode, StaticMode::IgnoreLandwalkForBlocking { .. }),
            "cross-qualifier text must not produce IgnoreLandwalkForBlocking, got {:?}",
            def.mode
        );
    }
}

#[test]
fn static_bonesplitter() {
    let def = parse_static_line("Equipped creature gets +2/+0.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: 2 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddToughness { value: 0 }));
}

#[test]
fn static_rancor() {
    let def = parse_static_line("Enchanted creature gets +2/+0 and has trample.").unwrap();
    assert!(def.modifications.len() >= 3); // +2, +0, trample
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Trample
        }));
}

#[test]
fn static_archetype_cant_have_keyword() {
    // CR 613.1f + CR 702: Theros Archetype cycle / Arcane Lighthouse —
    // "... can't have or gain [keyword]" must emit a Layer 6 CantHaveKeyword denial
    // static (scoped to the same subject) alongside the keyword removal.
    let defs = parse_static_line_multi(
        "Creatures your opponents control lose flying and can't have or gain flying.",
    );
    assert!(
        defs.iter().any(|d| matches!(
            &d.mode,
            StaticMode::CantHaveKeyword { keyword } if *keyword == Keyword::Flying
        )),
        "expected a CantHaveKeyword(Flying) denial static, got modes {:?}",
        defs.iter().map(|d| &d.mode).collect::<Vec<_>>()
    );
}

#[test]
fn static_cant_be_blocked_by_power_le() {
    // CR 509.1b: Questing Beast — can't be blocked by creatures with power 2 or less
    let def =
        parse_static_line("Questing Beast can't be blocked by creatures with power 2 or less.")
            .unwrap();
    assert!(
        matches!(
            &def.mode,
            StaticMode::CantBeBlockedBy { filter }
            if matches!(filter, TargetFilter::Typed(tf) if tf.properties.contains(&FilterProp::PtComparison { stat: PtStat::Power, scope: PtValueScope::Current, comparator: Comparator::LE, value: QuantityExpr::Fixed { value: 2 } }))
        ),
        "Expected CantBeBlockedBy with PtComparison(Power, LE, 2), got {:?}",
        def.mode
    );
}

#[test]
fn static_cant_be_blocked_by_power_ge() {
    // CR 509.1b: April O'Neil — can't be blocked by creatures with power 3 or greater
    let def =
        parse_static_line("April O'Neil can't be blocked by creatures with power 3 or greater.")
            .unwrap();
    assert!(
        matches!(
            &def.mode,
            StaticMode::CantBeBlockedBy { filter }
            if matches!(filter, TargetFilter::Typed(tf) if tf.properties.contains(&FilterProp::PtComparison { stat: PtStat::Power, scope: PtValueScope::Current, comparator: Comparator::GE, value: QuantityExpr::Fixed { value: 3 } }))
        ),
        "Expected CantBeBlockedBy with PtComparison(Power, GE, 3), got {:?}",
        def.mode
    );
}

#[test]
fn static_cant_be_blocked_by_greater_power() {
    // CR 509.1b: Prehistoric Pet — can't be blocked by creatures with greater power
    let def = parse_static_line("This creature can't be blocked by creatures with greater power.")
        .unwrap();
    assert!(
        matches!(
            &def.mode,
            StaticMode::CantBeBlockedBy { filter }
            if matches!(filter, TargetFilter::Typed(tf) if tf.properties.contains(&FilterProp::PowerGTSource))
        ),
        "Expected CantBeBlockedBy with PowerGTSource, got {:?}",
        def.mode
    );
}

#[test]
fn static_cant_be_blocked_by_more_than_one_creature() {
    // CR 509.1b: Stalking Tiger — per-creature blocker maximum. Must NOT collapse
    // to CantBeBlocked (unblockable) or CantBeBlockedBy (quality filter).
    let def =
        parse_static_line("Stalking Tiger can't be blocked by more than one creature.").unwrap();
    assert_eq!(def.mode, StaticMode::CantBeBlockedByMoreThan { max: 1 });
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
}

#[test]
fn static_cant_be_blocked_by_more_than_two_creatures() {
    // The count is parameterized, not hard-coded to one.
    let def = parse_static_line("~ can't be blocked by more than two creatures.").unwrap();
    assert_eq!(def.mode, StaticMode::CantBeBlockedByMoreThan { max: 2 });
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
}

#[test]
fn static_attach_only_restriction_power_ge_lowers_to_filter() {
    // CR 301.5 + CR 303.4 + CR 701.3a: Strata Scythe class — a positive
    // attachment restriction lowers to `AttachmentRestriction` whose `filter` is
    // the reused `TargetFilter` for "a creature with power 3 or greater".
    let def =
        parse_static_line("~ can be attached only to a creature with power 3 or greater.").unwrap();
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert!(
        matches!(
            &def.mode,
            StaticMode::AttachmentRestriction { filter }
            if matches!(
                filter,
                TargetFilter::Typed(tf)
                    if tf.type_filters.contains(&TypeFilter::Creature)
                        && tf.properties.contains(&FilterProp::PtComparison {
                            stat: PtStat::Power,
                            scope: PtValueScope::Current,
                            comparator: Comparator::GE,
                            value: QuantityExpr::Fixed { value: 3 },
                        })
            )
        ),
        "Expected AttachmentRestriction with PtComparison(Power, GE, 3), got {:?}",
        def.mode
    );
}

#[test]
fn static_attach_only_restriction_legendary_lowers_to_filter() {
    // CR 301.5 + CR 303.4 + CR 701.3a: Konda's Banner class — "a legendary
    // creature" host whitelist via the reused `TargetFilter` legendary property.
    let def = parse_static_line("~ can be attached only to a legendary creature.").unwrap();
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert!(
        matches!(
            &def.mode,
            StaticMode::AttachmentRestriction { filter }
            if matches!(
                filter,
                TargetFilter::Typed(tf)
                    if tf.type_filters.contains(&TypeFilter::Creature)
                        && tf.properties.contains(&FilterProp::HasSupertype {
                            value: crate::types::card_type::Supertype::Legendary,
                        })
            )
        ),
        "Expected AttachmentRestriction with Legendary creature filter, got {:?}",
        def.mode
    );
}

#[test]
fn static_source_power_cant_block_creatures_you_control() {
    let def = parse_static_line(
        "Creatures with power less than ~'s power can't block creatures you control.",
    )
    .expect("Champion of Lambholt static should parse");
    assert!(matches!(
        def.affected,
        Some(TargetFilter::Typed(ref tf))
            if tf.type_filters.contains(&TypeFilter::Creature)
                && tf.controller == Some(ControllerRef::You)
    ));
    assert!(
        matches!(
            def.mode,
            StaticMode::CantBeBlockedBy { ref filter }
                if matches!(
                    filter,
                    TargetFilter::Typed(tf)
                        if tf.type_filters.contains(&TypeFilter::Creature)
                            && tf.properties.contains(&FilterProp::PtComparison {
                                stat: PtStat::Power,
                                scope: PtValueScope::Current,
                                comparator: Comparator::LT,
                                value: QuantityExpr::Ref {
                                    qty: QuantityRef::Power {
                                        scope: ObjectScope::Source
                                    }
                                }
                            })
                )
        ),
        "expected CantBeBlockedBy with source-power LT blocker filter, got {:?}",
        def.mode
    );
}

#[test]
fn static_creatures_you_control() {
    let def = parse_static_line("Creatures you control get +1/+1.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(matches!(
        def.affected,
        Some(TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::You),
            ..
        }))
    ));
}

#[test]
fn static_creatures_you_control_also_get_with_condition() {
    let def = parse_static_line(
            "Creatures you control also get +1/+0 and have trample as long as you control six or more creatures.",
        )
        .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(matches!(
        def.affected,
        Some(TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::You),
            ..
        }))
    ));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: 1 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Trample,
        }));
    assert!(
        def.condition.is_some(),
        "as-long-as condition should apply to the whole static"
    );
}

// --- New pattern tests ---

#[test]
fn static_self_referential_has_keyword() {
    let def = parse_static_line("Phage the Untouchable has deathtouch.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Deathtouch,
        }));
}

#[test]
fn static_enchanted_permanent() {
    let def = parse_static_line("Enchanted permanent has hexproof.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(matches!(
        def.affected,
        Some(TargetFilter::Typed(ref tf)) if tf.type_filters.contains(&TypeFilter::Permanent)
    ));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Hexproof,
        }));
}

#[test]
fn static_all_creatures() {
    let def = parse_static_line("All creatures get +1/+1.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(matches!(
        def.affected,
        Some(TargetFilter::Typed(ref tf)) if tf.type_filters.contains(&TypeFilter::Creature) && tf.controller.is_none()
    ));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: 1 }));
}

#[test]
fn static_subtype_creatures_you_control() {
    let def = parse_static_line("Elf creatures you control get +1/+1.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(matches!(
        def.affected,
        Some(TargetFilter::Typed(ref tf))
            if tf.type_filters.contains(&TypeFilter::Creature)
                && tf.type_filters.contains(&TypeFilter::Subtype("Elf".to_string()))
                && tf.controller == Some(ControllerRef::You)
    ));
}

#[test]
fn static_color_creatures_you_control() {
    let def = parse_static_line("White creatures you control get +1/+1.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(matches!(
        def.affected,
        Some(TargetFilter::Typed(ref tf))
            if tf.type_filters.contains(&TypeFilter::Creature)
                && tf.get_subtype().is_none()
                && tf.controller == Some(ControllerRef::You)
                && tf.properties == vec![FilterProp::HasColor { color: ManaColor::White }]
    ));
}

#[test]
fn static_other_subtype_you_control() {
    let def = parse_static_line("Other Zombies you control get +1/+1.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
}

#[test]
fn static_controlled_compound_subject_shares_continuous_predicate() {
    let def = parse_static_line(
        "Skeletons you control and other Zombies you control get +1/+1 and have deathtouch.",
    )
    .unwrap();

    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(matches!(
        def.affected,
        Some(TargetFilter::Or { ref filters })
            if filters.iter().any(|filter| matches!(
                filter,
                TargetFilter::Typed(typed)
                    if typed.controller == Some(ControllerRef::You)
                        && typed.type_filters.iter().any(|type_filter| matches!(
                            type_filter,
                            TypeFilter::Subtype(subtype) if subtype == "Skeleton"
                        ))
                        && !typed.properties.contains(&FilterProp::Another)
            ))
                && filters.iter().any(|filter| matches!(
                    filter,
                    TargetFilter::Typed(typed)
                        if typed.controller == Some(ControllerRef::You)
                            && typed.type_filters.iter().any(|type_filter| matches!(
                                type_filter,
                                TypeFilter::Subtype(subtype) if subtype == "Zombie"
                            ))
                            && typed.properties.contains(&FilterProp::Another)
                ))
    ));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: 1 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddToughness { value: 1 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Deathtouch,
        }));
}

#[test]
fn static_opponent_controlled_compound_subject_shares_continuous_predicate() {
    let def = parse_static_line(
        "Skeletons your opponents control and other Zombies your opponents control get -1/-1.",
    )
    .unwrap();

    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(matches!(
        def.affected,
        Some(TargetFilter::Or { ref filters })
            if filters.iter().any(|filter| matches!(
                filter,
                TargetFilter::Typed(typed)
                    if typed.controller == Some(ControllerRef::Opponent)
                        && typed.type_filters.iter().any(|type_filter| matches!(
                            type_filter,
                            TypeFilter::Subtype(subtype) if subtype == "Skeleton"
                        ))
                        && !typed.properties.contains(&FilterProp::Another)
            ))
                && filters.iter().any(|filter| matches!(
                    filter,
                    TargetFilter::Typed(typed)
                        if typed.controller == Some(ControllerRef::Opponent)
                            && typed.type_filters.iter().any(|type_filter| matches!(
                                type_filter,
                                TypeFilter::Subtype(subtype) if subtype == "Zombie"
                            ))
                            && typed.properties.contains(&FilterProp::Another)
                ))
    ));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: -1 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddToughness { value: -1 }));
}

#[test]
fn static_custom_capitalized_subtype_you_control_preserves_s_suffix() {
    let affected = parse_continuous_subject_filter("Anubis you control")
        .expect("subject should produce a filter");
    let TargetFilter::Typed(typed) = affected else {
        panic!("expected typed subject filter");
    };

    assert_eq!(typed.controller, Some(ControllerRef::You));
    assert!(
        typed.type_filters.iter().any(|type_filter| matches!(
            type_filter,
            TypeFilter::Subtype(subtype) if subtype == "Anubis"
        )),
        "expected Anubis subtype, got {:?}",
        typed.type_filters
    );
}

#[test]
fn static_cant_block() {
    let def = parse_static_line("Ragavan can't block.").unwrap();
    assert_eq!(def.mode, StaticMode::CantBlock);
    assert!(def.modifications.is_empty());
    assert!(def.description.is_some());
    // Regression: a plain restriction with no "if"/"unless" stays unconditional.
    assert_eq!(def.condition, None);
}

#[test]
fn static_cant_attack_alone() {
    // CR 506.5 + CR 508.1c: "can't attack alone" must NOT be swallowed by the
    // generic "can't attack" arm (which would blanket-prohibit attacking).
    let def = parse_static_line("Bonded Construct can't attack alone.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CombatAlone {
            action: CombatAloneAction::Attack,
            requirement: CombatAloneRequirement::NeedsCompanion,
        }
    );
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
}

#[test]
fn static_cant_block_alone() {
    let def = parse_static_line("~ can't block alone.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CombatAlone {
            action: CombatAloneAction::Block,
            requirement: CombatAloneRequirement::NeedsCompanion,
        }
    );
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
}

#[test]
fn static_cant_attack_or_block_alone_emits_both() {
    // CR 506.5: Mogg Flunkies — both restrictions from one clause.
    let defs = parse_static_line_multi("Mogg Flunkies can't attack or block alone.");
    assert_eq!(defs.len(), 2);
    assert!(defs.iter().any(|d| {
        d.mode
            == (StaticMode::CombatAlone {
                action: CombatAloneAction::Attack,
                requirement: CombatAloneRequirement::NeedsCompanion,
            })
    }));
    assert!(defs.iter().any(|d| {
        d.mode
            == (StaticMode::CombatAlone {
                action: CombatAloneAction::Block,
                requirement: CombatAloneRequirement::NeedsCompanion,
            })
    }));
}

#[test]
fn static_can_only_attack_alone_parses() {
    // CR 508.1c: Master of Cruelties — "can only attack alone" means the
    // creature may not be declared alongside other attackers.
    let def =
        parse_static_line("~ can only attack alone.").expect("can only attack alone should parse");
    assert_eq!(
        def.mode,
        StaticMode::CombatAlone {
            action: CombatAloneAction::Attack,
            requirement: CombatAloneRequirement::MustBeSole,
        }
    );
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
}

#[test]
fn static_can_only_attack_alone_not_swallowed_by_cant_attack_alone() {
    // Guard: the positive ("can only attack alone") must not be confused
    // with the negative ("can't attack alone") which means the opposite.
    let def_only = parse_static_line("~ can only attack alone.").unwrap();
    let def_cant = parse_static_line("~ can't attack alone.").unwrap();
    assert_eq!(
        def_only.mode,
        StaticMode::CombatAlone {
            action: CombatAloneAction::Attack,
            requirement: CombatAloneRequirement::MustBeSole,
        }
    );
    assert_eq!(
        def_cant.mode,
        StaticMode::CombatAlone {
            action: CombatAloneAction::Attack,
            requirement: CombatAloneRequirement::NeedsCompanion,
        }
    );
    assert_ne!(def_only.mode, def_cant.mode);
}

/// CR 508.1: "~ can't attack if defending player controls [filter]" attaches
/// the trailing "if" clause as a `DefendingPlayerControls` condition (Orgg,
/// Mogg Jailer). Before 5a the condition was dropped.
#[test]
fn static_cant_attack_if_defending_player_controls() {
    let def = parse_static_line(
        "~ can't attack if defending player controls an untapped creature with power 3 or greater.",
    )
    .expect("combat restriction should parse");
    assert_eq!(def.mode, StaticMode::CantAttack);
    assert!(
        matches!(
            def.condition,
            Some(StaticCondition::DefendingPlayerControls { .. })
        ),
        "expected DefendingPlayerControls condition, got {:?}",
        def.condition
    );
}

/// CR 509.1c: "~ can't block if you control [filter]" attaches the "if"
/// clause as a controller-scoped board-presence condition (Branded Brawlers).
#[test]
fn static_cant_block_if_you_control() {
    let def = parse_static_line("~ can't block if you control an untapped land.")
        .expect("combat restriction should parse");
    assert_eq!(def.mode, StaticMode::CantBlock);
    assert!(
        def.condition.is_some(),
        "the trailing \"if you control ...\" clause must attach a condition"
    );
}

#[test]
fn static_doesnt_untap() {
    let def =
        parse_static_line("Darksteel Sentinel doesn't untap during your untap step.").unwrap();
    assert_eq!(def.mode, StaticMode::CantUntap);
    assert!(def.description.is_some());
}

#[test]
fn static_cant_be_countered() {
    // CR 101.2: "can't be countered" emits CantBeCountered, not CantBeCast
    let def = parse_static_line("Carnage Tyrant can't be countered.").unwrap();
    assert_eq!(def.mode, StaticMode::CantBeCountered);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert!(def.description.is_some());
}

#[test]
fn static_this_spell_cant_be_copied() {
    // CR 707.10: "This spell can't be copied." — Choreographed Sparks-class.
    // "this spell" is a SELF_REF_PARSE_ONLY phrase (not normalized to ~),
    // so the parser must recognize it as a self-ref static directly.
    let def = parse_static_line("This spell can't be copied.").unwrap();
    assert_eq!(def.mode, StaticMode::CantBeCopied);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert!(def.description.is_some());
}

#[test]
fn static_cant_be_countered_typed_subject() {
    // Allosaurus Shepherd: "Green spells you control can't be countered."
    let def = parse_static_line("Green spells you control can't be countered.").unwrap();
    assert_eq!(def.mode, StaticMode::CantBeCountered);
    if let Some(TargetFilter::Typed(tf)) = &def.affected {
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert!(
            tf.properties
                .iter()
                .any(|p| matches!(p, FilterProp::HasColor { color } if *color == ManaColor::Green)),
            "Expected HasColor Green, got {:?}",
            tf.properties
        );
    } else {
        panic!("Expected Typed filter, got {:?}", def.affected);
    }
}

/// CR 601.2f: "This spell costs {N} less ..." must parse into a
/// self-scoped static — affected = SelfRef, active_zones = self_spell_cost_mod_active_zones() —
/// so the cast-time scanner finds it on the spell itself (not on the
/// battlefield). Regression guard for Tolarian Terror class.
#[test]
fn static_this_spell_cost_less_self_scoped_in_castable_zones() {
    let def = parse_static_line(
        "This spell costs {1} less to cast for each instant and sorcery card in your graveyard.",
    )
    .unwrap();
    assert!(matches!(
        def.mode,
        StaticMode::ModifyCost {
            mode: CostModifyMode::Reduce,
            amount: ManaCost::Cost { generic: 1, .. },
            dynamic_count: Some(_),
            ..
        }
    ));
    assert!(matches!(def.affected, Some(TargetFilter::SelfRef)));
    assert_eq!(
        def.active_zones,
        crate::types::zones::self_spell_cost_mod_active_zones()
    );
}

/// Issue #1372: Demilich's self-spell reduction must function from the graveyard
/// during cast-time cost determination.
#[test]
fn static_demilich_self_cost_reduction_includes_graveyard() {
    let def = parse_static_line(
        "This spell costs {U} less to cast for each instant and sorcery spell you've cast this turn.",
    )
    .unwrap();
    assert!(matches!(def.affected, Some(TargetFilter::SelfRef)));
    assert!(
        def.active_zones.contains(&Zone::Graveyard),
        "Demilich must reduce its cost while in the graveyard, got {:?}",
        def.active_zones
    );
}

#[test]
fn green_goblin_graveyard_cast_cost_reduction_diagnostic() {
    // Green Goblin: "Spells you cast from your graveyard cost {2} less to cast."
    // Diagnostic: confirm this parses to a cost-reduction static (not dropped).
    let def = parse_static_line("Spells you cast from your graveyard cost {2} less to cast.");
    assert!(
        def.is_some(),
        "graveyard-cast cost reduction did not parse to a static",
    );
    let def = def.unwrap();
    assert!(
        matches!(
            def.mode,
            StaticMode::ModifyCost {
                mode: CostModifyMode::Reduce,
                amount: ManaCost::Cost { generic: 2, .. },
                ..
            }
        ),
        "expected ModifyCost Reduce by {{2}}, got mode={:?} affected={:?}",
        def.mode,
        def.affected,
    );
}

#[test]
fn chandras_incinerator_self_cost_reduction_uses_noncombat_damage_to_opponents() {
    let def = parse_static_line(
        "This spell costs {X} less to cast, where X is the total amount of noncombat damage dealt to your opponents this turn.",
    )
    .unwrap();

    let StaticMode::ModifyCost {
        mode: CostModifyMode::Reduce,
        dynamic_count:
            Some(QuantityRef::DamageDealtThisTurn {
                damage_kind: DamageKindFilter::NoncombatOnly,
                target,
                ..
            }),
        ..
    } = def.mode
    else {
        panic!("expected dynamic self-spell ReduceCost, got {:?}", def.mode);
    };
    let TargetFilter::And { filters } = target.as_ref() else {
        panic!("expected opponent player target filter, got {target:?}");
    };
    assert_eq!(filters.len(), 2);
    assert!(matches!(filters[0], TargetFilter::Player));
    let TargetFilter::Typed(typed) = &filters[1] else {
        panic!("expected opponent typed filter, got {:?}", filters[1]);
    };
    assert_eq!(typed.controller, Some(ControllerRef::Opponent));
    assert!(matches!(def.affected, Some(TargetFilter::SelfRef)));
    assert_eq!(
        def.active_zones,
        crate::types::zones::self_spell_cost_mod_active_zones()
    );
}

#[test]
fn ghalta_self_cost_reduction_is_active_from_command_zone() {
    let def = parse_static_line(
        "This spell costs {X} less to cast, where X is the total power of creatures you control.",
    )
    .unwrap();

    let StaticMode::ModifyCost {
        mode: CostModifyMode::Reduce,
        dynamic_count:
            Some(QuantityRef::Aggregate {
                function: AggregateFunction::Sum,
                property: ObjectProperty::Power,
                ..
            }),
        ..
    } = def.mode
    else {
        panic!("expected dynamic self-spell ReduceCost, got {:?}", def.mode);
    };
    assert!(matches!(def.affected, Some(TargetFilter::SelfRef)));
    assert_eq!(
        def.active_zones,
        crate::types::zones::self_spell_cost_mod_active_zones()
    );
}

#[test]
fn self_cost_reduction_where_x_distinct_named_lands_uses_static_cost_seam() {
    let def = parse_static_line(
        "This spell costs {X} less to cast, where X is the number of differently named lands you control.",
    )
    .unwrap();

    let StaticMode::ModifyCost {
        mode: CostModifyMode::Reduce,
        amount,
        dynamic_count:
            Some(QuantityRef::ObjectCountDistinct {
                filter: TargetFilter::Typed(filter),
                qualities,
            }),
        ..
    } = def.mode
    else {
        panic!("expected dynamic self-spell ReduceCost, got {:?}", def.mode);
    };

    assert_eq!(amount, ManaCost::generic(1));
    assert!(filter.type_filters.contains(&TypeFilter::Land));
    assert_eq!(filter.controller, Some(ControllerRef::You));
    assert_eq!(qualities, vec![SharedQuality::Name]);
    assert!(matches!(def.affected, Some(TargetFilter::SelfRef)));
    assert_eq!(
        def.active_zones,
        crate::types::zones::self_spell_cost_mod_active_zones()
    );
}

#[test]
fn static_this_spell_cost_less_for_each_creature_that_attacked_this_turn() {
    let def = parse_static_line(
        "This spell costs {1} less to cast for each creature that attacked this turn.",
    )
    .unwrap();

    let StaticMode::ModifyCost {
        mode: CostModifyMode::Reduce,
        amount: ManaCost::Cost { generic: 1, .. },
        dynamic_count:
            Some(QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(filter),
            }),
        ..
    } = &def.mode
    else {
        panic!("expected self-spell dynamic ReduceCost, got {:?}", def.mode);
    };
    assert!(filter
        .type_filters
        .iter()
        .any(|filter| matches!(filter, TypeFilter::Creature)));
    assert!(filter
        .properties
        .iter()
        .any(|prop| matches!(prop, FilterProp::AttackedThisTurn)));
    assert!(matches!(def.affected, Some(TargetFilter::SelfRef)));
    assert_eq!(
        def.active_zones,
        crate::types::zones::self_spell_cost_mod_active_zones()
    );
}

#[test]
fn static_this_spell_cost_less_for_each_creature_you_attacked_with_this_turn() {
    let def = parse_static_line(
        "This spell costs {1} less to cast for each creature you attacked with this turn.",
    )
    .unwrap();

    assert!(matches!(
        def.mode,
        StaticMode::ModifyCost {
            mode: CostModifyMode::Reduce,
            amount: ManaCost::Cost { generic: 1, .. },
            dynamic_count: Some(QuantityRef::AttackedThisTurn {
                scope: CountScope::Controller,
                filter: None,
            }),
            ..
        }
    ));
    assert!(matches!(def.affected, Some(TargetFilter::SelfRef)));
    assert_eq!(
        def.active_zones,
        crate::types::zones::self_spell_cost_mod_active_zones()
    );
}

#[test]
fn self_cost_reduction_another_filtered_spell_requires_prior_matching_spell() {
    let def = parse_static_line(
            "This spell costs {2} less to cast if you've cast another instant or sorcery spell this turn.",
        )
        .unwrap();

    let Some(StaticCondition::QuantityComparison {
        lhs:
            QuantityExpr::Ref {
                qty:
                    QuantityRef::SpellsCastThisTurn {
                        scope: CountScope::Controller,
                        filter: Some(TargetFilter::Or { filters }),
                    },
            },
        comparator: Comparator::GE,
        rhs: QuantityExpr::Fixed { value: 1 },
    }) = def.condition
    else {
        panic!(
            "expected filtered prior-spell condition, got {:?}",
            def.condition
        );
    };
    assert!(
        filters.iter().any(|filter| matches!(
            filter,
            TargetFilter::Typed(TypedFilter { type_filters, .. })
                if type_filters == &vec![TypeFilter::Instant]
        )) && filters.iter().any(|filter| matches!(
            filter,
            TargetFilter::Typed(TypedFilter { type_filters, .. })
                if type_filters == &vec![TypeFilter::Sorcery]
        ))
    );
}

#[test]
fn self_cost_reduction_if_night_uses_day_night_condition() {
    let def = parse_static_line("This spell costs {2} less to cast if it's night.").unwrap();

    assert!(matches!(
        def.mode,
        StaticMode::ModifyCost {
            mode: CostModifyMode::Reduce,
            amount: ManaCost::Cost { generic: 2, .. },
            ..
        }
    ));
    assert_eq!(
        def.condition,
        Some(StaticCondition::DayNightIs {
            state: crate::types::game_state::DayNight::Night
        })
    );
    assert!(matches!(def.affected, Some(TargetFilter::SelfRef)));
    assert_eq!(
        def.active_zones,
        crate::types::zones::self_spell_cost_mod_active_zones()
    );
}

#[test]
fn self_cost_reduction_if_bargained_uses_additional_cost_paid_condition() {
    // CR 702.166a: "if it's bargained" routes to StaticCondition::AdditionalCostPaid
    // (Hamlet Glutton, Ice Out, Johann's Stopgap).
    for text in [
        "This spell costs {2} less to cast if it's bargained.",
        "This spell costs {2} less to cast if it is bargained.",
        "This spell costs {2} less to cast if it was bargained.",
        "This spell costs {2} less to cast if this spell is bargained.",
    ] {
        let def =
            parse_static_line(text).unwrap_or_else(|| panic!("expected a static for {text:?}"));
        assert!(
            matches!(
                def.mode,
                StaticMode::ModifyCost {
                    mode: CostModifyMode::Reduce,
                    amount: ManaCost::Cost { generic: 2, .. },
                    ..
                }
            ),
            "expected ReduceCost {{2}} for {text:?}, got {:?}",
            def.mode
        );
        assert_eq!(
            def.condition,
            Some(StaticCondition::AdditionalCostPaid),
            "expected AdditionalCostPaid condition for {text:?}"
        );
        assert!(matches!(def.affected, Some(TargetFilter::SelfRef)));
    }
}

#[test]
fn self_cost_reduction_if_control_wizard_still_uses_presence_condition() {
    // Regression: the bargained early-return must not divert other conditions.
    let def =
        parse_static_line("This spell costs {2} less to cast if you control a Wizard.").unwrap();
    assert!(matches!(
        def.mode,
        StaticMode::ModifyCost {
            mode: CostModifyMode::Reduce,
            ..
        }
    ));
    assert!(
        !matches!(def.condition, Some(StaticCondition::AdditionalCostPaid)),
        "control-a-Wizard must not parse as AdditionalCostPaid, got {:?}",
        def.condition
    );
    assert!(def.condition.is_some(), "expected a presence condition");
}

#[test]
fn static_this_spell_cost_less_if_it_targets_creature_filter() {
    let def = parse_static_line("This spell costs {2} less to cast if it targets a red creature.")
        .unwrap();

    assert!(matches!(
        def.mode,
        StaticMode::ModifyCost {
            mode: CostModifyMode::Reduce,
            amount: ManaCost::Cost { generic: 2, .. },
            ..
        }
    ));
    let StaticMode::ModifyCost {
        mode: CostModifyMode::Reduce,
        ref spell_filter,
        ..
    } = def.mode
    else {
        panic!("expected ReduceCost");
    };
    let filter = spell_filter
        .as_ref()
        .expect("expected target-gated spell filter");
    let TargetFilter::Typed(tf) = filter else {
        panic!("expected typed spell filter, got {filter:?}");
    };
    let targets_filter = tf
        .properties
        .iter()
        .find_map(|prop| match prop {
            FilterProp::Targets { filter } => Some(filter),
            _ => None,
        })
        .expect("expected Targets property");
    let TargetFilter::Typed(target_tf) = targets_filter.as_ref() else {
        panic!("expected typed target filter, got {targets_filter:?}");
    };
    assert!(target_tf.type_filters.contains(&TypeFilter::Creature));
    assert!(target_tf.properties.iter().any(|prop| matches!(
        prop,
        FilterProp::HasColor {
            color: ManaColor::Red
        }
    )));
    assert!(matches!(def.affected, Some(TargetFilter::SelfRef)));
    assert_eq!(
        def.active_zones,
        crate::types::zones::self_spell_cost_mod_active_zones()
    );
}

#[test]
fn static_this_spell_cost_less_if_it_targets_spell_or_ability_targeting_large_creature() {
    let def = parse_static_line(
        "This spell costs {7} less to cast if it targets a spell or ability that targets a creature you control with power 7 or greater.",
    )
    .unwrap();

    let StaticMode::ModifyCost {
        mode: CostModifyMode::Reduce,
        amount: ManaCost::Cost { generic: 7, .. },
        ref spell_filter,
        ..
    } = def.mode
    else {
        panic!("expected ReduceCost");
    };
    let filter = spell_filter
        .as_ref()
        .expect("expected stack-target-gated spell filter");
    let TargetFilter::Typed(tf) = filter else {
        panic!("expected typed spell filter, got {filter:?}");
    };
    let self_targets_filter = tf
        .properties
        .iter()
        .find_map(|prop| match prop {
            FilterProp::Targets { filter } => Some(filter.as_ref()),
            _ => None,
        })
        .expect("expected outer Targets property");
    let TargetFilter::And { filters } = self_targets_filter else {
        panic!("expected stack target conjunction, got {self_targets_filter:?}");
    };
    assert!(filters.iter().any(|filter| matches!(
        filter,
        TargetFilter::Or { filters }
            if filters.iter().any(|f| matches!(f, TargetFilter::StackSpell))
                && filters
                    .iter()
                    .any(|f| matches!(f, TargetFilter::StackAbility { controller: None, tag: None }))
    )));
    let stack_targets_filter = filters
        .iter()
        .find_map(|filter| match filter {
            TargetFilter::Typed(tf) => tf.properties.iter().find_map(|prop| match prop {
                FilterProp::Targets { filter } => Some(filter.as_ref()),
                _ => None,
            }),
            _ => None,
        })
        .expect("expected nested Targets property");
    let TargetFilter::Typed(creature_tf) = stack_targets_filter else {
        panic!("expected typed creature target filter, got {stack_targets_filter:?}");
    };
    assert!(creature_tf.type_filters.contains(&TypeFilter::Creature));
    assert_eq!(creature_tf.controller, Some(ControllerRef::You));
    assert!(creature_tf.properties.iter().any(|prop| matches!(
        prop,
        FilterProp::PtComparison {
            stat: PtStat::Power,
            scope: PtValueScope::Current,
            comparator: Comparator::GE,
            value: QuantityExpr::Fixed { value: 7 },
        }
    )));
    assert!(matches!(def.affected, Some(TargetFilter::SelfRef)));
    assert_eq!(
        def.active_zones,
        crate::types::zones::self_spell_cost_mod_active_zones()
    );
}

#[test]
fn static_this_spell_cost_less_if_it_targets_stack_object_fails_closed_on_trailing_text() {
    assert!(
        parse_static_line(
            "This spell costs {7} less to cast if it targets a spell or ability that targets a creature you control with power 7 or greater and toughness 7 or greater.",
        )
        .is_none(),
        "unconsumed nested target text must not become an unconditional cost reduction"
    );
}

#[test]
fn static_spells_cost_less() {
    let def = parse_static_line("Spells you cast cost {1} less to cast.").unwrap();
    assert!(matches!(
        def.mode,
        StaticMode::ModifyCost {
            mode: CostModifyMode::Reduce,
            spell_filter: None,
            dynamic_count: None,
            ..
        }
    ));
    // Verify amount is generic 1 (avoid assert_eq! on complex types — SIGABRT risk)
    assert!(matches!(
        def.mode,
        StaticMode::ModifyCost {
            mode: CostModifyMode::Reduce,
            amount: ManaCost::Cost { generic: 1, .. },
            ..
        }
    ));
}

#[test]
fn static_opponent_spells_cost_more() {
    let def = parse_static_line("Spells your opponents cast cost {1} more to cast.").unwrap();
    assert!(matches!(
        def.mode,
        StaticMode::ModifyCost {
            mode: CostModifyMode::Raise,
            spell_filter: None,
            dynamic_count: None,
            ..
        }
    ));
    assert!(matches!(
        def.mode,
        StaticMode::ModifyCost {
            mode: CostModifyMode::Raise,
            amount: ManaCost::Cost { generic: 1, .. },
            ..
        }
    ));
}

#[test]
fn static_opponent_spells_targeting_commanders_cost_more() {
    let def = parse_static_line(
            "Spells your opponents cast that target one or more commanders you control cost {3} more to cast.",
        )
        .unwrap();

    assert!(matches!(
        def.mode,
        StaticMode::ModifyCost {
            mode: CostModifyMode::Raise,
            amount: ManaCost::Cost { generic: 3, .. },
            ..
        }
    ));
    assert!(matches!(
        def.affected,
        Some(TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::Opponent),
            ..
        }))
    ));
    let StaticMode::ModifyCost {
        mode: CostModifyMode::Raise,
        ref spell_filter,
        ..
    } = def.mode
    else {
        panic!("expected RaiseCost");
    };
    let TargetFilter::Typed(tf) = spell_filter
        .as_ref()
        .expect("expected target-gated spell filter")
    else {
        panic!("expected typed spell filter");
    };
    let commander_filter = tf
        .properties
        .iter()
        .find_map(|prop| match prop {
            FilterProp::Targets { filter } => Some(filter),
            _ => None,
        })
        .expect("expected Targets property");
    let TargetFilter::Typed(commander_tf) = commander_filter.as_ref() else {
        panic!("expected typed commander filter");
    };
    assert_eq!(commander_tf.controller, Some(ControllerRef::You));
    assert!(commander_tf.type_filters.contains(&TypeFilter::Permanent));
    assert!(commander_tf.properties.contains(&FilterProp::IsCommander));
}

#[test]
fn static_spells_targeting_creature_cost_less() {
    let def =
        parse_static_line("Spells you cast that target a creature cost {2} less to cast.").unwrap();

    assert!(matches!(
        def.mode,
        StaticMode::ModifyCost {
            mode: CostModifyMode::Reduce,
            amount: ManaCost::Cost { generic: 2, .. },
            ..
        }
    ));
    let StaticMode::ModifyCost {
        mode: CostModifyMode::Reduce,
        ref spell_filter,
        ..
    } = def.mode
    else {
        panic!("expected ReduceCost");
    };
    let TargetFilter::Typed(tf) = spell_filter
        .as_ref()
        .expect("expected target-gated spell filter")
    else {
        panic!("expected typed spell filter");
    };
    let target_filter = tf
        .properties
        .iter()
        .find_map(|prop| match prop {
            FilterProp::Targets { filter } => Some(filter),
            _ => None,
        })
        .expect("expected Targets property");
    let TargetFilter::Typed(target_tf) = target_filter.as_ref() else {
        panic!("expected typed target filter");
    };
    assert!(target_tf.type_filters.contains(&TypeFilter::Creature));
}

#[test]
fn static_opponent_spells_from_zones_cost_more() {
    // Aven Interrupter: "Spells your opponents cast from graveyards or from exile cost {2} more to cast."
    let def = parse_static_line(
        "Spells your opponents cast from graveyards or from exile cost {2} more to cast.",
    )
    .unwrap();
    assert!(matches!(
        def.mode,
        StaticMode::ModifyCost {
            mode: CostModifyMode::Raise,
            amount: ManaCost::Cost { generic: 2, .. },
            ..
        }
    ));
    if let StaticMode::ModifyCost {
        mode: CostModifyMode::Raise,
        ref spell_filter,
        ..
    } = def.mode
    {
        let filter = spell_filter
            .as_ref()
            .expect("Expected spell_filter with zone constraint");
        match filter {
            TargetFilter::Typed(tf) => {
                assert!(
                    tf.properties.iter().any(|p| matches!(
                        p,
                        FilterProp::InAnyZone { zones }
                            if zones.contains(&Zone::Graveyard) && zones.contains(&Zone::Exile)
                    )),
                    "Expected InAnyZone with Graveyard and Exile, got {:?}",
                    tf.properties
                );
            }
            _ => panic!("Expected Typed filter, got {:?}", filter),
        }
    }
    // Affected should scope to opponents
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::Opponent));
        }
        other => panic!("Expected Typed affected with Opponent, got {:?}", other),
    }
}

#[test]
fn static_spells_from_exile_cost_less() {
    // "Spells you cast from exile this turn cost {X} less to cast" (without "this turn" dynamic)
    let def = parse_static_line("Spells you cast from exile cost {1} less to cast.").unwrap();
    assert!(matches!(
        def.mode,
        StaticMode::ModifyCost {
            mode: CostModifyMode::Reduce,
            amount: ManaCost::Cost { generic: 1, .. },
            ..
        }
    ));
    if let StaticMode::ModifyCost {
        mode: CostModifyMode::Reduce,
        ref spell_filter,
        ..
    } = def.mode
    {
        let filter = spell_filter
            .as_ref()
            .expect("Expected spell_filter with zone constraint");
        match filter {
            TargetFilter::Typed(tf) => {
                assert!(
                    tf.properties
                        .iter()
                        .any(|p| matches!(p, FilterProp::InZone { zone: Zone::Exile })),
                    "Expected InZone Exile, got {:?}",
                    tf.properties
                );
            }
            _ => panic!("Expected Typed filter"),
        }
    }
}

#[test]
fn static_creature_spells_cost_less() {
    // Goblin Electromancer-style: "Creature spells you cast cost {1} less to cast."
    let def = parse_static_line("Creature spells you cast cost {1} less to cast.").unwrap();
    assert!(matches!(
        def.mode,
        StaticMode::ModifyCost {
            mode: CostModifyMode::Reduce,
            amount: ManaCost::Cost { generic: 1, .. },
            ..
        }
    ));
    if let StaticMode::ModifyCost {
        mode: CostModifyMode::Reduce,
        ref spell_filter,
        ..
    } = def.mode
    {
        let filter = spell_filter.as_ref().expect("Expected spell_filter");
        match filter {
            TargetFilter::Typed(tf) => {
                assert!(
                    tf.type_filters
                        .iter()
                        .any(|t| matches!(t, TypeFilter::Creature)),
                    "Expected Creature type filter"
                );
            }
            _ => panic!("Expected Typed filter"),
        }
    }
}

#[test]
fn static_spells_of_chosen_type_cost_less_carries_chosen_card_type() {
    // Issue #930 — Cloud Key / Umori / Stenn:
    // "Spells you cast of the chosen type cost {1} less to cast."
    // CR 205.2a: the "of the chosen type" qualifier must narrow the
    // reduction to the chosen card type, not every spell. The "you cast"
    // infix previously prevented the discriminator from being extracted.
    let def =
        parse_static_line("Spells you cast of the chosen type cost {1} less to cast.").unwrap();
    let StaticMode::ModifyCost {
        mode: CostModifyMode::Reduce,
        spell_filter: Some(TargetFilter::Typed(ref tf)),
        ..
    } = def.mode
    else {
        panic!(
            "expected ReduceCost with a Typed spell_filter, got {:?}",
            def.mode
        );
    };
    assert!(
        tf.properties
            .iter()
            .any(|p| matches!(p, FilterProp::IsChosenCardType)),
        "chosen-type cost reduction must carry IsChosenCardType, got {:?}",
        tf.properties
    );
}

#[test]
fn static_creature_spells_of_chosen_type_cost_less_carries_chosen_creature_type() {
    // Issue #930 — Herald's Horn:
    // "Creature spells you cast of the chosen type cost {1} less to cast."
    // CR 205.2a: a creature-typed base pairs with a chosen CREATURE type.
    let def =
        parse_static_line("Creature spells you cast of the chosen type cost {1} less to cast.")
            .unwrap();
    let StaticMode::ModifyCost {
        mode: CostModifyMode::Reduce,
        spell_filter: Some(TargetFilter::Typed(ref tf)),
        ..
    } = def.mode
    else {
        panic!(
            "expected ReduceCost with a Typed spell_filter, got {:?}",
            def.mode
        );
    };
    assert!(
        tf.type_filters
            .iter()
            .any(|t| matches!(t, TypeFilter::Creature)),
        "expected Creature type filter, got {:?}",
        tf.type_filters
    );
    assert!(
        tf.properties
            .iter()
            .any(|p| matches!(p, FilterProp::IsChosenCreatureType)),
        "creature chosen-type reduction must carry IsChosenCreatureType, got {:?}",
        tf.properties
    );
}

#[test]
fn static_instant_sorcery_spells_cost_less() {
    // Goblin Electromancer: "Instant and sorcery spells you cast cost {1} less to cast."
    let def = parse_static_line("Instant and sorcery spells you cast cost {1} less to cast.");
    assert!(
        def.is_some(),
        "parse returned None for instant/sorcery cost reduction"
    );
    let def = def.unwrap();
    assert!(
        matches!(
            def.mode,
            StaticMode::ModifyCost {
                mode: CostModifyMode::Reduce,
                ..
            }
        ),
        "Expected ReduceCost mode"
    );
    if let StaticMode::ModifyCost {
        mode: CostModifyMode::Reduce,
        ref spell_filter,
        ..
    } = def.mode
    {
        assert!(
            spell_filter.is_some(),
            "Expected spell_filter for instant/sorcery"
        );
        let filter = spell_filter.as_ref().unwrap();
        // parse_type_phrase("instant and sorcery") → TargetFilter::Or { [Typed(Instant), Typed(Sorcery)] }
        fn contains_type(f: &TargetFilter, expected: TypeFilter) -> bool {
            match f {
                TargetFilter::Typed(tf) => tf.type_filters.contains(&expected),
                TargetFilter::Or { filters } => filters
                    .iter()
                    .any(|inner| contains_type(inner, expected.clone())),
                _ => false,
            }
        }
        assert!(
            contains_type(filter, TypeFilter::Instant),
            "Expected Instant in filter"
        );
        assert!(
            contains_type(filter, TypeFilter::Sorcery),
            "Expected Sorcery in filter"
        );
    }
}

#[test]
fn static_red_or_green_spell_cost_less_issue_141() {
    // Goblin Anarchomancer (issue #141): "Each spell you cast that's red or green …"
    let def = parse_static_line("Each spell you cast that's red or green costs {1} less to cast.");
    assert!(
        def.is_some(),
        "Goblin Anarchomancer cost-reduction line must parse"
    );
    let def = def.unwrap();
    let StaticMode::ModifyCost {
        mode: CostModifyMode::Reduce,
        spell_filter: Some(filter),
        ..
    } = def.mode
    else {
        panic!(
            "expected ModifyCost Reduce with spell_filter, got {:?}",
            def.mode
        );
    };
    match filter {
        TargetFilter::Typed(tf) => {
            let any_of = tf
                .properties
                .iter()
                .find_map(|p| match p {
                    FilterProp::AnyOf { props } => Some(props),
                    _ => None,
                })
                .expect("expected AnyOf red/green HasColor props");
            assert!(
                any_of.contains(&FilterProp::HasColor {
                    color: ManaColor::Red
                }),
                "expected Red in filter, got {any_of:?}"
            );
            assert!(
                any_of.contains(&FilterProp::HasColor {
                    color: ManaColor::Green
                }),
                "expected Green in filter, got {any_of:?}"
            );
        }
        other => panic!("expected Typed spell filter with color AnyOf, got {other:?}"),
    }
}

#[test]
fn static_typed_spell_with_that_clause_cost_less() {
    let def = parse_static_line("Each creature spell you cast that's red costs {1} less to cast.")
        .expect("typed spell cost modifier with that-clause should parse");
    let StaticMode::ModifyCost {
        mode: CostModifyMode::Reduce,
        spell_filter: Some(TargetFilter::Typed(filter)),
        ..
    } = def.mode
    else {
        panic!(
            "expected ModifyCost Reduce with typed spell_filter, got {:?}",
            def.mode
        );
    };

    assert!(filter.type_filters.contains(&TypeFilter::Creature));
    assert!(filter.properties.contains(&FilterProp::HasColor {
        color: ManaColor::Red
    }));
}

#[test]
fn static_white_spells_cost_more() {
    // "White spells your opponents cast cost {1} more to cast."
    let def = parse_static_line("White spells your opponents cast cost {1} more to cast.").unwrap();
    assert!(matches!(
        def.mode,
        StaticMode::ModifyCost {
            mode: CostModifyMode::Raise,
            ..
        }
    ));
    if let StaticMode::ModifyCost {
        mode: CostModifyMode::Raise,
        ref spell_filter,
        ..
    } = def.mode
    {
        let filter = spell_filter.as_ref().expect("Expected spell_filter");
        match filter {
            TargetFilter::Typed(tf) => {
                assert!(
                    tf.properties.iter().any(|p| matches!(
                        p,
                        FilterProp::HasColor { color } if *color == ManaColor::White
                    )),
                    "Expected HasColor White"
                );
            }
            _ => panic!("Expected Typed filter"),
        }
    }
}

#[test]
fn static_noncreature_spells_cost_more_thalia() {
    // Thalia: "Noncreature spells cost {1} more to cast."
    let def = parse_static_line("Noncreature spells cost {1} more to cast.").unwrap();
    assert!(matches!(
        def.mode,
        StaticMode::ModifyCost {
            mode: CostModifyMode::Raise,
            amount: ManaCost::Cost { generic: 1, .. },
            ..
        }
    ));
    if let StaticMode::ModifyCost {
        mode: CostModifyMode::Raise,
        ref spell_filter,
        ..
    } = def.mode
    {
        let filter = spell_filter.as_ref().expect("Expected spell_filter");
        match filter {
            TargetFilter::Typed(tf) => {
                // Noncreature → TypeFilter::Non(Creature)
                assert!(
                    tf.type_filters.iter().any(|t| matches!(
                        t,
                        TypeFilter::Non(inner) if matches!(inner.as_ref(), TypeFilter::Creature)
                    )),
                    "Expected Non(Creature) type filter"
                );
            }
            _ => panic!("Expected Typed filter"),
        }
    }
}

/// CR 201.3 / CR 113.6 + CR 601.2f: Disruptor Flute — "Spells with the
/// chosen name cost {3} more to cast." Bare "spells" (no type adjective)
/// composes with the `HasChosenName` filter so the cost bump applies only
/// to spells matching the source's bound `ChosenAttribute::CardName`, not
/// every spell on every player's stack. Regression discriminator for #603:
/// previously the chosen-name suffix was swallowed and the parser emitted
/// a bare `Typed(Card)` filter, taxing every spell in hand.
#[test]
fn static_spells_with_chosen_name_cost_more_disruptor_flute() {
    let def = parse_static_line("Spells with the chosen name cost {3} more to cast.").unwrap();
    let StaticMode::ModifyCost {
        mode: CostModifyMode::Raise,
        amount,
        spell_filter,
        dynamic_count,
    } = def.mode
    else {
        panic!("expected RaiseCost, got {:?}", def.mode);
    };
    assert!(matches!(amount, ManaCost::Cost { generic: 3, .. }));
    assert!(dynamic_count.is_none());
    assert_eq!(
        spell_filter,
        Some(TargetFilter::HasChosenName),
        "bare 'Spells with the chosen name' must lower to HasChosenName, not Typed(Card)"
    );
}

/// CR 601.2f: Trinisphere — the cost-floor static. The line begins with
/// "As long as ~ is untapped," (inverted form) which the static parser
/// rewrites to canonical "<effect> as long as <condition>" before
/// re-dispatching. The cost-floor arm catches the rewritten body and
/// produces `MinimumCost { amount: {3}, spell_filter: None }` with the
/// `Not(SourceIsTapped)` condition lifted into `definition.condition`.
#[test]
fn static_trinisphere_cost_floor_with_untapped_condition() {
    let def = parse_static_line(
            "As long as ~ is untapped, each spell that would cost less than three mana to cast costs three mana to cast.",
        )
        .expect("Trinisphere line must parse");
    match &def.mode {
        StaticMode::ModifyCost {
            mode: CostModifyMode::Minimum,
            amount,
            spell_filter,
            ..
        } => {
            assert_eq!(amount, &ManaCost::generic(3), "floor must be {{3}}");
            assert!(spell_filter.is_none(), "Trinisphere has no spell filter");
        }
        other => panic!("expected MinimumCost, got {other:?}"),
    }
    assert!(
        matches!(
            def.condition,
            Some(StaticCondition::Not { ref condition })
                if matches!(**condition, StaticCondition::SourceIsTapped)
        ),
        "Trinisphere must carry Not(SourceIsTapped); got {:?}",
        def.condition
    );
}

/// CR 601.2f: Building-block — the cost-floor parser handles canonical
/// (non-inverted) form too, with no trailing condition.
#[test]
fn static_cost_floor_canonical_form_no_condition() {
    let def = parse_static_line(
        "Each spell that would cost less than three mana to cast costs three mana to cast.",
    )
    .expect("canonical cost-floor line must parse");
    assert!(
        matches!(
            def.mode,
            StaticMode::ModifyCost {
                mode: CostModifyMode::Minimum,
                amount: ManaCost::Cost { generic: 3, .. },
                spell_filter: None,
                ..
            }
        ),
        "expected MinimumCost(3); got {:?}",
        def.mode
    );
    assert!(
        def.condition.is_none(),
        "canonical form has no trailing condition"
    );
}

/// CR 601.2f + CR 107.3 + CR 122.1a: Zimone, Infinite Analyst's per-counter
/// cost reduction — "The first spell you cast with {X} in its mana cost each
/// turn costs {1} less to cast for each +1/+1 counter on ~." This regressed
/// (issue #1359): the static dropped both the `{X}`-cost spell filter and the
/// "first spell each turn" gate, so the reduction applied to every spell, every
/// time. Asserts all three load-bearing axes are present:
///   1. `spell_filter` carries `HasXInManaCost` (CR 107.3) — only {X}-spells.
///   2. `condition` gates on `SpellsCastThisTurn(HasXInManaCost) == 0` — only
///      the FIRST such spell each turn. "each turn" carries NO `DuringYourTurn`,
///      so the first {X}-spell qualifies on any player's turn.
///   3. `dynamic_count` is `CountersOn { Source, Plus1Plus1 }` — reduces by K
///      for K +1/+1 counters on the source (CR 122.1a).
#[test]
fn static_zimone_first_x_spell_each_turn_reduces_per_plus1_counter() {
    let def = parse_static_line(
        "The first spell you cast with {X} in its mana cost each turn costs {1} less to cast for each +1/+1 counter on ~.",
    )
    .unwrap();

    let StaticMode::ModifyCost {
        mode: CostModifyMode::Reduce,
        ref spell_filter,
        ref dynamic_count,
        ..
    } = def.mode
    else {
        panic!("expected ReduceCost, got {:?}", def.mode);
    };

    // Axis 1: only spells with {X} in their mana cost are reduced.
    let filter = spell_filter
        .as_ref()
        .expect("expected {X}-cost spell filter");
    let TargetFilter::Typed(tf) = filter else {
        panic!("expected typed spell filter, got {filter:?}");
    };
    assert!(
        tf.properties.contains(&FilterProp::HasXInManaCost),
        "reduction must be gated on HasXInManaCost, got {tf:?}"
    );

    // Axis 3: per-+1/+1-counter dynamic quantity on the source.
    assert!(
        matches!(
            dynamic_count,
            Some(QuantityRef::CountersOn {
                scope: ObjectScope::Source,
                counter_type: Some(CounterType::Plus1Plus1),
            })
        ),
        "reduction amount must be per +1/+1 counter on the source, got {dynamic_count:?}"
    );

    // Axis 2: only the FIRST {X}-spell each turn — no DuringYourTurn for the
    // "each turn" timing (any player's turn qualifies).
    let condition = def.condition.expect("expected first-spell-each-turn gate");
    assert!(
        matches!(
            &condition,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::SpellsCastThisTurn {
                        scope: CountScope::Controller,
                        filter: Some(inner),
                    },
                },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            } if matches!(inner, TargetFilter::Typed(itf) if itf.properties.contains(&FilterProp::HasXInManaCost))
        ),
        "expected SpellsCastThisTurn(HasXInManaCost) == 0 with no DuringYourTurn, got {condition:?}"
    );
}

#[test]
fn static_first_qualified_spell_costs_less_has_filter_and_condition() {
    let def = parse_static_line(
            "The first non-Lemur creature spell with flying you cast during each of your turns costs {1} less to cast.",
        )
        .unwrap();

    assert!(matches!(
        def.mode,
        StaticMode::ModifyCost {
            mode: CostModifyMode::Reduce,
            ..
        }
    ));
    let StaticMode::ModifyCost {
        mode: CostModifyMode::Reduce,
        ref spell_filter,
        ..
    } = def.mode
    else {
        unreachable!();
    };
    let filter = spell_filter.as_ref().expect("expected spell filter");
    let TargetFilter::Typed(filter) = filter else {
        panic!("expected typed spell filter, got {filter:?}");
    };
    assert!(filter.type_filters.contains(&TypeFilter::Creature));
    assert!(filter.type_filters.iter().any(|entry| matches!(
            entry,
            TypeFilter::Non(inner) if matches!(inner.as_ref(), TypeFilter::Subtype(subtype) if subtype == "Lemur")
        )));
    assert!(filter.properties.iter().any(|prop| matches!(
        prop,
        FilterProp::WithKeyword { value } if *value == Keyword::Flying
    )));

    let condition = def.condition.expect("expected first-spell condition");
    let StaticCondition::And { conditions } = condition else {
        panic!("expected And condition");
    };
    assert!(conditions.contains(&StaticCondition::DuringYourTurn));
    assert!(conditions.iter().any(|condition| matches!(
            condition,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::SpellsCastThisTurn { scope: CountScope::Controller, filter: Some(inner) },
                },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            } if inner == spell_filter.as_ref().unwrap()
        )));
}

#[test]
fn static_first_qualified_spell_costs_more_handles_each_opponent_casts() {
    let def = parse_static_line(
        "The first noncreature spell each opponent casts each turn costs {1} more to cast.",
    )
    .unwrap();

    let StaticMode::ModifyCost {
        mode: CostModifyMode::Raise,
        ref spell_filter,
        ..
    } = def.mode
    else {
        panic!("expected RaiseCost, got {:?}", def.mode);
    };

    let filter = spell_filter.as_ref().expect("expected spell filter");
    assert!(matches!(
        filter,
        TargetFilter::Typed(tf)
            if tf
                .type_filters
                .iter()
                .any(|entry| matches!(entry, TypeFilter::Non(inner) if matches!(inner.as_ref(), TypeFilter::Creature)))
    ));

    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::card().controller(ControllerRef::Opponent)
        ))
    );

    let condition = def.condition.expect("expected first-spell condition");
    assert!(matches!(
        &condition,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::SpellsCastThisTurn {
                    scope: CountScope::Controller,
                    filter: Some(inner),
                },
            },
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: 0 },
        } if matches!(
            inner,
            TargetFilter::Typed(tf)
                if tf
                    .type_filters
                    .iter()
                    .any(|entry| matches!(entry, TypeFilter::Non(inner) if matches!(inner.as_ref(), TypeFilter::Creature)))
        )
    ));
}

#[test]
fn static_first_unqualified_spell_costs_less_keeps_first_spell_gate() {
    let def =
        parse_static_line("The first spell you cast each turn costs {1} less to cast.").unwrap();

    let StaticMode::ModifyCost {
        mode: CostModifyMode::Reduce,
        ref spell_filter,
        ..
    } = def.mode
    else {
        panic!("expected ReduceCost, got {:?}", def.mode);
    };

    assert_eq!(
        spell_filter,
        &Some(TargetFilter::Typed(TypedFilter::card()))
    );

    let condition = def.condition.expect("expected first-spell condition");
    assert!(matches!(
        &condition,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::SpellsCastThisTurn {
                    scope: CountScope::Controller,
                    filter: Some(inner),
                },
            },
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: 0 },
        } if inner == spell_filter.as_ref().unwrap()
    ));
}

/// CR 601.2f + CR 702.33d: "The first kicked spell you cast each turn costs {1}
/// less to cast." (Vine Gecko). The "kicked" qualifier — whether the spell's
/// kicker additional cost was paid — is not a representable spell-cost filter,
/// so the parser must DECLINE the cost static rather than emit a filterless,
/// conditionless reducer. A broad reducer would silently drop both the printed
/// "first … each turn" once-per-turn gate and the "kicked" qualifier, reducing
/// every spell the controller casts (the bug this guards against).
#[test]
fn static_first_kicked_spell_does_not_emit_broad_reducer() {
    let parsed =
        parse_static_line("The first kicked spell you cast each turn costs {1} less to cast.");

    assert!(
        parsed.is_none(),
        "kicked-spell cost reducer must be declined until paid-kicker state is representable; got {parsed:?}"
    );
}

#[test]
fn static_spells_cost_x_less_where_x_is_your_speed() {
    let def = parse_static_line(
        "Noncreature spells you cast cost {X} less to cast, where X is your speed.",
    )
    .unwrap();
    let StaticMode::ModifyCost {
        mode: CostModifyMode::Reduce,
        amount,
        dynamic_count,
        ..
    } = def.mode
    else {
        panic!("expected ReduceCost");
    };
    assert_eq!(amount, ManaCost::generic(1));
    assert_eq!(
        dynamic_count,
        Some(QuantityRef::Speed {
            player: PlayerScope::Controller
        })
    );
}

#[test]
fn static_noncreature_spells_cost_less_as_long_as_lesson_threshold() {
    let def = parse_static_line(
            "Noncreature spells you cast cost {1} less to cast as long as there are three or more Lesson cards in your graveyard.",
        )
        .unwrap();

    assert!(matches!(
        def.mode,
        StaticMode::ModifyCost {
            mode: CostModifyMode::Reduce,
            ..
        }
    ));
    assert!(matches!(
        def.condition,
        Some(StaticCondition::QuantityComparison {
            lhs:
                QuantityExpr::Ref {
                    qty: QuantityRef::ZoneCardCount {
                        zone: ZoneRef::Graveyard,
                        ref card_types,
                        scope: CountScope::Controller,
                        filter: None,
                    }
                },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 3 },
        }) if card_types == &vec![TypeFilter::Subtype("Lesson".to_string())]
    ));
}

// NOTE: static_enters_with_counters test moved to oracle_replacement tests —
// "enters with counters" is now parsed as a Moved replacement effect.

/// CR 113.6b + CR 207.2c + CR 408 + CR 601.2f: The Ur-Dragon's Eminence
/// line (canonical form) — "Other Dragon spells you cast cost {1} less to
/// cast as long as ~ is in the command zone or on the battlefield."
/// The condition disjunction must seed `active_zones` with both
/// `Battlefield` and `Command`, and produce a typed `Or { SourceInZone,
/// SourceInZone }` (no `SwallowedClause`).
#[test]
fn static_eminence_cost_reduction_command_zone_or_battlefield() {
    let def = parse_static_line(
            "Other Dragon spells you cast cost {1} less to cast as long as ~ is in the command zone or on the battlefield.",
        )
        .expect("Eminence cost-reduction line must parse");

    // Mode is unchanged: ReduceCost {1} with a Dragon spell filter.
    assert!(
        matches!(
            def.mode,
            StaticMode::ModifyCost {
                mode: CostModifyMode::Reduce,
                amount: ManaCost::Cost { generic: 1, .. },
                ..
            }
        ),
        "expected ReduceCost {{1}}, got {:?}",
        def.mode
    );

    // CR 113.6b: active_zones must include BOTH Battlefield and Command —
    // populate_active_zones_from_condition walks the typed Or-disjunction.
    assert!(
        def.active_zones.contains(&Zone::Battlefield),
        "active_zones must contain Battlefield, got {:?}",
        def.active_zones
    );
    assert!(
        def.active_zones.contains(&Zone::Command),
        "active_zones must contain Command, got {:?}",
        def.active_zones
    );

    // Condition is a typed Or-disjunction over SourceInZone variants —
    // NOT a SwallowedClause / Unrecognized fallback.
    match def.condition.as_ref().expect("condition must be set") {
        StaticCondition::Or { conditions } => {
            let zones: Vec<Zone> = conditions
                .iter()
                .filter_map(|c| match c {
                    StaticCondition::SourceInZone { zone } => Some(*zone),
                    _ => None,
                })
                .collect();
            assert!(zones.contains(&Zone::Command));
            assert!(zones.contains(&Zone::Battlefield));
        }
        other => panic!("expected Or-disjunction, got {other:?}"),
    }
}

/// CR 113.6b: Inverted Eminence form — "As long as ~ is in the command zone
/// or on the battlefield, other Dragon spells you cast cost {1} less to
/// cast." (The shape parsed straight off the printed Oracle text after the
/// Eminence ability-word strip.) Must converge to the same typed shape as
/// the canonical-form test.
#[test]
fn static_eminence_cost_reduction_inverted_form() {
    let def = parse_static_line(
            "As long as ~ is in the command zone or on the battlefield, other Dragon spells you cast cost {1} less to cast.",
        )
        .expect("inverted Eminence cost-reduction must parse");

    assert!(
        matches!(
            def.mode,
            StaticMode::ModifyCost {
                mode: CostModifyMode::Reduce,
                amount: ManaCost::Cost { generic: 1, .. },
                ..
            }
        ),
        "expected ReduceCost {{1}}, got {:?}",
        def.mode
    );
    assert!(def.active_zones.contains(&Zone::Battlefield));
    assert!(def.active_zones.contains(&Zone::Command));
    assert!(matches!(
        def.condition.as_ref().expect("condition must be set"),
        StaticCondition::Or { .. }
    ));
}

#[test]
fn static_as_long_as_chosen_color() {
    let def =
        parse_static_line("As long as the chosen color is blue, enchanted creature has flying.")
            .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(matches!(
        def.condition,
        Some(StaticCondition::ChosenColorIs {
            color: crate::types::mana::ManaColor::Blue
        })
    ));
}

#[test]
fn static_as_long_as_hand_size_gt_life() {
    use crate::types::ability::{Comparator, QuantityExpr, QuantityRef};
    let def = parse_static_line(
            "As long as the number of cards in your hand is greater than your life total, enchanted creature has trample.",
        )
        .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(matches!(
        def.condition,
        Some(StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::HandSize {
                    player: crate::types::ability::PlayerScope::Controller
                }
            },
            comparator: Comparator::GT,
            rhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal {
                    player: crate::types::ability::PlayerScope::Controller
                }
            },
        })
    ));
}

#[test]
fn static_keen_eyed_curator_condition_parses() {
    use crate::types::ability::{Comparator, QuantityExpr, QuantityRef};

    let def = parse_static_line(
            "As long as there are four or more card types among cards exiled with this creature, it gets +4/+4 and has trample.",
        )
        .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: 4 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddToughness { value: 4 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Trample,
        }));
    assert!(matches!(
        def.condition,
        Some(StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::DistinctCardTypes {
                    source: CardTypeSetSource::ExiledBySource,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 4 },
        })
    ));
}

#[test]
fn static_exactly_one_creature_binds_that_creature_to_controlled_creature() {
    let def = parse_static_line(
            "As long as you control exactly one creature, that creature gets +2/+0 and has deathtouch and lifelink.",
        )
        .unwrap();

    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(matches!(
        def.affected,
        Some(TargetFilter::Typed(ref filter))
            if filter.type_filters.iter().any(|type_filter| type_filter == &TypeFilter::Creature)
                && filter.controller == Some(ControllerRef::You)
    ));
    assert!(def
        .modifications
        .iter()
        .any(|modification| modification == &ContinuousModification::AddPower { value: 2 }));
    assert!(def.modifications.iter().any(|modification| modification
        == &ContinuousModification::AddKeyword {
            keyword: Keyword::Deathtouch,
        }));
    assert!(matches!(
        def.condition,
        Some(StaticCondition::QuantityComparison {
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: 1 },
            ..
        })
    ));
}

#[test]
fn static_exactly_one_qualified_creature_reuses_condition_filter() {
    let def = parse_static_line(
        "As long as you control exactly one creature with flying, that creature gets +2/+0.",
    )
    .unwrap();

    let condition_filter = match &def.condition {
        Some(StaticCondition::QuantityComparison {
            lhs:
                QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount { filter },
                },
            ..
        }) => filter,
        other => panic!("expected object-count condition, got {other:?}"),
    };

    assert_eq!(def.affected.as_ref(), Some(condition_filter));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: 2 }));
}

#[test]
fn static_self_and_land_creatures_you_control_share_pump() {
    let def = parse_static_line(
            "As long as you control six or more lands, this creature and land creatures you control get +2/+2.",
        )
        .unwrap();

    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(matches!(
        def.affected,
        Some(TargetFilter::Or { ref filters })
            if filters.iter().any(|filter| filter == &TargetFilter::SelfRef)
                && filters.iter().any(|filter| matches!(
                    filter,
                    TargetFilter::Typed(typed)
                        if typed.type_filters.iter().any(|type_filter| type_filter == &TypeFilter::Creature)
                            && typed.type_filters.iter().any(|type_filter| type_filter == &TypeFilter::Land)
                            && typed.controller == Some(ControllerRef::You)
                ))
    ));
    assert!(def
        .modifications
        .iter()
        .any(|modification| modification == &ContinuousModification::AddPower { value: 2 }));
    assert!(def
        .modifications
        .iter()
        .any(|modification| modification == &ContinuousModification::AddToughness { value: 2 }));
    assert!(matches!(
        def.condition,
        Some(StaticCondition::QuantityComparison {
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 6 },
            ..
        })
    ));
}

#[test]
fn static_self_and_group_subject_delegates_group_filter() {
    let def = parse_static_line(
            "As long as you control six or more lands, this creature and Warriors you control get +2/+2.",
        )
        .unwrap();

    assert!(matches!(
        def.affected,
        Some(TargetFilter::Or { ref filters })
            if filters.contains(&TargetFilter::SelfRef)
                && filters.iter().any(|filter| matches!(
                    filter,
                    TargetFilter::Typed(typed)
                        if typed.type_filters.iter().any(|type_filter| matches!(
                            type_filter,
                            TypeFilter::Subtype(subtype) if subtype == "Warrior"
                        ))
                            && typed.controller == Some(ControllerRef::You)
                ))
    ));
}

#[test]
fn static_as_long_as_unrecognized_condition() {
    // Conditions the parser cannot yet decompose fall through to Unrecognized.
    // The whole "As long as X, Y" string is captured permissively so the effect still fires.
    let def = parse_static_line(
        "As long as you cast this spell from exile, enchanted creature gets +1/+1.",
    )
    .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(matches!(
        def.condition,
        Some(StaticCondition::Unrecognized { .. })
    ));
}

#[test]
fn static_has_keyword_as_long_as() {
    let def = parse_static_line("Tarmogoyf has trample as long as a land card is in a graveyard.")
        .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Trample,
        }));
    assert!(matches!(
        def.condition,
        Some(StaticCondition::Unrecognized { .. })
    ));
}

#[test]
fn static_erebos_god_of_the_dead_type_removal() {
    // CR 613.1d: Layer-4 type-removal with an attached devotion condition.
    // Inverted form — clause splitter rewrites to canonical form and the
    // "~ isn't a creature" branch now attaches the condition.
    let def = parse_static_line(
        "As long as your devotion to black is less than five, ~ isn't a creature.",
    )
    .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.modifications,
        vec![ContinuousModification::RemoveType {
            core_type: CoreType::Creature,
        }]
    );
    // The condition is "devotion < 5" which the existing static-condition
    // parser renders as Not{DevotionGE{Black, 5}}.
    assert!(def.condition.is_some(), "condition must be extracted");
    assert!(
        !matches!(def.condition, Some(StaticCondition::Unrecognized { .. })),
        "condition must be typed, not Unrecognized"
    );
}

#[test]
fn static_type_removal_with_nondevotion_condition() {
    // The Warring Triad: non-devotion condition path. We don't assert the
    // condition variant (may or may not type via parse_static_condition),
    // but modifications MUST be non-empty regardless.
    let def = parse_static_line(
        "As long as there are fewer than eight cards in your graveyard, ~ isn't a creature.",
    )
    .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.modifications,
        vec![ContinuousModification::RemoveType {
            core_type: CoreType::Creature,
        }]
    );
    assert!(def.condition.is_some(), "condition must be extracted");
}

#[test]
fn static_can_attack_despite_defender_self_unconditional() {
    // CR 702.3b: bare ~ subject, no condition.
    let def = parse_static_line("~ can attack as though it didn't have defender.").unwrap();
    assert_eq!(def.mode, StaticMode::CanAttackWithDefender);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert!(def.condition.is_none());
}

#[test]
fn static_can_attack_despite_defender_self_conditional() {
    // CR 702.3b + CR 611.3a: ~ subject + "as long as" condition
    // (Bristlepack Sentry pattern).
    let def = parse_static_line(
            "As long as you control a creature with power 4 or greater, ~ can attack as though it didn't have defender.",
        )
        .unwrap();
    assert_eq!(def.mode, StaticMode::CanAttackWithDefender);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert!(def.condition.is_some(), "condition must be attached");
    assert!(
        !matches!(def.condition, Some(StaticCondition::Unrecognized { .. })),
        "condition must be typed, not Unrecognized"
    );
}

#[test]
fn static_can_attack_despite_defender_creatures_you_control_they() {
    // CR 702.3b: plural subject + "they" pronoun (High Alert pattern).
    let def =
        parse_static_line("Creatures you control can attack as though they didn't have defender.")
            .unwrap();
    assert_eq!(def.mode, StaticMode::CanAttackWithDefender);
    let Some(TargetFilter::Typed(tf)) = def.affected else {
        panic!("expected typed affected filter, got {:?}", def.affected);
    };
    assert_eq!(tf.controller, Some(ControllerRef::You));
    assert!(
        tf.type_filters.contains(&TypeFilter::Creature),
        "expected Creature type filter, got {:?}",
        tf.type_filters
    );
    assert!(
        tf.get_subtype().is_none(),
        "generic creatures must not become a Creature subtype filter: {:?}",
        tf
    );
}

#[test]
fn static_can_attack_despite_defender_modified_creatures_they() {
    // CR 700.9 + CR 702.3b: "modified creatures you control" subject
    // (Guardians of Oboro). Previously misparsed as Subtype("Modified");
    // now correctly maps to FilterProp::Modified.
    let def = parse_static_line(
        "Modified creatures you control can attack as though they didn't have defender.",
    )
    .unwrap();
    assert_eq!(def.mode, StaticMode::CanAttackWithDefender);
    match def.affected {
        Some(TargetFilter::Typed(ref tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(
                tf.properties.contains(&FilterProp::Modified),
                "expected FilterProp::Modified in {:?}",
                tf.properties
            );
            assert!(
                !tf.type_filters.iter().any(|t| matches!(
                    t,
                    TypeFilter::Subtype(s) if s.eq_ignore_ascii_case("modified")
                )),
                "must not emit Subtype(\"Modified\") (CR 205.3m — not a subtype)"
            );
        }
        _ => panic!("expected TargetFilter::Typed"),
    }
}

#[test]
fn static_can_attack_despite_defender_enchanted_creature() {
    // Enchanted-creature subject (Animate Wall pattern) — routed through
    // parse_enchanted_equipped_predicate which now accepts both pronouns.
    let def = parse_static_line("Enchanted creature can attack as though it didn't have defender.")
        .unwrap();
    assert_eq!(def.mode, StaticMode::CanAttackWithDefender);
}

#[test]
fn static_activate_abilities_as_though_haste_tyvar() {
    // CR 602.5a: Tyvar, Jubilant Brawler's exact Oracle text — plural form.
    let def = parse_static_line(
        "You may activate abilities of creatures you control as though those creatures had haste.",
    )
    .expect("Tyvar static must parse to a typed static");
    assert_eq!(def.mode, StaticMode::CanActivateAbilitiesAsThoughHaste);
    match def.affected {
        Some(TargetFilter::Typed(ref tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
        }
        other => panic!("expected Typed(creatures you control), got {other:?}"),
    }
}

#[test]
fn static_activate_abilities_as_though_haste_singular() {
    // CR 602.5a: singular "that creature had haste" form must also match.
    let def = parse_static_line(
        "You may activate abilities of artifacts you control as though that creature had haste.",
    )
    .expect("singular-form static must parse");
    assert_eq!(def.mode, StaticMode::CanActivateAbilitiesAsThoughHaste);
}

#[test]
fn static_activate_abilities_as_though_haste_no_you_may() {
    // The leading "you may " is optional — bare phrasing still matches.
    let def = parse_static_line(
        "Activate abilities of creatures you control as though those creatures had haste.",
    )
    .expect("bare-phrasing static must parse");
    assert_eq!(def.mode, StaticMode::CanActivateAbilitiesAsThoughHaste);
}

#[test]
fn static_activate_abilities_as_though_haste_negative_attack_form() {
    // CR 702.3b vs CR 602.5a: the combat "can attack as though it had haste"
    // form must NOT match the activation-haste branch.
    let def = parse_static_line("Enchanted creature can attack as though it had haste.").unwrap();
    assert_ne!(def.mode, StaticMode::CanActivateAbilitiesAsThoughHaste);
}

#[test]
fn static_life_more_than_starting_conditional() {
    let def = parse_static_line(
            "As long as you have at least 7 life more than your starting life total, creatures you control get +2/+2.",
        )
        .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(matches!(
        def.affected,
        Some(TargetFilter::Typed(ref tf))
            if tf.type_filters.contains(&TypeFilter::Creature)
                && tf.controller == Some(ControllerRef::You)
    ));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: 2 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddToughness { value: 2 }));
    assert_eq!(
        def.condition,
        Some(StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeAboveStarting
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 7 },
        })
    );
}

#[test]
fn static_devotion_condition() {
    use crate::types::mana::ManaColor;
    // CR 110.4b: "less than five" → Not(DevotionGE { threshold: 5 })
    let def = parse_static_line(
        "As long as your devotion to black is less than five, Erebos isn't a creature.",
    )
    .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.condition,
        Some(StaticCondition::Not {
            condition: Box::new(StaticCondition::DevotionGE {
                colors: vec![ManaColor::Black],
                threshold: 5,
            }),
        })
    );
}

#[test]
fn static_devotion_multicolor_condition() {
    use crate::types::mana::ManaColor;
    // CR 110.4b: "less than seven" → Not(DevotionGE { threshold: 7 })
    let def = parse_static_line(
        "As long as your devotion to white and black is less than seven, Athreos isn't a creature.",
    )
    .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.condition,
        Some(StaticCondition::Not {
            condition: Box::new(StaticCondition::DevotionGE {
                colors: vec![ManaColor::White, ManaColor::Black],
                threshold: 7,
            }),
        })
    );
}

#[test]
fn static_during_your_turn_condition() {
    let def =
        parse_static_line("As long as it's your turn, Triumphant Adventurer has first strike.")
            .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(def.condition, Some(StaticCondition::DuringYourTurn));
}

#[test]
fn static_control_presence_condition() {
    let def =
        parse_static_line("As long as you control a artifact, Toolcraft Exemplar gets +2/+1.")
            .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(matches!(
        def.condition,
        Some(StaticCondition::IsPresent { filter: Some(_) })
    ));
}

#[test]
fn static_control_creature_with_power_ge() {
    // "creature with power 4 or greater" — digit form
    let def = parse_static_line(
            "As long as you control a creature with power 4 or greater, Inspiring Commander gets +1/+1.",
        )
        .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(matches!(
        def.condition,
        Some(StaticCondition::IsPresent {
            filter: Some(TargetFilter::Typed(_))
        })
    ));
    // Modifications should include PT buff
    assert!(def
        .modifications
        .iter()
        .any(|m| matches!(m, ContinuousModification::AddPower { value: 1 })));
}

#[test]
fn static_control_creature_with_power_ge_word() {
    // "creature with power four or greater" — English word form via parse_number
    let def = parse_static_line(
        "As long as you control a creature with power four or greater, Target gets +2/+0.",
    )
    .unwrap();
    assert!(matches!(
        def.condition,
        Some(StaticCondition::IsPresent {
            filter: Some(TargetFilter::Typed(_))
        })
    ));
}

#[test]
fn static_control_creature_with_power_le() {
    // "creature with power 2 or less"
    let def = parse_static_line(
        "As long as you control a creature with power 2 or less, Target gets -1/-0.",
    )
    .unwrap();
    assert!(matches!(
        def.condition,
        Some(StaticCondition::IsPresent {
            filter: Some(TargetFilter::Typed(_))
        })
    ));
}

#[test]
fn static_lands_you_control_have() {
    let def = parse_static_line("Lands you control have 'Forests'.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddSubtype {
            subtype: "Forests".to_string(),
        }));
}

#[test]
fn static_cant_be_the_target() {
    // CR 702.11a: "can't be the target of spells or abilities your opponents
    // control" (Sphinx of the Final Word) IS Hexproof — the permanent's controller
    // can still target it — so it is modeled as a Hexproof keyword grant (which the
    // targeting check already enforces with the correct controller scope) rather
    // than a scope-less blanket static.
    let def = parse_static_line(
            "Sphinx of the Final Word can't be the target of spells or abilities your opponents control.",
        )
        .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Hexproof,
        }));
}

#[test]
fn static_cant_be_targeted_blanket_is_shroud_static() {
    // CR 702.18a: the unqualified "can't be the target of spells or abilities"
    // (no controller qualifier) is blanket Shroud — untargetable by any player,
    // including the controller — modeled as the CantBeTargeted static and enforced
    // in `targeting.rs::can_target`.
    let def =
        parse_static_line("Guardian Idol can't be the target of spells or abilities.").unwrap();
    assert_eq!(def.mode, StaticMode::CantBeTargeted);
}

#[test]
fn static_cant_be_sacrificed() {
    // CR 701.21: Self-referential sacrifice prohibition emits the canonical
    // `StaticMode::Other("CantBeSacrificed")` so the runtime guard in
    // `game::sacrifice` (`object_has_static_other(id, "CantBeSacrificed")`)
    // can observe it.
    let def = parse_static_line("Sigarda, Host of Herons can't be sacrificed.").unwrap();
    assert_eq!(def.mode, StaticMode::Other("CantBeSacrificed".to_string()));
    assert!(def.description.is_some());
}

#[test]
fn map_keyword_uses_fromstr() {
    // Test that map_keyword handles all standard keywords via FromStr
    assert_eq!(map_keyword("flying"), Some(Keyword::Flying));
    assert_eq!(map_keyword("first strike"), Some(Keyword::FirstStrike));
    assert_eq!(map_keyword("double strike"), Some(Keyword::DoubleStrike));
    assert_eq!(map_keyword("trample"), Some(Keyword::Trample));
    assert_eq!(map_keyword("deathtouch"), Some(Keyword::Deathtouch));
    assert_eq!(map_keyword("lifelink"), Some(Keyword::Lifelink));
    assert_eq!(map_keyword("vigilance"), Some(Keyword::Vigilance));
    assert_eq!(map_keyword("haste"), Some(Keyword::Haste));
    assert_eq!(map_keyword("reach"), Some(Keyword::Reach));
    assert_eq!(map_keyword("menace"), Some(Keyword::Menace));
    assert_eq!(map_keyword("hexproof"), Some(Keyword::Hexproof));
    assert_eq!(map_keyword("indestructible"), Some(Keyword::Indestructible));
    assert_eq!(map_keyword("defender"), Some(Keyword::Defender));
    assert_eq!(map_keyword("shroud"), Some(Keyword::Shroud));
    assert_eq!(map_keyword("flash"), Some(Keyword::Flash));
    assert_eq!(map_keyword("prowess"), Some(Keyword::Prowess));
    assert_eq!(map_keyword("fear"), Some(Keyword::Fear));
    assert_eq!(map_keyword("intimidate"), Some(Keyword::Intimidate));
    assert_eq!(map_keyword("wither"), Some(Keyword::Wither));
    assert_eq!(map_keyword("infect"), Some(Keyword::Infect));
    assert_eq!(
        map_keyword("firebending 5"),
        Some(Keyword::Firebending(QuantityExpr::Fixed { value: 5 }))
    );
    // Unknown returns None
    assert_eq!(map_keyword("notakeyword"), None);
}

#[test]
fn static_multiple_keywords() {
    let def = parse_static_line("Enchanted creature has flying, trample, and haste.").unwrap();
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Flying,
        }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Trample,
        }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Haste,
        }));
}

#[test]
fn static_legendary_gets_and_has_compound() {
    let def = parse_static_line(
        "Enchanted creature is legendary, gets +1/+1, and has flying, vigilance, and lifelink.",
    )
    .unwrap();
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddSupertype {
            supertype: Supertype::Legendary,
        }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: 1 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddToughness { value: 1 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Flying,
        }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Vigilance,
        }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Lifelink,
        }));
}

#[test]
fn static_self_gets_pt() {
    let def = parse_static_line("Tarmogoyf gets +1/+2.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: 1 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddToughness { value: 2 }));
}

#[test]
fn static_have_keyword() {
    let def = parse_static_line("Creatures you control have vigilance.").unwrap();
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Vigilance,
        }));
}

#[test]
fn during_your_turn_has_lifelink() {
    let def = parse_static_line("During your turn, this creature has lifelink.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert_eq!(def.condition, Some(StaticCondition::DuringYourTurn));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Lifelink,
        }));
}

#[test]
fn suffix_during_your_turn_has_first_strike() {
    // Razorkin Needlehead: "This creature has first strike during your turn."
    let def = parse_static_line("This creature has first strike during your turn.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert_eq!(def.condition, Some(StaticCondition::DuringYourTurn));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::FirstStrike,
        }));
}

#[test]
fn suffix_during_turns_other_than_yours() {
    let def =
        parse_static_line("This creature has hexproof during turns other than yours.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert_eq!(
        def.condition,
        Some(StaticCondition::Not {
            condition: Box::new(StaticCondition::DuringYourTurn),
        })
    );
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Hexproof,
        }));
}

#[test]
fn this_land_is_the_chosen_type() {
    let def = parse_static_line("This land is the chosen type.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert_eq!(
        def.modifications,
        vec![ContinuousModification::AddChosenSubtype {
            kind: ChosenSubtypeKind::BasicLandType,
        }]
    );
}

#[test]
fn enchanted_land_is_the_chosen_type() {
    // CR 305.7 + CR 305.6: Phantasmal Terrain / Convincing Mirage. The Aura's
    // continuous static must set the enchanted land's subtype to the chosen
    // basic land type with replacement semantics (SetChosenBasicLandType), not
    // additive (AddChosenSubtype) — that variant clears the land's old types and
    // rules-text abilities, which AddChosenSubtype does not.
    let def = parse_static_line("Enchanted land is the chosen type.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::land().properties(vec![FilterProp::EnchantedBy])
        ))
    );
    assert_eq!(
        def.modifications,
        vec![ContinuousModification::SetChosenBasicLandType]
    );

    // Tail variants ("and loses its other land types"/"types") parse identically
    // — CR 305.7 already removes the old land types, so the tail is cosmetic.
    for line in [
        "Enchanted land is the chosen type",
        "Enchanted land is the chosen type and loses its other land types.",
        "Enchanted land is the chosen type and loses its other types.",
    ] {
        let def = parse_static_line(line).unwrap();
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::SetChosenBasicLandType],
            "line did not parse to SetChosenBasicLandType: {line}"
        );
    }
}

#[test]
fn this_creature_is_the_chosen_type() {
    let def = parse_static_line("This creature is the chosen type in addition to its other types.")
        .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert_eq!(
        def.modifications,
        vec![ContinuousModification::AddChosenSubtype {
            kind: ChosenSubtypeKind::CreatureType,
        }]
    );
}

#[test]
fn issue_1593_abomination_of_llanowar_cda_sums_battlefield_and_graveyard() {
    // Issue #1593 — Abomination of Llanowar:
    // "~'s power and toughness are each equal to the number of Elves you
    //  control plus the number of Elf cards in your graveyard."
    // CR 604.3: the CDA must parse to a SUM of two cross-zone object counts
    // — battlefield Elves you control + Elf cards in your graveyard — not
    // fall through to an Unimplemented static.
    let def = parse_static_line(
            "Abomination of Llanowar's power and toughness are each equal to the number of Elves you control plus the number of Elf cards in your graveyard.",
        )
        .expect("CDA must parse, not fall through to Unimplemented");
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert!(def.characteristic_defining, "must be a CDA");
    assert_eq!(def.modifications.len(), 2, "power + toughness");

    // Both P/T resolve to the same summed quantity. Assert the structure of
    // each: Sum[ ObjectCount{Elf, controller: You}, ZoneCardCount{Graveyard,
    // [Elf], Controller} ].
    let assert_sum = |value: &QuantityExpr| {
        let QuantityExpr::Sum { exprs } = value else {
            panic!("expected Sum, got {value:?}");
        };
        assert_eq!(exprs.len(), 2, "two summed operands");

        // Operand 1: battlefield Elves you control.
        let QuantityExpr::Ref {
            qty:
                QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(tf),
                },
        } = &exprs[0]
        else {
            panic!("operand 0 must be ObjectCount, got {:?}", exprs[0]);
        };
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert!(
            tf.type_filters
                .iter()
                .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "Elf")),
            "operand 0 must filter Elf, got {:?}",
            tf.type_filters
        );

        // Operand 2: Elf cards in your graveyard.
        let QuantityExpr::Ref {
            qty:
                QuantityRef::ZoneCardCount {
                    zone: ZoneRef::Graveyard,
                    card_types,
                    scope: CountScope::Controller,
                    filter: None,
                },
        } = &exprs[1]
        else {
            panic!(
                "operand 1 must be a Graveyard ZoneCardCount, got {:?}",
                exprs[1]
            );
        };
        assert!(
            card_types
                .iter()
                .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "Elf")),
            "operand 1 must filter Elf cards, got {card_types:?}"
        );
    };

    for m in &def.modifications {
        match m {
            ContinuousModification::SetDynamicPower { value }
            | ContinuousModification::SetDynamicToughness { value } => assert_sum(value),
            other => panic!("unexpected modification {other:?}"),
        }
    }
}

#[test]
fn static_tarmogoyf_cda() {
    let def = parse_static_line(
            "Tarmogoyf's power is equal to the number of card types among cards in all graveyards and its toughness is equal to that number plus 1.",
        )
        .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert!(def.characteristic_defining);
    assert!(def
        .modifications
        .contains(&ContinuousModification::SetDynamicPower {
            value: QuantityExpr::Ref {
                qty: QuantityRef::DistinctCardTypes {
                    source: CardTypeSetSource::Zone {
                        zone: ZoneRef::Graveyard,
                        scope: CountScope::All,
                    },
                },
            },
        }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::SetDynamicToughness {
            value: QuantityExpr::Offset {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::DistinctCardTypes {
                        source: CardTypeSetSource::Zone {
                            zone: ZoneRef::Graveyard,
                            scope: CountScope::All,
                        },
                    },
                }),
                offset: 1,
            },
        }));
}

#[test]
fn attacks_each_combat_unless_you_control_is_conditional_must_attack() {
    // CR 508.1d + CR 604.1: Reckless Cohort — "attacks each combat if able
    // unless you control another Ally" is a MustAttack rule-static gated by a
    // negated control-presence condition (the requirement applies only while you
    // do NOT control another Ally).
    let def = parse_static_line(
        "This creature attacks each combat if able unless you control another Ally.",
    )
    .unwrap();
    assert_eq!(def.mode, StaticMode::MustAttack);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert!(
        matches!(def.condition, Some(StaticCondition::Not { .. })),
        "expected a negated control-presence condition, got {:?}",
        def.condition
    );
}

#[test]
fn attacks_each_combat_if_able_unconditional_has_no_condition() {
    // Regression: the plain rule-static (no "unless") still parses to an
    // unconditional MustAttack.
    let def = parse_static_line("This creature attacks each combat if able.").unwrap();
    assert_eq!(def.mode, StaticMode::MustAttack);
    assert!(def.condition.is_none());
}

#[test]
fn static_unlicensed_hearse_counts_cards_exiled_with_it() {
    let def = parse_static_line(
            "Unlicensed Hearse's power and toughness are each equal to the number of cards exiled with it.",
        )
        .unwrap();

    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert!(def.characteristic_defining);
    assert_eq!(
        def.modifications,
        vec![
            ContinuousModification::SetDynamicPower {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::CardsExiledBySource,
                },
            },
            ContinuousModification::SetDynamicToughness {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::CardsExiledBySource,
                },
            },
        ]
    );
}

#[test]
fn static_crackling_drake_counts_owned_instant_sorcery_exile_and_graveyard() {
    let def = parse_static_line(
            "Crackling Drake's power is equal to the total number of instant and sorcery cards you own in exile and in your graveyard.",
        )
        .unwrap();

    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert!(def.characteristic_defining);
    let expected = QuantityExpr::Sum {
        exprs: vec![
            QuantityExpr::Ref {
                qty: QuantityRef::ZoneCardCount {
                    zone: ZoneRef::Exile,
                    card_types: vec![TypeFilter::Instant, TypeFilter::Sorcery],
                    scope: CountScope::Owner,
                    filter: None,
                },
            },
            QuantityExpr::Ref {
                qty: QuantityRef::ZoneCardCount {
                    zone: ZoneRef::Graveyard,
                    card_types: vec![TypeFilter::Instant, TypeFilter::Sorcery],
                    scope: CountScope::Owner,
                    filter: None,
                },
            },
        ],
    };
    assert_eq!(
        def.modifications,
        vec![ContinuousModification::SetDynamicPower { value: expected }]
    );
}

#[test]
fn static_multani_cda_total_cards_in_all_players_hands() {
    let qty = QuantityExpr::Ref {
        qty: QuantityRef::HandSize {
            player: PlayerScope::AllPlayers {
                aggregate: AggregateFunction::Sum,
                exclude: None,
            },
        },
    };
    let def = parse_static_line(
            "Multani, Maro-Sorcerer's power and toughness are each equal to the total number of cards in all players' hands.",
        )
        .unwrap();

    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert!(def.characteristic_defining);
    assert_eq!(
        def.modifications,
        vec![
            ContinuousModification::SetDynamicPower { value: qty.clone() },
            ContinuousModification::SetDynamicToughness { value: qty },
        ]
    );
}

#[test]
fn static_enchanted_creature_doesnt_untap() {
    let def =
        parse_static_line("Enchanted creature doesn't untap during its controller's untap step.")
            .unwrap();
    assert_eq!(def.mode, StaticMode::CantUntap);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
        ))
    );
}

#[test]
fn static_creatures_with_counters_dont_untap() {
    let def = parse_static_line(
        "Creatures with ice counters on them don't untap during their controllers' untap steps.",
    )
    .unwrap();
    assert_eq!(def.mode, StaticMode::CantUntap);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(TypedFilter::creature().properties(
            vec![FilterProp::Counters {
                counters: CounterMatch::OfType(crate::types::counter::CounterType::Generic(
                    "ice".to_string()
                )),
                comparator: Comparator::GE,
                count: QuantityExpr::Fixed { value: 1 },
            },]
        )))
    );
}

#[test]
fn static_this_creature_attacks_each_combat_if_able() {
    let def = parse_static_line("This creature attacks each combat if able.").unwrap();
    assert_eq!(def.mode, StaticMode::MustAttack);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
}

#[test]
fn static_enchanted_creature_attacks_each_combat_if_able() {
    let def = parse_static_line("Enchanted creature attacks each combat if able.").unwrap();
    assert_eq!(def.mode, StaticMode::MustAttack);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
        ))
    );
}

#[test]
fn static_keyword_grant_and_attack_if_able_emits_both_defs() {
    let defs =
        parse_static_line_multi("All creatures have double strike and attack each combat if able.");
    assert_eq!(defs.len(), 2);
    assert_eq!(defs[0].mode, StaticMode::Continuous);
    assert!(defs[0]
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::DoubleStrike,
        }));
    assert_eq!(defs[1].mode, StaticMode::MustAttack);
    assert_eq!(defs[1].affected, defs[0].affected);
}

#[test]
fn static_keyword_grant_and_attack_or_block_if_able_emits_three_defs() {
    let defs = parse_static_line_multi(
        "All creatures have vigilance and attack or block each combat if able.",
    );
    assert_eq!(defs.len(), 3);
    assert_eq!(defs[0].mode, StaticMode::Continuous);
    assert!(defs[0]
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Vigilance,
        }));
    assert_eq!(defs[1].mode, StaticMode::MustAttack);
    assert_eq!(defs[2].mode, StaticMode::MustBlock);
    assert_eq!(defs[1].affected, defs[0].affected);
    assert_eq!(defs[2].affected, defs[0].affected);
}

#[test]
fn static_comma_keyword_grant_and_attack_if_able_emits_both_defs() {
    let defs = parse_static_line_multi(
        "Creatures you control have double strike, trample, and must attack if able.",
    );
    assert_eq!(defs.len(), 2);
    assert_eq!(defs[0].mode, StaticMode::Continuous);
    assert!(defs[0]
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::DoubleStrike,
        }));
    assert!(defs[0]
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Trample,
        }));
    assert_eq!(defs[1].mode, StaticMode::MustAttack);
    assert_eq!(defs[1].affected, defs[0].affected);
}

#[test]
fn static_comma_rule_statics_share_subject() {
    let defs = parse_static_line_multi(
            "This creature attacks each combat if able, can't be sacrificed, and can't attack its owner.",
        );
    assert_eq!(defs.len(), 3);
    assert_eq!(defs[0].mode, StaticMode::MustAttack);
    assert_eq!(
        defs[1].mode,
        StaticMode::Other("CantBeSacrificed".to_string())
    );
    assert_eq!(defs[2].mode, StaticMode::CantAttack);
    assert_eq!(
        defs[2].attack_defended,
        Some(crate::types::triggers::AttackTargetFilter::Owner)
    );
    assert!(defs
        .iter()
        .all(|def| def.affected == Some(TargetFilter::SelfRef)));
}

#[test]
fn static_pump_and_must_be_blocked_if_able_emits_both_defs() {
    let defs =
        parse_static_line_multi("Enchanted creature gets +3/+3 and must be blocked if able.");
    assert_eq!(defs.len(), 2);
    assert_eq!(defs[0].mode, StaticMode::Continuous);
    assert!(defs[0]
        .modifications
        .contains(&ContinuousModification::AddPower { value: 3 }));
    assert!(defs[0]
        .modifications
        .contains(&ContinuousModification::AddToughness { value: 3 }));
    assert_eq!(defs[1].mode, StaticMode::MustBeBlocked);
    assert_eq!(defs[1].affected, defs[0].affected);
}

#[test]
fn parse_continuous_modifications_are_goaded_emits_goaded_static_mode() {
    let mods = parse_continuous_modifications("are goaded for the rest of the game");
    assert!(mods.iter().any(|m| matches!(
        m,
        ContinuousModification::AddStaticMode {
            mode: StaticMode::Goaded
        }
    )));
}

#[test]
fn static_pump_must_be_blocked_and_goaded_emits_all_defs() {
    let defs = parse_static_line_multi(
        "Enchanted creature gets +3/+3, must be blocked if able, and is goaded.",
    );
    assert_eq!(defs.len(), 3);
    assert_eq!(defs[0].mode, StaticMode::Continuous);
    assert!(defs[0]
        .modifications
        .contains(&ContinuousModification::AddPower { value: 3 }));
    assert!(defs[0]
        .modifications
        .contains(&ContinuousModification::AddToughness { value: 3 }));
    assert_eq!(defs[1].mode, StaticMode::MustBeBlocked);
    assert_eq!(defs[2].mode, StaticMode::Goaded);
    assert_eq!(defs[1].affected, defs[0].affected);
    assert_eq!(defs[2].affected, defs[0].affected);
}

#[test]
fn static_pump_and_goaded_emits_both_defs() {
    let defs = parse_static_line_multi("Enchanted creature gets +2/+2 and is goaded.");
    assert_eq!(defs.len(), 2);
    assert_eq!(defs[0].mode, StaticMode::Continuous);
    assert!(defs[0]
        .modifications
        .contains(&ContinuousModification::AddPower { value: 2 }));
    assert!(defs[0]
        .modifications
        .contains(&ContinuousModification::AddToughness { value: 2 }));
    assert_eq!(defs[1].mode, StaticMode::Goaded);
    assert_eq!(defs[1].affected, defs[0].affected);
}

#[test]
fn static_this_creature_can_block_only_creatures_with_flying() {
    use crate::types::statics::block_only_creatures_with_flying_filter;

    let def = parse_static_line("This creature can block only creatures with flying.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::BlockRestriction {
            filter: block_only_creatures_with_flying_filter(),
        }
    );
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
}

#[test]
fn static_this_token_can_block_only_creatures_with_flying() {
    use crate::types::statics::block_only_creatures_with_flying_filter;

    let def = parse_static_line("This token can block only creatures with flying.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::BlockRestriction {
            filter: block_only_creatures_with_flying_filter(),
        }
    );
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
}

#[test]
fn static_you_have_shroud() {
    let def = parse_static_line("You have shroud.").unwrap();
    assert_eq!(def.mode, StaticMode::Shroud);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You),
        ))
    );
}

/// CR 702.11: "You have hexproof." (Crystal Barricade) must produce a
/// player-scope `StaticMode::Hexproof`, not a bogus
/// `ContinuousModification::AddKeyword(Hexproof)` on an empty-typed
/// controller-scoped filter (which would wrongly grant hexproof to every
/// permanent you control instead of to the player).
#[test]
fn static_you_have_hexproof() {
    let def = parse_static_line("You have hexproof.").unwrap();
    assert_eq!(def.mode, StaticMode::Hexproof);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You),
        ))
    );
}

#[test]
fn static_you_have_no_maximum_hand_size() {
    let def = parse_static_line("You have no maximum hand size.").unwrap();
    assert_eq!(def.mode, StaticMode::NoMaximumHandSize);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You),
        ))
    );
}

#[test]
fn static_each_player_may_play_an_additional_land() {
    let def = parse_static_line("Each player may play an additional land on each of their turns.")
        .unwrap();
    assert_eq!(def.mode, StaticMode::MayPlayAdditionalLand);
    assert_eq!(def.affected, Some(TargetFilter::Player));
}

#[test]
fn static_you_may_choose_not_to_untap_self() {
    let def =
        parse_static_line("You may choose not to untap this creature during your untap step.")
            .unwrap();
    assert_eq!(def.mode, StaticMode::MayChooseNotToUntap);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
}

#[test]
fn static_you_may_look_at_top_card_of_library() {
    let def = parse_static_line("You may look at the top card of your library any time.").unwrap();
    assert_eq!(def.mode, StaticMode::MayLookAtTopOfLibrary);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You),
        ))
    );
}

#[test]
fn static_same_turn_loyalty_abilities_activate_as_instant() {
    let def = parse_static_line(
            "As long as ~ entered this turn, you may activate her loyalty abilities any time you could cast an instant.",
        )
        .unwrap();
    assert_eq!(
        def.mode,
        StaticMode::ActivateAsInstant {
            cost_category: CostCategory::PaysLoyalty,
        }
    );
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert_eq!(def.condition, Some(StaticCondition::SourceEnteredThisTurn));
}

#[test]
fn static_cards_in_graveyards_lose_all_abilities() {
    let def = parse_static_line("Cards in graveyards lose all abilities.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(TypedFilter::card().properties(vec![
            FilterProp::InZone {
                zone: crate::types::zones::Zone::Graveyard,
            },
        ])))
    );
    assert_eq!(
        def.modifications,
        vec![ContinuousModification::RemoveAllAbilities]
    );
}

#[test]
fn static_black_creatures_get_plus_one_plus_one() {
    let def = parse_static_line("Black creatures get +1/+1.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(TypedFilter::creature().properties(
            vec![FilterProp::HasColor {
                color: ManaColor::Black,
            }]
        )))
    );
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: 1 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddToughness { value: 1 }));
}

#[test]
fn static_creatures_you_control_with_mana_value_filter() {
    let def =
        parse_static_line("Creatures you control with mana value 3 or less get +1/+0.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::Cmc {
                    comparator: Comparator::LE,
                    value: QuantityExpr::Fixed { value: 3 },
                }]),
        ))
    );
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: 1 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddToughness { value: 0 }));
}

#[test]
fn static_creatures_you_control_with_flying_filter() {
    let def = parse_static_line("Creatures you control with flying get +1/+1.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::WithKeyword {
                    value: Keyword::Flying,
                }]),
        ))
    );
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: 1 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddToughness { value: 1 }));
}

#[test]
fn static_other_zombie_creatures_have_swampwalk() {
    let def = parse_static_line("Other Zombie creatures have swampwalk.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature()
                .subtype("Zombie".to_string())
                .properties(vec![FilterProp::Another]),
        ))
    );
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Landwalk("Swamp".to_string()),
        }));
}

#[test]
fn static_creature_tokens_you_control_lose_all_abilities_and_have_base_pt() {
    let def = parse_static_line(
        "Creature tokens you control lose all abilities and have base power and toughness 3/3.",
    )
    .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::Token]),
        ))
    );
    assert!(def
        .modifications
        .contains(&ContinuousModification::RemoveAllAbilities));
    assert!(def
        .modifications
        .contains(&ContinuousModification::SetPower { value: 3 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::SetToughness { value: 3 }));
}

#[test]
fn static_target_subject_can_set_base_power_without_toughness() {
    let modifications = parse_continuous_modifications("has base power 3 until end of turn");
    assert_eq!(
        modifications,
        vec![ContinuousModification::SetPower { value: 3 }]
    );
}

#[test]
fn static_enchanted_land_has_quoted_ability() {
    let def =
        parse_static_line("Enchanted land has \"{T}: Add two mana of any one color.\"").unwrap();
    // Should produce a GrantAbility with a typed activated AbilityDefinition
    let grant = def
        .modifications
        .iter()
        .find(|m| matches!(m, ContinuousModification::GrantAbility { .. }));
    assert!(
        grant.is_some(),
        "should contain a GrantAbility modification"
    );
    if let ContinuousModification::GrantAbility { definition } = grant.unwrap() {
        assert_eq!(definition.kind, AbilityKind::Activated);
        assert!(definition.cost.is_some());
    }
}

#[test]
fn quoted_activated_restriction_grants_ability_not_static_mode() {
    let def =
        parse_static_line("Enchanted land has \"{T}: Target creature can't block this turn.\"")
            .unwrap();

    assert!(
        !def.modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddStaticMode {
                mode: StaticMode::CantBlock
            }
        )),
        "quoted activated ability must not become a static CantBlock grant"
    );
    let grant = def
        .modifications
        .iter()
        .find(|m| matches!(m, ContinuousModification::GrantAbility { .. }))
        .expect("should grant the quoted activated ability");
    let ContinuousModification::GrantAbility { definition } = grant else {
        unreachable!();
    };
    assert_eq!(definition.kind, AbilityKind::Activated);
    assert!(definition.cost.is_some());
    assert_eq!(definition.duration, Some(Duration::UntilEndOfTurn));
    assert!(matches!(&*definition.effect, Effect::GenericEffect { .. }));
}

#[test]
fn quoted_ability_sacrifice_cost_separator() {
    // CR 118.12: "Sacrifice this token: Add {C}." should parse as an activated ability
    // with sacrifice cost and mana effect, not a spell-like sacrifice effect.
    let def = parse_quoted_ability("Sacrifice this token: Add {C}.");
    assert_eq!(def.kind, AbilityKind::Activated);
    assert!(def.cost.is_some(), "should have a cost");
    assert!(
        !matches!(
            *def.effect,
            crate::types::ability::Effect::Unimplemented { .. }
        ),
        "effect should not be Unimplemented, got {:?}",
        def.effect
    );
}

#[test]
fn quoted_ability_preserves_activation_restrictions() {
    // CR 602.5c: A granted activated ability carrying a trailing use
    // restriction inside its quoted text must surface that restriction on the
    // acquired ability — not drop it as an unparsed sentence. CR 602.5d:
    // "Activate only as a sorcery" → AsSorcery timing gate. This is the path
    // for Skygames ("Enchanted land has \"{T}: ... Activate only as a
    // sorcery.\""), Mindwhip Sliver, and Squirrel anthem cards.
    let def =
        parse_static_line("Enchanted land has \"{T}: Draw a card. Activate only as a sorcery.\"")
            .unwrap();
    let grant = def
        .modifications
        .iter()
        .find(|m| matches!(m, ContinuousModification::GrantAbility { .. }))
        .expect("should grant the quoted activated ability");
    let ContinuousModification::GrantAbility { definition } = grant else {
        unreachable!();
    };
    assert_eq!(definition.kind, AbilityKind::Activated);
    assert!(definition.cost.is_some(), "should retain the tap cost");
    assert!(
        definition.is_sorcery_speed(),
        "AsSorcery must mark the granted ability as sorcery-speed"
    );
    assert!(
        definition
            .activation_restrictions
            .contains(&ActivationRestriction::AsSorcery),
        "granted ability must carry AsSorcery, got {:?}",
        definition.activation_restrictions
    );
    assert!(
        !matches!(
            *definition.effect,
            crate::types::ability::Effect::Unimplemented { .. }
        ),
        "the draw effect must still parse, got {:?}",
        definition.effect
    );

    // CR 602.5b: Non-sorcery restrictions use the same single-authority
    // extractor and must surface on the granted copy too.
    let once = parse_static_line(
        "Creatures you control have \"{T}: Draw a card. Activate only during your turn and only once each turn.\"",
    )
    .unwrap();
    let grant = once
        .modifications
        .iter()
        .find(|m| matches!(m, ContinuousModification::GrantAbility { .. }))
        .expect("should grant the quoted activated ability");
    let ContinuousModification::GrantAbility { definition } = grant else {
        unreachable!();
    };
    assert!(
        definition
            .activation_restrictions
            .contains(&ActivationRestriction::DuringYourTurn),
        "granted ability must carry DuringYourTurn, got {:?}",
        definition.activation_restrictions
    );
    assert!(
        definition
            .activation_restrictions
            .contains(&ActivationRestriction::OnlyOnceEachTurn),
        "granted ability must carry OnlyOnceEachTurn, got {:?}",
        definition.activation_restrictions
    );
}

#[test]
fn quoted_self_rule_static_grants_static_mode() {
    let modifications = parse_quoted_ability_modifications(
        "It gains \"This creature attacks each combat if able.\"",
    );
    assert_eq!(
        modifications,
        vec![ContinuousModification::AddStaticMode {
            mode: StaticMode::MustAttack,
        }]
    );
}

/// CR 113.3d + CR 604.1: A quoted continuous static whose inner scope is
/// not `SelfRef` (e.g. Dancer's Chakrams' "Other commanders you control
/// get +2/+2 and have lifelink") must emit `GrantStaticAbility` carrying
/// the inner `StaticDefinition` verbatim — NOT a fallback `GrantAbility`
/// wrapping a `Pump` effect, and NOT an `AddStaticMode` with a discarded
/// scope.
#[test]
fn quoted_non_selfref_static_grants_full_static_definition() {
    // Trailing comma mirrors how the host clause splits the quoted text.
    let modifications = parse_quoted_ability_modifications(
        "\"Other commanders you control get +2/+2 and have lifelink,\"",
    );
    assert_eq!(modifications.len(), 1, "expected one granted static");
    let definition = match &modifications[0] {
        ContinuousModification::GrantStaticAbility { definition } => definition.as_ref(),
        other => panic!("expected GrantStaticAbility, got {:?}", other),
    };
    assert_eq!(definition.mode, StaticMode::Continuous);
    // The recipient's controller, not SelfRef.
    match &definition.affected {
        Some(TargetFilter::Typed(t)) => {
            assert!(
                t.properties.contains(&FilterProp::IsCommander),
                "filter must require IsCommander"
            );
            assert!(
                t.properties.contains(&FilterProp::Another),
                "filter must exclude the recipient via Another"
            );
            assert_eq!(t.controller, Some(ControllerRef::You));
        }
        other => panic!("expected Typed filter, got {:?}", other),
    }
    // Inner modifications: +2/+2 and lifelink (no spurious Pump or Unimplemented).
    assert!(
        definition
            .modifications
            .contains(&ContinuousModification::AddPower { value: 2 }),
        "missing AddPower +2 in {:?}",
        definition.modifications,
    );
    assert!(
        definition
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 2 }),
        "missing AddToughness +2 in {:?}",
        definition.modifications,
    );
    assert!(
        definition
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Lifelink
            }),
        "missing AddKeyword(Lifelink) in {:?}",
        definition.modifications,
    );
}

#[test]
fn static_other_tapped_creatures_you_control_have_indestructible() {
    let def = parse_static_line("Other tapped creatures you control have indestructible.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::Tapped, FilterProp::Another]),
        ))
    );
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Indestructible,
        }));
}

#[test]
fn static_attacking_creatures_you_control_have_double_strike() {
    let def = parse_static_line("Attacking creatures you control have double strike.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::Attacking]),
        ))
    );
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::DoubleStrike,
        }));
}

#[test]
fn static_during_your_turn_creatures_you_control_have_hexproof() {
    let def = parse_static_line("During your turn, creatures you control have hexproof.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(def.condition, Some(StaticCondition::DuringYourTurn));
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You),
        ))
    );
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Hexproof,
        }));
}

#[test]
fn static_during_your_turn_equipped_creatures_you_control_have_double_strike() {
    let def = parse_static_line(
        "During your turn, equipped creatures you control have double strike and haste.",
    )
    .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(def.condition, Some(StaticCondition::DuringYourTurn));
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::HasAttachment {
                    kind: AttachmentKind::Equipment,
                    controller: None,
                    exclude_source: crate::types::ability::SourceExclusion::Include,
                }]),
        ))
    );
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::DoubleStrike,
        }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Haste,
        }));
}

#[test]
fn parse_compound_static_kaito_animation() {
    let text = "During your turn, as long as ~ has one or more loyalty counters on him, he's a 3/4 Ninja creature and has hexproof.";
    let def = parse_static_line(text).unwrap();

    // Verify compound condition
    assert!(matches!(
        def.condition,
        Some(StaticCondition::And { ref conditions })
        if conditions.len() == 2
    ));
    if let Some(StaticCondition::And { ref conditions }) = def.condition {
        assert!(matches!(conditions[0], StaticCondition::DuringYourTurn));
        assert!(matches!(
            conditions[1],
            StaticCondition::HasCounters {
                counters: CounterMatch::OfType(crate::types::counter::CounterType::Loyalty),
                minimum: 1,
                ..
            }
        ));
    }

    // Verify self-referencing
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));

    // Verify modifications
    assert!(def
        .modifications
        .contains(&ContinuousModification::SetPower { value: 3 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::SetToughness { value: 4 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddType {
            core_type: crate::types::card_type::CoreType::Creature,
        }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddSubtype {
            subtype: "Ninja".to_string(),
        }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Hexproof,
        }));
}

// ── New static routing tests (Steps 4-5) ─────────────────────────────

#[test]
fn static_must_be_blocked_if_able() {
    // CR 509.1b: "must be blocked if able"
    let def = parse_static_line("Darksteel Myr must be blocked if able.").unwrap();
    assert_eq!(def.mode, StaticMode::MustBeBlocked);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
}

#[test]
fn static_legend_rule_global_exemption() {
    // CR 704.5j: Mirror Gallery — "The legend rule doesn't apply." (global).
    let def = parse_static_line("The \"legend rule\" doesn't apply.").unwrap();
    assert_eq!(def.mode, StaticMode::LegendRuleDoesntApply);
    assert_eq!(def.affected, None);
}

#[test]
fn static_legend_rule_permanents_you_control() {
    // CR 704.5j: Sakashima of a Thousand Faces / Mirror Box — controller scope.
    let def =
        parse_static_line("The \"legend rule\" doesn't apply to permanents you control.").unwrap();
    assert_eq!(def.mode, StaticMode::LegendRuleDoesntApply);
    assert!(matches!(
        def.affected,
        Some(TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::You),
            ..
        }))
    ));
}

#[test]
fn static_legend_rule_subtype_scope() {
    // CR 704.5j: Sliver Gravemother — "doesn't apply to Slivers you control."
    let def =
        parse_static_line("The \"legend rule\" doesn't apply to Slivers you control.").unwrap();
    assert_eq!(def.mode, StaticMode::LegendRuleDoesntApply);
    match def.affected {
        Some(TargetFilter::Typed(ref typed)) => {
            assert_eq!(typed.controller, Some(ControllerRef::You));
            assert!(typed.type_filters.iter().any(|t| matches!(
                t,
                crate::types::ability::TypeFilter::Subtype(s) if s == "Sliver"
            )));
        }
        other => panic!("expected typed subtype filter, got {other:?}"),
    }
}

#[test]
fn static_legend_rule_routes_through_classifier() {
    // The classifier must route exemption lines to the static parser.
    assert!(crate::parser::oracle_classifier::is_static_pattern(
        "the \"legend rule\" doesn't apply to permanents you control."
    ));
    assert!(crate::parser::oracle_classifier::is_static_pattern(
        "the \"legend rule\" doesn't apply."
    ));
}

#[test]
fn static_legend_rule_creature_tokens_scope() {
    // CR 704.5j: The Master, Multiplied — "doesn't apply to creature tokens you control."
    let def =
        parse_static_line("The \"legend rule\" doesn't apply to creature tokens you control.")
            .unwrap();
    assert_eq!(def.mode, StaticMode::LegendRuleDoesntApply);
    assert!(matches!(
        def.affected,
        Some(TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::You),
            properties,
            ..
        })) if properties.contains(&FilterProp::Token)
    ));
}

#[test]
fn static_cant_cause_sacrifice_or_exile_creature_tokens() {
    // CR 603.2 + CR 609.3: The Master, Multiplied.
    let def = parse_static_line(
        "Triggered abilities you control can't cause you to sacrifice or exile creature tokens you control.",
    )
    .unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantCauseSacrificeOrExile {
            cause: ProhibitionScope::Controller,
        }
    );
    assert!(matches!(
        def.affected,
        Some(TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::You),
            properties,
            ..
        })) if properties.contains(&FilterProp::Token)
    ));
}

#[test]
fn static_legend_rule_defers_unparseable_scopes() {
    // CR 704.5j: scopes this parser cannot resolve precisely, and conditional
    // forms, must NOT be emitted as a LegendRuleDoesntApply static — they are
    // deferred (left Unimplemented), never misparsed into a no-op exemption.
    for text in [
            "The \"legend rule\" doesn't apply to tokens you control.", // Cadric
            "The \"legend rule\" doesn't apply to commanders you control.", // Try-My-Deck Elemental
            "If there are exactly two permanents named Brothers Yamazaki on the battlefield, the \"legend rule\" doesn't apply to them.",
        ] {
            assert!(
                !matches!(
                    parse_static_line(text),
                    Some(StaticDefinition {
                        mode: StaticMode::LegendRuleDoesntApply,
                        ..
                    })
                ),
                "scope must be deferred, not misparsed: {text}"
            );
        }
}

#[test]
fn static_opponents_cant_gain_life() {
    // CR 119.7: Lifegain prevention — opponent scope
    let def = parse_static_line("Your opponents can't gain life.").unwrap();
    assert_eq!(def.mode, StaticMode::CantGainLife);
    assert!(matches!(
        def.affected,
        Some(TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::Opponent),
            ..
        }))
    ));
}

#[test]
fn static_you_cant_gain_life() {
    // CR 119.7: Lifegain prevention — self scope
    let def = parse_static_line("You can't gain life.").unwrap();
    assert_eq!(def.mode, StaticMode::CantGainLife);
    assert!(matches!(
        def.affected,
        Some(TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::You),
            ..
        }))
    ));
}

#[test]
fn static_players_cant_gain_life() {
    // CR 119.7: Lifegain prevention — all players
    let def = parse_static_line("Players can't gain life.").unwrap();
    assert_eq!(def.mode, StaticMode::CantGainLife);
    // No controller restriction — affects all
    assert!(matches!(
        def.affected,
        Some(TargetFilter::Typed(TypedFilter {
            controller: None,
            ..
        }))
    ));
}

#[test]
fn static_cast_as_though_flash() {
    // CR 601.3b + CR 702.8a: "You may cast [type] spells as though they had
    // flash" must emit a `CastWithKeyword { Flash }` static — the only mode the
    // flash-timing path (granted_spell_keywords) reads — with the spell-type
    // filter preserved. Issue #1957: Vivien, Champion of the Wilds restricts the
    // grant to CREATURE spells, and the dead `CastWithFlash` mode dropped both
    // the timing grant and the type restriction.
    let def = parse_static_line("You may cast creature spells as though they had flash.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CastWithKeyword {
            keyword: Keyword::Flash
        }
    );
    let Some(TargetFilter::Typed(tf)) = &def.affected else {
        panic!(
            "affected must be a Typed creature filter, got {:?}",
            def.affected
        );
    };
    assert!(
        tf.type_filters.contains(&TypeFilter::Creature),
        "filter must constrain to creature spells, got {:?}",
        tf.type_filters
    );
    assert_eq!(
        tf.controller,
        Some(ControllerRef::You),
        "grant must scope to spells you cast"
    );
    assert_eq!(def.active_zones, vec![Zone::Battlefield]);
}

#[test]
fn static_cast_as_though_flash_all_spells() {
    // CR 601.3b: the bare "spells" form (Leyline of Anticipation, Vedalken
    // Orrery) grants flash to every spell the controller casts.
    let def = parse_static_line("You may cast spells as though they had flash.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CastWithKeyword {
            keyword: Keyword::Flash
        }
    );
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::card().controller(ControllerRef::You)
        ))
    );
}

#[test]
fn static_cast_as_though_flash_compound_spell_types_scope_each_leg_to_you() {
    let def = parse_static_line(
        "You may cast legendary spells and artifact spells as though they had flash.",
    )
    .unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CastWithKeyword {
            keyword: Keyword::Flash
        }
    );
    let Some(TargetFilter::Or { filters }) = &def.affected else {
        panic!("expected Or affected filter, got {:?}", def.affected);
    };
    assert!(
        filters.iter().all(|filter| matches!(
            filter,
            TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::You),
                ..
            })
        )),
        "each disjunct must be scoped to spells you cast, got {filters:?}"
    );
}

#[test]
fn static_cast_as_though_flash_players_may_forms_are_unscoped() {
    for text in [
        "Players may cast enchantment spells as though they had flash.",
        "Any player may cast Sliver spells as though they had flash.",
    ] {
        let def = parse_static_line(text).unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CastWithKeyword {
                keyword: Keyword::Flash
            }
        );
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert_eq!(tf.controller, None, "{text}: must apply to every player");
            }
            other => panic!("{text}: expected Typed affected filter, got {other:?}"),
        }
    }
}

#[test]
fn static_can_block_additional_creature() {
    let def =
        parse_static_line("Palace Guard can block an additional creature each combat.").unwrap();
    assert_eq!(def.mode, StaticMode::ExtraBlockers { count: Some(1) });
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
}

#[test]
fn static_can_block_any_number() {
    let def = parse_static_line("Hundred-Handed One can block any number of creatures.").unwrap();
    assert_eq!(def.mode, StaticMode::ExtraBlockers { count: None });
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
}

#[test]
fn static_play_two_additional_lands() {
    // "play two additional lands" — not handled by the subject-predicate parser
    let def =
        parse_static_line("You may play two additional lands on each of your turns.").unwrap();
    assert_eq!(def.mode, StaticMode::AdditionalLandDrop { count: 2 });
}

#[test]
fn parse_compound_static_counter_minimum_variants() {
    // "a" counter variant
    let text =
            "During your turn, as long as ~ has a loyalty counter on it, it's a 2/2 Ninja creature and has hexproof.";
    let def = parse_static_line(text).unwrap();
    if let Some(StaticCondition::And { ref conditions }) = def.condition {
        assert!(matches!(
            conditions[1],
            StaticCondition::HasCounters { minimum: 1, .. }
        ));
    }
    assert!(def
        .modifications
        .contains(&ContinuousModification::SetPower { value: 2 }));
}

// ── CR 510.1c: AssignDamageFromToughness (Doran-class) ─────────────

#[test]
fn static_assigns_damage_from_toughness_basic() {
    // CR 510.1c: "Each creature you control assigns combat damage equal to its toughness"
    let def = parse_static_line(
            "Each creature you control assigns combat damage equal to its toughness rather than its power.",
        )
        .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You),
        ))
    );
    assert!(def
        .modifications
        .contains(&ContinuousModification::AssignDamageFromToughness));
}

#[test]
fn static_assigns_damage_from_toughness_with_defender() {
    // CR 510.1c: "Each creature you control with defender assigns combat damage..."
    let def = parse_static_line(
            "Each creature you control with defender assigns combat damage equal to its toughness rather than its power.",
        )
        .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::WithKeyword {
                    value: Keyword::Defender,
                }]),
        ))
    );
    assert!(def
        .modifications
        .contains(&ContinuousModification::AssignDamageFromToughness));
}

#[test]
fn static_assigns_damage_from_toughness_gt_power() {
    // CR 510.1c: "Each creature you control with toughness greater than its power..."
    let def = parse_static_line(
            "Each creature you control with toughness greater than its power assigns combat damage equal to its toughness rather than its power.",
        )
        .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::ToughnessGTPower]),
        ))
    );
    assert!(def
        .modifications
        .contains(&ContinuousModification::AssignDamageFromToughness));
}

#[test]
fn static_enchanted_creature_gets_pt_and_assigns_damage_from_toughness() {
    let def = parse_static_line(
            "Enchanted creature gets +0/+2 and assigns combat damage equal to its toughness rather than its power.",
        )
        .expect("Gauntlets of Light static must parse");
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
        ))
    );
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: 0 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddToughness { value: 2 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AssignDamageFromToughness));
}

#[test]
fn static_attached_conditional_assigns_damage_from_toughness() {
    let cases = [
            (
                "As long as equipped creature's toughness is greater than its power, it assigns combat damage equal to its toughness rather than its power.",
                vec![FilterProp::EquippedBy, FilterProp::ToughnessGTPower],
            ),
            (
                "As long as enchanted creature has vigilance, it assigns combat damage equal to its toughness rather than its power.",
                vec![
                    FilterProp::EnchantedBy,
                    FilterProp::WithKeyword {
                        value: Keyword::Vigilance,
                    },
                ],
            ),
        ];

    for (text, properties) in cases {
        let def = parse_static_line(text).expect("attached toughness-damage static must parse");
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(properties),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AssignDamageFromToughness));
    }
}

// --- Conditional counter-based keyword grants (CR 613.7) ---

#[test]
fn static_each_creature_with_counter_has_trample() {
    let def =
        parse_static_line("Each creature you control with a +1/+1 counter on it has trample.")
            .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    match &def.affected {
        Some(TargetFilter::Typed(ref tf))
            if tf.type_filters.contains(&TypeFilter::Creature)
                && tf.controller == Some(ControllerRef::You) =>
        {
            let properties = &tf.properties;
            assert!(properties.iter().any(|p| matches!(
                p,
                FilterProp::Counters {
                    counters: CounterMatch::OfType(crate::types::counter::CounterType::Plus1Plus1),
                    comparator: Comparator::GE,
                    count: QuantityExpr::Fixed { value: 1 },
                }
            )));
        }
        other => panic!("Expected Typed creature filter, got {:?}", other),
    }
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Trample
        }));
}

#[test]
fn static_creatures_with_counters_have_haste() {
    let def =
        parse_static_line("Creatures you control with +1/+1 counters on them have haste.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    match &def.affected {
        Some(TargetFilter::Typed(ref tf))
            if tf.type_filters.contains(&TypeFilter::Creature)
                && tf.controller == Some(ControllerRef::You) =>
        {
            let properties = &tf.properties;
            assert!(properties.iter().any(|p| matches!(
                p,
                FilterProp::Counters {
                    counters: CounterMatch::OfType(crate::types::counter::CounterType::Plus1Plus1),
                    comparator: Comparator::GE,
                    count: QuantityExpr::Fixed { value: 1 },
                }
            )));
        }
        other => panic!("Expected Typed creature filter, got {:?}", other),
    }
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Haste
        }));
}

#[test]
fn static_other_creatures_with_any_counters_have_flying_and_haste() {
    let def = parse_static_line(
        "Other creatures you control with counters on them have flying and haste.",
    )
    .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    match &def.affected {
        Some(TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::You),
            properties,
            type_filters,
            ..
        })) => {
            assert!(type_filters.contains(&TypeFilter::Creature));
            assert!(properties.contains(&FilterProp::Another));
            assert!(properties.iter().any(|p| matches!(
                p,
                FilterProp::Counters {
                    counters: CounterMatch::Any,
                    comparator: Comparator::GE,
                    count: QuantityExpr::Fixed { value: 1 },
                }
            )));
        }
        other => panic!("Expected typed creature filter, got {other:?}"),
    }
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Flying
        }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Haste
        }));
}

#[test]
fn static_creatures_with_counter_get_pump() {
    let def =
        parse_static_line("Creatures you control with a +1/+1 counter on it gets +2/+2.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    match &def.affected {
        Some(TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::You),
            properties,
            ..
        })) => {
            assert!(properties.iter().any(|p| matches!(
                p,
                FilterProp::Counters {
                    counters: CounterMatch::OfType(crate::types::counter::CounterType::Plus1Plus1),
                    comparator: Comparator::GE,
                    count: QuantityExpr::Fixed { value: 1 },
                }
            )));
        }
        other => panic!("Expected Typed creature filter, got {:?}", other),
    }
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: 2 }));
}

// --- split_keyword_list protection-awareness tests ---

/// Helper: collect split results as owned strings for easy comparison.
fn kw_list(text: &str) -> Vec<String> {
    split_keyword_list(text)
        .into_iter()
        .map(|c| c.into_owned())
        .collect()
}

#[test]
fn split_keyword_list_two_color_protections() {
    assert_eq!(
        kw_list("protection from black and from red"),
        vec!["protection from black", "protection from red"]
    );
}

#[test]
fn split_keyword_list_non_protection_and() {
    assert_eq!(
        kw_list("flying and first strike"),
        vec!["flying", "first strike"]
    );
}

#[test]
fn split_keyword_list_mixed_keywords_and_protection() {
    // expand_protection_parts lowercases protection fragments
    assert_eq!(
        kw_list("flying, protection from Demons and from Dragons, and first strike"),
        vec![
            "flying",
            "protection from demons",
            "protection from dragons",
            "first strike"
        ]
    );
}

#[test]
fn split_keyword_list_three_way_inline_protection() {
    assert_eq!(
        kw_list("protection from red and from blue and from green"),
        vec![
            "protection from red",
            "protection from blue",
            "protection from green"
        ]
    );
}

#[test]
fn split_keyword_list_comma_continuation_protection() {
    // expand_protection_parts lowercases protection fragments
    assert_eq!(
        kw_list("protection from Vampires, from Werewolves, and from Zombies"),
        vec![
            "protection from vampires",
            "protection from werewolves",
            "protection from zombies"
        ]
    );
}

#[test]
fn split_keyword_list_protection_from_everything_no_split() {
    assert_eq!(
        kw_list("protection from everything"),
        vec!["protection from everything"]
    );
}

#[test]
fn continuous_mods_protection_from_two_colors() {
    use crate::types::keywords::ProtectionTarget;
    use crate::types::mana::ManaColor;
    let mods = parse_continuous_modifications("has protection from black and from red");
    let prot_keywords: Vec<_> = mods
        .iter()
        .filter_map(|m| match m {
            ContinuousModification::AddKeyword {
                keyword: Keyword::Protection(pt),
            } => Some(pt.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        prot_keywords,
        vec![
            ProtectionTarget::Color(ManaColor::Black),
            ProtectionTarget::Color(ManaColor::Red),
        ]
    );
}

#[test]
fn continuous_mods_grant_keyword_and_cant_be_blocked() {
    let mods = parse_continuous_modifications("gains flying and can't be blocked this turn");
    assert!(
        mods.contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Flying,
        }),
        "missing flying grant in {mods:?}"
    );
    assert!(
        mods.iter().any(|m| matches!(
            m,
            ContinuousModification::AddStaticMode {
                mode: StaticMode::CantBeBlocked
            }
        )),
        "missing CantBeBlocked grant in {mods:?}"
    );
}

/// CR 509.1b + CR 613.4b: Atomic Microsizer — evasion restriction and base P/T
/// set must parse from one compound predicate.
#[test]
fn continuous_mods_cant_be_blocked_and_has_base_pt() {
    let mods = parse_continuous_modifications(
        "can't be blocked this turn and has base power and toughness 1/1 until end of turn",
    );
    assert!(
        mods.iter().any(|m| matches!(
            m,
            ContinuousModification::AddStaticMode {
                mode: StaticMode::CantBeBlocked
            }
        )),
        "missing CantBeBlocked grant in {mods:?}"
    );
    assert!(
        mods.iter()
            .any(|m| matches!(m, ContinuousModification::SetPower { value: 1 })),
        "missing SetPower(1) in {mods:?}"
    );
    assert!(
        mods.iter()
            .any(|m| matches!(m, ContinuousModification::SetToughness { value: 1 })),
        "missing SetToughness(1) in {mods:?}"
    );
}

/// Extract the subtype string from a single-subtype `IsPresent` filter, for
/// asserting per-subtype conditional keyword grants.
fn is_present_subtype(cond: &StaticCondition) -> Option<String> {
    let StaticCondition::IsPresent { filter: Some(f) } = cond else {
        return None;
    };
    let TargetFilter::Typed(tf) = f else {
        return None;
    };
    tf.type_filters.iter().find_map(|tfilter| match tfilter {
        TypeFilter::Subtype(s) => Some(s.clone()),
        _ => None,
    })
}

fn add_keyword_mods(def: &StaticDefinition) -> Vec<Keyword> {
    def.modifications
        .iter()
        .filter_map(|m| match m {
            ContinuousModification::AddKeyword { keyword } => Some(keyword.clone()),
            _ => None,
        })
        .collect()
}

/// CR 509.1b + CR 613.1f + CR 702.18a: Whispersilk Cloak — a `CantBeBlocked`
/// restriction conjoined with a keyword grant must emit BOTH a
/// `CantBeBlocked` def and a `Continuous{AddKeyword(Shroud)}` companion, each
/// affecting the equipped creature.
#[test]
fn attached_compound_cant_be_blocked_and_keyword() {
    let defs = parse_static_line_multi("Equipped creature can't be blocked and has shroud.");
    assert_eq!(defs.len(), 2, "expected 2 defs, got {defs:?}");

    let restriction = defs
        .iter()
        .find(|d| matches!(d.mode, StaticMode::CantBeBlocked))
        .expect("missing CantBeBlocked def");
    let keyword_def = defs
        .iter()
        .find(|d| matches!(d.mode, StaticMode::Continuous))
        .expect("missing Continuous keyword companion");

    assert_eq!(
        keyword_def.modifications,
        vec![ContinuousModification::AddKeyword {
            keyword: Keyword::Shroud
        }],
        "companion must grant exactly Shroud"
    );
    let equipped =
        TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EquippedBy]));
    assert_eq!(restriction.affected.as_ref(), Some(&equipped));
    assert_eq!(keyword_def.affected.as_ref(), Some(&equipped));
}

/// CR 613.1f + CR 702.17a: Swashbuckler's Whip — "has reach" plus two quoted
/// granted abilities must merge into ONE `Continuous` def carrying
/// `AddKeyword(Reach)` and two `GrantAbility` modifications.
#[test]
fn attached_compound_keyword_and_quoted_abilities() {
    let defs = parse_static_line_multi(
            "Equipped creature has reach, \"{2}, {T}: Tap target artifact or creature,\" and \"{8}, {T}: Discover 10.\"",
        );
    assert_eq!(defs.len(), 1, "expected 1 merged def, got {defs:?}");
    let def = &defs[0];
    assert!(
        matches!(def.mode, StaticMode::Continuous),
        "expected Continuous mode"
    );
    assert!(
        add_keyword_mods(def).contains(&Keyword::Reach),
        "missing AddKeyword(Reach) in {:?}",
        def.modifications
    );
    let grant_count = def
        .modifications
        .iter()
        .filter(|m| matches!(m, ContinuousModification::GrantAbility { .. }))
        .count();
    assert_eq!(grant_count, 2, "expected 2 GrantAbility mods in {def:?}");
}

/// CR 613.1f + CR 611.3a: Multiclass Baldric — four per-subtype conditional
/// keyword grants, each its own `Continuous{AddKeyword}` gated on
/// `IsPresent{<subtype>}`.
#[test]
fn attached_conditional_keyword_list() {
    let defs = parse_static_line_multi(
            "Equipped creature has lifelink if you control a Cleric, deathtouch if you control a Rogue, haste if you control a Warrior, and flying if you control a Wizard.",
        );
    assert_eq!(defs.len(), 4, "expected 4 defs, got {defs:?}");

    let expected = [
        (Keyword::Lifelink, "Cleric"),
        (Keyword::Deathtouch, "Rogue"),
        (Keyword::Haste, "Warrior"),
        (Keyword::Flying, "Wizard"),
    ];
    for (def, (kw, subtype)) in defs.iter().zip(expected.iter()) {
        assert!(matches!(def.mode, StaticMode::Continuous));
        assert_eq!(add_keyword_mods(def), vec![kw.clone()]);
        let cond = def.condition.as_ref().expect("missing condition");
        assert_eq!(
            is_present_subtype(cond).as_deref(),
            Some(*subtype),
            "condition {cond:?} should be IsPresent {subtype}"
        );
    }
}

/// CR 604.1 + CR 611.3a + CR 613.1f: Hunter's Blowgun — a turn-gated keyword
/// alternative emits `AddKeyword(Deathtouch)` gated `DuringYourTurn` and
/// `AddKeyword(Reach)` gated `Not(DuringYourTurn)`.
#[test]
fn attached_otherwise_turn_gated_keywords() {
    let defs = parse_static_line_multi(
        "Equipped creature has deathtouch during your turn. Otherwise, it has reach.",
    );
    assert_eq!(defs.len(), 2, "expected 2 defs, got {defs:?}");

    let deathtouch = &defs[0];
    assert_eq!(add_keyword_mods(deathtouch), vec![Keyword::Deathtouch]);
    assert_eq!(
        deathtouch.condition.as_ref(),
        Some(&StaticCondition::DuringYourTurn)
    );

    let reach = &defs[1];
    assert_eq!(add_keyword_mods(reach), vec![Keyword::Reach]);
    assert_eq!(
        reach.condition.as_ref(),
        Some(&StaticCondition::Not {
            condition: Box::new(StaticCondition::DuringYourTurn)
        })
    );
}

/// CR 611.3a: the ". Otherwise" split must work for an "as long as <cond>"
/// head condition (not only the turn-gated case). Clutch of Undeath-style
/// "gets +3/+3 as long as it's a Zombie. Otherwise, it gets -3/-3." must emit
/// two MUTUALLY EXCLUSIVE defs: the head gated on its own condition and the
/// companion gated on `Not(<head condition>)`. A companion with `condition ==
/// None` would apply both clauses at once (net +0/+0) — the regression this
/// guards against.
#[test]
fn attached_otherwise_as_long_as_gated() {
    let defs = parse_static_line_multi(
        "Enchanted creature gets +3/+3 as long as it's a Zombie. Otherwise, it gets -3/-3.",
    );
    assert_eq!(defs.len(), 2, "expected 2 defs, got {defs:?}");

    // The head carries its own "as long as" gating condition.
    let head_condition = defs[0]
        .condition
        .clone()
        .expect("head def must retain its as-long-as condition");

    // The companion must be the strict complement of the head condition,
    // never unconditional.
    assert_eq!(
        defs[1].condition.as_ref(),
        Some(&StaticCondition::Not {
            condition: Box::new(head_condition)
        }),
        "companion must be Not(<head condition>), not None"
    );
}

/// CR 509.1b + CR 702.18a: the compound restriction+keyword split applies to
/// all attached-subject prefixes, with the correct `EnchantedBy`/`EquippedBy`
/// filter.
#[test]
fn attached_compound_split_all_subjects() {
    let cases = [
        (
            "Enchanted creature can't be blocked and has shroud.",
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EnchantedBy])),
        ),
        (
            "Enchanted permanent can't be blocked and has shroud.",
            TargetFilter::Typed(TypedFilter::permanent().properties(vec![FilterProp::EnchantedBy])),
        ),
        (
            "Enchanted land can't be blocked and has shroud.",
            TargetFilter::Typed(TypedFilter::land().properties(vec![FilterProp::EnchantedBy])),
        ),
    ];
    for (line, expected_filter) in cases {
        let defs = parse_static_line_multi(line);
        assert_eq!(defs.len(), 2, "{line}: expected 2 defs, got {defs:?}");
        assert!(
            defs.iter()
                .any(|d| matches!(d.mode, StaticMode::CantBeBlocked)),
            "{line}: missing CantBeBlocked"
        );
        let kw_def = defs
            .iter()
            .find(|d| matches!(d.mode, StaticMode::Continuous))
            .expect("missing keyword companion");
        assert_eq!(
            kw_def.modifications,
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Shroud
            }]
        );
        assert_eq!(kw_def.affected.as_ref(), Some(&expected_filter));
    }
}

/// GAP-1 regression: benign continuous lines must NOT split. A "gets +N/+M
/// and has <keywords>" line is merged into ONE Continuous def by
/// `parse_continuous_modifications` and must return as a single def.
#[test]
fn attached_continuous_gets_and_keywords_no_split() {
    let defs =
        parse_static_line_multi("Equipped creature gets +1/+1 and has trample and lifelink.");
    assert_eq!(defs.len(), 1, "expected exactly 1 def, got {defs:?}");
    assert_eq!(
        defs[0].modifications,
        vec![
            ContinuousModification::AddPower { value: 1 },
            ContinuousModification::AddToughness { value: 1 },
            ContinuousModification::AddKeyword {
                keyword: Keyword::Trample
            },
            ContinuousModification::AddKeyword {
                keyword: Keyword::Lifelink
            },
        ]
    );

    // Loxodon Warhammer's grant line.
    let warhammer =
        parse_static_line_multi("Equipped creature gets +3/+0 and has trample and lifelink.");
    assert_eq!(
        warhammer.len(),
        1,
        "Warhammer: expected 1 def, got {warhammer:?}"
    );
    assert_eq!(
        warhammer[0].modifications,
        vec![
            ContinuousModification::AddPower { value: 3 },
            ContinuousModification::AddToughness { value: 0 },
            ContinuousModification::AddKeyword {
                keyword: Keyword::Trample
            },
            ContinuousModification::AddKeyword {
                keyword: Keyword::Lifelink
            },
        ]
    );
}

#[test]
fn continuous_mods_grant_chosen_color_hexproof_and_block_restriction() {
    use crate::types::keywords::{HexproofFilter, Keyword};

    let mods = parse_continuous_modifications(
            "gains hexproof from that color until end of turn and can't be blocked by creatures of that color this turn",
        );

    assert!(
        mods.contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::HexproofFrom(HexproofFilter::ChosenColor),
        }),
        "missing typed HexproofFrom(ChosenColor) grant in {mods:?}"
    );

    let Some(filter) = mods.iter().find_map(|m| match m {
        ContinuousModification::AddStaticMode {
            mode: StaticMode::CantBeBlockedBy { filter },
        } => Some(filter),
        _ => None,
    }) else {
        panic!("missing CantBeBlockedBy grant in {mods:?}");
    };
    let TargetFilter::Typed(tf) = filter else {
        panic!("expected typed filter, got {filter:?}");
    };
    assert!(
        tf.properties
            .iter()
            .any(|prop| matches!(prop, FilterProp::IsChosenColor)),
        "missing IsChosenColor filter prop in {tf:?}"
    );
}

// --- Graveyard cast permission tests ---

#[test]
fn graveyard_cast_permission_lurrus() {
    let text = "Once during each of your turns, you may cast a permanent spell with mana value 2 or less from your graveyard.";
    let def = parse_static_line(text).expect("should parse Lurrus text");
    assert!(matches!(
        def.mode,
        StaticMode::GraveyardCastPermission {
            frequency: CastFrequency::OncePerTurn,
            play_mode: CardPlayMode::Cast,
            ..
        }
    ));
    let filter = def.affected.expect("should have affected filter");
    if let TargetFilter::Typed(tf) = &filter {
        assert!(tf.type_filters.contains(&TypeFilter::Permanent));
        assert!(
            tf.properties.iter().any(|p| matches!(
                p,
                FilterProp::Cmc {
                    comparator: Comparator::LE,
                    ..
                }
            )),
            "Expected CmcLE property, got: {:?}",
            tf.properties
        );
    } else {
        panic!("Expected Typed filter, got: {filter:?}");
    }
}

#[test]
fn graveyard_cast_permission_karador() {
    let def = parse_static_line(
        "Once during each of your turns, you may cast a creature spell from your graveyard.",
    )
    .unwrap();
    assert!(matches!(
        def.mode,
        StaticMode::GraveyardCastPermission {
            frequency: CastFrequency::OncePerTurn,
            play_mode: CardPlayMode::Cast,
            ..
        }
    ));
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
        }
        other => panic!("Expected Typed creature filter for Karador, got {other:?}"),
    }
}

#[test]
fn graveyard_cast_permission_kess() {
    let def = parse_static_line(
            "Once during each of your turns, you may cast an instant or sorcery spell from your graveyard."
        ).unwrap();
    assert!(matches!(
        def.mode,
        StaticMode::GraveyardCastPermission {
            frequency: CastFrequency::OncePerTurn,
            play_mode: CardPlayMode::Cast,
            ..
        }
    ));
    // Should parse as a union or typed filter covering instant/sorcery
    assert!(def.affected.is_some());
}

#[test]
fn graveyard_cast_permission_exile_rider() {
    let def = parse_static_line(
            "Once during each of your turns, you may cast an instant or sorcery spell from your graveyard. If a spell cast this way would be put into your graveyard, exile it instead."
        ).unwrap();
    assert!(matches!(
        def.mode,
        StaticMode::GraveyardCastPermission {
            frequency: CastFrequency::OncePerTurn,
            play_mode: CardPlayMode::Cast,
            graveyard_destination_replacement: Some(Zone::Exile),
        }
    ));
}

#[test]
fn graveyard_cast_permission_gisa_geralf() {
    let text =
        "Once during each of your turns, you may cast a Zombie creature spell from your graveyard.";
    let lower = text.to_lowercase();
    let def =
        try_parse_graveyard_cast_permission(text, &lower).expect("should parse Gisa+Geralf text");
    assert!(matches!(
        def.mode,
        StaticMode::GraveyardCastPermission {
            frequency: CastFrequency::OncePerTurn,
            play_mode: CardPlayMode::Cast,
            ..
        }
    ));
    // "zombie creature" → parse_type_phrase recognizes "zombie" as subtype.
    // card_type may be None (subtype alone) or Creature depending on parser —
    // either is functionally correct since Zombie is exclusively a creature subtype.
    if let Some(TargetFilter::Typed(tf)) = &def.affected {
        assert_eq!(tf.get_subtype(), Some("Zombie"));
    } else {
        panic!("Expected Typed filter with Zombie subtype");
    }
}

#[test]
fn graveyard_cast_permission_gravecrawler_self_ref_condition() {
    let text = "You may cast this card from your graveyard as long as you control a Zombie.";
    let def = parse_static_line(text).expect("should parse Gravecrawler text");
    assert!(matches!(
        def.mode,
        StaticMode::GraveyardCastPermission {
            frequency: CastFrequency::Unlimited,
            play_mode: CardPlayMode::Cast,
            ..
        }
    ));
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert_eq!(def.active_zones, vec![Zone::Graveyard]);
    match def.condition {
        Some(StaticCondition::IsPresent {
            filter: Some(TargetFilter::Typed(tf)),
        }) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(
                tf.type_filters
                    .contains(&TypeFilter::Subtype("Zombie".to_string())),
                "expected Zombie subtype condition, got: {:?}",
                tf.type_filters
            );
            assert!(
                tf.properties.contains(&FilterProp::InZone {
                    zone: Zone::Battlefield,
                }),
                "expected battlefield control condition, got: {:?}",
                tf.properties
            );
        }
        other => panic!("expected Zombie presence condition, got {other:?}"),
    }
}

#[test]
fn graveyard_cast_permission_scourge_of_nel_toth_self_ref() {
    // Regression for #525: Scourge of Nel Toth's "this creature" self-reference
    // is normalized to the `~` token by `normalize_self_references` before the
    // static parser runs (unlike "this card", which is parse-only and survives
    // normalization). The `~` filter must lower to TargetFilter::SelfRef, NOT an
    // empty match-all Typed filter (which would grant permission to cast ANY
    // graveyard card).
    let text = "You may cast ~ from your graveyard by paying {B}{B} \
                    and sacrificing two creatures rather than paying its mana cost.";
    let def = parse_static_line(text).expect("should parse Scourge of Nel Toth text");
    assert!(matches!(
        def.mode,
        StaticMode::GraveyardCastPermission {
            frequency: CastFrequency::Unlimited,
            play_mode: CardPlayMode::Cast,
            ..
        }
    ));
    // The bug: affected was Typed { type_filters: [], .. } (match-all).
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    // Self-ref permission must be zone-restricted to the graveyard (CR 113.6b).
    assert_eq!(def.active_zones, vec![Zone::Graveyard]);
    // Explicitly reject the buggy empty-Typed shape.
    assert!(
        !matches!(def.affected, Some(TargetFilter::Typed(_))),
        "graveyard-cast permission must not be a match-all Typed filter"
    );
}

/// CR 601.3 + CR 113.6b: Oathsworn Vampire — "You may cast this card from
/// your graveyard if you gained life this turn." The trailing turn-history
/// "if" gate must attach as the permission's `condition`; without it the
/// permission would be unconditional. Regression for the swallowed
/// `Condition_If` clause.
#[test]
fn graveyard_cast_permission_oathsworn_vampire_if_gate() {
    use crate::types::ability::{Comparator, QuantityExpr, QuantityRef};
    let text = "You may cast this card from your graveyard if you gained life this turn.";
    let def = parse_static_line(text).expect("should parse Oathsworn Vampire text");
    assert!(matches!(
        def.mode,
        StaticMode::GraveyardCastPermission {
            frequency: CastFrequency::Unlimited,
            play_mode: CardPlayMode::Cast,
            ..
        }
    ));
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert_eq!(def.active_zones, vec![Zone::Graveyard]);
    match def.condition {
        Some(StaticCondition::QuantityComparison {
            lhs:
                QuantityExpr::Ref {
                    qty: QuantityRef::LifeGainedThisTurn { player },
                },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        }) => {
            assert_eq!(player, PlayerScope::Controller);
        }
        other => panic!("expected LifeGainedThisTurn >= 1 condition, got {other:?}"),
    }
}

#[test]
fn graveyard_keyword_grant_clause_flashback() {
    let (filter, kind) = try_parse_graveyard_keyword_grant_clause(
        "Each instant and sorcery card in your graveyard has flashback.",
    )
    .expect("should parse flashback grant clause");
    assert_eq!(kind, GraveyardGrantedKeywordKind::Flashback);
    match filter {
        TargetFilter::Or { filters } => {
            assert_eq!(filters.len(), 2);
            for branch in filters {
                let TargetFilter::Typed(tf) = branch else {
                    panic!("expected typed branch, got {branch:?}");
                };
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.properties.contains(&FilterProp::InZone {
                    zone: Zone::Graveyard,
                }));
            }
        }
        other => panic!("expected instant/sorcery graveyard filter, got {other:?}"),
    }
}

#[test]
fn graveyard_keyword_grant_clause_escape() {
    let (filter, kind) =
        try_parse_graveyard_keyword_grant_clause("Each nonland card in your graveyard has escape.")
            .expect("should parse escape grant clause");
    assert_eq!(kind, GraveyardGrantedKeywordKind::Escape);
    let TargetFilter::Typed(tf) = filter else {
        panic!("expected typed graveyard filter");
    };
    assert_eq!(tf.controller, Some(ControllerRef::You));
    assert!(
        tf.properties.contains(&FilterProp::InZone {
            zone: Zone::Graveyard
        }),
        "missing graveyard zone: {:?}",
        tf.properties
    );
    assert!(
        tf.type_filters
            .contains(&TypeFilter::Non(Box::new(TypeFilter::Land))),
        "missing nonland type filter: {:?}",
        tf.type_filters
    );
}

#[test]
fn graveyard_keyword_grant_clause_rejects_non_you_scope() {
    let clause = try_parse_graveyard_keyword_grant_clause(
        "Each nonland card in their graveyard has escape.",
    );
    assert!(
        clause.is_none(),
        "only your graveyard scope is currently supported"
    );
}

// --- Graveyard play permission tests (Crucible of Worlds / Icetill Explorer) ---

#[test]
fn graveyard_play_permission_crucible() {
    let text = "You may play lands from your graveyard.";
    let def = parse_static_line(text).expect("should parse Crucible text");
    assert!(matches!(
        def.mode,
        StaticMode::GraveyardCastPermission {
            frequency: CastFrequency::Unlimited,
            play_mode: CardPlayMode::Play,
            ..
        }
    ));
    if let Some(TargetFilter::Typed(tf)) = &def.affected {
        assert!(tf.type_filters.contains(&TypeFilter::Land));
    } else {
        panic!(
            "Expected Typed filter with Land type, got: {:?}",
            def.affected
        );
    }
}

#[test]
fn graveyard_cast_permission_conduit_of_worlds() {
    let text = "You may cast permanent spells from your graveyard.";
    let def = parse_static_line(text).expect("should parse Conduit text");
    assert!(matches!(
        def.mode,
        StaticMode::GraveyardCastPermission {
            frequency: CastFrequency::Unlimited,
            play_mode: CardPlayMode::Cast,
            ..
        }
    ));
    if let Some(TargetFilter::Typed(tf)) = &def.affected {
        assert!(tf.type_filters.contains(&TypeFilter::Permanent));
    } else {
        panic!(
            "Expected Typed filter with Permanent type, got: {:?}",
            def.affected
        );
    }
}

// --- Muldrotha-class once-per-turn-per-permanent-type tests (CR 110.4) ---

/// CR 110.4 + CR 305.1 + CR 601.2a: Muldrotha, the Gravetide — combined
/// "play a land or cast a permanent spell of each permanent type from
/// your graveyard" produces a single `GraveyardCastPermission` with the
/// `OncePerTurnPerPermanentType` frequency, `play_mode: Play` (covers
/// both lands and permanent spells), and a `Permanent` type filter.
#[test]
fn graveyard_cast_permission_muldrotha_canonical_or() {
    let text = "During each of your turns, you may play a land or cast a permanent spell of each permanent type from your graveyard.";
    let def = parse_static_line(text).expect("should parse Muldrotha canonical text");
    assert!(
        matches!(
            def.mode,
            StaticMode::GraveyardCastPermission {
                frequency: CastFrequency::OncePerTurnPerPermanentType,
                play_mode: CardPlayMode::Play,
                ..
            }
        ),
        "expected OncePerTurnPerPermanentType + Play, got {:?}",
        def.mode
    );
    let filter = def.affected.expect("should have affected filter");
    let TargetFilter::Typed(tf) = filter else {
        panic!("expected Typed filter, got: {filter:?}");
    };
    assert!(
        tf.type_filters.contains(&TypeFilter::Permanent),
        "expected Permanent type filter, got: {:?}",
        tf.type_filters
    );
}

/// CR 110.4: Older "play a land and cast" wording is equivalent to the
/// canonical "play a land or cast" — both produce the same static.
#[test]
fn graveyard_cast_permission_muldrotha_legacy_and() {
    let text = "During each of your turns, you may play a land and cast a permanent spell of each permanent type from your graveyard.";
    let def = parse_static_line(text).expect("should parse Muldrotha legacy text");
    assert!(matches!(
        def.mode,
        StaticMode::GraveyardCastPermission {
            frequency: CastFrequency::OncePerTurnPerPermanentType,
            play_mode: CardPlayMode::Play,
            ..
        }
    ));
}

// --- Alt-cost rider tests (Ninja Teen et al., CR 118.9 / CR 702.190a) ---

#[test]
fn graveyard_cast_permission_ninja_teen_sneak_rider() {
    // Ninja Teen Level 3 rider: grants GY-cast permission gated on Sneak.
    let text = "You may cast creature spells from your graveyard using their sneak abilities.";
    let def = parse_static_line(text).expect("should parse Ninja Teen rider");
    assert!(matches!(
        def.mode,
        StaticMode::GraveyardCastPermission {
            frequency: CastFrequency::Unlimited,
            play_mode: CardPlayMode::Cast,
            ..
        }
    ));
    let filter = def.affected.expect("should have affected filter");
    let TargetFilter::Typed(tf) = filter else {
        panic!("expected Typed filter, got: {filter:?}");
    };
    assert!(tf.type_filters.contains(&TypeFilter::Creature));
    assert!(
        tf.properties.iter().any(|p| matches!(
            p,
            FilterProp::HasKeywordKind {
                value: KeywordKind::Sneak
            }
        )),
        "expected HasKeywordKind{{Sneak}} in properties, got: {:?}",
        tf.properties
    );
}

#[test]
fn graveyard_cast_permission_self_ref_rider_all_keywords() {
    // Self-referential riders on the 5 shipping cards (Brokkos/Mutate,
    // Phoenix/Bestow, Sabin+Underdog/Blitz, Timeline Culler/Warp).
    let cases = [
        ("mutate", KeywordKind::Mutate),
        ("bestow", KeywordKind::Bestow),
        ("blitz", KeywordKind::Blitz),
        ("warp", KeywordKind::Warp),
    ];
    for (name, expected_kind) in cases {
        let text = format!("You may cast this card from your graveyard using its {name} ability.");
        let def = parse_static_line(&text)
            .unwrap_or_else(|| panic!("should parse self-ref rider for {name}"));
        let filter = def
            .affected
            .unwrap_or_else(|| panic!("missing affected filter for {name}"));
        let has_kind = match filter {
            TargetFilter::Typed(tf) => tf.properties.iter().any(|p| {
                matches!(
                    p,
                    FilterProp::HasKeywordKind { value } if *value == expected_kind
                )
            }),
            TargetFilter::And { filters } => filters.iter().any(|f| {
                matches!(f, TargetFilter::Typed(tf)
                    if tf.properties.iter().any(|p| matches!(
                        p,
                        FilterProp::HasKeywordKind { value } if *value == expected_kind
                    ))
                )
            }),
            _ => false,
        };
        assert!(
            has_kind,
            "missing HasKeywordKind{{{expected_kind:?}}} for {name}"
        );
    }
}

/// Issue #594 (Maralen, Fae Ascendant) — parser test for the new exile
/// cast permission class. The full static line must lower to
/// `StaticMode::ExileCastPermission { OncePerTurn, Cast, without_paying }`
/// with the affected filter carrying the dynamic CMC cap. Anchored on
/// `parse_static_line` so the dispatch routing through `is_static_pattern`
/// → `parse_static_line_multi` → `parse_static_line_inner` is exercised
/// end-to-end.
#[test]
fn exile_cast_permission_maralen_fae_ascendant() {
    let text = "Once each turn, you may cast a spell with mana value \
                    less than or equal to the number of Elves and Faeries \
                    you control from among cards exiled with ~ this turn \
                    without paying its mana cost.";
    let def = parse_static_line(text).expect("Maralen static must parse");
    assert_eq!(
        def.mode,
        StaticMode::ExileCastPermission {
            frequency: CastFrequency::OncePerTurn,
            play_mode: CardPlayMode::Cast,
            cost: ExileCastCost::WithoutPayingManaCost,
            pool: ExileCardPool::ThisTurn,
            timing: ExileCastTiming::AnyTime,
        },
        "expected ExileCastPermission, got {:?}",
        def.mode
    );
    let affected = def.affected.as_ref().expect("affected filter present");
    let TargetFilter::Typed(tf) = affected else {
        panic!("expected typed filter, got {affected:?}");
    };
    let has_cmc_le = tf.properties.iter().any(|p| {
        matches!(
            p,
            FilterProp::Cmc {
                comparator: Comparator::LE,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount { .. },
                },
            }
        )
    });
    assert!(
        has_cmc_le,
        "Maralen filter must carry a Cmc(LE, ObjectCount) predicate: {:?}",
        tf.properties
    );
}

/// Issue #594 sibling — the parser must accept the longer "once during
/// each of your turns" synonym, leaving the rest of the lowering
/// unchanged. No card prints this shape today, but `add-engine-variant`
/// requires the class be built for the pattern, not the single card.
#[test]
fn exile_cast_permission_during_each_of_your_turns_synonym() {
    let text = "Once during each of your turns, you may cast a spell \
                    with mana value 3 or less from among cards exiled with \
                    ~ this turn without paying its mana cost.";
    let def = parse_static_line(text).expect("synonym shape must parse");
    assert!(
        matches!(
            def.mode,
            StaticMode::ExileCastPermission {
                frequency: CastFrequency::OncePerTurn,
                play_mode: CardPlayMode::Cast,
                cost: ExileCastCost::WithoutPayingManaCost,
                pool: ExileCardPool::ThisTurn,
                timing: ExileCastTiming::AnyTime,
            }
        ),
        "expected ExileCastPermission(OncePerTurn, Cast, free), got {:?}",
        def.mode
    );
}

/// CR 113.6b: The "this turn" suffix is structural. A line that names
/// "cards exiled with ~" but omits "this turn" must NOT match this
/// permission class — that would belong to the open-ended
/// `ExiledBySource` family (Court of Locthwain, Bag of Holding, etc.)
/// and is parsed elsewhere.
#[test]
fn exile_cast_permission_rejects_missing_this_turn_suffix() {
    let text = "Once each turn, you may cast a spell with mana value 3 \
                    or less from among cards exiled with ~ without paying \
                    its mana cost.";
    let lower = text.to_lowercase();
    assert!(
        try_parse_exile_cast_permission(text, &lower).is_none(),
        "Open-ended exile filter must not match the per-turn class"
    );
}

/// CR 601.2a: The graveyard sibling handler must NOT intercept the
/// exile-cast permission line. Regression guard against accidentally
/// over-anchoring the graveyard branch on "you may cast" alone.
#[test]
fn exile_cast_permission_not_intercepted_by_graveyard_branch() {
    let text = "Once each turn, you may cast a spell with mana value \
                    less than or equal to the number of Elves and Faeries \
                    you control from among cards exiled with ~ this turn \
                    without paying its mana cost.";
    let lower = text.to_lowercase();
    assert!(try_parse_graveyard_cast_permission(text, &lower).is_none());
    assert!(try_parse_exile_cast_permission(text, &lower).is_some());
}

/// CR 113.6b + CR 305.1 + CR 406.6 + CR 117.1c: The Matrix-of-Time persistent
/// exile-play line lowers to `ExileCastPermission { pool: Persistent, play_mode:
/// Play, frequency: Unlimited, timing: YourTurnOnly }` with `affected: Any`.
#[test]
fn persistent_exile_play_permission_matrix_form() {
    let text =
        "During your turn, you may play lands and cast spells from among cards exiled with ~.";
    let def = parse_static_line(text).expect("Matrix-class static must parse");
    assert_eq!(
        def.mode,
        StaticMode::ExileCastPermission {
            frequency: CastFrequency::Unlimited,
            play_mode: CardPlayMode::Play,
            cost: ExileCastCost::PayNormalCost,
            pool: ExileCardPool::Persistent,
            timing: ExileCastTiming::YourTurnOnly,
        },
        "expected persistent your-turn Play permission, got {:?}",
        def.mode
    );
    assert_eq!(
        def.affected,
        Some(TargetFilter::Any),
        "the persistent pool is the scope; affected must be Any"
    );
}

/// CR 601.3f + CR 305.1: The "you may look at cards exiled with ~, and you may
/// play lands and cast spells from among those cards." variant lowers to the
/// same persistent Play permission, but without the your-turn timing gate.
#[test]
fn persistent_exile_play_permission_look_at_variant() {
    let text = "You may look at cards exiled with ~, and you may play lands and cast spells from among those cards.";
    let def = parse_static_line(text).expect("look-at variant must parse");
    assert_eq!(
        def.mode,
        StaticMode::ExileCastPermission {
            frequency: CastFrequency::Unlimited,
            play_mode: CardPlayMode::Play,
            cost: ExileCastCost::PayNormalCost,
            pool: ExileCardPool::Persistent,
            timing: ExileCastTiming::AnyTime,
        },
        "expected persistent any-time Play permission, got {:?}",
        def.mode
    );
}

/// CR 113.6b: The persistent handler must NOT swallow the Maralen "this turn"
/// per-turn-pool line — that belongs to `try_parse_exile_cast_permission`.
#[test]
fn persistent_exile_play_permission_rejects_maralen_this_turn() {
    let text = "Once each turn, you may cast a spell from among cards exiled with ~ this turn.";
    let lower = text.to_lowercase();
    assert!(
        try_parse_persistent_exile_play_permission(text, &lower).is_none(),
        "the per-turn Maralen line must not match the persistent class"
    );
}

/// Issue #1524 — Serpent's Soul-Jar: persistent exile pool without "this turn".
#[test]
fn exile_cast_permission_soul_jar_persistent_creature_pool() {
    let text = "Once each turn, you may cast a creature spell from among cards \
                exiled with this artifact.";
    let def = parse_static_line(text).expect("Soul-Jar static must parse");
    assert_eq!(
        def.mode,
        StaticMode::ExileCastPermission {
            frequency: CastFrequency::OncePerTurn,
            play_mode: CardPlayMode::Cast,
            cost: ExileCastCost::PayNormalCost,
            pool: ExileCardPool::Persistent,
            timing: ExileCastTiming::AnyTime,
        },
        "expected persistent ExileCastPermission, got {:?}",
        def.mode
    );
}

#[test]
fn graveyard_cast_permission_no_rider_leaves_filter_clean() {
    // Lurrus / Muldrotha / Karador / Conduit / Yawgmoth's Will regression:
    // permissions without a rider must not carry any HasKeywordKind prop.
    let cases = [
            "Once during each of your turns, you may cast a permanent spell with mana value 2 or less from your graveyard.",
            "Once during each of your turns, you may cast a creature spell from your graveyard.",
            "You may cast permanent spells from your graveyard.",
        ];
    for text in cases {
        let def = parse_static_line(text)
            .unwrap_or_else(|| panic!("should parse no-rider text: {text:?}"));
        if let Some(TargetFilter::Typed(tf)) = def.affected {
            assert!(
                !tf.properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::HasKeywordKind { .. })),
                "unexpected HasKeywordKind in {text:?}: {:?}",
                tf.properties
            );
        }
    }
}

// --- Hand cast free permission tests (Omniscience) ---

#[test]
fn hand_cast_free_omniscience() {
    let text = "You may cast spells from your hand without paying their mana costs.";
    let def = parse_static_line(text).expect("should parse Omniscience text");
    assert_eq!(
        def.mode,
        StaticMode::CastFromHandFree {
            frequency: CastFrequency::Unlimited,
            origin: CastFreeOrigin::Hand,
        }
    );
    assert_eq!(def.affected, Some(TargetFilter::Any));
}

#[test]
fn hand_cast_free_rejects_without_free() {
    // "you may cast ... from your hand" without "without paying" is not a free-cast static
    let text = "You may cast a spell from your hand.";
    let lower = text.to_lowercase();
    assert!(try_parse_cast_free_permission(text, &lower).is_none());
}

/// CR 601.2b: Zaffai and the Tempests — once-per-turn cast-from-hand-free.
#[test]
fn hand_cast_free_zaffai_once_per_turn() {
    let text = "Once during each of your turns, you may cast an instant or sorcery spell from your hand without paying its mana cost.";
    let def = parse_static_line(text).expect("should parse Zaffai text");
    assert!(
        matches!(
            def.mode,
            StaticMode::CastFromHandFree {
                frequency: CastFrequency::OncePerTurn,
                origin: CastFreeOrigin::Hand,
            }
        ),
        "expected CastFromHandFree {{ OncePerTurn }}, got: {:?}",
        def.mode
    );
    // Affected filter must reject non-instant/sorcery hand spells.
    let filter = def.affected.expect("should have affected filter");
    match filter {
        TargetFilter::Or { .. } | TargetFilter::Typed(_) => {
            // Either an Or { Instant, Sorcery } union or a Typed filter whose
            // type_filters cover instant/sorcery — both are structurally valid.
        }
        other => panic!("unexpected filter for Zaffai: {other:?}"),
    }
}

/// CR 601.2b: Zaffai parser must NOT be intercepted by the graveyard-cast
/// permission branch when the zone is "from your hand".
#[test]
fn hand_cast_free_zaffai_not_intercepted_by_graveyard_branch() {
    let text = "Once during each of your turns, you may cast an instant or sorcery spell from your hand without paying its mana cost.";
    let lower = text.to_lowercase();
    // Graveyard branch must decline (zone is hand, not graveyard).
    assert!(try_parse_graveyard_cast_permission(text, &lower).is_none());
    // Hand-free branch must succeed.
    assert!(try_parse_cast_free_permission(text, &lower).is_some());
}

// CR 601.2 + CR 118.9a: B10 Dracogenesis — Omniscience-class static with
// the zone qualifier omitted ("you may cast Dragon spells without paying
// their mana costs"). The mana-cost replacement applies from built-in cast
// zones already authorized for that spell, including CR 903.8 command zone casts.
#[test]
fn cast_free_dracogenesis_no_zone_qualifier() {
    let text = "You may cast Dragon spells without paying their mana costs.";
    let def = parse_static_line(text).expect("should parse Dracogenesis text");
    assert_eq!(
        def.mode,
        StaticMode::CastFromHandFree {
            frequency: CastFrequency::Unlimited,
            origin: CastFreeOrigin::DefaultCastPermission,
        }
    );
    // Dragon subtype filter must survive.
    let filter = def.affected.expect("should have affected filter");
    match filter {
        TargetFilter::Typed(tf) => {
            assert_eq!(tf.get_subtype(), Some("Dragon"));
        }
        other => panic!("expected Typed[Subtype: Dragon] for Dracogenesis, got {other:?}"),
    }
}

// CR 601.2 + CR 119.3: Unqualified branch now accepts dynamic mana-value
// filters whose RHS is any `parse_quantity_ref` phrase (Fires of Invention
// class). Earlier the comparator only matched the trigger-anaphoric
// `that <type>` form, so this filter fell through to a partial parse and
// the test asserted the rejection (better-decline-than-overgrant). The
// comparator was extended to delegate the RHS to the shared
// `parse_quantity_ref` building block, so the filter now fully types as
// `CmcLE { value: Ref { ObjectCount { Land, You } } }` and the cast-free
// permission can carry it. The test is inverted: it now asserts the
// typed filter is preserved end-to-end.
#[test]
fn cast_free_unqualified_accepts_dynamic_mv_filter() {
    use crate::types::ability::{FilterProp, QuantityExpr, QuantityRef, TargetFilter};
    let text = "You may cast spells with mana value less than or equal to the number of lands you control without paying their mana costs.";
    let lower = text.to_lowercase();
    let def = try_parse_cast_free_permission(text, &lower)
        .expect("dynamic-MV filter should parse end-to-end");
    let filter = def.affected.expect("affected filter must be present");
    let TargetFilter::Typed(tf) = filter else {
        panic!("expected Typed filter for Fires-of-Invention class");
    };
    let has_dynamic_cmc_le = tf.properties.iter().any(|p| {
        matches!(
            p,
            FilterProp::Cmc {
                comparator: Comparator::LE,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount { .. }
                }
            }
        )
    });
    assert!(
        has_dynamic_cmc_le,
        "expected CmcLE with dynamic ObjectCount RHS, got {:?}",
        tf.properties
    );
}

// Negative test: text without "without paying" must not match the
// free-cast combinator under either zone-qualifier branch.
#[test]
fn cast_free_rejects_text_without_without_paying() {
    let text = "You may cast Dragon spells from your hand.";
    let lower = text.to_lowercase();
    assert!(try_parse_cast_free_permission(text, &lower).is_none());

    let text2 = "You may cast Dragon spells.";
    let lower2 = text2.to_lowercase();
    assert!(try_parse_cast_free_permission(text2, &lower2).is_none());
}

// ── Fix 1: Irregular plural subtype normalization ──

#[test]
fn static_elves_you_control_uses_elf_subtype() {
    // CR 205.3m: "Elves" must normalize to "Elf", not "Elve"
    let def = parse_static_line("Other Elves you control get +1/+1.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    if let Some(TargetFilter::And { filters }) = &def.affected {
        let has_elf = filters
            .iter()
            .any(|f| matches!(f, TargetFilter::Typed(tf) if tf.get_subtype() == Some("Elf")));
        assert!(has_elf, "Expected Elf subtype, got {:?}", def.affected);
    } else if let Some(TargetFilter::Typed(tf)) = &def.affected {
        assert_eq!(tf.get_subtype(), Some("Elf"));
    } else {
        panic!("Expected filter with Elf subtype, got {:?}", def.affected);
    }
}

#[test]
fn static_dwarves_you_control_uses_dwarf_subtype() {
    // CR 205.3m: "Dwarves" must normalize to "Dwarf", not "Dwarve"
    let def = parse_static_line("Dwarves you control get +1/+1.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    if let Some(TargetFilter::Typed(tf)) = &def.affected {
        assert_eq!(tf.get_subtype(), Some("Dwarf"));
    } else {
        panic!(
            "Expected Typed filter with Dwarf subtype, got {:?}",
            def.affected
        );
    }
}

#[test]
fn parse_creature_subject_filter_generic_and_irregular_plurals() {
    let filter = parse_creature_subject_filter("Creatures you control").unwrap();
    if let TargetFilter::Typed(tf) = &filter {
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert_eq!(tf.get_subtype(), None);
    } else {
        panic!("Expected generic Creature filter, got {:?}", filter);
    }

    let filter = parse_creature_subject_filter("Other creatures you control").unwrap();
    if let TargetFilter::Typed(tf) = &filter {
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert_eq!(tf.get_subtype(), None);
        assert!(tf.properties.contains(&FilterProp::Another));
    } else {
        panic!("Expected generic other Creature filter, got {:?}", filter);
    }

    // Single-word plural subtypes should resolve via parse_subtype
    let filter = parse_creature_subject_filter("Elves").unwrap();
    if let TargetFilter::Typed(tf) = &filter {
        assert_eq!(tf.get_subtype(), Some("Elf"));
    } else {
        panic!("Expected Typed filter with Elf subtype, got {:?}", filter);
    }

    let filter = parse_creature_subject_filter("Wolves").unwrap();
    if let TargetFilter::Typed(tf) = &filter {
        assert_eq!(tf.get_subtype(), Some("Wolf"));
    } else {
        panic!("Expected Typed filter with Wolf subtype, got {:?}", filter);
    }
}

#[test]
fn continuous_subject_filter_nontoken_is_negation_not_subtype() {
    // CR 111.1 / CR 205.3: "Nontoken creatures you control" (Ashaya, Soul of
    // the Wild) is a type phrase with a token-identity negation, NOT a
    // subtype. The negation guard in `parse_creature_subject_filter` must
    // return None so the phrase falls through to `parse_type_phrase`, which
    // produces a `Creature` filter with the `NonToken` property.
    let filter = parse_continuous_subject_filter("Nontoken creatures you control")
        .expect("nontoken creature subject should parse");
    let TargetFilter::Typed(tf) = &filter else {
        panic!("Expected Typed filter, got {:?}", filter);
    };
    assert!(
        tf.get_subtype().is_none(),
        "must NOT fabricate a subtype, got {:?}",
        tf.get_subtype()
    );
    assert!(
        tf.type_filters.contains(&TypeFilter::Creature),
        "expected Creature type filter, got {:?}",
        tf.type_filters
    );
    assert!(
        tf.properties.contains(&FilterProp::NonToken),
        "expected NonToken property, got {:?}",
        tf.properties
    );
    assert_eq!(tf.controller, Some(ControllerRef::You));
}

#[test]
fn continuous_subject_filter_legendary_is_supertype_not_subtype() {
    // CR 205.4a: "Legendary creatures you control" names the legendary
    // supertype plus the creature card type, not a creature subtype named
    // "Legendary". This is the Jodah, the Unifier anthem subject shape.
    let filter = parse_continuous_subject_filter("Legendary creatures you control")
        .expect("legendary creature subject should parse");
    let TargetFilter::Typed(tf) = &filter else {
        panic!("Expected Typed filter, got {:?}", filter);
    };
    assert!(
        tf.get_subtype().is_none(),
        "must NOT fabricate a subtype, got {:?}",
        tf.get_subtype()
    );
    assert!(
        tf.type_filters.contains(&TypeFilter::Creature),
        "expected Creature type filter, got {:?}",
        tf.type_filters
    );
    assert!(
        tf.properties.contains(&FilterProp::HasSupertype {
            value: Supertype::Legendary,
        }),
        "expected HasSupertype(Legendary), got {:?}",
        tf.properties
    );
    assert_eq!(tf.controller, Some(ControllerRef::You));
}

#[test]
fn static_jodah_anthem_affected_filter_uses_legendary_supertype() {
    // CR 205.4a + CR 613.4c: Jodah, the Unifier's anthem affects
    // legendary creatures you control and scales by that same population.
    let def = parse_static_line(
            "Legendary creatures you control get +X/+X, where X is the number of legendary creatures you control.",
        )
        .expect("Jodah anthem static should parse");
    let Some(TargetFilter::Typed(tf)) = &def.affected else {
        panic!("Expected Typed affected filter, got {:?}", def.affected);
    };
    assert!(
        tf.get_subtype().is_none(),
        "must NOT fabricate Legendary as a subtype, got {:?}",
        tf.get_subtype()
    );
    assert!(
        tf.properties.contains(&FilterProp::HasSupertype {
            value: Supertype::Legendary,
        }),
        "expected affected filter to use HasSupertype(Legendary), got {:?}",
        tf.properties
    );
    assert!(def
        .modifications
        .iter()
        .any(|m| matches!(m, ContinuousModification::AddDynamicPower { .. })));
    assert!(def
        .modifications
        .iter()
        .any(|m| matches!(m, ContinuousModification::AddDynamicToughness { .. })));
}

#[test]
fn continuous_subject_filter_capitalized_subtype_still_works() {
    // Negative control: a genuine capitalized subtype descriptor must still
    // route through the `is_capitalized_words` path — the negation guard
    // must not fire on an ordinary subtype that happens to start with a
    // capital. "Angel" does not begin with the `non` negation prefix.
    let filter = parse_continuous_subject_filter("Angel creatures you control")
        .expect("Angel creature subject should parse");
    let TargetFilter::Typed(tf) = &filter else {
        panic!("Expected Typed filter, got {:?}", filter);
    };
    assert_eq!(tf.get_subtype(), Some("Angel"));
    assert_eq!(tf.controller, Some(ControllerRef::You));
}

#[test]
fn continuous_subject_filter_noncreature_word_boundary_anchor() {
    // Word-boundary anchor check: the `non` guard fires for genuine negation
    // descriptors ("Nonland creatures"), and the negated word reaches
    // `classify_negation` via `parse_type_phrase`. This confirms the guard
    // is not over-broad — it only fires when `non` heads a real descriptor
    // token, which is always true for a `parse_creature_subject_filter`
    // descriptor extracted by stripping " creatures".
    let filter = parse_continuous_subject_filter("Nonland creatures you control")
        .expect("nonland creature subject should parse");
    let TargetFilter::Typed(tf) = &filter else {
        panic!("Expected Typed filter, got {:?}", filter);
    };
    assert!(
        tf.get_subtype().is_none(),
        "must NOT fabricate a subtype, got {:?}",
        tf.get_subtype()
    );
    assert!(
        tf.type_filters
            .iter()
            .any(|t| matches!(t, TypeFilter::Non(_))),
        "expected a negated type filter, got {:?}",
        tf.type_filters
    );
}

#[test]
fn static_pump_line_nontoken_subject_routes_through_negation_guard() {
    // CR 111.1 / CR 205.3: A pump/keyword static whose subject is a `non`
    // negation descriptor ("Nontoken creatures you control get/have ...")
    // must NOT fabricate a `Subtype("Nontoken")`. This exercises the
    // `parse_typed_you_control` negation guard (`:2764`/`:2783`): the guard
    // returns None, dispatch falls through, and `parse_type_phrase`'s
    // negation loop yields the correct `Creature` + `NonToken` filter.
    for line in [
        "Nontoken creatures you control get +1/+1.",
        "Nontoken creatures you control have flying.",
    ] {
        let def =
            parse_static_line(line).unwrap_or_else(|| panic!("static line should parse: {line:?}"));
        assert_eq!(def.mode, StaticMode::Continuous);
        let Some(TargetFilter::Typed(tf)) = &def.affected else {
            panic!(
                "Expected Typed affected filter for {line:?}, got {:?}",
                def.affected
            );
        };
        assert!(
            tf.get_subtype().is_none(),
            "{line:?}: must NOT fabricate a subtype, got {:?}",
            tf.get_subtype()
        );
        assert!(
            tf.type_filters.contains(&TypeFilter::Creature),
            "{line:?}: expected Creature type filter, got {:?}",
            tf.type_filters
        );
        assert!(
            tf.properties.contains(&FilterProp::NonToken),
            "{line:?}: expected NonToken property, got {:?}",
            tf.properties
        );
        assert_eq!(
            tf.controller,
            Some(ControllerRef::You),
            "{line:?}: expected controller You"
        );
    }
}

#[test]
fn static_unblocked_attacking_ninjas_you_control_have_lifelink() {
    let def = parse_static_line("Unblocked attacking Ninjas you control have lifelink.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    if let Some(TargetFilter::Typed(tf)) = &def.affected {
        assert_eq!(tf.get_subtype(), Some("Ninja"));
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert!(tf.properties.contains(&FilterProp::Unblocked));
        assert!(tf.properties.contains(&FilterProp::Attacking));
    } else {
        panic!(
            "Expected Typed filter with Ninja subtype, got {:?}",
            def.affected
        );
    }
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Lifelink,
        }));
}

#[test]
fn static_attacking_ninjas_you_control_have_deathtouch() {
    let def = parse_static_line("Attacking Ninjas you control have deathtouch.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    if let Some(TargetFilter::Typed(tf)) = &def.affected {
        assert_eq!(tf.get_subtype(), Some("Ninja"));
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert!(tf.properties.contains(&FilterProp::Attacking));
        assert!(!tf.properties.contains(&FilterProp::Unblocked));
    } else {
        panic!(
            "Expected Typed filter with Ninja subtype, got {:?}",
            def.affected
        );
    }
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Deathtouch,
        }));
}

#[test]
fn static_other_ninja_and_rogue_creatures_you_control_get_plus1() {
    let def = parse_static_line("Other Ninja and Rogue creatures you control get +1/+1.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    if let Some(TargetFilter::Or { filters }) = &def.affected {
        assert_eq!(filters.len(), 2);
        for f in filters {
            if let TargetFilter::Typed(tf) = f {
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.properties.contains(&FilterProp::Another));
                assert!(tf.get_subtype() == Some("Ninja") || tf.get_subtype() == Some("Rogue"));
            } else {
                panic!("Expected Typed filter in Or, got {f:?}");
            }
        }
    } else {
        panic!("Expected Or filter, got {:?}", def.affected);
    }
}

#[test]
fn static_elf_or_warrior_creatures_you_control_have_trample() {
    let def = parse_static_line("Elf or Warrior creatures you control have trample.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    if let Some(TargetFilter::Or { filters }) = &def.affected {
        assert_eq!(filters.len(), 2);
    } else {
        panic!("Expected Or filter, got {:?}", def.affected);
    }
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Trample,
        }));
}

#[test]
fn static_parse_for_each_attached_to_self_kellan() {
    // CR 301.5 + CR 303.4: Kellan, the Fae-Blooded — "Other creatures you
    // control get +1/+0 for each Aura and Equipment attached to ~." The
    // multiplier was previously dropped (boost frozen at +1/+0); now the
    // for-each clause emits an `AddDynamicPower` over an `ObjectCount`
    // filtered by `AttachedToSource` so the boost scales with attachments.
    let result = parse_static_line(
        "Other creatures you control get +1/+0 for each Aura and Equipment attached to ~.",
    );
    let def = result.expect("Kellan static must parse");
    assert_eq!(def.mode, StaticMode::Continuous);
    let dynamic_power = def
        .modifications
        .iter()
        .find_map(|m| match m {
            ContinuousModification::AddDynamicPower { value } => Some(value),
            _ => None,
        })
        .expect("expected AddDynamicPower");
    match dynamic_power {
        QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        } => match filter {
            TargetFilter::Typed(tf) => {
                assert!(
                    tf.properties.contains(&FilterProp::AttachedToSource),
                    "filter must carry AttachedToSource, got {:?}",
                    tf.properties
                );
            }
            other => panic!("expected Typed filter, got {other:?}"),
        },
        other => panic!("expected ObjectCount Ref, got {other:?}"),
    }
}

#[test]
fn static_parse_for_each_clause_other_creature() {
    // Verify parse_for_each_clause handles "other creature you control"
    let result =
        crate::parser::oracle_quantity::parse_for_each_clause("other creature you control");
    assert!(
        result.is_some(),
        "parse_for_each_clause should handle 'other creature you control'"
    );
    assert!(
        matches!(result.unwrap(), QuantityRef::ObjectCount { .. }),
        "Expected ObjectCount"
    );
}

#[test]
fn static_rampant_frogantua_lost_game_multiplier() {
    let def =
        parse_static_line("This creature gets +10/+10 for each player who has lost the game.")
            .expect("Rampant Frogantua lost-game static should parse");
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(
        def.modifications.iter().any(|m| {
            matches!(
                m,
                ContinuousModification::AddDynamicPower { value, .. }
                    | ContinuousModification::AddDynamicToughness { value, .. }
                    if matches!(
                        value,
                        QuantityExpr::Multiply {
                            factor: 10,
                            inner,
                        } if matches!(
                            inner.as_ref(),
                            QuantityExpr::Ref {
                                qty: QuantityRef::PlayerCount {
                                    filter: PlayerFilter::HasLostTheGame,
                                },
                            }
                        )
                    )
            )
        }),
        "expected +10/+10 keyed on HasLostTheGame, got {:?}",
        def.modifications
    );
}

#[test]
fn static_self_gets_dynamic_power_for_each_creature() {
    // CR 613.4c: "~ gets +1/+0 for each other creature you control"
    let result = parse_static_line("~ gets +1/+0 for each other creature you control.");
    assert!(result.is_some(), "Should parse 'gets +N/+M for each'");
    let def = result.unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(
        def.modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::AddDynamicPower { .. })),
        "Expected AddDynamicPower, got {:?}",
        def.modifications
    );
    // Should NOT have AddDynamicToughness since toughness is +0
    assert!(
        !def.modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::AddDynamicToughness { .. })),
        "Should not have AddDynamicToughness for +0"
    );
}

#[test]
fn static_self_gets_dynamic_pt_for_each_unspent_green_mana() {
    // CR 106.4 + CR 613.4c: Omnath, Locus of Mana — "~ gets +1/+1 for each
    // unspent green mana you have." The per-mana multiplier was dropped (frozen
    // at +1/+1); now both P/T modifications scale dynamically with the floating
    // green mana in the controller's pool via `QuantityRef::UnspentMana`.
    use crate::types::ability::QuantityRef;
    let def = parse_static_line("~ gets +1/+1 for each unspent green mana you have.")
        .expect("Omnath dynamic P/T static must parse");
    assert_eq!(def.mode, StaticMode::Continuous);

    let expected = QuantityExpr::Ref {
        qty: QuantityRef::UnspentMana {
            color: Some(ManaColor::Green),
        },
    };
    let dynamic_power = def
        .modifications
        .iter()
        .find_map(|m| match m {
            ContinuousModification::AddDynamicPower { value } => Some(value),
            _ => None,
        })
        .expect("expected AddDynamicPower");
    assert_eq!(
        dynamic_power, &expected,
        "power scales with unspent green mana"
    );
    let dynamic_toughness = def
        .modifications
        .iter()
        .find_map(|m| match m {
            ContinuousModification::AddDynamicToughness { value } => Some(value),
            _ => None,
        })
        .expect("expected AddDynamicToughness");
    assert_eq!(dynamic_toughness, &expected, "toughness scales too");
}

#[test]
fn for_each_clause_unspent_mana() {
    // Building-block: the for-each quantity parser maps "unspent <color> mana
    // you have" to UnspentMana{color}, and the colorless "unspent mana you have"
    // to UnspentMana{None} (all colors).
    use crate::types::ability::QuantityRef;
    assert_eq!(
        crate::parser::oracle_quantity::parse_for_each_clause("unspent green mana you have"),
        Some(QuantityRef::UnspentMana {
            color: Some(ManaColor::Green),
        }),
    );
    assert_eq!(
        crate::parser::oracle_quantity::parse_for_each_clause("unspent mana you have"),
        Some(QuantityRef::UnspentMana { color: None }),
    );
}

#[test]
fn static_self_gets_dynamic_pt_for_each_permanent_you_control_but_dont_own() {
    let def = parse_static_line("~ gets +1/+1 for each land you control but don't own.")
        .expect("control-without-ownership dynamic P/T static must parse");
    assert_eq!(def.mode, StaticMode::Continuous);

    let dynamic_power = def
        .modifications
        .iter()
        .find_map(|m| match m {
            ContinuousModification::AddDynamicPower { value } => Some(value),
            _ => None,
        })
        .expect("expected AddDynamicPower");
    match dynamic_power {
        QuantityExpr::Ref {
            qty:
                QuantityRef::ObjectCount {
                    filter: TargetFilter::And { filters },
                },
        } => {
            assert!(matches!(
                filters.first(),
                Some(TargetFilter::Typed(TypedFilter {
                    type_filters,
                    controller: Some(ControllerRef::You),
                    ..
                })) if type_filters == &vec![TypeFilter::Land]
            ));
            assert!(matches!(filters.get(1), Some(TargetFilter::Not { .. })));
        }
        other => panic!("expected ObjectCount over And filter, got {other:?}"),
    }
    assert!(
        def.modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::AddDynamicToughness { .. })),
        "expected AddDynamicToughness, got {:?}",
        def.modifications
    );
}

#[test]
fn dynamic_pt_in_text_x_over_0_without_where_clause_defaults_to_cost_x_paid() {
    // CR 107.3i: Kessig Wolf Run's activated ability text "Target creature
    // gets +X/+0 and gains trample until end of turn." has no "where X is …"
    // binding clause, so X in the effect refers to the value chosen for
    // the ability's cost. `parse_dynamic_pt_in_text` previously gated the
    // entire dynamic-PT path on a required `where_x_expression`, silently
    // dropping the +X/+0 modification. The fix defaults the X-bound
    // quantity to `QuantityRef::CostXPaid` when no clause is present.
    let mods = parse_dynamic_pt_in_text(
        "target creature gets +x/+0 and gains trample until end of turn.",
        None,
    )
    .expect("dynamic-PT helper must emit modifications without a where-X clause");

    let dyn_pow = mods
        .iter()
        .find_map(|m| match m {
            ContinuousModification::AddDynamicPower { value } => Some(value),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected AddDynamicPower; got mods: {mods:?}"));
    assert!(
        matches!(
            dyn_pow,
            QuantityExpr::Ref {
                qty: QuantityRef::CostXPaid
            }
        ),
        "expected QuantityExpr::Ref(CostXPaid), got {dyn_pow:?}"
    );

    // No AddDynamicToughness — the +0 leg must not emit a modification.
    assert!(
        !mods
            .iter()
            .any(|m| matches!(m, ContinuousModification::AddDynamicToughness { .. })),
        "must not emit AddDynamicToughness for the +0 leg, got {mods:?}"
    );
}

#[test]
fn dynamic_pt_in_text_x_over_x_without_where_clause_defaults_both_to_cost_x_paid() {
    // CR 107.3i: When neither leg has a "where X is …" binding, both
    // AddDynamicPower and AddDynamicToughness must default to
    // `QuantityRef::CostXPaid`. Covers the symmetric +X/+X pump variant.
    let mods = parse_dynamic_pt_in_text("target creature gets +x/+x until end of turn.", None)
        .expect("symmetric +X/+X must emit modifications without a where-X clause");

    let dyn_pow = mods
        .iter()
        .find_map(|m| match m {
            ContinuousModification::AddDynamicPower { value } => Some(value),
            _ => None,
        })
        .expect("expected AddDynamicPower");
    assert!(
        matches!(
            dyn_pow,
            QuantityExpr::Ref {
                qty: QuantityRef::CostXPaid
            }
        ),
        "power must be Ref(CostXPaid), got {dyn_pow:?}"
    );

    let dyn_tou = mods
        .iter()
        .find_map(|m| match m {
            ContinuousModification::AddDynamicToughness { value } => Some(value),
            _ => None,
        })
        .expect("expected AddDynamicToughness");
    assert!(
        matches!(
            dyn_tou,
            QuantityExpr::Ref {
                qty: QuantityRef::CostXPaid
            }
        ),
        "toughness must be Ref(CostXPaid), got {dyn_tou:?}"
    );
}

#[test]
fn dynamic_pt_in_text_x_over_0_with_where_clause_still_uses_where_clause() {
    // CR 107.3i regression guard: when an explicit "where X is …" clause
    // is present, the dynamic-PT branch must still resolve X via that
    // clause (here, an ObjectCount) and NOT fall back to CostXPaid. This
    // protects every existing dynamic-PT card (Craterhoof Behemoth-style)
    // from being silently rewritten to read the cost-X channel.
    let mods = parse_dynamic_pt_in_text(
        "target creature gets +x/+0 until end of turn",
        Some("the number of creatures you control"),
    )
    .expect("where-X branch must still emit modifications");

    let dyn_pow = mods
        .iter()
        .find_map(|m| match m {
            ContinuousModification::AddDynamicPower { value } => Some(value),
            _ => None,
        })
        .expect("expected AddDynamicPower");
    match dyn_pow {
        QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        } => match filter {
            TargetFilter::Typed(TypedFilter {
                type_filters,
                controller,
                ..
            }) => {
                assert_eq!(type_filters.as_slice(), [TypeFilter::Creature]);
                assert_eq!(controller.as_ref(), Some(&ControllerRef::You));
            }
            other => panic!("expected Typed(Creature, You) filter, got {other:?}"),
        },
        QuantityExpr::Ref {
            qty: QuantityRef::CostXPaid,
        } => panic!(
            "where-X clause must take precedence over CostXPaid default; \
                 parser regressed to CostXPaid"
        ),
        other => panic!("expected Ref(ObjectCount), got {other:?}"),
    }
}

#[test]
fn dynamic_pt_in_text_minus_x_over_0_without_where_clause_defaults_to_cost_x_paid() {
    // CR 107.3i: Negated +X/+0 mirrors the positive variant — when no
    // "where X is …" clause is present, X binds to the activated ability's
    // cost-X (`QuantityRef::CostXPaid`). The `-X` leg wraps that ref in
    // `QuantityExpr::Multiply { factor: -1, .. }` per the sign-handling
    // block in `parse_dynamic_pt_in_text`. The `-0` leg must NOT emit an
    // `AddDynamicToughness` modification.
    let mods = parse_dynamic_pt_in_text("target creature gets -x/-0 until end of turn.", None)
        .expect("dynamic-PT helper must emit modifications for -X/-0 without a where-X clause");

    let dyn_pow = mods
        .iter()
        .find_map(|m| match m {
            ContinuousModification::AddDynamicPower { value } => Some(value),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected AddDynamicPower; got mods: {mods:?}"));
    match dyn_pow {
        QuantityExpr::Multiply { factor: -1, inner } => assert!(
            matches!(
                inner.as_ref(),
                QuantityExpr::Ref {
                    qty: QuantityRef::CostXPaid
                }
            ),
            "expected Multiply {{ factor: -1, inner: Ref(CostXPaid) }}, got inner={inner:?}"
        ),
        other => {
            panic!("expected Multiply {{ factor: -1, inner: Ref(CostXPaid) }}, got {other:?}")
        }
    }

    // No AddDynamicToughness — the -0 leg must not emit a modification.
    assert!(
        !mods
            .iter()
            .any(|m| matches!(m, ContinuousModification::AddDynamicToughness { .. })),
        "must not emit AddDynamicToughness for the -0 leg, got {mods:?}"
    );
}

#[test]
fn dynamic_pt_in_text_minus_x_over_minus_x_without_where_clause_defaults_both_to_cost_x_paid() {
    // CR 107.3i: Symmetric -X/-X with no binding clause must default both
    // legs to `QuantityRef::CostXPaid` wrapped in
    // `QuantityExpr::Multiply { factor: -1, .. }` per the sign-handling
    // block in `parse_dynamic_pt_in_text`.
    let mods = parse_dynamic_pt_in_text("target creature gets -x/-x until end of turn.", None)
        .expect("symmetric -X/-X must emit modifications without a where-X clause");

    let dyn_pow = mods
        .iter()
        .find_map(|m| match m {
            ContinuousModification::AddDynamicPower { value } => Some(value),
            _ => None,
        })
        .expect("expected AddDynamicPower");
    match dyn_pow {
        QuantityExpr::Multiply { factor: -1, inner } => assert!(
            matches!(
                inner.as_ref(),
                QuantityExpr::Ref {
                    qty: QuantityRef::CostXPaid
                }
            ),
            "power must be Multiply {{ factor: -1, inner: Ref(CostXPaid) }}, got inner={inner:?}"
        ),
        other => {
            panic!("power must be Multiply {{ factor: -1, inner: Ref(CostXPaid) }}, got {other:?}")
        }
    }

    let dyn_tou = mods
        .iter()
        .find_map(|m| match m {
            ContinuousModification::AddDynamicToughness { value } => Some(value),
            _ => None,
        })
        .expect("expected AddDynamicToughness");
    match dyn_tou {
            QuantityExpr::Multiply { factor: -1, inner } => assert!(
                matches!(
                    inner.as_ref(),
                    QuantityExpr::Ref {
                        qty: QuantityRef::CostXPaid
                    }
                ),
                "toughness must be Multiply {{ factor: -1, inner: Ref(CostXPaid) }}, got inner={inner:?}"
            ),
            other => panic!(
                "toughness must be Multiply {{ factor: -1, inner: Ref(CostXPaid) }}, got {other:?}"
            ),
        }
}

#[test]
fn dynamic_pt_in_text_plus_0_over_plus_x_without_where_clause_defaults_to_cost_x_paid() {
    // CR 107.3i: Toughness-only asymmetric +0/+X must emit a single
    // `AddDynamicToughness` carrying `Ref(CostXPaid)` and NOT emit
    // `AddDynamicPower` — the +0 power leg must drop out per the
    // `if p_is_x` guard in `parse_dynamic_pt_in_text`.
    let mods = parse_dynamic_pt_in_text("target creature gets +0/+x until end of turn.", None)
        .expect("dynamic-PT helper must emit modifications for +0/+X without a where-X clause");

    let dyn_tou = mods
        .iter()
        .find_map(|m| match m {
            ContinuousModification::AddDynamicToughness { value } => Some(value),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected AddDynamicToughness; got mods: {mods:?}"));
    assert!(
        matches!(
            dyn_tou,
            QuantityExpr::Ref {
                qty: QuantityRef::CostXPaid
            }
        ),
        "expected QuantityExpr::Ref(CostXPaid), got {dyn_tou:?}"
    );

    // No AddDynamicPower — the +0 leg must not emit a modification.
    assert!(
        !mods
            .iter()
            .any(|m| matches!(m, ContinuousModification::AddDynamicPower { .. })),
        "must not emit AddDynamicPower for the +0 leg, got {mods:?}"
    );
}

#[test]
fn static_reduce_ability_cost_ninjutsu() {
    // CR 601.2f: "Ninjutsu abilities you activate cost {1} less to activate"
    let def = parse_static_line("Ninjutsu abilities you activate cost {1} less to activate.")
        .expect("should parse ReduceAbilityCost");
    assert!(
        matches!(
            def.mode,
            StaticMode::ReduceAbilityCost {
                ref keyword,
                amount: 1,
                minimum_mana: None,
                dynamic_count: None,
            } if keyword == "ninjutsu"
        ),
        "Expected ReduceAbilityCost {{ keyword: ninjutsu, amount: 1 }}, got {:?}",
        def.mode
    );
}

#[test]
fn static_reduce_equip_abilities_with_object_qualifier() {
    let def = parse_static_line(
        "Equip abilities you activate of other Equipment cost {1} less to activate.",
    )
    .expect("should parse ReduceAbilityCost");
    assert_eq!(
        def.mode,
        StaticMode::ReduceAbilityCost {
            keyword: "equip".to_string(),
            amount: 1,
            minimum_mana: None,
            dynamic_count: None,
        }
    );
}

// --- Phase 33-01: Conditional, dynamic, and non-standard enchanted/equipped patterns ---

#[test]
fn static_enchanted_creature_has_keyword_as_long_as_control() {
    // Conditional grant: "enchanted creature has flying as long as you control a Wizard"
    let def = parse_static_line("Enchanted creature has flying as long as you control a Wizard.")
        .expect("should parse conditional enchanted grant");
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
        ))
    );
    assert!(
        def.modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Flying,
            }),
        "Expected AddKeyword(Flying), got {:?}",
        def.modifications
    );
    assert!(
        matches!(def.condition, Some(StaticCondition::IsPresent { .. })),
        "Expected IsPresent condition, got {:?}",
        def.condition
    );
}

#[test]
fn static_as_long_as_enchanted_permanent_is_creature_sets_attached_condition() {
    let def = parse_static_line(
        "As long as enchanted permanent is a creature, enchanted creature gets +1/+1.",
    )
    .expect("should parse attached-object condition");
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: 1 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddToughness { value: 1 }));
    match def.condition {
        Some(StaticCondition::IsPresent {
            filter: Some(TargetFilter::Typed(tf)),
        }) => {
            assert!(tf.properties.contains(&FilterProp::EnchantedBy));
            assert!(tf.type_filters.contains(&TypeFilter::Permanent));
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
        }
        other => panic!("expected attached-object IsPresent condition, got {other:?}"),
    }
}

#[test]
fn static_as_long_as_equipped_creature_is_legendary_grants_to_equipped_creature() {
    let def = parse_static_line("As long as equipped creature is legendary, it has hexproof.")
        .expect("should parse attached-subject inverted grant");
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(TypedFilter::creature().properties(
            vec![
                FilterProp::EquippedBy,
                FilterProp::HasSupertype {
                    value: Supertype::Legendary,
                },
            ]
        )))
    );
    assert!(
        def.modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Hexproof,
            }),
        "Expected AddKeyword(Hexproof), got {:?}",
        def.modifications
    );
    assert_eq!(def.condition, None);
}

#[test]
fn static_as_long_as_enchanted_creature_is_legendary_grants_to_enchanted_creature() {
    let def = parse_static_line(
        "As long as enchanted creature is legendary, it gets +1/+1 and has ward {1}.",
    )
    .expect("should parse enchanted-subject inverted grant");
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(TypedFilter::creature().properties(
            vec![
                FilterProp::EnchantedBy,
                FilterProp::HasSupertype {
                    value: Supertype::Legendary,
                },
            ]
        )))
    );
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: 1 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddToughness { value: 1 }));
    assert!(
        def.modifications.iter().any(|modification| matches!(
            modification,
            ContinuousModification::AddKeyword {
                keyword: Keyword::Ward { .. },
            }
        )),
        "Expected AddKeyword(Ward), got {:?}",
        def.modifications
    );
    assert_eq!(def.condition, None);
}

// CR 105.2b + CR 613: the inverted attached-subject grant must bind its
// `affected` filter to the enchanted creature for ANY characteristic, not just
// `legendary`. A color qualifier ("is white") previously fell through and left
// `affected = SelfRef` (the Aura), so the grant never reached the host (#2818,
// Shield of the Oversoul).
#[test]
fn static_as_long_as_enchanted_creature_is_color_grants_to_enchanted_creature() {
    let def =
        parse_static_line("As long as enchanted creature is white, it gets +1/+1 and has flying.")
            .expect("should parse color-qualified inverted attached grant");
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(TypedFilter::creature().properties(
            vec![
                FilterProp::EnchantedBy,
                FilterProp::HasColor {
                    color: ManaColor::White,
                },
            ]
        )))
    );
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: 1 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddToughness { value: 1 }));
    assert!(def.modifications.iter().any(|modification| matches!(
        modification,
        ContinuousModification::AddKeyword {
            keyword: Keyword::Flying,
        }
    )));
    // Qualifier folded into the affected filter — no residual presence condition.
    assert_eq!(def.condition, None);
}

#[test]
fn static_as_long_as_enchanted_creature_is_second_color_grants_to_enchanted_creature() {
    // Shield of the Oversoul's second clause (green → indestructible).
    let def = parse_static_line(
        "As long as enchanted creature is green, it gets +1/+1 and has indestructible.",
    )
    .expect("should parse second color-qualified inverted attached grant");
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(TypedFilter::creature().properties(
            vec![
                FilterProp::EnchantedBy,
                FilterProp::HasColor {
                    color: ManaColor::Green,
                },
            ]
        )))
    );
    assert!(def.modifications.iter().any(|modification| matches!(
        modification,
        ContinuousModification::AddKeyword {
            keyword: Keyword::Indestructible,
        }
    )));
    assert_eq!(def.condition, None);
}

#[test]
fn static_as_long_as_enchanted_creature_is_type_grants_to_enchanted_creature() {
    // A type qualifier folds into the affected filter the same way.
    let def = parse_static_line("As long as enchanted creature is an artifact, it gets +1/+1.")
        .expect("should parse type-qualified inverted attached grant");
    let Some(TargetFilter::Typed(tf)) = def.affected.as_ref() else {
        panic!("expected Typed affected filter, got {:?}", def.affected);
    };
    assert!(tf.properties.contains(&FilterProp::EnchantedBy));
    assert!(tf.type_filters.contains(&TypeFilter::Artifact));
    assert_eq!(def.condition, None);
}

#[test]
fn static_as_long_as_equipped_creature_is_color_grants_to_equipped_creature() {
    let def =
        parse_static_line("As long as equipped creature is red, it gets +1/+1 and has haste.")
            .expect("should parse equipped color-qualified inverted attached grant");
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(TypedFilter::creature().properties(
            vec![
                FilterProp::EquippedBy,
                FilterProp::HasColor {
                    color: ManaColor::Red,
                },
            ]
        )))
    );
    assert_eq!(def.condition, None);
}

#[test]
fn static_enchanted_creature_gets_pt_as_long_as() {
    // Conditional grant: "enchanted creature gets +1/+1 as long as you control a Wizard"
    let def = parse_static_line("Enchanted creature gets +1/+1 as long as you control a Wizard.")
        .expect("should parse conditional enchanted P/T grant");
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
        ))
    );
    assert!(
        def.modifications
            .contains(&ContinuousModification::AddPower { value: 1 }),
        "Expected AddPower(1)"
    );
    assert!(
        def.modifications
            .contains(&ContinuousModification::AddToughness { value: 1 }),
        "Expected AddToughness(1)"
    );
    assert!(
        matches!(def.condition, Some(StaticCondition::IsPresent { .. })),
        "Expected IsPresent condition, got {:?}",
        def.condition
    );
}

#[test]
fn static_enchanted_creature_dynamic_for_each() {
    // Dynamic grant: "enchanted creature gets +1/+1 for each creature you control"
    let def = parse_static_line("Enchanted creature gets +1/+1 for each creature you control.")
        .expect("should parse dynamic enchanted P/T grant");
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
        ))
    );
    assert!(
        def.modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::AddDynamicPower { .. })),
        "Expected AddDynamicPower, got {:?}",
        def.modifications
    );
    assert!(
        def.modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::AddDynamicToughness { .. })),
        "Expected AddDynamicToughness, got {:?}",
        def.modifications
    );
}

#[test]
fn static_enchanted_creature_for_each_its_controllers_hand_is_dynamic() {
    let def =
        parse_static_line("Enchanted creature gets +1/+1 for each card in its controller's hand.")
            .expect("Righteous Authority-style static must parse");
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
        ))
    );

    let dyn_pow = def
        .modifications
        .iter()
        .find_map(|m| match m {
            ContinuousModification::AddDynamicPower { value } => Some(value),
            _ => None,
        })
        .expect("expected AddDynamicPower");
    assert_eq!(
        dyn_pow,
        &QuantityExpr::Ref {
            qty: QuantityRef::HandSize {
                player: PlayerScope::RecipientController,
            },
        }
    );
    assert!(def.modifications.iter().any(|m| matches!(
        m,
        ContinuousModification::AddDynamicToughness {
            value: QuantityExpr::Ref {
                qty: QuantityRef::HandSize {
                    player: PlayerScope::RecipientController
                }
            }
        }
    )));
    assert!(
        !def.modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddPower { .. } | ContinuousModification::AddToughness { .. }
        )),
        "must not emit flat P/T modifications alongside dynamic ones: {:?}",
        def.modifications
    );
}

#[test]
fn static_wordmail_name_word_count_is_recipient_dynamic_pt() {
    let def = parse_static_line("Enchanted creature gets +1/+1 for each word in its name.")
        .expect("Wordmail static must parse");
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
        ))
    );

    let expected = QuantityExpr::Ref {
        qty: QuantityRef::ObjectNameWordCount {
            scope: ObjectScope::Recipient,
        },
    };
    assert!(def.modifications.iter().any(|m| {
        matches!(
            m,
            ContinuousModification::AddDynamicPower { value } if value == &expected
        )
    }));
    assert!(def.modifications.iter().any(|m| {
        matches!(
            m,
            ContinuousModification::AddDynamicToughness { value } if value == &expected
        )
    }));
    assert!(
        !def.modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddPower { .. } | ContinuousModification::AddToughness { .. }
        )),
        "must not emit flat P/T modifications alongside dynamic ones: {:?}",
        def.modifications
    );
}

#[test]
fn static_self_ref_alrund_sum_for_each_emits_dynamic_pt() {
    let def = parse_static_line(
        "~ gets +1/+1 for each card in your hand and each foretold card you own in exile.",
    )
    .expect("Alrund static must parse");
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));

    let dyn_pow = def
        .modifications
        .iter()
        .find_map(|m| match m {
            ContinuousModification::AddDynamicPower { value } => Some(value),
            _ => None,
        })
        .expect("expected dynamic power modification");
    assert!(
        matches!(dyn_pow, QuantityExpr::Sum { exprs } if exprs.len() == 2),
        "expected Sum quantity for Alrund static, got {dyn_pow:?}"
    );
    assert!(
            def.modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::AddDynamicToughness { value } if matches!(value, QuantityExpr::Sum { exprs } if exprs.len() == 2))),
            "expected dynamic toughness Sum, got {:?}",
            def.modifications
        );
    assert!(
        !def.modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddPower { .. } | ContinuousModification::AddToughness { .. }
        )),
        "must not emit flat P/T modifications alongside dynamic ones: {:?}",
        def.modifications
    );
}

#[test]
fn static_self_ref_exact_base_power_object_count_filter() {
    let def = parse_static_line(
        "~ gets +X/+0, where X is the number of other creatures you control with base power 1.",
    )
    .expect("Zinnia-style static must parse");
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));

    let dyn_pow = def
        .modifications
        .iter()
        .find_map(|m| match m {
            ContinuousModification::AddDynamicPower { value } => Some(value),
            _ => None,
        })
        .expect("expected AddDynamicPower for the X scaling");

    let QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(typed),
        },
    } = dyn_pow
    else {
        panic!("expected ObjectCount over Typed filter, got {dyn_pow:?}");
    };
    assert_eq!(typed.controller, Some(ControllerRef::You));
    assert!(typed.type_filters.contains(&TypeFilter::Creature));
    assert!(typed.properties.contains(&FilterProp::Another));
    assert!(typed.properties.contains(&FilterProp::PtComparison {
        stat: PtStat::Power,
        scope: PtValueScope::Base,
        comparator: Comparator::EQ,
        value: QuantityExpr::Fixed { value: 1 },
    }));
}

#[test]
fn static_strong_back_attached_to_recipient_emits_attached_to_recipient_prop() {
    // CR 301.5 + CR 303.4 + CR 613.4c: Strong Back's third static —
    // "Enchanted creature gets +2/+2 for each Aura and Equipment attached
    // to it." The pronoun "it" is anaphoric on the enchanted creature
    // (the per-recipient affected of the boost), not on the Aura source.
    // The static must therefore lower to a `QuantityRef::ObjectCount`
    // whose filter carries `FilterProp::AttachedToRecipient`, NOT
    // `FilterProp::AttachedToSource`. The legacy bug was a flat
    // `AddPower(2) + AddToughness(2)` because the for-each clause did not
    // recognize "attached to it" and the parser fell through to the
    // fixed-P/T fallback.
    let def = parse_static_line(
        "Enchanted creature gets +2/+2 for each Aura and Equipment attached to it.",
    )
    .expect("Strong Back static must parse");
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
        ))
    );

    // Capture the dynamic-power modification's QuantityExpr for inspection.
    let dyn_pow = def
        .modifications
        .iter()
        .find_map(|m| match m {
            ContinuousModification::AddDynamicPower { value } => Some(value),
            _ => None,
        })
        .expect("expected AddDynamicPower for the for-each scaling");

    // The factor-2 multiplier wraps an ObjectCount whose filter carries
    // AttachedToRecipient — confirming the per-recipient referent.
    let inner = match dyn_pow {
        QuantityExpr::Multiply { factor, inner } => {
            assert_eq!(*factor, 2);
            inner.as_ref()
        }
        other => panic!("expected QuantityExpr::Multiply, got {other:?}"),
    };
    match inner {
        QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        } => match filter {
            TargetFilter::Typed(TypedFilter { properties, .. }) => {
                assert!(
                    properties.contains(&FilterProp::AttachedToRecipient),
                    "filter must carry AttachedToRecipient, got {properties:?}"
                );
                assert!(
                    !properties.contains(&FilterProp::AttachedToSource),
                    "filter must NOT carry AttachedToSource (would point at the Aura)"
                );
            }
            other => panic!("expected Typed filter, got {other:?}"),
        },
        other => panic!("expected ObjectCount ref, got {other:?}"),
    }

    // Negative regression: ensure the parser is not also producing a
    // bogus flat `AddPower(2)` alongside the dynamic version. (Layered
    // application would otherwise grant +2 *plus* +2/attached, which is
    // a different bug from the original 0-multiplier symptom but equally
    // wrong.)
    assert!(
        !def.modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::AddPower { .. })),
        "must not emit a flat AddPower alongside AddDynamicPower; got {:?}",
        def.modifications
    );
}

#[test]
fn static_alpha_status_shared_creature_type_emits_dynamic_pt() {
    let def = parse_static_line(
            "Enchanted creature gets +2/+2 for each other creature on the battlefield that shares a creature type with it.",
        )
        .expect("Alpha Status static must parse");
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
        ))
    );

    let dyn_pow = def
        .modifications
        .iter()
        .find_map(|m| match m {
            ContinuousModification::AddDynamicPower { value } => Some(value),
            _ => None,
        })
        .expect("expected AddDynamicPower");

    let inner = match dyn_pow {
        QuantityExpr::Multiply { factor, inner } => {
            assert_eq!(*factor, 2);
            inner.as_ref()
        }
        other => panic!("expected QuantityExpr::Multiply, got {other:?}"),
    };
    match inner {
        QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        } => match filter {
            TargetFilter::Typed(TypedFilter {
                type_filters,
                properties,
                ..
            }) => {
                assert_eq!(type_filters, &vec![TypeFilter::Creature]);
                assert!(properties.iter().any(|prop| prop == &FilterProp::Another));
                assert!(properties.iter().any(|prop| matches!(
                    prop,
                    FilterProp::SharesQuality {
                        quality: SharedQuality::CreatureType,
                        reference: Some(reference),
                        relation: SharedQualityRelation::Shares,
                    } if matches!(reference.as_ref(), TargetFilter::ParentTarget)
                )));
            }
            other => panic!("expected Typed filter, got {other:?}"),
        },
        other => panic!("expected ObjectCount ref, got {other:?}"),
    }

    assert!(def.modifications.iter().any(|m| matches!(
        m,
        ContinuousModification::AddDynamicToughness {
            value: QuantityExpr::Multiply { factor: 2, .. }
        }
    )));
    assert!(
        !def.modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddPower { .. } | ContinuousModification::AddToughness { .. }
        )),
        "must not emit flat P/T modifications alongside dynamic ones: {:?}",
        def.modifications
    );
}

#[test]
fn static_each_creature_shares_at_least_one_type_emits_dynamic_pt() {
    let def = parse_static_line(
            "Each creature gets +1/+1 for each other creature on the battlefield that shares at least one creature type with it.",
        )
        .expect("Coat of Arms static must parse");
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(TypedFilter::creature()))
    );

    let expected = QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                controller: None,
                properties: vec![
                    FilterProp::Another,
                    FilterProp::SharesQuality {
                        quality: SharedQuality::CreatureType,
                        reference: Some(Box::new(TargetFilter::ParentTarget)),
                        relation: SharedQualityRelation::Shares,
                    },
                ],
            }),
        },
    };

    assert!(def.modifications.iter().any(
        |m| matches!(m, ContinuousModification::AddDynamicPower { value } if value == &expected)
    ));
    assert!(def.modifications.iter().any(
        |m| matches!(m, ContinuousModification::AddDynamicToughness { value } if value == &expected)
    ));
    assert!(
        !def.modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddPower { .. } | ContinuousModification::AddToughness { .. }
        )),
        "must not emit flat P/T modifications alongside dynamic ones: {:?}",
        def.modifications
    );
}

#[test]
fn static_for_each_of_its_colors_emits_recipient_color_count() {
    let def = parse_static_line("Each creature you control gets +1/+1 for each of its colors.")
        .expect("color-count anthem static must parse");

    let dyn_pow = def
        .modifications
        .iter()
        .find_map(|m| match m {
            ContinuousModification::AddDynamicPower { value } => Some(value),
            _ => None,
        })
        .expect("expected dynamic power");
    assert_eq!(
        dyn_pow,
        &QuantityExpr::Ref {
            qty: QuantityRef::ObjectColorCount {
                scope: ObjectScope::Recipient,
            },
        }
    );
    assert!(def.modifications.iter().any(|m| matches!(
        m,
        ContinuousModification::AddDynamicToughness {
            value: QuantityExpr::Ref {
                qty: QuantityRef::ObjectColorCount {
                    scope: ObjectScope::Recipient
                }
            }
        }
    )));
    assert!(!def
        .modifications
        .iter()
        .any(|m| matches!(m, ContinuousModification::AddPower { .. })));
}

#[test]
fn static_for_each_mana_symbol_in_its_mana_cost_emits_recipient_symbol_count() {
    let def = parse_static_line(
        "Each creature you control gets +1/+1 for each white mana symbol in its mana cost.",
    )
    .expect("mana-symbol-count anthem static must parse");

    let dyn_pow = def
        .modifications
        .iter()
        .find_map(|m| match m {
            ContinuousModification::AddDynamicPower { value } => Some(value),
            _ => None,
        })
        .expect("expected dynamic power");
    assert_eq!(
        dyn_pow,
        &QuantityExpr::Ref {
            qty: QuantityRef::ManaSymbolsInManaCost {
                scope: ObjectScope::Recipient,
                color: ManaColor::White,
            },
        }
    );
    assert!(def.modifications.iter().any(|m| matches!(
        m,
        ContinuousModification::AddDynamicToughness {
            value: QuantityExpr::Ref {
                qty: QuantityRef::ManaSymbolsInManaCost {
                    scope: ObjectScope::Recipient,
                    color: ManaColor::White,
                }
            }
        }
    )));
    assert!(!def
        .modifications
        .iter()
        .any(|m| matches!(m, ContinuousModification::AddPower { .. })));
}

#[test]
fn static_enchanted_creature_dynamic_where_x() {
    // Dynamic grant: "enchanted creature gets +X/+X, where X is the number of cards in your hand"
    let def = parse_static_line(
        "Enchanted creature gets +X/+X, where X is the number of cards in your hand.",
    )
    .expect("should parse dynamic enchanted where-X grant");
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
        ))
    );
    assert!(
        def.modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::AddDynamicPower { .. })),
        "Expected AddDynamicPower, got {:?}",
        def.modifications
    );
    assert!(
        def.modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::AddDynamicToughness { .. })),
        "Expected AddDynamicToughness, got {:?}",
        def.modifications
    );
}

#[test]
fn static_enchanted_creature_can_attack_as_though_haste() {
    // Non-standard keyword: "enchanted creature can attack as though it had haste"
    // CR 702.10: Haste-equivalent for aura-granted attack permission.
    let def = parse_static_line("Enchanted creature can attack as though it had haste.")
        .expect("should parse 'can attack as though it had haste'");
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
        ))
    );
    assert!(
        def.modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Haste,
            }),
        "Expected AddKeyword(Haste), got {:?}",
        def.modifications
    );
}

#[test]
fn static_enchanted_creature_cant_be_blocked() {
    // Non-standard: "enchanted creature can't be blocked"
    // CR 509.1b: Unblockable via aura.
    let def = parse_static_line("Enchanted creature can't be blocked.")
        .expect("should parse enchanted can't be blocked");
    assert_eq!(def.mode, StaticMode::CantBeBlocked);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
        ))
    );
}

// --- MustAttack / MustBlock combat requirement pattern tests ---

#[test]
fn static_must_attack_each_combat_if_able() {
    let def = parse_static_line("This creature must attack each combat if able.").unwrap();
    assert_eq!(def.mode, StaticMode::MustAttack);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
}

#[test]
fn static_no_more_than_one_creature_can_attack_each_combat() {
    let def = parse_static_line("No more than one creature can attack each combat.").unwrap();
    assert_eq!(def.mode, StaticMode::MaxAttackersEachCombat { max: 1 });
}

#[test]
fn static_no_more_than_two_creatures_can_attack_each_combat() {
    let def = parse_static_line("No more than two creatures can attack each combat.").unwrap();
    assert_eq!(def.mode, StaticMode::MaxAttackersEachCombat { max: 2 });
}

#[test]
fn static_no_more_than_one_creature_can_block_each_combat() {
    let def = parse_static_line("No more than one creature can block each combat.").unwrap();
    assert_eq!(def.mode, StaticMode::MaxBlockersEachCombat { max: 1 });
}

#[test]
fn static_attacks_or_blocks_each_combat_if_able_emits_both_defs() {
    let direct = try_parse_scoped_must_attack_block(
        "this creature attacks or blocks each combat if able.",
        "This creature attacks or blocks each combat if able.",
    );
    assert!(direct.is_some(), "direct scoped parser failed");
    let defs = parse_static_line_multi("This creature attacks or blocks each combat if able.");

    assert_eq!(defs.len(), 2);
    assert_eq!(defs[0].mode, StaticMode::MustAttack);
    assert_eq!(defs[1].mode, StaticMode::MustBlock);
    assert!(defs
        .iter()
        .all(|def| def.affected == Some(TargetFilter::SelfRef)));
}

#[test]
fn static_attacks_each_turn_if_able() {
    let def = parse_static_line("Enchanted creature attacks each turn if able.").unwrap();
    assert_eq!(def.mode, StaticMode::MustAttack);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
        ))
    );
}

#[test]
fn static_equipped_creature_regression() {
    // Regression: existing equipped creature pattern still works.
    let def = parse_static_line("Equipped creature has first strike and lifelink.")
        .expect("should parse equipped creature keywords");
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EquippedBy]),
        ))
    );
    assert!(
        def.modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::FirstStrike,
            }),
        "Expected AddKeyword(FirstStrike)"
    );
    assert!(
        def.modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Lifelink,
            }),
        "Expected AddKeyword(Lifelink)"
    );
}

#[test]
fn static_enchanted_creature_gets_pt_regression() {
    // Regression: basic enchanted creature P/T pattern still works.
    let def = parse_static_line("Enchanted creature gets +2/+2.")
        .expect("should parse enchanted creature P/T");
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
        ))
    );
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: 2 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddToughness { value: 2 }));
}

// --- Lord pattern tests (Plan 33-02) ---

#[test]
fn lord_bare_creatures_have_keyword() {
    // "Creatures you control have vigilance" (e.g., Brave the Sands)
    let result = parse_static_line("Creatures you control have vigilance.");
    assert!(result.is_some(), "should parse bare keyword lord");
    let def = result.unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    // Verify affected filter is creature + controller You
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert_eq!(tf.controller, Some(ControllerRef::You));
        }
        _ => panic!("Expected Typed creature filter with controller You"),
    }
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Vigilance,
        }));
}

#[test]
fn lord_other_creatures_have_keyword() {
    // CR 613.7: "Other creatures you control have hexproof" (e.g., Shalai, Voice of Plenty)
    // Must produce Continuous with AddKeyword(Hexproof) and Another filter to exclude self.
    let result = parse_static_line("Other creatures you control have hexproof.");
    assert!(
        result.is_some(),
        "should parse other creatures keyword lord"
    );
    let def = result.unwrap();
    assert!(matches!(def.mode, StaticMode::Continuous), "not continuous");
    let has_hexproof = def.modifications.iter().any(|m| {
        matches!(
            m,
            ContinuousModification::AddKeyword {
                keyword: Keyword::Hexproof
            }
        )
    });
    assert!(has_hexproof, "no hexproof keyword");
    // CR 613.7: "Other" means the static excludes the source permanent itself.
    let has_another = match &def.affected {
        Some(TargetFilter::Typed(tf)) => tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::Another)),
        _ => false,
    };
    assert!(has_another, "no Another property for 'other' lord");
}

#[test]
fn lord_subtype_creatures_have_keyword() {
    // "Pirate creatures you control have menace" (e.g., Dire Fleet Neckbreaker variant)
    let result = parse_static_line("Pirate creatures you control have menace.");
    assert!(result.is_some(), "should parse subtype keyword lord");
    let def = result.unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Menace,
        }));
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf
                .type_filters
                .contains(&TypeFilter::Subtype("Pirate".to_string())));
            assert_eq!(tf.controller, Some(ControllerRef::You));
        }
        _ => panic!("Expected Typed filter"),
    }
}

#[test]
fn lord_conditional_as_long_as_control() {
    // "As long as you control a Wizard, creatures you control get +1/+1"
    // (e.g., Adeliz, the Cinder Wind variant)
    let result =
        parse_static_line("As long as you control a Wizard, creatures you control get +1/+1.");
    assert!(result.is_some(), "should parse conditional lord");
    let def = result.unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: 1 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddToughness { value: 1 }));
    assert!(def.condition.is_some(), "Expected a StaticCondition");
    match def.condition {
        Some(StaticCondition::IsPresent { .. }) => {}
        _ => panic!("Expected IsPresent condition"),
    }
}

#[test]
fn lord_each_creature_with_keyword() {
    // "Each creature you control with flying gets +1/+1"
    // (e.g., Favorable Winds, Empyrean Eagle)
    let result = parse_static_line("Each creature you control with flying gets +1/+1.");
    assert!(result.is_some(), "should parse keyword-filtered lord");
    let def = result.unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: 1 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddToughness { value: 1 }));
    // Should have a filter with WithKeyword for flying
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.properties.contains(&FilterProp::WithKeyword {
                value: Keyword::Flying,
            }));
        }
        _ => panic!("Expected Typed filter with keyword property"),
    }
}

#[test]
fn lord_other_zombie_creatures_regression() {
    // Regression: "Other Zombie creatures you control get +1/+1" still works
    let result = parse_static_line("Other Zombie creatures you control get +1/+1.");
    assert!(result.is_some(), "should parse other subtype lord");
    let def = result.unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: 1 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddToughness { value: 1 }));
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf
                .type_filters
                .contains(&TypeFilter::Subtype("Zombie".to_string())));
            assert!(tf.properties.contains(&FilterProp::Another));
        }
        _ => panic!("Expected Typed filter"),
    }
}

#[test]
fn enchanted_land_is_a_mountain_produces_set_basic_land_type() {
    let def = parse_static_line("Enchanted land is a Mountain.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(matches!(
        def.modifications.as_slice(),
        [ContinuousModification::SetBasicLandType { land_type }]
        if *land_type == BasicLandType::Mountain
    ));
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Land));
            assert!(tf.properties.contains(&FilterProp::EnchantedBy));
        }
        _ => panic!("Expected Typed land filter with EnchantedBy"),
    }
}

#[test]
fn enchanted_land_is_a_plains_produces_set_basic_land_type() {
    let def = parse_static_line("Enchanted land is a Plains.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(matches!(
        def.modifications.as_slice(),
        [ContinuousModification::SetBasicLandType { land_type }]
        if *land_type == BasicLandType::Plains
    ));
}

#[test]
fn enchanted_land_is_a_forest_in_addition_produces_add_subtype() {
    let def =
        parse_static_line("Enchanted land is a Forest in addition to its other types.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.modifications,
        vec![ContinuousModification::AddSubtype {
            subtype: "Forest".to_string(),
        }]
    );
}

#[test]
fn enchanted_land_is_a_swamp_in_addition_produces_add_subtype() {
    let def =
        parse_static_line("Enchanted land is a Swamp in addition to its other types.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.modifications,
        vec![ContinuousModification::AddSubtype {
            subtype: "Swamp".to_string(),
        }]
    );
}

/// CR 205.3 + CR 700.8: Self type-grant Oxford-comma party subtype list.
/// Source acquires all four party subtypes so it counts itself toward the
/// controller's party regardless of its printed subtypes.
#[test]
fn self_is_also_a_four_party_subtypes() {
    let def = parse_static_line("~ is also a Cleric, Rogue, Warrior, and Wizard.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert_eq!(
        def.modifications,
        vec![
            ContinuousModification::AddSubtype {
                subtype: "Cleric".to_string(),
            },
            ContinuousModification::AddSubtype {
                subtype: "Rogue".to_string(),
            },
            ContinuousModification::AddSubtype {
                subtype: "Warrior".to_string(),
            },
            ContinuousModification::AddSubtype {
                subtype: "Wizard".to_string(),
            },
        ]
    );
}

/// CR 205.3: Single-subtype self type-grant (e.g. "Kentaro, the Smiling
/// Cat is also a Spirit.") — degenerate one-element list path.
#[test]
fn self_is_also_a_single_subtype() {
    let def = parse_static_line("~ is also a Spirit.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert_eq!(
        def.modifications,
        vec![ContinuousModification::AddSubtype {
            subtype: "Spirit".to_string(),
        }]
    );
}

/// CR 205.3: Vowel-opening subtype — exercises the `"~ is also an "`
/// arm so a future Elf/Angel/Eldrazi/Imp/Otter party-tribal printing
/// (or any other vowel-opening self-typegrant) reaches the parser via
/// the classifier's `"is also an "` contains pattern instead of being
/// dropped on the floor.
#[test]
fn self_is_also_an_vowel_opening_subtype_list() {
    let def = parse_static_line("~ is also an Elf, Angel, and Eldrazi.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert_eq!(
        def.modifications,
        vec![
            ContinuousModification::AddSubtype {
                subtype: "Elf".to_string(),
            },
            ContinuousModification::AddSubtype {
                subtype: "Angel".to_string(),
            },
            ContinuousModification::AddSubtype {
                subtype: "Eldrazi".to_string(),
            },
        ]
    );
}

/// CR 205.3d: Non-creature subtypes ("X is also a Forest" / "is also an
/// Aura") must not be silently added to the source — the pithy
/// `is also a[n]` phrasing is exclusively creature-subtype grants, and
/// land/artifact/enchantment-subtype additions use the
/// `in addition to its other types` phrasing handled by
/// `parse_subject_additive_type_static`. The arm must return None so
/// other parser arms can claim the line.
#[test]
fn self_is_also_a_rejects_non_creature_subtype() {
    assert!(parse_static_line("~ is also a Forest.").is_none());
    assert!(parse_static_line("~ is also an Aura.").is_none());
    assert!(parse_static_line("~ is also an Equipment.").is_none());
}

/// CR 205.3: Two-subtype list without Oxford comma — `<X> and <Y>`.
/// Exercises the bare " and " separator without intermediate comma.
#[test]
fn self_is_also_a_two_subtypes_no_comma() {
    let def = parse_static_line("~ is also a Spirit and Wizard.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert_eq!(
        def.modifications,
        vec![
            ContinuousModification::AddSubtype {
                subtype: "Spirit".to_string(),
            },
            ContinuousModification::AddSubtype {
                subtype: "Wizard".to_string(),
            },
        ]
    );
}

#[test]
fn darksteel_mutation_full_modification_set() {
    // CR 205.1a/b + CR 613.1d/f: the " with base power and toughness N/N "
    // split must not discard the "and has indestructible, and it loses all
    // other ..." clause.
    let def = parse_static_line(
        "Enchanted creature is an Insect artifact creature with base power and \
             toughness 0/1 and has indestructible, and it loses all other abilities, \
             card types, and creature types.",
    )
    .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    let mods = &def.modifications;
    assert!(
        mods.contains(&ContinuousModification::SetCardTypes {
            core_types: vec![CoreType::Artifact, CoreType::Creature],
        }),
        "expected SetCardTypes[Artifact,Creature], got {mods:?}"
    );
    assert!(mods.contains(&ContinuousModification::RemoveAllAbilities));
    assert!(mods.contains(&ContinuousModification::RemoveAllSubtypes {
        set: crate::types::card_type::SubtypeSet::Creature,
    }));
    assert!(mods.contains(&ContinuousModification::AddSubtype {
        subtype: "Insect".to_string(),
    }));
    assert!(mods.contains(&ContinuousModification::AddKeyword {
        keyword: Keyword::Indestructible,
    }));
    assert!(mods.contains(&ContinuousModification::SetPower { value: 0 }));
    assert!(mods.contains(&ContinuousModification::SetToughness { value: 1 }));
    // CR 613.7 written-order contract: RemoveAllSubtypes must precede the
    // AddSubtype(Insect) so Insect survives the subtype wipe; and
    // RemoveAllAbilities must precede AddKeyword so indestructible survives.
    let pos = |m: &ContinuousModification| mods.iter().position(|x| x == m).unwrap();
    assert!(
        pos(&ContinuousModification::RemoveAllSubtypes {
            set: crate::types::card_type::SubtypeSet::Creature,
        }) < pos(&ContinuousModification::AddSubtype {
            subtype: "Insect".to_string(),
        }),
        "RemoveAllSubtypes must precede AddSubtype(Insect): {mods:?}"
    );
    assert!(
        pos(&ContinuousModification::RemoveAllAbilities)
            < pos(&ContinuousModification::AddKeyword {
                keyword: Keyword::Indestructible,
            }),
        "RemoveAllAbilities must precede AddKeyword: {mods:?}"
    );
}

#[test]
fn enchanted_is_type_with_base_pt_preserves_trailing_keyword_clause() {
    // Building-block check: the trailing "and has <kw> ... loses all
    // abilities" clause survives the base-P/T split.
    let def = parse_static_line(
        "Enchanted creature is a Bear artifact creature with base power and \
             toughness 2/2 and has flying and it loses all other abilities.",
    )
    .unwrap();
    let mods = &def.modifications;
    assert!(mods.contains(&ContinuousModification::AddKeyword {
        keyword: Keyword::Flying,
    }));
    assert!(mods.contains(&ContinuousModification::RemoveAllAbilities));
    assert!(mods.contains(&ContinuousModification::SetPower { value: 2 }));
    assert!(mods.contains(&ContinuousModification::SetToughness { value: 2 }));
}

// --- Land type-changing statics (CR 305.7) ---

#[test]
fn nonbasic_lands_are_mountains_blood_moon() {
    let def = parse_static_line("Nonbasic lands are Mountains.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(matches!(
        def.modifications.as_slice(),
        [ContinuousModification::SetBasicLandType { land_type }]
        if *land_type == BasicLandType::Mountain
    ));
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Land));
            assert!(tf.properties.contains(&FilterProp::NotSupertype {
                value: Supertype::Basic,
            }));
        }
        _ => panic!("Expected Typed nonbasic land filter"),
    }
}

#[test]
fn nonbasic_lands_are_islands_harbinger() {
    let def = parse_static_line("Nonbasic lands are Islands.").unwrap();
    assert!(matches!(
        def.modifications.as_slice(),
        [ContinuousModification::SetBasicLandType { land_type }]
        if *land_type == BasicLandType::Island
    ));
}

#[test]
fn lands_you_control_are_plains_celestial_dawn() {
    let def = parse_static_line("Lands you control are Plains.").unwrap();
    assert!(matches!(
        def.modifications.as_slice(),
        [ContinuousModification::SetBasicLandType { land_type }]
        if *land_type == BasicLandType::Plains
    ));
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Land));
            assert_eq!(tf.controller, Some(ControllerRef::You));
        }
        _ => panic!("Expected Typed land filter with you-control"),
    }
}

#[test]
fn all_mountains_are_plains_conversion() {
    // CR 305.7: Conversion / Glaciers — "All Mountains are Plains" sets every
    // Mountain (any controller) to Plains via SetBasicLandType. Covers the
    // "All <basic land type> are <type>" subject class.
    let def = parse_static_line("All Mountains are Plains.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(matches!(
        def.modifications.as_slice(),
        [ContinuousModification::SetBasicLandType { land_type }]
        if *land_type == BasicLandType::Plains
    ));
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Land));
            assert!(tf
                .type_filters
                .contains(&TypeFilter::Subtype("Mountain".to_string())));
            assert_eq!(tf.controller, None, "battlefield-wide, any controller");
        }
        _ => panic!("Expected Typed land filter with Mountain subtype"),
    }
}

#[test]
fn each_land_is_a_swamp_in_addition_urborg() {
    let def =
        parse_static_line("Each land is a Swamp in addition to its other land types.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.modifications,
        vec![ContinuousModification::AddSubtype {
            subtype: "Swamp".to_string(),
        }]
    );
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Land));
            assert!(tf.controller.is_none());
        }
        _ => panic!("Expected Typed land filter (all lands)"),
    }
}

#[test]
fn all_lands_are_creatures_living_plane() {
    use crate::types::card_type::CoreType;

    // CR 613.1d + CR 613.4b + CR 205.1b: Living Plane / Nature's Revolt —
    // a continuous static animating every land into a creature while it stays
    // a land. P/T is set (Layer 7b), the creature type is added (Layer 4), and
    // the land type is retained (additive types, confirmed by "that are still
    // lands").
    let def = parse_static_line("All lands are 1/1 creatures that are still lands.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    let mods = &def.modifications;
    assert!(
        mods.iter().any(|m| matches!(
            m,
            ContinuousModification::AddType { core_type } if *core_type == CoreType::Creature
        )),
        "must add the Creature type (Layer 4): {mods:?}"
    );
    assert!(
        mods.iter()
            .any(|m| matches!(m, ContinuousModification::SetPower { value } if *value == 1)),
        "must set power to 1 (Layer 7b): {mods:?}"
    );
    assert!(
        mods.iter()
            .any(|m| matches!(m, ContinuousModification::SetToughness { value } if *value == 1)),
        "must set toughness to 1 (Layer 7b): {mods:?}"
    );
    // No SetBasicLandType / type replacement — the land keeps its land type.
    assert!(
        !mods.iter().any(|m| matches!(
            m,
            ContinuousModification::SetBasicLandType { .. }
                | ContinuousModification::SetCardTypes { .. }
        )),
        "must not replace types (CR 205.1b retention): {mods:?}"
    );
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Land));
            assert!(tf.controller.is_none(), "all lands — no controller scope");
        }
        _ => panic!("Expected Typed land filter (all lands)"),
    }
}

#[test]
fn lands_you_control_are_creatures_scope_and_pt() {
    use crate::types::card_type::CoreType;

    // Same building block, controller-scoped subject + a different P/T proves
    // the animation spec (not a hardcoded 1/1) drives the result.
    let def =
        parse_static_line("Lands you control are 2/2 creatures that are still lands.").unwrap();
    let mods = &def.modifications;
    assert!(mods.iter().any(|m| matches!(
        m,
        ContinuousModification::AddType { core_type } if *core_type == CoreType::Creature
    )));
    assert!(mods
        .iter()
        .any(|m| matches!(m, ContinuousModification::SetPower { value } if *value == 2)));
    assert!(mods
        .iter()
        .any(|m| matches!(m, ContinuousModification::SetToughness { value } if *value == 2)));
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Land));
            assert_eq!(tf.controller, Some(ControllerRef::You));
        }
        _ => panic!("Expected Typed land filter scoped to you"),
    }
}

#[test]
fn all_lands_are_islands_in_addition_stormtide() {
    let def = parse_static_line("All lands are Islands in addition to their other types.").unwrap();
    assert_eq!(
        def.modifications,
        vec![ContinuousModification::AddSubtype {
            subtype: "Island".to_string(),
        }]
    );
}

#[test]
fn lands_you_control_every_basic_land_type_prismatic_omen() {
    let def = parse_static_line(
        "Lands you control are every basic land type in addition to their other types.",
    )
    .unwrap();
    assert_eq!(
        def.modifications,
        vec![ContinuousModification::AddAllBasicLandTypes]
    );
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Land));
            assert_eq!(tf.controller, Some(ControllerRef::You));
        }
        _ => panic!("Expected Typed land filter with you-control"),
    }
}

// --- CR 702.73a + CR 205.3: "[subject] {is|are} every creature type" ---

#[test]
fn self_ref_every_creature_type_is_cda() {
    // CR 604.3: Mistform Ultimus / Dr. Julius Jumblemorph — the parenthetical
    // "(even if this card isn't on the battlefield)" is reminder text that
    // the static-line parser already strips. The grant must function in
    // all zones, so the StaticDefinition is flagged as a CDA.
    let def = parse_static_line("~ is every creature type.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(def.characteristic_defining);
    assert_eq!(
        def.modifications,
        vec![ContinuousModification::AddAllCreatureTypes]
    );
    assert!(matches!(def.affected, Some(TargetFilter::SelfRef)));
}

#[test]
fn creatures_you_control_every_creature_type_maskwood_nexus() {
    // CR 702.73a + CR 205.3: Maskwood Nexus first sentence. Filter-subject
    // grant — battlefield-only, not a CDA. Reached via the
    // `parse_continuous_gets_has` → `parse_continuous_modifications` path
    // once the "are every creature type" arm recognizes the predicate.
    let def = parse_static_line("Creatures you control are every creature type.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(!def.characteristic_defining);
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddAllCreatureTypes));
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert_eq!(tf.controller, Some(ControllerRef::You));
        }
        other => panic!("Expected Typed creature with you-control, got {other:?}"),
    }
}

#[test]
fn conjunctive_every_creature_type_arachnoform() {
    // CR 702.73a + CR 613.1d: Aura compound static — Arachnoform.
    // "is every creature type" must not be silently dropped from the
    // modification chain. The +2/+2 and reach modifications are also
    // preserved.
    let def =
        parse_static_line("Enchanted creature gets +2/+2, has reach, and is every creature type.")
            .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: 2 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddToughness { value: 2 }));
    assert!(def
        .modifications
        .iter()
        .any(|m| matches!(m, ContinuousModification::AddKeyword { .. })));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddAllCreatureTypes));
}

#[test]
fn conjunctive_every_creature_type_runed_stalactite() {
    // CR 702.73a + CR 613.1d: Equipment compound static — Runed Stalactite.
    let def =
        parse_static_line("Equipped creature gets +1/+1 and is every creature type.").unwrap();
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: 1 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddToughness { value: 1 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddAllCreatureTypes));
}

#[test]
fn parse_continuous_modifications_picks_up_every_creature_type() {
    // Direct test of the parse_continuous_modifications arm — used by
    // every conjunctive caller (parse_continuous_gets_has,
    // parse_subject_continuous_static, parse_typed_you_control).
    let mods = parse_continuous_modifications("is every creature type");
    assert!(mods.contains(&ContinuousModification::AddAllCreatureTypes));

    let mods_plural = parse_continuous_modifications("are every creature type");
    assert!(mods_plural.contains(&ContinuousModification::AddAllCreatureTypes));
}

#[test]
fn omo_land_every_land_type_is_add_all_land_types() {
    // CR 205.3i + CR 305.7: Omo, Queen of Vesuva — "Each land with an
    // everything counter on it is every land type in addition to its other
    // types." Must produce the additive `AddAllLandTypes` marker, NOT a
    // no-op `AddType { Land }`.
    let def = parse_static_line(
            "Each land with an everything counter on it is every land type in addition to its other types.",
        )
        .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.modifications,
        vec![ContinuousModification::AddAllLandTypes]
    );
    // Regression guard: the old broken parse produced AddType { Land }.
    assert!(!def
        .modifications
        .iter()
        .any(|m| matches!(m, ContinuousModification::AddType { .. })));
    // The affected subject carries the everything-counter FilterProp.
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Land));
            assert!(tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::Counters { .. })));
        }
        other => panic!("Expected Typed land with counter prop, got {other:?}"),
    }
}

#[test]
fn omo_nonland_creature_counter_subject_is_all_creature_types() {
    // CR 205.3 + CR 122.1: Omo, Queen of Vesuva — "Each nonland creature
    // with an everything counter on it is every creature type." The subject
    // is a nonland creature gated on the everything counter; the grant is
    // the existing `AddAllCreatureTypes` modification.
    let def = parse_static_line(
        "Each nonland creature with an everything counter on it is every creature type.",
    )
    .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.modifications,
        vec![ContinuousModification::AddAllCreatureTypes]
    );
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(tf
                .type_filters
                .contains(&TypeFilter::Non(Box::new(TypeFilter::Land))));
            assert!(tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::Counters { .. })));
        }
        other => panic!("Expected Typed nonland creature with counter prop, got {other:?}"),
    }
}

// --- CantCastDuring: turn/phase-scoped casting prohibitions ---

#[test]
fn static_cant_cast_opponents_during_your_turn() {
    // CR 101.2: Teferi, Time Raveler — "Your opponents can't cast spells during your turn."
    let def = parse_static_line("Your opponents can't cast spells during your turn.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantCastDuring {
            who: ProhibitionScope::Opponents,
            when: CastingProhibitionCondition::DuringYourTurn,
        }
    );
}

#[test]
fn static_cant_cast_players_during_combat() {
    // CR 101.2: "Players can't cast spells during combat."
    let def = parse_static_line("Players can't cast spells during combat.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantCastDuring {
            who: ProhibitionScope::AllPlayers,
            when: CastingProhibitionCondition::DuringCombat,
        }
    );
}

#[test]
fn static_cant_cast_from_still_works() {
    // Regression: CantCastFrom (zone-based) must not be affected. "Players" → AllPlayers.
    let def = parse_static_line("Players can't cast spells from graveyards or libraries.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantCastFrom {
            who: ProhibitionScope::AllPlayers,
        }
    );
    // The prohibited zones ride the affected filter via InAnyZone.
    assert!(matches!(
        def.affected,
        Some(TargetFilter::Typed(ref tf))
            if tf.properties.iter().any(|p| matches!(
                p,
                FilterProp::InAnyZone { zones }
                    if zones.contains(&Zone::Graveyard) && zones.contains(&Zone::Library)
            ))
    ));
}

#[test]
fn static_cant_cast_during_serde_roundtrip() {
    let mode = StaticMode::CantCastDuring {
        who: ProhibitionScope::Opponents,
        when: CastingProhibitionCondition::DuringYourTurn,
    };
    let json = serde_json::to_string(&mode).unwrap();
    let deserialized: StaticMode = serde_json::from_str(&json).unwrap();
    assert_eq!(mode, deserialized);
}

#[test]
fn static_cant_cast_during_display_roundtrip() {
    let mode = StaticMode::CantCastDuring {
        who: ProhibitionScope::Opponents,
        when: CastingProhibitionCondition::DuringYourTurn,
    };
    let s = mode.to_string();
    assert_eq!(StaticMode::from_str(&s).unwrap(), mode);

    let mode2 = StaticMode::CantCastDuring {
        who: ProhibitionScope::AllPlayers,
        when: CastingProhibitionCondition::DuringCombat,
    };
    let s2 = mode2.to_string();
    assert_eq!(StaticMode::from_str(&s2).unwrap(), mode2);
}

// --- PerTurnCastLimit tests ---

#[test]
fn per_turn_cast_limit_all_players() {
    // CR 101.2 + CR 604.1: Rule of Law — "Each player can't cast more than one spell each turn."
    let def = parse_static_line("Each player can't cast more than one spell each turn.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::PerTurnCastLimit {
            who: ProhibitionScope::AllPlayers,
            max: 1,
            spell_filter: None,
        }
    );
}

#[test]
fn per_turn_cast_limit_opponents() {
    let def = parse_static_line("Each opponent can't cast more than one spell each turn.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::PerTurnCastLimit {
            who: ProhibitionScope::Opponents,
            max: 1,
            spell_filter: None,
        }
    );
}

#[test]
fn per_turn_cast_limit_controller() {
    let def = parse_static_line("You can't cast more than one spell each turn.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::PerTurnCastLimit {
            who: ProhibitionScope::Controller,
            max: 1,
            spell_filter: None,
        }
    );
}

#[test]
fn per_turn_cast_limit_noncreature_filter() {
    // Deafening Silence: "Each player can't cast more than one noncreature spell each turn."
    let def =
        parse_static_line("Each player can't cast more than one noncreature spell each turn.")
            .unwrap();
    let StaticMode::PerTurnCastLimit {
        who,
        max,
        spell_filter,
    } = &def.mode
    else {
        panic!("expected PerTurnCastLimit");
    };
    assert_eq!(*who, ProhibitionScope::AllPlayers);
    assert_eq!(*max, 1);
    // Filter should be Non(Creature)
    let Some(TargetFilter::Typed(tf)) = spell_filter else {
        panic!("expected typed spell filter, got {spell_filter:?}");
    };
    assert_eq!(
        tf.type_filters,
        vec![TypeFilter::Non(Box::new(TypeFilter::Creature))]
    );
}

#[test]
fn per_turn_cast_limit_max_two() {
    // Fires of Invention (standalone clause): "You can cast no more than two spells each turn."
    let def = parse_static_line("You can cast no more than two spells each turn.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::PerTurnCastLimit {
            who: ProhibitionScope::Controller,
            max: 2,
            spell_filter: None,
        }
    );
}

#[test]
fn per_turn_cast_limit_ethersworn_canonist_nonartifact() {
    // CR 101.2 + CR 604.1: Ethersworn Canonist — conditional-subject phrasing
    // semantically equivalent to "Each player can't cast more than one nonartifact
    // spell each turn." Reduces to PerTurnCastLimit{ AllPlayers, max=1, Non(Artifact) }.
    let def = parse_static_line(
            "Each player who has cast a nonartifact spell this turn can't cast additional nonartifact spells.",
        )
        .unwrap();
    let StaticMode::PerTurnCastLimit {
        who,
        max,
        spell_filter,
    } = &def.mode
    else {
        panic!("expected PerTurnCastLimit, got {:?}", def.mode);
    };
    assert_eq!(*who, ProhibitionScope::AllPlayers);
    assert_eq!(*max, 1);
    let Some(TargetFilter::Typed(tf)) = spell_filter else {
        panic!("expected typed spell filter, got {spell_filter:?}");
    };
    assert_eq!(
        tf.type_filters,
        vec![TypeFilter::Non(Box::new(TypeFilter::Artifact))]
    );
}

#[test]
fn per_turn_cast_limit_conditional_subject_creature_filter() {
    // Class test: same conditional-subject grammar with a different matched
    // type — proves the building block works across the type-filter axis,
    // not just Ethersworn's Non(Artifact). Hypothetical future printed text.
    let def = parse_static_line(
            "Each player who has cast a creature spell this turn can't cast additional creature spells.",
        )
        .unwrap();
    let StaticMode::PerTurnCastLimit {
        who,
        max,
        spell_filter,
    } = &def.mode
    else {
        panic!("expected PerTurnCastLimit, got {:?}", def.mode);
    };
    assert_eq!(*who, ProhibitionScope::AllPlayers);
    assert_eq!(*max, 1);
    let Some(TargetFilter::Typed(tf)) = spell_filter else {
        panic!("expected typed spell filter, got {spell_filter:?}");
    };
    assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
}

#[test]
fn per_turn_cast_limit_conditional_subject_instant_filter() {
    // Class test: third filter axis to lock in the building-block behavior.
    let def = parse_static_line(
        "Each player who has cast an instant spell this turn can't cast additional instant spells.",
    )
    .unwrap();
    let StaticMode::PerTurnCastLimit { spell_filter, .. } = &def.mode else {
        panic!("expected PerTurnCastLimit, got {:?}", def.mode);
    };
    let Some(TargetFilter::Typed(tf)) = spell_filter else {
        panic!("expected typed spell filter, got {spell_filter:?}");
    };
    assert_eq!(tf.type_filters, vec![TypeFilter::Instant]);
}

#[test]
fn per_turn_cast_limit_conditional_subject_each_opponent_scope() {
    // Class test (subject axis): "Each opponent who has cast..." must produce
    // `Opponents` scope, not the hard-coded `AllPlayers`. Proves the subject
    // prefix is dispatched through `strip_casting_prohibition_subject` instead
    // of being inlined. Hypothetical future printed text within the class.
    let def = parse_static_line(
            "Each opponent who has cast a creature spell this turn can't cast additional creature spells.",
        )
        .unwrap();
    let StaticMode::PerTurnCastLimit { who, max, .. } = &def.mode else {
        panic!("expected PerTurnCastLimit, got {:?}", def.mode);
    };
    assert_eq!(*who, ProhibitionScope::Opponents);
    assert_eq!(*max, 1);
}

#[test]
fn per_turn_cast_limit_conditional_subject_plural_agreement() {
    // Sibling coverage: plural subjects use "who have cast", and the parser
    // should still flow through the shared subject and type-filter axes.
    let def = parse_static_line(
        "Players who have cast a creature spell this turn can't cast additional creature spells.",
    )
    .unwrap();
    let StaticMode::PerTurnCastLimit { who, max, .. } = &def.mode else {
        panic!("expected PerTurnCastLimit, got {:?}", def.mode);
    };
    assert_eq!(*who, ProhibitionScope::AllPlayers);
    assert_eq!(*max, 1);
}

#[test]
fn per_turn_cast_limit_conditional_subject_singular_additional_spell() {
    // Sibling coverage: some Oracle-style restrictions use singular
    // "additional [type] spell" rather than plural "spells".
    let def = parse_static_line(
        "Each player who has cast an instant spell this turn can't cast additional instant spell.",
    )
    .unwrap();
    let StaticMode::PerTurnCastLimit { spell_filter, .. } = &def.mode else {
        panic!("expected PerTurnCastLimit, got {:?}", def.mode);
    };
    let Some(TargetFilter::Typed(tf)) = spell_filter else {
        panic!("expected typed spell filter, got {spell_filter:?}");
    };
    assert_eq!(tf.type_filters, vec![TypeFilter::Instant]);
}

#[test]
fn per_turn_cast_limit_conditional_subject_you_scope() {
    // Class test (subject axis): the helper accepts the "you " subject prefix;
    // we lock in
    // the building-block behavior for completeness across the
    // `strip_casting_prohibition_subject` outputs that have a trailing space
    // suitable for the "who have cast" continuation. The "you " arm of the
    // shared subject helper covers cards like Arcane Laboratory variants.
    let def = parse_static_line(
        "You who have cast a creature spell this turn can't cast additional creature spells.",
    )
    .unwrap();
    let StaticMode::PerTurnCastLimit { who, .. } = &def.mode else {
        panic!("expected PerTurnCastLimit, got {:?}", def.mode);
    };
    assert_eq!(*who, ProhibitionScope::Controller);
}

#[test]
fn per_turn_cast_limit_conditional_subject_mismatched_types_rejected() {
    // Defensive: if subject and object types diverge, the max=1 reduction is
    // no longer sound. The parser must not silently mis-model such a card.
    // (No known printed card uses this shape; the test guards future text.)
    let def = parse_static_line(
            "Each player who has cast a creature spell this turn can't cast additional noncreature spells.",
        );
    // Either falls through to a different parser (None preferred) or is not the
    // conditional-subject mode. The key invariant: it must NOT produce a
    // PerTurnCastLimit with one type's filter on the other.
    if let Some(def) = def {
        if let StaticMode::PerTurnCastLimit { .. } = def.mode {
            panic!("mismatched-type conditional subject must not collapse to PerTurnCastLimit");
        }
    }
}

#[test]
fn per_turn_cast_limit_compound_clause() {
    // Fires of Invention: compound "and" clause with per-turn limit in second half
    let def = parse_static_line(
            "You can cast spells only during your turn and you can cast no more than two spells each turn.",
        );
    assert!(def.is_some(), "expected Some for compound clause");
    let def = def.unwrap();
    assert_eq!(
        def.mode,
        StaticMode::PerTurnCastLimit {
            who: ProhibitionScope::Controller,
            max: 2,
            spell_filter: None,
        }
    );
}

#[test]
fn only_during_your_turn_standalone() {
    // CR 117.1a + CR 604.1: "You can cast spells only during your turn."
    let def = parse_static_line("You can cast spells only during your turn.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantCastDuring {
            who: ProhibitionScope::Controller,
            when: CastingProhibitionCondition::NotDuringYourTurn,
        }
    );
}

#[test]
fn per_turn_cast_limit_does_not_affect_cant_cast_during() {
    // Regression: CantCastDuring must still parse correctly
    let def = parse_static_line("Your opponents can't cast spells during your turn.").unwrap();
    assert!(matches!(def.mode, StaticMode::CantCastDuring { .. }));
}

#[test]
fn per_turn_cast_limit_does_not_affect_cant_cast_from() {
    // Regression: CantCastFrom must still parse correctly
    let def = parse_static_line("Players can't cast spells from graveyards or libraries.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantCastFrom {
            who: ProhibitionScope::AllPlayers,
        }
    );
}

#[test]
fn static_cant_cast_from_anywhere_other_than_hand_drannith_magistrate() {
    // CR 601.3 + CR 109.5: Drannith Magistrate — "Your opponents can't cast spells
    // from anywhere other than their hands." Subject → Opponents scope; the inverse
    // "anywhere other than [hand]" clause expands to every cast-capable zone except
    // the hand (graveyard, library, exile, command) on the affected filter.
    let def =
        parse_static_line("Your opponents can't cast spells from anywhere other than their hands.")
            .unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantCastFrom {
            who: ProhibitionScope::Opponents,
        }
    );
    let Some(TargetFilter::Typed(ref tf)) = def.affected else {
        panic!("expected typed affected filter, got {:?}", def.affected);
    };
    let zones = tf
        .properties
        .iter()
        .find_map(|p| match p {
            FilterProp::InAnyZone { zones } => Some(zones.clone()),
            _ => None,
        })
        .expect("expected InAnyZone prohibited-zone list");
    // Hand is the only allowed zone; every other cast-capable zone is prohibited.
    assert!(!zones.contains(&Zone::Hand));
    assert!(zones.contains(&Zone::Graveyard));
    assert!(zones.contains(&Zone::Library));
    assert!(zones.contains(&Zone::Exile));
    assert!(zones.contains(&Zone::Command));
}

// --- MustAttack / MustBlock additional combat requirement tests ---

#[test]
fn static_must_attack_if_able() {
    let def = parse_static_line("This creature must attack if able.").unwrap();
    assert_eq!(def.mode, StaticMode::MustAttack);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
}

#[test]
fn static_must_block_each_combat_if_able() {
    let def = parse_static_line("This creature must block each combat if able.").unwrap();
    assert_eq!(def.mode, StaticMode::MustBlock);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
}

#[test]
fn static_blocks_each_combat_if_able() {
    let def = parse_static_line("Enchanted creature blocks each combat if able.").unwrap();
    assert_eq!(def.mode, StaticMode::MustBlock);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
        ))
    );
}

#[test]
fn static_must_block_if_able() {
    let def = parse_static_line("This creature must block if able.").unwrap();
    assert_eq!(def.mode, StaticMode::MustBlock);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
}

#[test]
fn static_blocks_each_turn_if_able() {
    let def = parse_static_line("This creature blocks each turn if able.").unwrap();
    assert_eq!(def.mode, StaticMode::MustBlock);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
}

#[test]
fn static_unrelated_text_not_must_attack() {
    // "gets +1/+1" should not produce MustAttack
    let def = parse_static_line("This creature gets +1/+1.").unwrap();
    assert_ne!(def.mode, StaticMode::MustAttack);
    assert_ne!(def.mode, StaticMode::MustBlock);
}

#[test]
fn map_keyword_all_creature_types_returns_changeling() {
    // CR 702.73a: "all creature types" is the Changeling CDA effect.
    assert_eq!(map_keyword("all creature types"), Some(Keyword::Changeling));
    assert_eq!(map_keyword("All Creature Types"), Some(Keyword::Changeling));
}

#[test]
fn gain_all_creature_types_produces_add_keyword_changeling() {
    let mods = parse_continuous_modifications("gain all creature types");
    assert!(
        mods.iter().any(|m| matches!(
            m,
            ContinuousModification::AddKeyword {
                keyword: Keyword::Changeling
            }
        )),
        "Should produce AddKeyword(Changeling), got: {mods:?}"
    );
}

#[test]
fn static_condition_source_in_graveyard() {
    let cond = parse_static_condition("this card is in your graveyard");
    assert!(
        matches!(
            cond,
            Some(StaticCondition::SourceInZone {
                zone: Zone::Graveyard
            })
        ),
        "Expected SourceInZone(Graveyard), got: {cond:?}"
    );
}

#[test]
fn static_condition_source_in_hand() {
    let cond = parse_static_condition("~ is in your hand");
    assert!(
        matches!(
            cond,
            Some(StaticCondition::SourceInZone { zone: Zone::Hand })
        ),
        "Expected SourceInZone(Hand), got: {cond:?}"
    );
}

#[test]
fn static_condition_compound_and() {
    let cond = parse_static_condition("this card is in your graveyard and you control a Mountain");
    assert!(
        matches!(cond, Some(StaticCondition::And { ref conditions }) if conditions.len() == 2),
        "Expected And with 2 conditions, got: {cond:?}"
    );
}

#[test]
fn static_condition_no_false_split_noun_phrase() {
    // "artifacts and creatures you control" is NOT a compound condition
    let cond = parse_static_condition("artifacts and creatures you control");
    assert!(
        !matches!(cond, Some(StaticCondition::And { .. })),
        "Should not split noun phrase, got: {cond:?}"
    );
}

// --- Task 1: as-long-as condition splitting in parse_continuous_gets_has ---

#[test]
fn static_self_ref_gets_as_long_as_control_forest() {
    // Kird Ape: "~ gets +1/+2 as long as you control a Forest"
    let def = parse_static_line("Kird Ape gets +1/+2 as long as you control a Forest.");
    assert!(def.is_some(), "Should parse 'gets +1/+2 as long as' static");
    let def = def.unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(
        def.condition.is_some(),
        "Expected non-null condition for 'as long as' static, got None"
    );
}

#[test]
fn static_self_ref_gets_as_long_as_regression_for_each() {
    // "for each" split must still work after adding "as long as" split
    let def = parse_static_line("~ gets +1/+1 for each creature you control.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    // Should have dynamic P/T modifications, not a condition
    assert!(def.condition.is_none());
}

#[test]
fn static_self_ref_gets_without_condition_regression() {
    // Plain "gets +2/+2" without condition must still work
    let def = parse_static_line("~ gets +2/+2.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(def.condition.is_none());
}

#[test]
fn static_condition_you_have_n_or_more_life() {
    // "you have 5 or more life" should parse as a QuantityComparison
    let cond = parse_static_condition("you have 5 or more life");
    assert!(
        matches!(
            cond,
            Some(StaticCondition::QuantityComparison {
                comparator: Comparator::GE,
                ..
            })
        ),
        "Expected QuantityComparison with GE, got: {cond:?}"
    );
}

#[test]
fn static_conditional_cant_untap_with_if() {
    // "~ doesn't untap during your untap step if enchanted creature is blue"
    // Should produce CantUntap with a condition populated
    let def = parse_static_line(
        "~ doesn't untap during your untap step as long as enchanted creature is tapped.",
    );
    // For now, just check it parses as CantUntap (condition handling is new)
    assert!(def.is_some(), "Should parse conditional CantUntap");
    let def = def.unwrap();
    assert_eq!(def.mode, StaticMode::CantUntap);
}

#[test]
fn control_enchanted_creature() {
    // CR 303.4e + CR 613.2: "You control enchanted creature" (Control Magic pattern)
    let def = parse_static_line("You control enchanted creature.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(def
        .modifications
        .contains(&ContinuousModification::ChangeController));
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(tf.properties.contains(&FilterProp::EnchantedBy));
        }
        _ => panic!("expected Typed filter"),
    }
    // Also works without trailing period
    let def2 = parse_static_line("You control enchanted creature").unwrap();
    assert_eq!(def2.mode, StaticMode::Continuous);
}

#[test]
fn control_enchanted_permanent() {
    let def = parse_static_line("You control enchanted permanent.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Permanent));
            assert!(tf.properties.contains(&FilterProp::EnchantedBy));
        }
        _ => panic!("expected Typed filter"),
    }
}

#[test]
fn control_enchanted_land() {
    let def = parse_static_line("You control enchanted land.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Land));
            assert!(tf.properties.contains(&FilterProp::EnchantedBy));
        }
        _ => panic!("expected Typed filter"),
    }
}

#[test]
fn control_enchanted_artifact() {
    let def = parse_static_line("You control enchanted artifact.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Artifact));
            assert!(tf.properties.contains(&FilterProp::EnchantedBy));
        }
        _ => panic!("expected Typed filter"),
    }
}

#[test]
fn control_enchanted_planeswalker() {
    // Not yet in Oracle text but structurally valid — the generic pattern should handle it
    let def = parse_static_line("You control enchanted planeswalker.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Planeswalker));
            assert!(tf.properties.contains(&FilterProp::EnchantedBy));
        }
        _ => panic!("expected Typed filter"),
    }
}

#[test]
fn core_type_creature_filter() {
    // CR 205.2a: "Artifact creatures you control get +1/+1" → Creature + Artifact
    let def = parse_static_line("Artifact creatures you control get +1/+1.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(tf.type_filters.contains(&TypeFilter::Artifact));
            assert_eq!(tf.controller, Some(ControllerRef::You));
        }
        _ => panic!("expected Typed filter with Creature + Artifact"),
    }
}

#[test]
fn other_enchantment_creatures() {
    // "Other enchantment creatures you control get +1/+1"
    let def = parse_static_line("Other enchantment creatures you control get +1/+1.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(tf.type_filters.contains(&TypeFilter::Enchantment));
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(tf.properties.contains(&FilterProp::Another));
        }
        _ => panic!("expected Typed filter with Creature + Enchantment + Another"),
    }
}

#[test]
fn creatures_you_control_that_are_enchanted() {
    // CR 613.1: "Creatures you control that are enchanted get +1/+1"
    let def = parse_static_line("Creatures you control that are enchanted get +1/+1.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(matches!(
                tf.properties.as_slice(),
                [FilterProp::HasAttachment {
                    kind: AttachmentKind::Aura,
                    controller: None,
                    exclude_source: crate::types::ability::SourceExclusion::Include
                }]
            ));
        }
        _ => panic!("expected Typed filter with enchanted property"),
    }
}

#[test]
fn creatures_you_control_that_are_enchanted_or_equipped_have_keyword() {
    let def = parse_static_line(
        "Creatures you control that are enchanted or equipped have double strike.",
    )
    .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(matches!(
                tf.properties.as_slice(),
                [FilterProp::HasAnyAttachmentOf { kinds, controller }]
                    if controller.is_none()
                        && kinds.len() == 2
                        && kinds.contains(&AttachmentKind::Aura)
                        && kinds.contains(&AttachmentKind::Equipment)
            ));
        }
        _ => panic!("expected Typed filter with attachment disjunction"),
    }
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::DoubleStrike,
        }));
}

#[test]
fn negative_dynamic_power() {
    // CR 613.4c: "gets -X/-0, where X is the number of creatures you control"
    let def = parse_static_line(
        "Enchanted creature gets -X/-0, where X is the number of creatures you control.",
    )
    .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    // Should have AddDynamicPower with Multiply(-1, ...) but NOT AddDynamicToughness
    let has_neg_power = def.modifications.iter().any(|m| {
        matches!(
            m,
            ContinuousModification::AddDynamicPower {
                value: QuantityExpr::Multiply { factor: -1, .. },
            }
        )
    });
    assert!(
        has_neg_power,
        "Expected negative dynamic power: {:?}",
        def.modifications
    );
    let has_dynamic_toughness = def
        .modifications
        .iter()
        .any(|m| matches!(m, ContinuousModification::AddDynamicToughness { .. }));
    assert!(
        !has_dynamic_toughness,
        "Should NOT have dynamic toughness for -X/-0"
    );
}

#[test]
fn skip_draw_step() {
    let def = parse_static_line("Skip your draw step.").unwrap();
    assert_eq!(def.mode, StaticMode::SkipStep { step: Phase::Draw });
}

#[test]
fn skip_untap_step() {
    let def = parse_static_line("Skip your untap step.").unwrap();
    assert_eq!(def.mode, StaticMode::SkipStep { step: Phase::Untap });
}

#[test]
fn skip_upkeep_step() {
    let def = parse_static_line("Skip your upkeep step.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::SkipStep {
            step: Phase::Upkeep
        }
    );
}

#[test]
fn positive_dynamic_pt() {
    // CR 613.4c: "gets +X/+X, where X is the number of creatures you control"
    let def = parse_static_line(
        "Enchanted creature gets +X/+X, where X is the number of creatures you control.",
    )
    .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    let has_power = def
        .modifications
        .iter()
        .any(|m| matches!(m, ContinuousModification::AddDynamicPower { .. }));
    let has_toughness = def
        .modifications
        .iter()
        .any(|m| matches!(m, ContinuousModification::AddDynamicToughness { .. }));
    assert!(has_power, "Expected dynamic power");
    assert!(has_toughness, "Expected dynamic toughness");
}

#[test]
fn dynamic_keyword_annihilator_x() {
    // "~ has annihilator X, where X is the number of +1/+1 counters on it."
    let def =
        parse_static_line("~ has annihilator X, where X is the number of +1/+1 counters on it.")
            .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    let has_dynamic_keyword = def.modifications.iter().any(|m| {
        matches!(
            m,
            ContinuousModification::AddDynamicKeyword {
                kind: crate::types::keywords::DynamicKeywordKind::Annihilator,
                ..
            }
        )
    });
    assert!(
        has_dynamic_keyword,
        "Expected AddDynamicKeyword(Annihilator), got {:?}",
        def.modifications
    );
}

#[test]
fn cant_be_blocked_unconditional() {
    let def = parse_static_line("This creature can't be blocked.").unwrap();
    assert_eq!(def.mode, StaticMode::CantBeBlocked);
    assert!(def.condition.is_none());
}

/// Issue #496: "except by N or more creatures" → typed count constraint.
/// `classify_block_exception` is the shared count-vs-quality detector used by
/// both parser entry points (`parse_enchanted_equipped_predicate` here and
/// `parse_restriction_modes` in `oracle_effect/subject.rs`).
#[test]
fn classify_block_exception_count_vs_quality() {
    assert_eq!(
        classify_block_exception("three or more creatures."),
        BlockExceptionKind::MinBlockers { min: 3 }
    );
    assert_eq!(
        classify_block_exception("six or more creatures"),
        BlockExceptionKind::MinBlockers { min: 6 }
    );
    assert!(
        matches!(
            classify_block_exception("artifact creatures."),
            BlockExceptionKind::Quality(_)
        ),
        "Expected Quality kind for a quality phrase"
    );
}

#[test]
fn cant_be_blocked_as_long_as_defending_controls() {
    // CR 509.1a: "can't be blocked as long as defending player controls an artifact"
    let def = parse_static_line(
        "This creature can't be blocked as long as defending player controls an artifact.",
    )
    .unwrap();
    assert_eq!(def.mode, StaticMode::CantBeBlocked);
    assert!(
        matches!(
            &def.condition,
            Some(StaticCondition::DefendingPlayerControls { .. })
        ),
        "Expected DefendingPlayerControls condition, got: {:?}",
        def.condition
    );
}

#[test]
fn cant_be_blocked_attacking_alone() {
    // CR 506.5: "can't be blocked as long as it's attacking alone"
    let def = parse_static_line("This creature can't be blocked as long as it's attacking alone.")
        .unwrap();
    assert_eq!(def.mode, StaticMode::CantBeBlocked);
    assert_eq!(def.condition, Some(StaticCondition::SourceAttackingAlone));
}

#[test]
fn enchanted_creature_cant_be_blocked_as_long_as_you_control_gate() {
    let def =
        parse_static_line("Enchanted creature can't be blocked as long as you control a Gate.")
            .unwrap();
    assert_eq!(def.mode, StaticMode::CantBeBlocked);
    assert!(matches!(
        def.affected,
        Some(TargetFilter::Typed(TypedFilter { properties, .. }))
            if properties.contains(&FilterProp::EnchantedBy)
    ));
    assert!(matches!(
        def.condition,
        Some(StaticCondition::IsPresent { filter: Some(TargetFilter::Typed(tf)) })
            if tf.get_subtype() == Some("Gate")
    ));
}

#[test]
fn equipped_creature_cant_be_blocked_condition_uses_recipient_power() {
    let def =
        parse_static_line("Equipped creature can't be blocked as long as its power is 3 or less.")
            .unwrap();
    assert_eq!(def.mode, StaticMode::CantBeBlocked);
    assert!(matches!(
        def.affected,
        Some(TargetFilter::Typed(TypedFilter { properties, .. }))
            if properties.contains(&FilterProp::EquippedBy)
    ));
    assert!(matches!(
        def.condition,
        Some(StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Recipient,
                },
            },
            comparator: Comparator::LE,
            rhs: QuantityExpr::Fixed { value: 3 },
        })
    ));
}

#[test]
fn equipped_creature_continuous_condition_uses_recipient_power() {
    let def = parse_static_line("Equipped creature gets +1/+1 as long as its power is 3 or less.")
        .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(matches!(
        def.condition,
        Some(StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Recipient,
                },
            },
            comparator: Comparator::LE,
            rhs: QuantityExpr::Fixed { value: 3 },
        })
    ));
}

#[test]
fn equipped_creature_counter_condition_uses_recipient_counter_scope() {
    let def = parse_static_line("Equipped creature gets +1/+1 as long as it has a counter on it.")
        .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(matches!(
        def.condition,
        Some(StaticCondition::RecipientHasCounters {
            counters: CounterMatch::Any,
            minimum: 1,
            maximum: None,
        })
    ));
}

#[test]
fn enchanted_artifact_is_creature_with_base_pt() {
    // CR 613.1d: Ensoul Artifact pattern
    let def = parse_static_line(
            "Enchanted artifact is a creature with base power and toughness 5/5 in addition to its other types.",
        )
        .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddType {
            core_type: crate::types::card_type::CoreType::Creature,
        }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::SetPower { value: 5 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::SetToughness { value: 5 }));
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Artifact));
            assert!(tf.properties.contains(&FilterProp::EnchantedBy));
        }
        _ => panic!("expected Typed filter"),
    }
}

#[test]
fn enchanted_permanent_loses_abilities_becomes_land() {
    // CR 613.1d: Imprisoned in the Moon pattern
    let def = parse_static_line("Enchanted permanent loses all abilities and is a colorless land.")
        .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(def
        .modifications
        .contains(&ContinuousModification::RemoveAllAbilities));
    // NOTE: This was previously asserting AddType{Land} (broken behavior).
    // After the !is_additive fix, non-additive "is a colorless land"
    // correctly emits SetCardTypes (CR 205.1a replacement).
    assert!(def
        .modifications
        .contains(&ContinuousModification::SetCardTypes {
            core_types: vec![crate::types::card_type::CoreType::Land],
        }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::SetColor { colors: vec![] }));
}

#[test]
fn enchanted_creature_loses_abilities_becomes_insect() {
    // CR 613.1d: Darksteel Mutation pattern — non-additive, so SetCardTypes/SetColor/RemoveAllSubtypes.
    let def = parse_static_line(
        "Enchanted creature loses all abilities and is a 0/1 green Insect creature.",
    )
    .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    let mods = &def.modifications;
    assert!(mods.contains(&ContinuousModification::RemoveAllAbilities));
    assert!(mods.contains(&ContinuousModification::SetPower { value: 0 }));
    assert!(mods.contains(&ContinuousModification::SetToughness { value: 1 }));
    // CR 205.1a + CR 613.1d: non-additive → SetCardTypes, not AddType.
    assert!(
        mods.contains(&ContinuousModification::SetCardTypes {
            core_types: vec![crate::types::card_type::CoreType::Creature],
        }),
        "expected SetCardTypes[Creature]: {mods:?}"
    );
    // CR 613.1e: non-additive → SetColor, not AddColor.
    assert!(
        mods.contains(&ContinuousModification::SetColor {
            colors: vec![crate::types::mana::ManaColor::Green],
        }),
        "expected SetColor[Green]: {mods:?}"
    );
    // CR 205.1a: non-additive creature subtype auto-wipe.
    assert!(
        mods.contains(&ContinuousModification::RemoveAllSubtypes {
            set: crate::types::card_type::SubtypeSet::Creature,
        }),
        "expected RemoveAllSubtypes{{Creature}}: {mods:?}"
    );
    assert!(
        mods.contains(&ContinuousModification::AddSubtype {
            subtype: "Insect".to_string(),
        }),
        "expected AddSubtype(Insect): {mods:?}"
    );
    // Written-order: wipe before grant.
    let pos = |m: &ContinuousModification| mods.iter().position(|x| x == m).unwrap();
    assert!(
        pos(&ContinuousModification::RemoveAllSubtypes {
            set: crate::types::card_type::SubtypeSet::Creature,
        }) < pos(&ContinuousModification::AddSubtype {
            subtype: "Insect".to_string(),
        }),
        "RemoveAllSubtypes must precede AddSubtype(Insect): {mods:?}"
    );
}

#[test]
fn enchanted_creature_is_blue_frog() {
    // Frogify — CR 613.1d: non-additive → SetCardTypes; CR 613.1e: SetColor; CR 205.1a: RemoveAllSubtypes
    let def = parse_static_line(
        "Enchanted creature loses all abilities and is a 1/1 blue Frog creature.",
    )
    .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    let mods = &def.modifications;
    assert!(mods.contains(&ContinuousModification::RemoveAllAbilities));
    assert!(
        mods.contains(&ContinuousModification::SetCardTypes {
            core_types: vec![crate::types::card_type::CoreType::Creature],
        }),
        "non-additive must use SetCardTypes: {mods:?}"
    );
    assert!(
        mods.contains(&ContinuousModification::SetColor {
            colors: vec![crate::types::mana::ManaColor::Blue],
        }),
        "non-additive must use SetColor: {mods:?}"
    );
    assert!(
        mods.contains(&ContinuousModification::RemoveAllSubtypes {
            set: crate::types::card_type::SubtypeSet::Creature,
        }),
        "must auto-inject RemoveAllSubtypes{{Creature}}: {mods:?}"
    );
    assert!(
        mods.contains(&ContinuousModification::AddSubtype {
            subtype: "Frog".to_string(),
        }),
        "must emit AddSubtype(Frog): {mods:?}"
    );
    assert!(mods.contains(&ContinuousModification::SetPower { value: 1 }));
    assert!(mods.contains(&ContinuousModification::SetToughness { value: 1 }));
    // CR 613.7 written-order: RemoveAllSubtypes must precede AddSubtype(Frog)
    let pos = |m: &ContinuousModification| mods.iter().position(|x| x == m).unwrap();
    assert!(
        pos(&ContinuousModification::RemoveAllSubtypes {
            set: crate::types::card_type::SubtypeSet::Creature,
        }) < pos(&ContinuousModification::AddSubtype {
            subtype: "Frog".to_string(),
        }),
        "RemoveAllSubtypes must precede AddSubtype(Frog): {mods:?}"
    );
}

#[test]
fn enchanted_creature_is_blue_creature_no_subtype() {
    // CR 205.1a: no new creature subtype granted → no Oracle instruction to wipe existing subtypes.
    let def = parse_static_line("Enchanted creature is a blue creature.").unwrap();
    let mods = &def.modifications;
    assert!(
        !mods
            .iter()
            .any(|m| matches!(m, ContinuousModification::RemoveAllSubtypes { .. })),
        "no RemoveAllSubtypes when no new subtype granted: {mods:?}"
    );
    assert!(mods.contains(&ContinuousModification::SetCardTypes {
        core_types: vec![crate::types::card_type::CoreType::Creature],
    }));
    assert!(mods.contains(&ContinuousModification::SetColor {
        colors: vec![crate::types::mana::ManaColor::Blue],
    }));
}

// --- CantBeCast (blanket casting prohibition) tests ---

#[test]
fn cant_cast_creature_spells() {
    // CR 101.2: Steel Golem — "You can't cast creature spells."
    let def = parse_static_line("You can't cast creature spells.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantBeCast {
            who: ProhibitionScope::Controller,
        }
    );
}

#[test]
fn cant_cast_instant_or_sorcery_spells() {
    // CR 101.2: Hymn of the Wilds — "You can't cast instant or sorcery spells."
    let def = parse_static_line("You can't cast instant or sorcery spells.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantBeCast {
            who: ProhibitionScope::Controller,
        }
    );
}

#[test]
fn cant_cast_noncreature_spells() {
    // CR 101.2: Generic noncreature prohibition
    let def = parse_static_line("You can't cast noncreature spells.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantBeCast {
            who: ProhibitionScope::Controller,
        }
    );
}

// --- "don't lose the game" ---

#[test]
fn dont_lose_the_game() {
    // CR 104.3b: Phyrexian Unlife — "You don't lose the game for having 0 or less life."
    let def = parse_static_line("You don't lose the game for having 0 or less life.").unwrap();
    assert_eq!(def.mode, StaticMode::CantLoseTheGame);
}

// --- PerTurnDrawLimit tests ---

#[test]
fn per_turn_draw_limit_all_players() {
    // CR 101.2: Spirit of the Labyrinth
    let def = parse_static_line("Each player can't draw more than one card each turn.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::PerTurnDrawLimit {
            who: ProhibitionScope::AllPlayers,
            max: 1,
        }
    );
}

#[test]
fn per_turn_draw_limit_opponents() {
    // CR 101.2: Narset, Parter of Veils
    let def = parse_static_line("Each opponent can't draw more than one card each turn.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::PerTurnDrawLimit {
            who: ProhibitionScope::Opponents,
            max: 1,
        }
    );
}

#[test]
fn cant_draw_all_players() {
    let def = parse_static_line("Players can't draw cards.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantDraw {
            who: ProhibitionScope::AllPlayers,
        }
    );
}

#[test]
fn cant_draw_controller() {
    let def = parse_static_line("You can't draw cards.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantDraw {
            who: ProhibitionScope::Controller,
        }
    );
}

#[test]
fn cant_draw_opponents() {
    let def = parse_static_line("Your opponents can't draw cards.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantDraw {
            who: ProhibitionScope::Opponents,
        }
    );
}

#[test]
fn spell_cost_reduction_uses_card_types_in_graveyard_quantity() {
    let def = parse_static_line(
        "This spell costs {1} less to cast for each card type among cards in your graveyard.",
    )
    .unwrap();
    match def.mode {
        StaticMode::ModifyCost {
            mode: CostModifyMode::Reduce,
            dynamic_count:
                Some(QuantityRef::DistinctCardTypes {
                    source:
                        CardTypeSetSource::Zone {
                            zone: ZoneRef::Graveyard,
                            scope,
                        },
                }),
            ..
        } => assert_eq!(scope, CountScope::Controller),
        other => panic!("expected card-types-in-graveyard cost reduction, got {other:?}"),
    }
}

// --- Expanded CantBeCast pattern tests ---

#[test]
fn cant_cast_passive_voice_creature_spells() {
    // Aether Storm: "Creature spells can't be cast."
    let def = parse_static_line("Creature spells can't be cast.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantBeCast {
            who: ProhibitionScope::AllPlayers,
        }
    );
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
        }
        other => panic!("Expected Typed filter with Creature, got {other:?}"),
    }
}

#[test]
fn cant_cast_spells_with_mana_value_or_less() {
    // Brisela: "Your opponents can't cast spells with mana value 3 or less."
    let def =
        parse_static_line("Your opponents can't cast spells with mana value 3 or less.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantBeCast {
            who: ProhibitionScope::Opponents,
        }
    );
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.properties.iter().any(|p| matches!(
                p,
                FilterProp::Cmc {
                    comparator: Comparator::LE,
                    value: QuantityExpr::Fixed { value: 3 }
                }
            )));
        }
        other => panic!("Expected Typed filter with CmcLE, got {other:?}"),
    }
}

#[test]
fn cant_cast_spells_with_chosen_name() {
    // Alhammarret: "Your opponents can't cast spells with the chosen name."
    let def = parse_static_line("Your opponents can't cast spells with the chosen name.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantBeCast {
            who: ProhibitionScope::Opponents,
        }
    );
    assert_eq!(def.affected, Some(TargetFilter::HasChosenName));
}

#[test]
fn passive_spells_with_chosen_name_cant_be_cast() {
    // CR 101.2 + CR 201.2: passive-voice name-lock — Meddling Mage, Nevermore,
    // Voidstone Gargoyle. "All players" scope (no subject). The active-voice
    // form ("[subject] can't cast spells with the chosen name") is covered above.
    for text in [
        "Spells with the chosen name can't be cast.",
        "Spells with the chosen name can't be cast",
    ] {
        let def = parse_static_line(text)
            .unwrap_or_else(|| panic!("should parse passive name-lock: {text:?}"));
        assert_eq!(
            def.mode,
            StaticMode::CantBeCast {
                who: ProhibitionScope::AllPlayers,
            },
            "{text:?}"
        );
        assert_eq!(def.affected, Some(TargetFilter::HasChosenName), "{text:?}");
    }
}

#[test]
fn cant_cast_spells_with_chosen_name_parenthetical() {
    // Alhammarret full text with parenthetical condition
    let def = parse_static_line(
            "Your opponents can't cast spells with the chosen name (as long as this creature is on the battlefield).",
        )
        .unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantBeCast {
            who: ProhibitionScope::Opponents,
        }
    );
    assert_eq!(def.affected, Some(TargetFilter::HasChosenName));
}

// CR 201.3 / CR 113.6: Petrified Hamlet — "Lands with the chosen name
// have \"{T}: Add {C}.\"" grants a quoted mana ability to every land
// whose name matches the CardName persisted on the source by the
// preceding ETB choose-a-land-card-name trigger.
#[test]
fn lands_with_chosen_name_grant_quoted_ability() {
    let def = parse_static_line("Lands with the chosen name have \"{T}: Add {C}.\"").unwrap();
    match &def.affected {
        Some(TargetFilter::And { filters }) => {
            assert_eq!(filters.len(), 2);
            assert!(
                matches!(
                    &filters[0],
                    TargetFilter::Typed(tf) if tf.type_filters.contains(&TypeFilter::Land)
                ),
                "expected land typed filter, got {:?}",
                filters[0]
            );
            assert_eq!(filters[1], TargetFilter::HasChosenName);
        }
        other => panic!("expected And[Typed(Land), HasChosenName], got {other:?}"),
    }
    assert_eq!(def.modifications.len(), 1);
    assert!(
        matches!(
            &def.modifications[0],
            ContinuousModification::GrantAbility { .. }
        ),
        "expected GrantAbility, got {:?}",
        def.modifications[0]
    );
}

#[test]
fn cant_cast_spells_of_chosen_type() {
    // Archon of Valor's Reach: "Players can't cast spells of the chosen type."
    let def = parse_static_line("Players can't cast spells of the chosen type.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantBeCast {
            who: ProhibitionScope::AllPlayers,
        }
    );
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::IsChosenCardType)));
        }
        other => panic!("Expected Typed filter with IsChosenCardType, got {other:?}"),
    }
}

#[test]
fn enchanted_controller_cant_cast_creature_spells() {
    // Brand of Ill Omen: "Enchanted creature's controller can't cast creature spells."
    let def =
        parse_static_line("Enchanted creature's controller can't cast creature spells.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantBeCast {
            who: ProhibitionScope::EnchantedCreatureController,
        }
    );
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
        }
        other => panic!("Expected Typed filter with Creature, got {other:?}"),
    }
}

#[test]
fn cant_cast_mana_value_or_greater() {
    // Angel of Eternal Dawn pattern: "can't cast spells with mana value 5 or greater"
    let def = parse_static_line("Your opponents can't cast spells with mana value 5 or greater.")
        .unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantBeCast {
            who: ProhibitionScope::Opponents,
        }
    );
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.properties.iter().any(|p| matches!(
                p,
                FilterProp::Cmc {
                    comparator: Comparator::GE,
                    value: QuantityExpr::Fixed { value: 5 }
                }
            )));
        }
        other => panic!("Expected Typed filter with CmcGE, got {other:?}"),
    }
}

#[test]
fn cant_cast_opponents_creature_spells() {
    // "Your opponents can't cast creature spells." — existing pattern with opponent scope
    let def = parse_static_line("Your opponents can't cast creature spells.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantBeCast {
            who: ProhibitionScope::Opponents,
        }
    );
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
        }
        other => panic!("Expected Typed filter with Creature, got {other:?}"),
    }
}

// --- MaximumHandSize tests ---

#[test]
fn max_hand_size_set_to_two() {
    let def = parse_static_line("Your maximum hand size is two.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::MaximumHandSize {
            modification: HandSizeModification::SetTo(2),
        }
    );
    assert!(matches!(
        def.affected,
        Some(TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::You),
            ..
        }))
    ));
}

#[test]
fn max_hand_size_set_to_twenty() {
    let def = parse_static_line("Your maximum hand size is twenty.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::MaximumHandSize {
            modification: HandSizeModification::SetTo(20),
        }
    );
}

#[test]
fn max_hand_size_increased_by_one() {
    let def = parse_static_line("Your maximum hand size is increased by one.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::MaximumHandSize {
            modification: HandSizeModification::AdjustedBy(1),
        }
    );
}

#[test]
fn max_hand_size_reduced_by_three() {
    let def = parse_static_line("Your maximum hand size is reduced by three.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::MaximumHandSize {
            modification: HandSizeModification::AdjustedBy(-3),
        }
    );
}

#[test]
fn max_hand_size_opponent_reduced_by_one() {
    let def = parse_static_line("Each opponent's maximum hand size is reduced by one.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::MaximumHandSize {
            modification: HandSizeModification::AdjustedBy(-1),
        }
    );
    assert!(matches!(
        def.affected,
        Some(TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::Opponent),
            ..
        }))
    ));
}

#[test]
fn max_hand_size_set_to_five() {
    let def = parse_static_line("Your maximum hand size is five.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::MaximumHandSize {
            modification: HandSizeModification::SetTo(5),
        }
    );
}

// --- Group A: AssignDamageFromToughness global and self-referential variants ---

#[test]
fn static_assigns_damage_from_toughness_all_creatures() {
    // CR 510.1c: Global variant without "you control" — affects all creatures.
    let def = parse_static_line(
        "Each creature assigns combat damage equal to its toughness rather than its power.",
    )
    .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(TypedFilter::creature()))
    );
    assert!(def
        .modifications
        .contains(&ContinuousModification::AssignDamageFromToughness));
}

#[test]
fn static_assigns_damage_from_toughness_self() {
    // CR 510.1c: Self-referential variant — "This creature assigns..."
    let def = parse_static_line(
        "This creature assigns combat damage equal to its toughness rather than its power.",
    )
    .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AssignDamageFromToughness));
}

#[test]
fn static_assign_damage_as_though_unblocked_self() {
    let def = parse_static_line(
        "You may have this creature assign its combat damage as though it weren't blocked.",
    )
    .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AssignDamageAsThoughUnblocked));
}

#[test]
fn static_assign_damage_as_though_unblocked_enchanted_controller() {
    let def = parse_static_line(
            "Enchanted creature's controller may have it assign its combat damage as though it weren't blocked.",
        )
        .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
        ))
    );
    assert!(def
        .modifications
        .contains(&ContinuousModification::AssignDamageAsThoughUnblocked));
}

// --- Group C: Casting prohibition variants ---

#[test]
fn cant_cast_during_your_turn_opponents() {
    // CR 101.2: Temporal-prefix pattern — "During your turn, your opponents can't cast spells"
    let def = parse_static_line(
            "During your turn, your opponents can't cast spells or activate abilities of artifacts, creatures, or enchantments.",
        )
        .unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantCastDuring {
            who: ProhibitionScope::Opponents,
            when: CastingProhibitionCondition::DuringYourTurn,
        }
    );
}

#[test]
fn cant_cast_opponents_same_name() {
    // CR 101.2: "can't cast spells with the same name as" — approximate prohibition
    let def = parse_static_line(
            "Your opponents can't cast spells with the same name as a card exiled with Dragonlord Dromoka.",
        )
        .unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantBeCast {
            who: ProhibitionScope::Opponents,
        }
    );
}

#[test]
fn cant_cast_noncreature_mv4_or_greater() {
    // CR 101.2: Passive voice with mana value filter
    let def = parse_static_line("Noncreature spells with mana value 4 or greater can't be cast.")
        .unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantBeCast {
            who: ProhibitionScope::AllPlayers,
        }
    );
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf
                .type_filters
                .contains(&TypeFilter::Non(Box::new(TypeFilter::Creature))));
            assert!(tf.properties.iter().any(|p| matches!(
                p,
                FilterProp::Cmc {
                    comparator: Comparator::GE,
                    value: QuantityExpr::Fixed { value: 4 }
                }
            )));
        }
        other => panic!("Expected Typed filter with Noncreature + CmcGE, got {other:?}"),
    }
}

#[test]
fn cant_cast_enchanted_player_per_turn_limit() {
    // CR 101.2 + CR 303.4e: "Enchanted player can't cast more than one spell each turn."
    let def =
        parse_static_line("Enchanted player can't cast more than one spell each turn.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::PerTurnCastLimit {
            who: ProhibitionScope::EnchantedCreatureController,
            max: 1,
            spell_filter: None,
        }
    );
}

#[test]
fn cant_cast_during_combat_instants() {
    // CR 101.2: Temporal-prefix — "During combat, players can't cast instant spells..."
    let def = parse_static_line(
            "During combat, players can't cast instant spells or activate abilities that aren't mana abilities.",
        )
        .unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantCastDuring {
            who: ProhibitionScope::AllPlayers,
            when: CastingProhibitionCondition::DuringCombat,
        }
    );
    // Should have instant spell filter
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Instant));
        }
        other => panic!("Expected Typed filter with Instant, got {other:?}"),
    }
}

#[test]
fn cant_cast_spells_of_chosen_color() {
    // CR 101.2: "can't cast spells of the chosen color"
    let def = parse_static_line("Your opponents can't cast spells of the chosen color.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantBeCast {
            who: ProhibitionScope::Opponents,
        }
    );
}

#[test]
fn cant_cast_spells_with_even_mana_values() {
    // CR 101.2: "can't cast spells with even mana values"
    let def = parse_static_line("Your opponents can't cast spells with even mana values.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantBeCast {
            who: ProhibitionScope::Opponents,
        }
    );
}

#[test]
fn cant_cast_by_paying_alternative_costs() {
    // CR 101.2: "can't cast spells by paying alternative costs"
    let def = parse_static_line("Players can't cast spells by paying alternative costs.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantBeCast {
            who: ProhibitionScope::AllPlayers,
        }
    );
}

#[test]
fn cant_cast_opponent_attacked_this_turn() {
    // CR 101.2 + CR 601.3a: "Each opponent who attacked with a creature this
    // turn can't cast spells" — the per-affected-player turn-activity predicate
    // must be preserved in `per_player_condition`, NOT dropped (Angelic Arbiter).
    let def = parse_static_line(
        "Each opponent who attacked with a creature this turn can't cast spells.",
    )
    .unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CantBeCast {
            who: ProhibitionScope::Opponents,
        }
    );
    assert_eq!(
        def.per_player_condition,
        Some(ParsedCondition::YouAttackedThisTurn),
        "the turn-activity predicate must be carried, not approximated away"
    );
    // `condition` (the source-relative functioning gate) must stay None so the
    // prohibition is not globally gated on/off.
    assert_eq!(def.condition, None);
}

#[test]
fn cant_attack_opponent_cast_spell_this_turn() {
    // CR 508.1 + CR 109.5: "Each opponent who cast a spell this turn can't
    // attack with creatures" — restricts OPPONENTS' creatures, not the source
    // (Angelic Arbiter). Regression guard against the prior SelfRef misparse.
    let def =
        parse_static_line("Each opponent who cast a spell this turn can't attack with creatures.")
            .unwrap();
    assert_eq!(def.mode, StaticMode::CantAttack);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::Opponent)
        )),
        "affected must be opponents' creatures (CR 109.5)"
    );
    // Regression guard: the prior misparse set affected = SelfRef.
    assert_ne!(def.affected, Some(TargetFilter::SelfRef));
    assert_eq!(
        def.per_player_condition,
        Some(ParsedCondition::YouCastSpellThisTurn { filter: None }),
    );
    assert_eq!(def.condition, None);
}

// --- Group A: Enchanted land type changes ---

#[test]
fn enchanted_land_is_island() {
    // CR 305.7: "Enchanted land is an Island." — replacement semantics via "is an"
    let def = parse_static_line("Enchanted land is an Island.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    if let Some(TargetFilter::Typed(ref tf)) = def.affected {
        assert!(
            tf.type_filters.contains(&TypeFilter::Land),
            "Expected Land type filter"
        );
        assert!(
            tf.properties.contains(&FilterProp::EnchantedBy),
            "Expected EnchantedBy property"
        );
    } else {
        panic!(
            "Expected Typed filter with Land + EnchantedBy, got {:?}",
            def.affected
        );
    }
    assert!(
        def.modifications
            .contains(&ContinuousModification::SetBasicLandType {
                land_type: BasicLandType::Island,
            }),
        "Expected SetBasicLandType Island, got {:?}",
        def.modifications
    );
}

#[test]
fn enchanted_land_every_basic_land_type() {
    // CR 305.7: "Enchanted land is every basic land type in addition to its other types."
    let def = parse_static_line(
        "Enchanted land is every basic land type in addition to its other types.",
    )
    .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    if let Some(TargetFilter::Typed(ref tf)) = def.affected {
        assert!(tf.properties.contains(&FilterProp::EnchantedBy));
    } else {
        panic!("Expected Typed filter with EnchantedBy");
    }
    assert!(
        def.modifications
            .contains(&ContinuousModification::AddAllBasicLandTypes),
        "Expected AddAllBasicLandTypes, got {:?}",
        def.modifications
    );
}

#[test]
fn enchanted_land_multiple_types() {
    // CR 305.7: "Enchanted land is a Mountain, Forest, and Plains." — multi-type replacement
    let def = parse_static_line("Enchanted land is a Mountain, Forest, and Plains.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    if let Some(TargetFilter::Typed(ref tf)) = def.affected {
        assert!(tf.properties.contains(&FilterProp::EnchantedBy));
    } else {
        panic!("Expected Typed filter with EnchantedBy");
    }
    // First type is SetBasicLandType (clears old subtypes), rest are AddSubtype
    assert!(
        def.modifications
            .contains(&ContinuousModification::SetBasicLandType {
                land_type: BasicLandType::Mountain,
            }),
        "Expected SetBasicLandType Mountain"
    );
    assert!(
        def.modifications
            .contains(&ContinuousModification::AddSubtype {
                subtype: "Forest".to_string(),
            }),
        "Expected AddSubtype Forest"
    );
    assert!(
        def.modifications
            .contains(&ContinuousModification::AddSubtype {
                subtype: "Plains".to_string(),
            }),
        "Expected AddSubtype Plains"
    );
}

// --- Group B: Colorless/Multicolored/Snow lord pump ---

#[test]
fn static_other_colorless_creatures_get_plus() {
    // CR 105.2c: "Other colorless creatures you control get +1/+1."
    let def = parse_static_line("Other colorless creatures you control get +1/+1.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    if let Some(TargetFilter::Typed(ref tf)) = def.affected {
        assert!(tf.properties.contains(&FilterProp::ColorCount {
            comparator: Comparator::EQ,
            count: 0,
        }));
        assert!(tf.properties.contains(&FilterProp::Another));
        assert_eq!(tf.controller, Some(ControllerRef::You));
    } else {
        panic!("Expected Typed filter, got {:?}", def.affected);
    }
}

#[test]
fn static_other_monocolored_creatures_get_plus() {
    // CR 105.2a: "Other monocolored creatures you control get +1/+1."
    let def = parse_static_line("Other monocolored creatures you control get +1/+1.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    if let Some(TargetFilter::Typed(ref tf)) = def.affected {
        assert!(tf.properties.contains(&FilterProp::ColorCount {
            comparator: Comparator::EQ,
            count: 1,
        }));
        assert!(tf.properties.contains(&FilterProp::Another));
        assert_eq!(tf.controller, Some(ControllerRef::You));
    } else {
        panic!("Expected Typed filter, got {:?}", def.affected);
    }
}

#[test]
fn static_ygra_additive_food_artifact_grants_food_ability() {
    let def = parse_static_line(
            "Other creatures are Food artifacts in addition to their other types and have \"{2}, {T}, Sacrifice this permanent: You gain 3 life.\"",
        )
        .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::Another]),
        ))
    );
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddSubtype {
            subtype: "Food".to_string(),
        }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddType {
            core_type: CoreType::Artifact,
        }));
    let grant = def
        .modifications
        .iter()
        .find(|m| matches!(m, ContinuousModification::GrantAbility { .. }));
    assert!(grant.is_some(), "expected granted activated Food ability");
    if let Some(ContinuousModification::GrantAbility { definition }) = grant {
        assert_eq!(definition.kind, AbilityKind::Activated);
        assert!(definition.cost.is_some());
    }
}

#[test]
fn static_kudo_adds_bear_subtype_alongside_base_pt() {
    let def = parse_static_line(
            "Other creatures have base power and toughness 2/2 and are Bears in addition to their other types.",
        )
        .unwrap();
    assert!(def
        .modifications
        .contains(&ContinuousModification::SetPower { value: 2 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::SetToughness { value: 2 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddSubtype {
            subtype: "Bear".to_string(),
        }));
}

#[test]
fn static_hivestone_adds_sliver_subtype_to_creatures_you_control() {
    let def = parse_static_line(
        "Creatures you control are Slivers in addition to their other creature types.",
    )
    .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You),
        ))
    );
    assert_eq!(
        def.modifications,
        vec![ContinuousModification::AddSubtype {
            subtype: "Sliver".to_string(),
        }]
    );
}

#[test]
fn static_other_multicolored_creatures_get_plus() {
    // CR 105.2: "Other multicolored creatures you control get +1/+0."
    let def = parse_static_line("Other multicolored creatures you control get +1/+0.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    if let Some(TargetFilter::Typed(ref tf)) = def.affected {
        assert!(tf.properties.contains(&FilterProp::ColorCount {
            comparator: Comparator::GE,
            count: 2,
        }));
        assert!(tf.properties.contains(&FilterProp::Another));
        assert_eq!(tf.controller, Some(ControllerRef::You));
    } else {
        panic!("Expected Typed filter, got {:?}", def.affected);
    }
}

#[test]
fn static_other_snow_zombie_creatures_get_plus() {
    // CR 205.4a: "Other snow and Zombie creatures you control get +1/+1."
    let def = parse_static_line("Other snow and Zombie creatures you control get +1/+1.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    if let Some(TargetFilter::Typed(ref tf)) = def.affected {
        assert!(
            tf.properties.contains(&FilterProp::HasSupertype {
                value: Supertype::Snow,
            }),
            "Expected HasSupertype Snow, got {:?}",
            tf.properties
        );
        assert!(
            tf.type_filters
                .contains(&TypeFilter::Subtype("Zombie".to_string())),
            "Expected Zombie subtype, got {:?}",
            tf.type_filters
        );
        assert!(tf.properties.contains(&FilterProp::Another));
    } else {
        panic!("Expected Typed filter, got {:?}", def.affected);
    }
}

// --- Group C: All permanents are [type] ---

#[test]
fn static_all_permanents_are_artifacts() {
    // CR 205.1a: "All permanents are artifacts in addition to their other types."
    let def = parse_static_line("All permanents are artifacts in addition to their other types.")
        .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    if let Some(TargetFilter::Typed(ref tf)) = def.affected {
        assert!(
            tf.type_filters.contains(&TypeFilter::Permanent),
            "Expected Permanent type filter"
        );
    } else {
        panic!("Expected Typed filter with Permanent");
    }
    assert!(
        def.modifications
            .contains(&ContinuousModification::AddType {
                core_type: crate::types::card_type::CoreType::Artifact,
            }),
        "Expected AddType Artifact, got {:?}",
        def.modifications
    );
}

#[test]
fn static_all_permanents_are_enchantments() {
    // CR 205.1a: "All permanents are enchantments in addition to their other types."
    let def =
        parse_static_line("All permanents are enchantments in addition to their other types.")
            .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(
        def.modifications
            .contains(&ContinuousModification::AddType {
                core_type: crate::types::card_type::CoreType::Enchantment,
            }),
        "Expected AddType Enchantment"
    );
}

// --- Group C2: All [subject] are [color] (global color-defining statics) ---

#[test]
fn static_all_creatures_are_black() {
    // CR 613.1e + CR 105.1: Darkest Hour — "All creatures are black."
    let def = parse_static_line("All creatures are black.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    if let Some(TargetFilter::Typed(ref tf)) = def.affected {
        assert!(
            tf.type_filters.contains(&TypeFilter::Creature),
            "Expected Creature type filter, got {:?}",
            tf.type_filters
        );
    } else {
        panic!("Expected Typed filter, got {:?}", def.affected);
    }
    assert_eq!(
        def.modifications,
        vec![ContinuousModification::SetColor {
            colors: vec![ManaColor::Black]
        }]
    );
}

#[test]
fn static_all_permanents_are_colorless() {
    // CR 613.1e + CR 105.2c: Thran Lens — "All permanents are colorless."
    let def = parse_static_line("All permanents are colorless.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    if let Some(TargetFilter::Typed(ref tf)) = def.affected {
        assert!(
            tf.type_filters.contains(&TypeFilter::Permanent),
            "Expected Permanent type filter, got {:?}",
            tf.type_filters
        );
    } else {
        panic!("Expected Typed filter, got {:?}", def.affected);
    }
    assert_eq!(
        def.modifications,
        vec![ContinuousModification::SetColor { colors: vec![] }]
    );
}

#[test]
fn static_all_slivers_are_colorless() {
    // CR 613.1e + CR 105.2c: Ghostflame Sliver — "All Slivers are colorless."
    // Plural subtype path: parse_subtype canonicalizes "Slivers" → "Sliver".
    let def = parse_static_line("All Slivers are colorless.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    if let Some(TargetFilter::Typed(ref tf)) = def.affected {
        assert!(
            tf.type_filters
                .contains(&TypeFilter::Subtype("Sliver".to_string())),
            "Expected Sliver subtype filter, got {:?}",
            tf.type_filters
        );
    } else {
        panic!("Expected Typed filter, got {:?}", def.affected);
    }
    assert_eq!(
        def.modifications,
        vec![ContinuousModification::SetColor { colors: vec![] }]
    );
}

#[test]
fn static_all_subject_are_color_does_not_eat_get_plus_lines() {
    // Regression guard: "All creatures get +1/+1." must still reach the
    // gets_has branch, not be swallowed by the color-set handler.
    let def = parse_static_line("All creatures get +1/+1.").unwrap();
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: 1 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddToughness { value: 1 }));
    for m in &def.modifications {
        assert!(
            !matches!(m, ContinuousModification::SetColor { .. }),
            "Unexpected SetColor in gets-pump line, got {:?}",
            def.modifications
        );
    }
}

#[test]
fn static_all_subject_are_color_rejects_in_addition_type_form() {
    // Regression guard: "All permanents are artifacts in addition to ..."
    // must route to parse_all_permanents_are_type (AddType), not be mis-parsed
    // here. parse_color_predicate rejects the trailing " in addition..." suffix
    // because it's not a bare color word.
    let def = parse_static_line("All permanents are artifacts in addition to their other types.")
        .unwrap();
    for m in &def.modifications {
        assert!(
            !matches!(m, ContinuousModification::SetColor { .. }),
            "Unexpected SetColor for type-addition line, got {:?}",
            def.modifications
        );
    }
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddType {
            core_type: crate::types::card_type::CoreType::Artifact,
        }));
}

#[test]
fn static_all_elves_are_green() {
    // CR 613.1e + CR 105.1: non-black, non-colorless color on a plural
    // creature subtype — exercises the parse_color_list single-color path
    // plus typed_filter_for_subtype routing.
    let def = parse_static_line("All Elves are green.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    if let Some(TargetFilter::Typed(ref tf)) = def.affected {
        assert!(
            tf.type_filters.contains(&TypeFilter::Creature),
            "Expected Creature type filter (Elves route via typed_filter_for_subtype), \
                 got {:?}",
            tf.type_filters
        );
        assert!(
            tf.type_filters
                .contains(&TypeFilter::Subtype("Elf".to_string())),
            "Expected Elf subtype filter, got {:?}",
            tf.type_filters
        );
    } else {
        panic!("Expected Typed filter, got {:?}", def.affected);
    }
    assert_eq!(
        def.modifications,
        vec![ContinuousModification::SetColor {
            colors: vec![ManaColor::Green]
        }]
    );
}

#[test]
fn static_all_treasures_are_colorless() {
    // CR 613.1e + CR 105.2c: artifact-subtype subject — `typed_filter_for_subtype`
    // must route Treasure → Artifact core type, not default to Creature.
    let def = parse_static_line("All Treasures are colorless.").unwrap();
    if let Some(TargetFilter::Typed(ref tf)) = def.affected {
        assert!(
            tf.type_filters.contains(&TypeFilter::Artifact),
            "Expected Artifact core type for Treasures, got {:?}",
            tf.type_filters
        );
        assert!(
            tf.type_filters
                .contains(&TypeFilter::Subtype("Treasure".to_string())),
            "Expected Treasure subtype filter, got {:?}",
            tf.type_filters
        );
    } else {
        panic!("Expected Typed filter, got {:?}", def.affected);
    }
    assert_eq!(
        def.modifications,
        vec![ContinuousModification::SetColor { colors: vec![] }]
    );
}

#[test]
fn static_all_creatures_are_white_and_blue() {
    // CR 105.1: multi-color predicate via parse_color_list. Verifies the
    // predicate path is not limited to single colors.
    let def = parse_static_line("All creatures are white and blue.").unwrap();
    assert_eq!(
        def.modifications,
        vec![ContinuousModification::SetColor {
            colors: vec![ManaColor::White, ManaColor::Blue]
        }]
    );
}

#[test]
fn static_all_creatures_are_all_colors() {
    let def = parse_static_line("All creatures are all colors.").unwrap();
    assert_eq!(
        def.modifications,
        vec![ContinuousModification::SetColor {
            colors: ManaColor::ALL.to_vec()
        }]
    );
}

#[test]
fn static_all_subject_are_color_falls_through_to_land_type_change() {
    // Regression guard: "All lands are Plains." has a non-color predicate,
    // so parse_color_predicate must reject and allow the outer dispatcher
    // to continue through to parse_land_type_change. Expect SetBasicLandType
    // (or equivalent land-type machinery) — not SetColor.
    let def = parse_static_line("All lands are Plains.").unwrap();
    assert!(
        !def.modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::SetColor { .. })),
        "land type-change line must not produce SetColor, got {:?}",
        def.modifications
    );
    assert!(
        def.modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::SetBasicLandType { .. }
                | ContinuousModification::AddSubtype { .. }
        )),
        "expected a land-type modification, got {:?}",
        def.modifications
    );
}

#[test]
fn static_self_is_colorless_is_cda_all_zones() {
    // CR 604.3 + CR 604.3a + CR 105.2c: Ghostfire-style self color CDA.
    let def = parse_static_line("~ is colorless.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert!(def.characteristic_defining);
    assert_eq!(
        def.modifications,
        vec![ContinuousModification::SetColor { colors: vec![] }]
    );
    assert_eq!(
        def.active_zones,
        vec![
            Zone::Library,
            Zone::Hand,
            Zone::Battlefield,
            Zone::Graveyard,
            Zone::Stack,
            Zone::Exile,
            Zone::Command,
        ]
    );
}

#[test]
fn static_raw_cardname_is_colorless_is_not_contextless_self_cda() {
    assert!(parse_static_line("Ghostfire is colorless.").is_none());
}

#[test]
fn static_self_is_multicolor_cda() {
    let def = parse_static_line("~ is white and blue.").unwrap();
    assert_eq!(
        def.modifications,
        vec![ContinuousModification::SetColor {
            colors: vec![ManaColor::White, ManaColor::Blue]
        }]
    );
    assert!(def.characteristic_defining);
}

#[test]
fn static_self_is_all_colors_cda() {
    let def = parse_static_line("~ is all colors.").unwrap();
    assert_eq!(
        def.modifications,
        vec![ContinuousModification::SetColor {
            colors: ManaColor::ALL.to_vec()
        }]
    );
    assert!(def.characteristic_defining);
}

// --- Group A: Chosen color/type creature pump ---

#[test]
fn static_chosen_color_pump() {
    // Hall of Triumph: "Creatures you control of the chosen color get +1/+1."
    let def = parse_static_line("Creatures you control of the chosen color get +1/+1.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(
                tf.properties.contains(&FilterProp::IsChosenColor),
                "Expected IsChosenColor property"
            );
        }
        other => panic!("Expected Some(Typed filter), got {other:?}"),
    }
}

#[test]
fn static_chosen_type_pump() {
    // "Creatures of the chosen type your opponents control get -1/-1."
    let def = parse_static_line("Creatures of the chosen type your opponents control get -1/-1.")
        .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::Opponent));
            assert!(
                tf.properties.contains(&FilterProp::IsChosenCreatureType),
                "Expected IsChosenCreatureType property"
            );
        }
        other => panic!("Expected Some(Typed filter), got {other:?}"),
    }
}

#[test]
fn parser_shape_arcane_adaptation_chosen_type_applies_to_creatures_you_control() {
    let def = parse_static_line(
        "Creatures you control are the chosen type in addition to their other types.",
    )
    .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(def.modifications.iter().any(|modification| matches!(
        modification,
        ContinuousModification::AddChosenSubtype {
            kind: ChosenSubtypeKind::CreatureType
        }
    )));
    assert_eq!(
            def.description.as_deref(),
            Some("Creatures you control are the chosen type in addition to their other types."),
            "the unsupported creature-spell/nonbattlefield-card tail must not be represented by the battlefield-only static"
        );
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(tf
                .type_filters
                .iter()
                .any(|filter| matches!(filter, TypeFilter::Creature)));
        }
        other => panic!("Expected Some(Typed filter), got {other:?}"),
    }
}

// CR 607.2d + CR 301.7: Lifecraft Engine grants the chosen creature subtype to
// Vehicle permanents you control — not the Creature card type. The additive-type
// fallback must not mis-tokenize "the chosen creature type" as AddType(Creature).
#[test]
fn parser_shape_lifecraft_engine_vehicle_chosen_creature_type_static() {
    let def = parse_static_line(
        "Vehicle creatures you control are the chosen creature type in addition to their other types.",
    )
    .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(def.modifications.iter().all(|modification| matches!(
        modification,
        ContinuousModification::AddChosenSubtype {
            kind: ChosenSubtypeKind::CreatureType
        }
    )));
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert_eq!(tf.get_subtype(), Some("Vehicle"));
            assert!(
                !tf.type_filters
                    .iter()
                    .any(|filter| matches!(filter, TypeFilter::Creature)),
                "Vehicle scope must not require the Creature card type"
            );
        }
        other => panic!("Expected Some(Typed filter), got {other:?}"),
    }
}

// CR 613.1d + CR 205.3m: Maskwood Nexus's battlefield static — "Creatures
// you control are every creature type." — must lower to a Layer 4
// type-changing effect that adds every creature type (CR 205.3m) to each
// creature the controller has on the battlefield. The non-battlefield
// "the same is true for ..." tail is stripped by the dispatcher in
// `oracle.rs`; this test pins the battlefield-only static directly.
#[test]
fn parser_shape_maskwood_nexus_every_creature_type_applies_to_creatures_you_control() {
    let def = parse_static_line("Creatures you control are every creature type.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(def
        .modifications
        .iter()
        .any(|modification| matches!(modification, ContinuousModification::AddAllCreatureTypes)));
    assert_eq!(
        def.description.as_deref(),
        Some("Creatures you control are every creature type."),
    );
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(tf
                .type_filters
                .iter()
                .any(|filter| matches!(filter, TypeFilter::Creature)));
        }
        other => panic!("Expected Some(Typed filter), got {other:?}"),
    }
}

// Symmetric "each creature you control is every creature type" variant.
// No known printing uses this exact phrasing, but the parser's subject
// combinator already accepts it (parallel to Arcane Adaptation /
// Xenograft), so we pin the variant to guard against regressions.
#[test]
fn parser_shape_every_creature_type_applies_to_each_creature_you_control() {
    let def = parse_static_line("Each creature you control is every creature type.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(def
        .modifications
        .iter()
        .any(|modification| matches!(modification, ContinuousModification::AddAllCreatureTypes)));
}

#[test]
fn parser_shape_xenograft_chosen_type_applies_to_each_creature_you_control() {
    let def = parse_static_line(
        "Each creature you control is the chosen type in addition to its other types.",
    )
    .unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert!(def.modifications.iter().any(|modification| matches!(
        modification,
        ContinuousModification::AddChosenSubtype {
            kind: ChosenSubtypeKind::CreatureType
        }
    )));
    assert_eq!(
        def.description.as_deref(),
        Some("Each creature you control is the chosen type in addition to its other types.")
    );
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(tf
                .type_filters
                .iter()
                .any(|filter| matches!(filter, TypeFilter::Creature)));
        }
        other => panic!("Expected Some(Typed filter), got {other:?}"),
    }
}

#[test]
fn parser_shape_evelyn_collection_counter_play_permission_static_is_not_unimplemented() {
    let def = parse_static_line(
            "Once each turn, you may play a card from exile with a collection counter on it if it was exiled by an ability you controlled, and you may spend mana as though it were mana of any color to cast it.",
        )
        .unwrap();
    assert_eq!(
        def.mode,
        StaticMode::Other("LinkedCollectionCounterPlayPermission".to_string())
    );
}

// --- Group B: Generic activated ability cost reduction ---

#[test]
fn static_reduce_activated_ability_cost_generic() {
    // Training Grounds: "Activated abilities of creatures you control cost {2} less to activate."
    let def = parse_static_line(
        "Activated abilities of creatures you control cost {2} less to activate.",
    )
    .unwrap();
    assert_eq!(
        def.mode,
        StaticMode::ReduceAbilityCost {
            keyword: "activated".to_string(),
            amount: 2,
            minimum_mana: None,
            dynamic_count: None,
        }
    );
}

#[test]
fn static_reduce_activated_ability_cost_generic_with_minimum() {
    let def = parse_static_line(
            "Activated abilities of creatures you control cost {2} less to activate. This effect can't reduce the mana in that cost to less than one mana.",
        )
        .unwrap();
    assert_eq!(
        def.mode,
        StaticMode::ReduceAbilityCost {
            keyword: "activated".to_string(),
            amount: 2,
            minimum_mana: Some(1),
            dynamic_count: None,
        }
    );
}

#[test]
fn static_reduce_activated_ability_cost_enchanted_artifact_with_minimum() {
    let def = parse_static_line(
            "Enchanted artifact's activated abilities cost {2} less to activate. This effect can't reduce the mana in that cost to less than one mana.",
        )
        .unwrap();
    assert_eq!(
        def.mode,
        StaticMode::ReduceAbilityCost {
            keyword: "activated".to_string(),
            amount: 2,
            minimum_mana: Some(1),
            dynamic_count: None,
        }
    );
    assert!(matches!(
        def.affected,
        Some(TargetFilter::Typed(TypedFilter { .. }))
    ));
}

#[test]
fn static_reduce_activated_ability_cost_equipped_artifact_with_minimum() {
    let def = parse_static_line(
            "Equipped artifact's activated abilities cost {2} less to activate. This effect can't reduce the mana in that cost to less than one mana.",
        )
        .unwrap();
    assert_eq!(
        def.mode,
        StaticMode::ReduceAbilityCost {
            keyword: "activated".to_string(),
            amount: 2,
            minimum_mana: Some(1),
            dynamic_count: None,
        }
    );
    assert!(matches!(
        def.affected,
        Some(TargetFilter::Typed(TypedFilter { .. }))
    ));
}

// --- Group C: Spells you cast have keyword ---

#[test]
fn static_creature_spells_have_convoke() {
    // "Creature spells you cast have convoke."
    let def = parse_static_line("Creature spells you cast have convoke.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CastWithKeyword {
            keyword: Keyword::Convoke,
        }
    );
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "Expected Creature type filter"
            );
        }
        other => panic!("Expected Some(Typed filter), got {other:?}"),
    }
}

#[test]
fn static_noncreature_spells_have_convoke() {
    // "Noncreature spells you cast have convoke."
    let def = parse_static_line("Noncreature spells you cast have convoke.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CastWithKeyword {
            keyword: Keyword::Convoke,
        }
    );
}

#[test]
fn static_spells_from_exile_have_convoke() {
    // "Spells you cast from exile have convoke."
    let def = parse_static_line("Spells you cast from exile have convoke.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CastWithKeyword {
            keyword: Keyword::Convoke,
        }
    );
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(
                tf.properties
                    .contains(&FilterProp::InZone { zone: Zone::Exile }),
                "Expected InZone(Exile) property"
            );
        }
        other => panic!("Expected Some(Typed filter), got {other:?}"),
    }
}

// Witherbloom, the Balancer regression: "Instant and sorcery spells you cast
// have affinity for creatures." Two parser issues had to be fixed:
//  (1) `Keyword::from_str("affinity for creatures")` previously returned
//      `Keyword::Unknown` — so `apply_affinity_reduction` silently skipped
//      the granted keyword and no cost reduction was applied at cast time.
//  (2) `parse_type_phrase("Instant and sorcery")` returns `TargetFilter::Or`,
//      which the old `match TargetFilter::Typed(tf) => tf, _ => card()`
//      arm discarded — leaving the static affecting every spell card the
//      player casts (CR 113.3a: affected filter must scope recipients).
#[test]
fn static_instant_and_sorcery_spells_have_affinity_for_creatures() {
    let def = parse_static_line("Instant and sorcery spells you cast have affinity for creatures.")
        .unwrap();
    match &def.mode {
        StaticMode::CastWithKeyword {
            keyword: Keyword::Affinity(tf),
        } => {
            assert_eq!(
                tf.type_filters,
                vec![TypeFilter::Creature],
                "granted Affinity must carry the Creature type filter, not be Unknown"
            );
        }
        other => panic!(
            "expected CastWithKeyword(Affinity(Creature)), got {other:?}; \
                 if this panics with Unknown(\"affinity for creatures\") the keyword \
                 parser regressed"
        ),
    }
    match &def.affected {
        Some(TargetFilter::Or { filters }) => {
            assert_eq!(
                filters.len(),
                2,
                "expected two-branch Or for instant/sorcery"
            );
            let has_instant = filters.iter().any(|f| {
                matches!(
                    f,
                    TargetFilter::Typed(tf)
                        if tf.type_filters == vec![TypeFilter::Instant]
                            && tf.controller == Some(ControllerRef::You)
                )
            });
            let has_sorcery = filters.iter().any(|f| {
                matches!(
                    f,
                    TargetFilter::Typed(tf)
                        if tf.type_filters == vec![TypeFilter::Sorcery]
                            && tf.controller == Some(ControllerRef::You)
                )
            });
            assert!(
                has_instant && has_sorcery,
                "expected Or to contain both Instant(You) and Sorcery(You) branches, \
                     got {filters:?}"
            );
        }
        other => panic!(
            "expected Or(Instant, Sorcery), got {other:?}; if Typed(Card) the \
                 compound-type-phrase fallback regressed"
        ),
    }
}

// CR 702.74a + CR 613.1f: Ashling, the Limitless line 1 — "Elemental permanent
// spells you cast from your hand gain evoke {4} as you cast them." Exercises the
// new " gain " grant verb, the trailing " as you cast them" strip, and the
// granted-evoke keyword payload. The static must yield CastWithKeyword(Evoke).
#[test]
fn static_elemental_permanent_spells_gain_evoke() {
    use crate::types::keywords::{EvokeCost, Keyword};

    let def = parse_static_line(
        "Elemental permanent spells you cast from your hand gain evoke {4} as you cast them.",
    )
    .unwrap();
    match &def.mode {
        StaticMode::CastWithKeyword {
            keyword: Keyword::Evoke(EvokeCost::Mana(cost)),
        } => {
            assert_eq!(
                cost.mana_value(),
                4,
                "granted evoke must carry the {{4}} cost, got {cost:?}"
            );
        }
        other => panic!("expected CastWithKeyword(Evoke(Mana({{4}}))), got {other:?}"),
    }
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(
                tf.type_filters.contains(&TypeFilter::Permanent),
                "expected Permanent type filter, got {:?}",
                tf.type_filters
            );
            assert_eq!(
                tf.get_subtype(),
                Some("Elemental"),
                "expected Elemental subtype, got {:?}",
                tf.type_filters
            );
            assert!(
                tf.properties
                    .contains(&FilterProp::InZone { zone: Zone::Hand }),
                "expected InZone(Hand) property, got {:?}",
                tf.properties
            );
        }
        other => panic!("expected Typed(Elemental permanent you cast), got {other:?}"),
    }
    assert_eq!(def.active_zones, vec![Zone::Battlefield]);
}

// Building-block test: the new " gain " separator is general, not Ashling-
// specific. "Creature spells you cast gain trample" → CastWithKeyword(Trample).
#[test]
fn static_creature_spells_gain_trample() {
    let def = parse_static_line("Creature spells you cast gain trample.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CastWithKeyword {
            keyword: Keyword::Trample,
        }
    );
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "expected Creature type filter, got {:?}",
                tf.type_filters
            );
        }
        other => panic!("expected Typed(Creature you cast), got {other:?}"),
    }
}

#[test]
fn static_spells_with_mana_value_ge_have_cascade() {
    // Imoti, Celebrant of Bounty: "Spells you cast with mana value 6 or greater have cascade."
    let def =
        parse_static_line("Spells you cast with mana value 6 or greater have cascade.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CastWithKeyword {
            keyword: Keyword::Cascade,
        }
    );
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(
                tf.properties.contains(&FilterProp::Cmc {
                    comparator: Comparator::GE,
                    value: QuantityExpr::Fixed { value: 6 },
                }),
                "Expected CmcGE(6) property, got {:?}",
                tf.properties
            );
        }
        other => panic!("Expected Some(Typed filter), got {other:?}"),
    }
}

#[test]
fn static_spells_from_hand_with_dynamic_mana_value_have_cascade() {
    let text = "During your turn, spells you cast from your hand with mana value X or less have cascade, where X is the total amount of life your opponents have lost this turn.";
    assert!(
        parse_spells_have_keyword_for_test(text).is_some(),
        "CastWithKeyword parser should own the Abaddon shape"
    );
    let def = parse_static_line(text).unwrap();

    assert_eq!(
        def.mode,
        StaticMode::CastWithKeyword {
            keyword: Keyword::Cascade,
        }
    );
    assert_eq!(def.condition, Some(StaticCondition::DuringYourTurn));
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(
                tf.properties
                    .contains(&FilterProp::InZone { zone: Zone::Hand }),
                "Expected InZone(Hand), got {:?}",
                tf.properties
            );
            assert!(
                tf.properties.contains(&FilterProp::Cmc {
                    comparator: Comparator::LE,
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::LifeLostThisTurn {
                            player: PlayerScope::Opponent {
                                aggregate: AggregateFunction::Sum,
                            },
                        },
                    },
                }),
                "Expected dynamic CmcLE(opponents life lost), got {:?}",
                tf.properties
            );
        }
        other => panic!("Expected Some(Typed filter), got {other:?}"),
    }
}

#[test]
fn static_creature_spells_with_mana_value_ge_have_keyword() {
    // Type-prefixed + MV qualifier: confirms the type filter and the
    // CmcGE prop coexist on the same affected filter.
    let def =
        parse_static_line("Creature spells you cast with mana value 4 or greater have trample.")
            .unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CastWithKeyword {
            keyword: Keyword::Trample,
        }
    );
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "Expected Creature type filter, got {:?}",
                tf.type_filters
            );
            assert!(
                tf.properties.contains(&FilterProp::Cmc {
                    comparator: Comparator::GE,
                    value: QuantityExpr::Fixed { value: 4 },
                }),
                "Expected CmcGE(4), got {:?}",
                tf.properties
            );
        }
        other => panic!("Expected Some(Typed filter), got {other:?}"),
    }
}

#[test]
fn static_spells_from_exile_with_mana_value_ge_have_keyword() {
    // Combined zone + MV qualifier — both should land on the same filter.
    let def =
        parse_static_line("Spells you cast from exile with mana value 4 or greater have cascade.")
            .unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CastWithKeyword {
            keyword: Keyword::Cascade,
        }
    );
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(
                tf.properties
                    .contains(&FilterProp::InZone { zone: Zone::Exile }),
                "Expected InZone(Exile), got {:?}",
                tf.properties
            );
            assert!(
                tf.properties.contains(&FilterProp::Cmc {
                    comparator: Comparator::GE,
                    value: QuantityExpr::Fixed { value: 4 },
                }),
                "Expected CmcGE(4), got {:?}",
                tf.properties
            );
        }
        other => panic!("Expected Some(Typed filter), got {other:?}"),
    }
}

#[test]
fn static_creature_spells_have_convoke_no_mv_regression() {
    // Regression: bare "have keyword" without an MV/zone qualifier still
    // parses cleanly (the cursor walk must not require any qualifier).
    let def = parse_static_line("Creature spells you cast have convoke.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CastWithKeyword {
            keyword: Keyword::Convoke,
        }
    );
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(
                !tf.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::Cmc {
                        comparator: Comparator::GE,
                        ..
                    } | FilterProp::Cmc {
                        comparator: Comparator::LE,
                        ..
                    }
                )),
                "Did not expect any Cmc property, got {:?}",
                tf.properties
            );
        }
        other => panic!("Expected Some(Typed filter), got {other:?}"),
    }
}

#[test]
fn static_each_instant_and_sorcery_spell_you_cast_has_casualty() {
    let def = parse_static_line("Each instant and sorcery spell you cast has casualty 1.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CastWithKeyword {
            keyword: Keyword::Casualty(1),
        }
    );
    match &def.affected {
        Some(TargetFilter::Or { filters }) => {
            assert!(
                filters.iter().all(|filter| matches!(
                    filter,
                    TargetFilter::Typed(tf)
                        if tf.controller == Some(ControllerRef::You)
                            && (tf.type_filters.contains(&TypeFilter::Instant)
                                || tf.type_filters.contains(&TypeFilter::Sorcery))
                )),
                "Expected instant/sorcery filters controlled by You, got {filters:?}"
            );
        }
        other => panic!("Expected Some(Or instant/sorcery filter), got {other:?}"),
    }
}

#[test]
fn static_creature_cards_not_on_battlefield_have_flash() {
    // Leyline of Anticipation variant: "Creature cards you own that aren't on the battlefield have flash."
    let def =
        parse_static_line("Creature cards you own that aren't on the battlefield have flash.")
            .unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CastWithKeyword {
            keyword: Keyword::Flash,
        }
    );
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "Expected Creature type filter"
            );
        }
        other => panic!("Expected Some(Typed filter), got {other:?}"),
    }
}

// --- Group: legendary + colored qualifiers on spell-keyword statics ---
// CR 205.4a (supertype) + CR 105.2 (color count). Amazing Spider-Man's back
// face grants web-slinging only to "legendary spells … that's one or more
// colors"; the affected filter must carry BOTH qualifiers, and the supertype
// must be emitted exactly once (no parse_type_phrase double-emit).

#[test]
fn static_legendary_colored_spells_have_web_slinging() {
    // Amazing Spider-Man (SPM #10), back face.
    let def = parse_static_line(
        "Each legendary spell you cast that's one or more colors has web-slinging {G}{W}{U}.",
    )
    .unwrap();
    match &def.mode {
        StaticMode::CastWithKeyword {
            keyword: Keyword::WebSlinging(cost),
        } => {
            let ManaCost::Cost { shards, generic } = cost else {
                panic!("expected {{G}}{{W}}{{U}} Cost, got {cost:?}");
            };
            assert_eq!(*generic, 0, "web-slinging cost has no generic mana");
            use crate::types::mana::ManaCostShard;
            assert!(
                shards.contains(&ManaCostShard::Green)
                    && shards.contains(&ManaCostShard::White)
                    && shards.contains(&ManaCostShard::Blue),
                "expected G/W/U shards, got {shards:?}"
            );
        }
        other => panic!("expected CastWithKeyword(WebSlinging), got {other:?}"),
    }
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(
                tf.properties.contains(&FilterProp::HasSupertype {
                    value: Supertype::Legendary,
                }),
                "expected HasSupertype(Legendary), got {:?}",
                tf.properties
            );
            assert!(
                tf.properties.contains(&FilterProp::ColorCount {
                    comparator: Comparator::GE,
                    count: 1,
                }),
                "expected ColorCount(GE,1), got {:?}",
                tf.properties
            );
        }
        other => panic!("expected Some(Typed) affected filter, got {other:?}"),
    }
}

#[test]
fn static_legendary_creature_spells_emit_supertype_once() {
    // Compound subject: supertype must be emitted exactly once (peel here OR
    // parse_type_phrase, never both) and the Creature type must be present.
    let def = parse_static_line("Each legendary creature spell you cast has flash.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CastWithKeyword {
            keyword: Keyword::Flash,
        }
    );
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            let supertype_count = tf
                .properties
                .iter()
                .filter(|p| {
                    matches!(
                        p,
                        FilterProp::HasSupertype {
                            value: Supertype::Legendary,
                        }
                    )
                })
                .count();
            assert_eq!(
                supertype_count, 1,
                "HasSupertype(Legendary) must appear exactly once, got {:?}",
                tf.properties
            );
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "expected Creature type filter, got {:?}",
                tf.type_filters
            );
        }
        other => panic!("expected Some(Typed) affected filter, got {other:?}"),
    }
}

#[test]
fn static_exactly_n_color_spells_carry_color_count_eq() {
    // "exactly three colors" → ColorCount{EQ,3} on the affected filter.
    // (Threefold Signal's real text grants "replicate {3}", but `replicate`
    // is not yet a grantable keyword in this parser path — a pre-existing
    // limitation unrelated to the color-count clause under test. Use a
    // known-grantable keyword (flash) so the static parses end-to-end while
    // still exercising the "exactly N colors" branch.)
    let def =
        parse_static_line("Each spell you cast that's exactly three colors has flash.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::CastWithKeyword {
            keyword: Keyword::Flash,
        }
    );
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(
                tf.properties.contains(&FilterProp::ColorCount {
                    comparator: Comparator::EQ,
                    count: 3,
                }),
                "expected ColorCount(EQ,3), got {:?}",
                tf.properties
            );
        }
        other => panic!("expected Some(Typed) affected filter, got {other:?}"),
    }
}

#[test]
fn static_plain_spells_have_flash_no_qualifier_leak() {
    // "Spells you cast have flash." — no ColorCount / HasSupertype must leak in.
    let def = parse_static_line("Spells you cast have flash.").unwrap();
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(
                !tf.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::ColorCount { .. } | FilterProp::HasSupertype { .. }
                )),
                "no ColorCount/HasSupertype should be present, got {:?}",
                tf.properties
            );
        }
        other => panic!("expected Some(Typed) affected filter, got {other:?}"),
    }
}

// --- Group: Prohibition-family statics (CR 305.1, 701.21, 701.27, 702.5, 702.6) ---
// Each test proves that `parse_static_line` / `parse_static_line_multi` emits the
// canonical `StaticMode::Other("...")` name so the corresponding runtime guard in
// the engine (e.g., `object_has_static_other(id, "CantBeSacrificed")`) can observe it.

#[test]
fn static_cant_be_sacrificed_self_ref() {
    // CR 701.21: Hithlain Rope — "This artifact can't be sacrificed."
    let def = parse_static_line("This artifact can't be sacrificed.").unwrap();
    assert_eq!(def.mode, StaticMode::Other("CantBeSacrificed".to_string()));
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
}

#[test]
fn static_cant_be_enchanted_self_ref() {
    // CR 702.5: Anti-Magic Aura variant — "This creature can't be enchanted by other Auras."
    let def = parse_static_line("This creature can't be enchanted by other Auras.").unwrap();
    assert_eq!(def.mode, StaticMode::Other("CantBeEnchanted".to_string()));
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
}

#[test]
fn static_cant_be_equipped_self_ref() {
    // CR 702.6: Goblin Brawler — "This creature can't be equipped."
    let def = parse_static_line("This creature can't be equipped.").unwrap();
    assert_eq!(def.mode, StaticMode::Other("CantBeEquipped".to_string()));
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
}

#[test]
fn static_cant_pay_life_or_sacrifice_nonland_permanents_emits_cost_locks() {
    let defs = parse_static_line_multi(
            "Players can't pay life or sacrifice nonland permanents to cast spells or activate abilities.",
        );
    assert_eq!(defs.len(), 2, "expected pay-life and sacrifice locks");

    assert!(defs.iter().any(|def| matches!(
        def.mode,
        StaticMode::CantPayCost {
            who: ProhibitionScope::AllPlayers,
            cost: CostPaymentProhibition::PayLife,
        }
    )));
    assert!(defs.iter().any(|def| matches!(
        &def.mode,
        StaticMode::CantPayCost {
            who: ProhibitionScope::AllPlayers,
            cost: CostPaymentProhibition::Sacrifice {
                filter: TargetFilter::Typed(filter),
            },
        } if filter.type_filters.contains(&TypeFilter::Permanent)
            && filter
                .type_filters
                .contains(&TypeFilter::Non(Box::new(TypeFilter::Land)))
    )));
}

#[test]
fn static_life_total_cant_change_emits_both_locks_self_scope() {
    // CR 119.7 + CR 119.8: Platinum Emperion — "Your life total can't change."
    // Must emit BOTH CantGainLife and CantLoseLife scoped to controller.
    let defs = parse_static_line_multi("Your life total can't change.");
    let modes: Vec<_> = defs.iter().map(|d| d.mode.clone()).collect();
    assert_eq!(modes.len(), 2, "expected exactly 2 statics, got {modes:?}");
    assert!(modes.contains(&StaticMode::CantGainLife));
    assert!(modes.contains(&StaticMode::CantLoseLife));
    for def in &defs {
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::You),
                ..
            }))
        ));
    }
}

#[test]
fn static_life_total_cant_change_opponent_scope() {
    // CR 119.7 + CR 119.8: "Your opponents' life totals can't change."
    let defs = parse_static_line_multi("Your opponents' life totals can't change.");
    let modes: Vec<_> = defs.iter().map(|d| d.mode.clone()).collect();
    assert_eq!(modes.len(), 2);
    assert!(modes.contains(&StaticMode::CantGainLife));
    assert!(modes.contains(&StaticMode::CantLoseLife));
    for def in &defs {
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::Opponent),
                ..
            }))
        ));
    }
}

#[test]
fn static_life_total_cannot_change_alt_spelling() {
    // "cannot" alternative phrasing should also work.
    let defs = parse_static_line_multi("Your life total cannot change.");
    let modes: Vec<_> = defs.iter().map(|d| d.mode.clone()).collect();
    assert_eq!(modes.len(), 2);
    assert!(modes.contains(&StaticMode::CantGainLife));
    assert!(modes.contains(&StaticMode::CantLoseLife));
}

#[test]
fn static_retain_unspent_colored_mana_across_steps_and_phases() {
    use crate::types::mana::StepEndManaAction;
    let def =
        parse_static_line("You don't lose unspent red mana as steps and phases end.").unwrap();

    assert_eq!(
        def.mode,
        StaticMode::StepEndUnspentMana {
            filter: Some(ManaColor::Red),
            action: StepEndManaAction::Retain,
        }
    );
    assert_eq!(def.affected, Some(TargetFilter::Controller));
}

#[test]
fn static_retain_all_unspent_mana_across_steps_and_phases() {
    use crate::types::mana::StepEndManaAction;
    let def = parse_static_line("You don't lose unspent mana as steps and phases end.").unwrap();

    assert_eq!(
        def.mode,
        StaticMode::StepEndUnspentMana {
            filter: None,
            action: StepEndManaAction::Retain,
        }
    );
    assert_eq!(def.affected, Some(TargetFilter::Controller));
}

#[test]
fn static_retain_unspent_mana_accepts_curly_apostrophe() {
    use crate::types::mana::StepEndManaAction;
    let def =
        parse_static_line("You don’t lose unspent green mana as steps and phases end.").unwrap();

    assert_eq!(
        def.mode,
        StaticMode::StepEndUnspentMana {
            filter: Some(ManaColor::Green),
            action: StepEndManaAction::Retain,
        }
    );
}

#[test]
fn static_retain_unspent_mana_players_subject() {
    // CR 703.4q: Upwelling — "Players don't lose unspent mana as steps and
    // phases end." Affected scope widens from controller to every player.
    use crate::types::mana::StepEndManaAction;
    let def =
        parse_static_line("Players don't lose unspent mana as steps and phases end.").unwrap();

    assert_eq!(
        def.mode,
        StaticMode::StepEndUnspentMana {
            filter: None,
            action: StepEndManaAction::Retain,
        }
    );
    assert_eq!(def.affected, Some(TargetFilter::Player));
}

#[test]
fn static_transform_unspent_mana_colorless() {
    // CR 614.1a + CR 703.4q: Horizon Stone / Kruphix.
    use crate::types::mana::{ManaType, StepEndManaAction};
    let def =
        parse_static_line("If you would lose unspent mana, that mana becomes colorless instead.")
            .unwrap();

    assert_eq!(
        def.mode,
        StaticMode::StepEndUnspentMana {
            filter: None,
            action: StepEndManaAction::Transform(ManaType::Colorless),
        }
    );
    assert_eq!(def.affected, Some(TargetFilter::Controller));
}

#[test]
fn static_transform_unspent_mana_to_color() {
    use crate::types::mana::{ManaType, StepEndManaAction};
    // CR 614.1a + CR 703.4q: Omnath, Locus of All (Black) and Ozai (Red).
    let black =
        parse_static_line("If you would lose unspent mana, that mana becomes black instead.")
            .unwrap();
    assert_eq!(
        black.mode,
        StaticMode::StepEndUnspentMana {
            filter: None,
            action: StepEndManaAction::Transform(ManaType::Black),
        }
    );

    let red = parse_static_line("If you would lose unspent mana, that mana becomes red instead.")
        .unwrap();
    assert_eq!(
        red.mode,
        StaticMode::StepEndUnspentMana {
            filter: None,
            action: StepEndManaAction::Transform(ManaType::Red),
        }
    );
}

/// Printed-card round-trip tests for the step-end unspent mana class.
/// Each test feeds the exact printed Oracle text for the matching clause
/// (verified against `client/public/card-data.json`) through the parser
/// to confirm the unified `StepEndUnspentMana` variant emerges with the
/// right filter and action.
#[test]
fn card_text_upwelling_players_retention() {
    // CR 703.4q: Upwelling printed text.
    use crate::types::mana::StepEndManaAction;
    let def =
        parse_static_line("Players don't lose unspent mana as steps and phases end.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::StepEndUnspentMana {
            filter: None,
            action: StepEndManaAction::Retain,
        }
    );
    assert_eq!(def.affected, Some(TargetFilter::Player));
}

#[test]
fn card_text_omnath_locus_of_mana_green_retention() {
    // CR 703.4q: Omnath, Locus of Mana — printed first ability line.
    // The card's other line ("Omnath gets +1/+1 for each unspent green
    // mana you have.") is a separate static parsed independently.
    use crate::types::mana::StepEndManaAction;
    let def =
        parse_static_line("You don't lose unspent green mana as steps and phases end.").unwrap();
    assert_eq!(
        def.mode,
        StaticMode::StepEndUnspentMana {
            filter: Some(ManaColor::Green),
            action: StepEndManaAction::Retain,
        }
    );
    assert_eq!(def.affected, Some(TargetFilter::Controller));
}

#[test]
fn card_text_horizon_stone_transforms_to_colorless() {
    // CR 614.1a + CR 703.4q: Horizon Stone printed text.
    use crate::types::mana::{ManaType, StepEndManaAction};
    let def =
        parse_static_line("If you would lose unspent mana, that mana becomes colorless instead.")
            .unwrap();
    assert_eq!(
        def.mode,
        StaticMode::StepEndUnspentMana {
            filter: None,
            action: StepEndManaAction::Transform(ManaType::Colorless),
        }
    );
    assert_eq!(def.affected, Some(TargetFilter::Controller));
}

#[test]
fn card_text_kruphix_transforms_to_colorless() {
    // CR 614.1a + CR 703.4q: Kruphix, God of Horizons — the transform
    // clause printed alongside indestructible / devotion / no-max-hand.
    // Same Oracle wording as Horizon Stone; the other clauses route
    // through their own parser paths.
    use crate::types::mana::{ManaType, StepEndManaAction};
    let def =
        parse_static_line("If you would lose unspent mana, that mana becomes colorless instead.")
            .unwrap();
    assert_eq!(
        def.mode,
        StaticMode::StepEndUnspentMana {
            filter: None,
            action: StepEndManaAction::Transform(ManaType::Colorless),
        }
    );
}

#[test]
fn card_text_omnath_locus_of_all_transforms_to_black() {
    // CR 614.1a + CR 703.4q: Omnath, Locus of All printed text.
    use crate::types::mana::{ManaType, StepEndManaAction};
    let def = parse_static_line("If you would lose unspent mana, that mana becomes black instead.")
        .unwrap();
    assert_eq!(
        def.mode,
        StaticMode::StepEndUnspentMana {
            filter: None,
            action: StepEndManaAction::Transform(ManaType::Black),
        }
    );
}

#[test]
fn card_text_ozai_transforms_to_red() {
    // CR 614.1a + CR 703.4q: Ozai, the Phoenix King printed text. The
    // surrounding keyword and as-long-as-flying clauses route through
    // their own parser paths.
    use crate::types::mana::{ManaType, StepEndManaAction};
    let def = parse_static_line("If you would lose unspent mana, that mana becomes red instead.")
        .unwrap();
    assert_eq!(
        def.mode,
        StaticMode::StepEndUnspentMana {
            filter: None,
            action: StepEndManaAction::Transform(ManaType::Red),
        }
    );
}

/// CR 611.2b + CR 703.4q: SHAPE test for The Last Agni Kai's *full
/// printed Oracle text* — the two-sentence card (fight + excess-damage
/// mana rider on line 1, retention static on line 2) routed through
/// the card-level entry point `parse_oracle_text`.
///
/// The pre-parser line-splitter delivers each sentence to its own
/// dispatch path, so the retention clause reaches the spell-effect
/// parser independently of the fight clause; the existing
/// `until_end_of_turn_retain_unspent_color_mana_installs_generic_effect`
/// test in `oracle_effect/mod.rs` already covers the second-line
/// behavior in isolation. This regression test pins the full printed
/// text so a future change to line splitting, chained-clause handling,
/// or sentence dispatch cannot silently drop the retention sub-effect.
#[test]
fn card_text_the_last_agni_kai_full_printed_text() {
    use crate::parser::oracle::parse_oracle_text;
    use crate::types::ability::{Duration, Effect};
    use crate::types::mana::{ManaColor, StepEndManaAction};

    let parsed = parse_oracle_text(
        "Target creature you control fights target creature an opponent \
             controls. If the creature the opponent controls is dealt excess \
             damage this way, add that much {R}.\n\
             Until end of turn, you don't lose unspent red mana as steps and \
             phases end.",
        "The Last Agni Kai",
        &[],
        &["Instant".to_string()],
        &[],
    );

    // Exactly two top-level spell abilities, one per printed sentence.
    assert_eq!(
        parsed.abilities.len(),
        2,
        "expected 2 spell abilities, got {:?}",
        parsed.abilities
    );

    // Sentence 2: the retention rider installs a turn-scoped
    // `StepEndUnspentMana { Red, Retain }` via `GenericEffect`.
    let retention_ability = parsed
        .abilities
        .iter()
        .find(|a| matches!(*a.effect, Effect::GenericEffect { .. }))
        .expect("retention sentence should parse as GenericEffect");
    let Effect::GenericEffect {
        ref static_abilities,
        ref duration,
        ..
    } = *retention_ability.effect
    else {
        unreachable!()
    };
    assert_eq!(*duration, Some(Duration::UntilEndOfTurn));
    assert_eq!(static_abilities.len(), 1);
    assert_eq!(
        static_abilities[0].mode,
        StaticMode::StepEndUnspentMana {
            filter: Some(ManaColor::Red),
            action: StepEndManaAction::Retain,
        }
    );
    assert_eq!(static_abilities[0].affected, Some(TargetFilter::Controller));
}

#[test]
fn static_cant_be_equipped_or_enchanted_compound_multi() {
    // CR 701.3 + CR 702.5 + CR 702.6: The compound phrase must emit BOTH
    // CantBeEquipped and CantBeEnchanted. Fortifications are excluded by wording,
    // so CantBeAttached must NOT be emitted.
    let defs = parse_static_line_multi("This creature can't be equipped or enchanted.");
    let modes: Vec<_> = defs.iter().map(|d| d.mode.clone()).collect();
    assert!(
        modes.contains(&StaticMode::Other("CantBeEquipped".to_string())),
        "expected CantBeEquipped in {modes:?}"
    );
    assert!(
        modes.contains(&StaticMode::Other("CantBeEnchanted".to_string())),
        "expected CantBeEnchanted in {modes:?}"
    );
    assert!(
        !modes.contains(&StaticMode::Other("CantBeAttached".to_string())),
        "CantBeAttached is a superset and must not be emitted"
    );
}

#[test]
fn static_enchanted_creature_loses_abilities_and_cant_attack_or_block() {
    let defs = parse_static_line_multi(
        "Enchanted creature loses all abilities and can't attack or block.",
    );
    assert_eq!(defs.len(), 2, "expected two statics, got {defs:?}");
    assert!(defs.iter().any(|def| {
        def.mode == StaticMode::Continuous
            && def
                .modifications
                .contains(&ContinuousModification::RemoveAllAbilities)
    }));
    assert!(defs
        .iter()
        .any(|def| def.mode == StaticMode::CantAttackOrBlock));
    for def in defs {
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy])
            ))
        );
    }
}

#[test]
fn static_enchanted_creature_cant_attack_or_block_uses_enchanted_subject() {
    let def = parse_static_line("Enchanted creature can't attack or block.").unwrap();
    assert_eq!(def.mode, StaticMode::CantAttackOrBlock);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EnchantedBy])
        ))
    );
}

#[test]
fn static_enchanted_creatures_you_control_uses_attachment_predicate() {
    let def = parse_static_line("Enchanted creatures you control get +2/+2.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::HasAttachment {
                    kind: AttachmentKind::Aura,
                    controller: None,
                    exclude_source: crate::types::ability::SourceExclusion::Include,
                }])
        ))
    );
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddPower { value: 2 }));
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddToughness { value: 2 }));
}

#[test]
fn static_cant_transform_self_ref() {
    // CR 701.27: Immerwolf-style "non-Human Werewolves you control can't transform"
    // after subject-stripping reduces to the self-ref form in parse_static_line.
    let def = parse_static_line("This creature can't transform.").unwrap();
    assert_eq!(def.mode, StaticMode::Other("CantTransform".to_string()));
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
}

#[test]
fn static_cant_play_lands_you() {
    // CR 305.1: Aggressive Mining — "You can't play lands."
    let def = parse_static_line("You can't play lands.").unwrap();
    assert_eq!(def.mode, StaticMode::Other("CantPlayLand".to_string()));
    assert!(
        def.affected.is_some(),
        "affected player-scope filter required"
    );
}

#[test]
fn static_cant_play_lands_players() {
    // CR 305.1: Worms of the Earth — "Players can't play lands."
    let def = parse_static_line("Players can't play lands.").unwrap();
    assert_eq!(def.mode, StaticMode::Other("CantPlayLand".to_string()));
    assert!(
        def.affected.is_some(),
        "affected player-scope filter required"
    );
}

// --- CR 602.5 + CR 603.2a: Global filter-scoped CantBeActivated (Clarion/Karn class) ---

#[test]
fn cant_be_activated_self_ref_preserves_legacy_semantics() {
    // CR 602.5: Self-reference form (Chalice-of-Life class) must emit the
    // unit-default shape: `who = AllPlayers, source_filter = SelfRef`.
    let def = parse_static_line("Its activated abilities can't be activated.").unwrap();
    match def.mode {
        StaticMode::CantBeActivated {
            who,
            source_filter,
            exemption,
        } => {
            assert_eq!(who, ProhibitionScope::AllPlayers);
            assert_eq!(source_filter, TargetFilter::SelfRef);
            // CR 605.1a: Self-ref form has no exemption suffix.
            assert_eq!(exemption, ActivationExemption::None);
        }
        other => panic!("expected CantBeActivated, got {other:?}"),
    }
}

#[test]
fn cant_be_activated_self_ref_mana_exemption_suffix() {
    let def = parse_static_line(
        "Its activated abilities can't be activated unless they're mana abilities.",
    )
    .expect("self-reference CantBeActivated with mana exemption should parse");
    match def.mode {
        StaticMode::CantBeActivated {
            who,
            source_filter,
            exemption,
        } => {
            assert_eq!(who, ProhibitionScope::AllPlayers);
            assert_eq!(source_filter, TargetFilter::SelfRef);
            assert_eq!(exemption, ActivationExemption::ManaAbilities);
        }
        other => panic!("expected CantBeActivated, got {other:?}"),
    }
}

#[test]
fn cant_be_activated_compound_aura_mana_exemption_suffix() {
    let defs = parse_static_line_multi(
            "Enchanted permanent can't attack or block, and its activated abilities can't be activated unless they're mana abilities.",
        );
    let cant_be_activated = defs
        .iter()
        .find(|def| matches!(def.mode, StaticMode::CantBeActivated { .. }))
        .expect("compound Aura text should emit CantBeActivated");
    match &cant_be_activated.mode {
        StaticMode::CantBeActivated {
            who,
            source_filter,
            exemption,
        } => {
            assert_eq!(*who, ProhibitionScope::AllPlayers);
            assert_eq!(source_filter, &TargetFilter::SelfRef);
            assert_eq!(*exemption, ActivationExemption::ManaAbilities);
        }
        other => panic!("expected CantBeActivated, got {other:?}"),
    }
}

#[test]
fn cant_be_activated_compound_aura_with_cant_crew() {
    let defs = parse_static_line_multi(
        "Enchanted permanent can't attack, block, crew Vehicles, or have its activated abilities activated.",
    );
    let modes: Vec<&StaticMode> = defs.iter().map(|def| &def.mode).collect();

    assert!(
        modes.contains(&&StaticMode::CantAttack),
        "compound Aura text should emit CantAttack"
    );
    assert!(
        modes.contains(&&StaticMode::CantBlock),
        "compound Aura text should emit CantBlock"
    );
    assert!(
        modes.contains(&&StaticMode::CantCrew),
        "compound Aura text should emit CantCrew"
    );
    assert!(
        defs.iter()
            .any(|def| matches!(def.mode, StaticMode::CantBeActivated { .. })),
        "compound Aura text should emit CantBeActivated"
    );
    assert!(defs.iter().all(|def| {
        def.affected
            == Some(TargetFilter::Typed(
                TypedFilter::permanent().properties(vec![FilterProp::EnchantedBy]),
            ))
    }));
}

#[test]
fn cant_be_activated_clarion_multi_type_filter() {
    // CR 602.5 + CR 603.2a: Clarion Conqueror — "Activated abilities of artifacts,
    // creatures, and planeswalkers your opponents control can't be activated."
    // The activator axis is AllPlayers; opponent-ness rides on the filter's
    // `ControllerRef::Opponent`. `parse_type_phrase` emits an `Or`-disjunction of
    // `Typed` filters when a comma-separated type list is present — each variant
    // inherits the shared controller suffix via the post-process pass.
    let def = parse_static_line(
            "Activated abilities of artifacts, creatures, and planeswalkers your opponents control can't be activated.",
        )
        .expect("Clarion Conqueror Oracle text should parse");
    match def.mode {
        StaticMode::CantBeActivated {
            who,
            source_filter: TargetFilter::Or { filters },
            exemption,
        } => {
            assert_eq!(who, ProhibitionScope::AllPlayers);
            assert_eq!(exemption, ActivationExemption::None);
            assert_eq!(filters.len(), 3, "three type variants expected");
            // Each disjunct should be a Typed filter with opponent controller and
            // one of the three expected type filters.
            let mut seen_types: Vec<TypeFilter> = Vec::new();
            for f in &filters {
                match f {
                    TargetFilter::Typed(tf) => {
                        assert_eq!(tf.controller, Some(ControllerRef::Opponent));
                        assert_eq!(tf.type_filters.len(), 1);
                        seen_types.push(tf.type_filters[0].clone());
                    }
                    other => panic!("expected Typed variant, got {other:?}"),
                }
            }
            assert!(seen_types.iter().any(|t| matches!(t, TypeFilter::Artifact)));
            assert!(seen_types.iter().any(|t| matches!(t, TypeFilter::Creature)));
            assert!(seen_types
                .iter()
                .any(|t| matches!(t, TypeFilter::Planeswalker)));
        }
        other => panic!("expected CantBeActivated with Or-disjunction filter, got {other:?}"),
    }
}

#[test]
fn cant_be_activated_karn_single_type_filter() {
    // CR 602.5 + CR 603.2a: Karn, the Great Creator — "Activated abilities of
    // artifacts your opponents control can't be activated."
    let def = parse_static_line(
        "Activated abilities of artifacts your opponents control can't be activated.",
    )
    .expect("Karn Oracle text should parse");
    match def.mode {
        StaticMode::CantBeActivated {
            who,
            source_filter: TargetFilter::Typed(tf),
            exemption,
        } => {
            assert_eq!(who, ProhibitionScope::AllPlayers);
            assert_eq!(exemption, ActivationExemption::None);
            assert_eq!(tf.controller, Some(ControllerRef::Opponent));
            assert_eq!(tf.type_filters, vec![TypeFilter::Artifact]);
        }
        other => panic!("expected CantBeActivated, got {other:?}"),
    }
}

#[test]
fn cant_be_activated_pithing_needle_chosen_name_with_mana_exemption() {
    // CR 605.1a + CR 602.5 + CR 603.2a: Pithing Needle —
    // "Activated abilities of sources with the chosen name can't be activated
    // unless they're mana abilities."
    // Source filter binds to `HasChosenName`; exemption captures the mana-ability suffix.
    let def = parse_static_line(
            "Activated abilities of sources with the chosen name can't be activated unless they're mana abilities.",
        )
        .expect("Pithing Needle Oracle text should parse");
    match def.mode {
        StaticMode::CantBeActivated {
            who,
            source_filter,
            exemption,
        } => {
            assert_eq!(who, ProhibitionScope::AllPlayers);
            assert_eq!(source_filter, TargetFilter::HasChosenName);
            assert_eq!(exemption, ActivationExemption::ManaAbilities);
        }
        other => panic!("expected CantBeActivated, got {other:?}"),
    }
}

#[test]
fn cant_be_activated_phyrexian_revoker_chosen_name_no_exemption_suffix() {
    // CR 602.5 + CR 603.2a: Phyrexian Revoker — MTGJSON Oracle text omits the
    // "unless they're mana abilities" suffix on this card. Same source filter
    // shape as Pithing Needle, but `ActivationExemption::None`. The parser must
    // produce the same `HasChosenName` AST shape regardless of exemption suffix —
    // demonstrating the optional suffix combinator works in both branches.
    let def = parse_static_line(
        "Activated abilities of sources with the chosen name can't be activated.",
    )
    .expect("Phyrexian Revoker Oracle text should parse");
    match def.mode {
        StaticMode::CantBeActivated {
            who,
            source_filter,
            exemption,
        } => {
            assert_eq!(who, ProhibitionScope::AllPlayers);
            assert_eq!(source_filter, TargetFilter::HasChosenName);
            assert_eq!(exemption, ActivationExemption::None);
        }
        other => panic!("expected CantBeActivated, got {other:?}"),
    }
}

#[test]
fn cant_be_activated_sorcerous_spyglass_chosen_name_with_mana_exemption() {
    // CR 605.1a + CR 602.5: Sorcerous Spyglass — identical static on an artifact
    // that reveals an opponent's hand on ETB. Exercises composability: the static
    // parses identically regardless of the surrounding ETB shape.
    let def = parse_static_line(
            "Activated abilities of sources with the chosen name can't be activated unless they're mana abilities.",
        )
        .expect("Sorcerous Spyglass Oracle text should parse");
    match def.mode {
        StaticMode::CantBeActivated {
            source_filter,
            exemption,
            ..
        } => {
            assert_eq!(source_filter, TargetFilter::HasChosenName);
            assert_eq!(exemption, ActivationExemption::ManaAbilities);
        }
        other => panic!("expected CantBeActivated, got {other:?}"),
    }
}

// --- CR 701.23 + CR 609.3: CantSearchLibrary (Ashiok class) ---

#[test]
fn cant_search_library_ashiok() {
    // CR 701.23 + CR 609.3: Ashiok, Dream Render — "Spells and abilities your
    // opponents control can't cause their controller to search their library."
    let def = parse_static_line(
            "Spells and abilities your opponents control can't cause their controller to search their library.",
        )
        .expect("Ashiok Oracle text should parse");
    assert_eq!(
        def.mode,
        StaticMode::CantSearchLibrary {
            cause: ProhibitionScope::Opponents,
        }
    );
}

#[test]
fn cant_search_library_controller_variant() {
    // Building-block coverage: `you control` should map to Controller scope.
    let def = parse_static_line(
        "Spells and abilities you control can't cause their controller to search their library.",
    )
    .expect("controller-scoped variant should parse");
    assert_eq!(
        def.mode,
        StaticMode::CantSearchLibrary {
            cause: ProhibitionScope::Controller,
        }
    );
}

#[test]
fn cant_search_library_mindlock_orb_players() {
    // CR 701.23 + CR 609.3: Mindlock Orb — blanket all-players search prohibition.
    let def = parse_static_line("Players can't search libraries.")
        .expect("Mindlock Orb Oracle text should parse");
    assert_eq!(
        def.mode,
        StaticMode::CantSearchLibrary {
            cause: ProhibitionScope::AllPlayers,
        }
    );
}

#[test]
fn cant_search_library_each_player_may_not_variant() {
    // Variant phrasing uses identical all-players scope.
    let def = parse_static_line("Each player may not search libraries.")
        .expect("each-player variant should parse");
    assert_eq!(
        def.mode,
        StaticMode::CantSearchLibrary {
            cause: ProhibitionScope::AllPlayers,
        }
    );
}

#[test]
fn cant_search_library_opponents_form_deferred() {
    // Opponent-scoped direct-search phrasing remains deferred until the runtime
    // cause-vs-searcher axis is split.
    assert!(parse_static_line("Your opponents can't search libraries.").is_none());
}

// --- CR 603.2g + CR 603.6a + CR 700.4: SuppressTriggers (Torpor Orb / Hushbringer) ---

#[test]
fn suppress_triggers_torpor_orb_etb_only() {
    use crate::types::statics::SuppressedTriggerEvent;

    // CR 603.2g + CR 603.6a: Torpor Orb — "Creatures entering the battlefield
    // don't cause abilities to trigger." Event set is [EntersBattlefield] only.
    let def =
        parse_static_line("Creatures entering the battlefield don't cause abilities to trigger.")
            .expect("Torpor Orb Oracle text should parse");
    match def.mode {
        StaticMode::SuppressTriggers {
            source_filter: TargetFilter::Typed(tf),
            events,
        } => {
            assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
            assert_eq!(events, vec![SuppressedTriggerEvent::EntersBattlefield]);
        }
        other => panic!("expected SuppressTriggers, got {other:?}"),
    }
}

#[test]
fn suppress_triggers_torpor_orb_etb_without_the_battlefield() {
    use crate::types::statics::SuppressedTriggerEvent;

    // Errata variant: some printings drop "the battlefield" and just say
    // "Creatures entering don't cause abilities to trigger." — same semantics.
    let def = parse_static_line("Creatures entering don't cause abilities to trigger.")
        .expect("Short-form Oracle should parse");
    match def.mode {
        StaticMode::SuppressTriggers { events, .. } => {
            assert_eq!(events, vec![SuppressedTriggerEvent::EntersBattlefield]);
        }
        other => panic!("expected SuppressTriggers, got {other:?}"),
    }
}

#[test]
fn suppress_triggers_hushbringer_accepts_and_dying_variant() {
    use crate::types::statics::SuppressedTriggerEvent;

    // CR 603.2g + CR 700.4: The "and dying" phrasing is also accepted for
    // defensive parsing of errata/near-variants. Same event set as "or dying".
    let def = parse_static_line(
        "Creatures entering the battlefield and dying don't cause abilities to trigger.",
    )
    .expect("'and dying' variant should parse");
    match def.mode {
        StaticMode::SuppressTriggers { events, .. } => {
            assert_eq!(
                events,
                vec![
                    SuppressedTriggerEvent::EntersBattlefield,
                    SuppressedTriggerEvent::Dies,
                ]
            );
        }
        other => panic!("expected SuppressTriggers, got {other:?}"),
    }
}

#[test]
fn suppress_triggers_hushbringer_etb_and_dies() {
    use crate::types::statics::SuppressedTriggerEvent;

    // CR 603.2g + CR 603.6a + CR 700.4: Hushbringer's actual MTGJSON Oracle
    // text is "Creatures entering or dying don't cause abilities to trigger."
    // Event set is [EntersBattlefield, Dies] in canonical order.
    let def = parse_static_line("Creatures entering or dying don't cause abilities to trigger.")
        .expect("Hushbringer Oracle text should parse");
    match def.mode {
        StaticMode::SuppressTriggers {
            source_filter: TargetFilter::Typed(tf),
            events,
        } => {
            assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
            assert_eq!(
                events,
                vec![
                    SuppressedTriggerEvent::EntersBattlefield,
                    SuppressedTriggerEvent::Dies,
                ]
            );
        }
        other => panic!("expected SuppressTriggers, got {other:?}"),
    }
}

// ------------------------------------------------------------------------
// Inverted "As long as <cond>, <effect>" rewrite tests (CR 611.3a)
// ------------------------------------------------------------------------

fn rewrite(text: &str) -> Option<String> {
    let stripped = strip_reminder_text(text);
    let lower = stripped.to_lowercase();
    let tp = TextPair::new(&stripped, &lower);
    try_split_inverted_as_long_as(&tp).map(|s| s.canonical)
}

fn split_condition(text: &str) -> Option<String> {
    let stripped = strip_reminder_text(text);
    let lower = stripped.to_lowercase();
    let tp = TextPair::new(&stripped, &lower);
    try_split_inverted_as_long_as(&tp).map(|s| s.condition_text)
}

#[test]
fn inverted_rewrites_auriok_shape() {
    let got = rewrite(
            "As long as ~ is equipped, each creature you control that's a Soldier or a Knight gets +1/+1.",
        )
        .expect("auriok shape must rewrite");
    assert_eq!(
            got,
            "each creature you control that's a Soldier or a Knight gets +1/+1 as long as ~ is equipped"
        );
}

#[test]
fn inverted_rewrites_watchdog_shape() {
    let got = rewrite("As long as ~ is untapped, all creatures attacking you get -1/-0.")
        .expect("watchdog shape must rewrite");
    assert_eq!(
        got,
        "all creatures attacking you get -1/-0 as long as ~ is untapped"
    );
}

#[test]
fn inverted_preserves_original_case() {
    let got = rewrite("As long as ~ is attacking, defending player can't cast spells.")
        .expect("should rewrite");
    assert!(got.contains("defending player can't cast spells")); // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    assert!(got.ends_with("as long as ~ is attacking")); // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
}

#[test]
fn inverted_returns_none_without_commas() {
    let got = rewrite("As long as ~ is red with no trailing clause at all without commas");
    assert!(got.is_none());
}

#[test]
fn inverted_liu_bei_internal_commas_without_effect_subject() {
    // Liu Bei, Lord of Shu: "you control a permanent named Guan Yu, Sainted Warrior or a
    // permanent named Zhang Fei, Fierce Warrior" — commas are inside the condition and
    // no trailing effect clause starts with a recognized subject, so the scanner must
    // not split (returns None).
    let got = rewrite(
            "As long as you control a permanent named Guan Yu, Sainted Warrior or a permanent named Zhang Fei, Fierce Warrior",
        );
    assert!(
        got.is_none(),
        "must not split on condition-internal commas without effect subject; got {got:?}"
    );
}

#[test]
fn inverted_handles_trailing_period() {
    let got = rewrite("As long as ~ is equipped, it gets +1/+1.").expect("must rewrite");
    assert!(!got.ends_with('.'));
    assert_eq!(got, "it gets +1/+1 as long as ~ is equipped");
}

#[test]
fn effect_subject_prefix_word_boundary() {
    assert!(parse_effect_subject_prefix("it gets +1/+1").is_ok());
    // Word boundary: "its mana value" must NOT match via "it ".
    assert!(parse_effect_subject_prefix("its mana value is 4").is_err());
    assert!(parse_effect_subject_prefix("each creature you control gets +1/+1").is_ok());
    assert!(parse_effect_subject_prefix("eachother").is_err());
}

#[test]
fn inverted_splits_auriok_condition_cleanly() {
    // The primary success criterion: the condition is separated from the effect clause.
    // Whether the effect clause parses into modifications depends on downstream
    // subject-phrase support, which is separate work.
    let cond = split_condition(
            "As long as ~ is equipped, each creature you control that's a Soldier or a Knight gets +1/+1.",
        )
        .expect("must split");
    assert_eq!(cond, "~ is equipped");
}

#[test]
fn inverted_splits_watchdog_condition_cleanly() {
    let cond = split_condition("As long as ~ is untapped, all creatures attacking you get -1/-0.")
        .expect("must split");
    assert_eq!(cond, "~ is untapped");
}

#[test]
fn inverted_end_to_end_auriok_no_effect_bleed() {
    // End-to-end: the returned StaticDefinition must have a condition text that is
    // ONLY the condition (no effect-clause bleed-through). Modifications may remain
    // empty if downstream subject-phrase parsing doesn't yet handle the effect,
    // but that is a separate issue (and explicitly out-of-scope per task spec).
    let def = parse_static_line(
            "As long as ~ is equipped, each creature you control that's a Soldier or a Knight gets +1/+1.",
        )
        .expect("must parse");
    assert_eq!(def.mode, StaticMode::Continuous);
    match &def.condition {
        Some(StaticCondition::Unrecognized { text }) => {
            assert_eq!(text, "~ is equipped", "condition must be cleanly split");
            assert!(
                !text.contains("gets +1/+1"), // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
                "effect clause bled into condition text: {text:?}"
            );
        }
        Some(other) => {
            // Typed condition recognized — also acceptable, just confirm it's not
            // the bleed-through fallback.
            eprintln!("auriok: got typed condition {other:?}");
        }
        None => panic!("condition must be set"),
    }
    assert_eq!(
            def.description.as_deref(),
            Some(
                "As long as ~ is equipped, each creature you control that's a Soldier or a Knight gets +1/+1."
            ),
            "description must equal the original printed oracle text"
        );
}

#[test]
fn inverted_end_to_end_watchdog_no_effect_bleed() {
    let def = parse_static_line("As long as ~ is untapped, all creatures attacking you get -1/-0.")
        .expect("must parse");
    assert_eq!(def.mode, StaticMode::Continuous);
    match &def.condition {
        Some(StaticCondition::Unrecognized { text }) => {
            assert_eq!(text, "~ is untapped");
            assert!(
                !text.contains("get -1/-0"), // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
                "effect clause bled into condition text: {text:?}"
            );
        }
        Some(_) => {}
        None => panic!("condition must be set"),
    }
    assert_eq!(
        def.description.as_deref(),
        Some("As long as ~ is untapped, all creatures attacking you get -1/-0.")
    );
}

#[test]
fn inverted_falls_through_when_no_effect_subject_found() {
    // With no recognized effect-subject prefix after any comma, behavior must equal
    // today's generic fallback: a Continuous static with Unrecognized condition text
    // (the old bleed-through behavior is preserved as a strict non-regression baseline).
    let def = parse_static_line(
            "As long as you control a permanent named Guan Yu, Sainted Warrior or a permanent named Zhang Fei, Fierce Warrior.",
        )
        .expect("fallback must still match");
    assert_eq!(def.mode, StaticMode::Continuous);
    match def.condition {
        Some(StaticCondition::Unrecognized { .. }) => {}
        other => panic!("expected Unrecognized condition via fallback, got {other:?}"),
    }
}

// --- Hand-zone keyword grant statics (CR 702.94a + CR 400.3) ---

/// CR 702.94a: "Each instant and sorcery card in your hand has miracle {2}"
/// (Lorehold, the Historian) must parse as a Continuous static whose
/// affected filter carries `InZone { zone: Hand }` and whose modification
/// is `AddKeyword(Miracle({2}))`.
#[test]
fn hand_grant_lorehold_miracle() {
    let text = "Each instant and sorcery card in your hand has miracle {2}.";
    let def = parse_static_line(text).expect("Lorehold text must parse");
    assert_eq!(def.mode, StaticMode::Continuous);
    let affected = def.affected.expect("should have affected filter");
    assert!(
        affected.extract_in_zone() == Some(Zone::Hand),
        "affected filter should carry InZone: Hand, got {affected:?}"
    );
    assert!(
        def.modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddKeyword {
                keyword: crate::types::keywords::Keyword::Miracle(_)
            }
        )),
        "modifications should include AddKeyword(Miracle), got {:?}",
        def.modifications,
    );
}

/// CR 400.3: "Sliver cards in your hand have warp {3}" (Sliver Weftwinder)
/// — single-subtype hand-grant keyword. Confirms the parser covers the
/// typed-subtype class beyond Lorehold's instant/sorcery pair.
#[test]
fn hand_grant_sliver_weftwinder_warp() {
    let text = "Sliver cards in your hand have warp {3}.";
    let defs = parse_static_line_multi(text);
    assert!(
        !defs.is_empty(),
        "parse_static_line_multi returned empty for: {text}"
    );
    let def = defs
        .into_iter()
        .find(|d| {
            d.mode == StaticMode::Continuous
                && d.affected
                    .as_ref()
                    .map(|a| a.extract_in_zone() == Some(Zone::Hand))
                    .unwrap_or(false)
        })
        .expect("expected a hand-zone Continuous static in output");
    assert!(
        def.modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddKeyword {
                keyword: crate::types::keywords::Keyword::Warp(_)
            }
        )),
        "modifications should include AddKeyword(Warp), got {:?}",
        def.modifications,
    );
}

// ---------------------------------------------------------------------
// Combat-tax static family — class-level parser coverage.
// CR 508.1d + CR 508.1h + CR 118.12a: "[subject] can't attack/block unless
// [controller] pays [cost] [per-creature qualifier]" produces a typed
// `StaticCondition::UnlessPay` with the correct `UnlessPayScaling` variant.
// ---------------------------------------------------------------------

use crate::types::ability::UnlessPayScaling;

/// Helper: extract the `UnlessPay { cost, scaling, .. }` from a parsed
/// combat-tax static. Walks `StaticCondition::And` to find the embedded
/// `UnlessPay` so this helper works for both bare-tax statics
/// (Ghostly Prison) and conditional-tax statics
/// (Archangel of Tithes — `And { [Not(SourceIsTapped), UnlessPay {..}] }`).
fn extract_unless_pay(def: &StaticDefinition) -> (ManaCost, UnlessPayScaling) {
    let cond = def
        .condition
        .as_ref()
        .expect("combat-tax static must carry a condition");
    find_unless_pay(cond)
        .map(|(c, s)| (c.clone(), s.clone()))
        .unwrap_or_else(|| panic!("expected UnlessPay (possibly nested in And), got {cond:?}"))
}

fn find_unless_pay(cond: &StaticCondition) -> Option<(&ManaCost, &UnlessPayScaling)> {
    match cond {
        StaticCondition::UnlessPay { cost, scaling, .. } => Some((cost, scaling)),
        StaticCondition::And { conditions } => conditions.iter().find_map(find_unless_pay),
        _ => None,
    }
}

/// CR 508.1h: Ghostly Prison / Propaganda — fixed per-attacker mana.
/// Parses to `CantAttack` + opponents'-creature filter + `PerAffectedCreature` scaling.
#[test]
fn combat_tax_ghostly_prison_per_affected_creature() {
    let def = parse_static_line(
            "Creatures can't attack you unless their controller pays {2} for each creature they control that's attacking you.",
        )
        .expect("Ghostly Prison should parse");
    assert_eq!(def.mode, StaticMode::CantAttack);
    let (cost, scaling) = extract_unless_pay(&def);
    assert_eq!(cost.mana_value(), 2);
    assert!(matches!(scaling, UnlessPayScaling::PerAffectedCreature));
}

/// CR 508.1h + CR 202.3e: Sphere of Safety — dynamic {X} per attacker where X
/// is a battlefield count. Parses to `PerAffectedAndQuantityRef`.
#[test]
fn combat_tax_sphere_of_safety_per_affected_and_ref() {
    let def = parse_static_line(
            "Creatures can't attack you or planeswalkers you control unless their controller pays {X} for each of those creatures, where X is the number of enchantments you control.",
        )
        .expect("Sphere of Safety should parse");
    assert_eq!(def.mode, StaticMode::CantAttack);
    let (_cost, scaling) = extract_unless_pay(&def);
    assert!(matches!(
        scaling,
        UnlessPayScaling::PerAffectedAndQuantityRef { .. }
    ));
}

/// CR 118.12a: Cowed by Wisdom — aura combat tax scaled by a game-state
/// quantity without multiplying by the number of affected creatures.
#[test]
fn combat_tax_enchanted_creature_for_each_quantity_ref() {
    let def = parse_static_line(
            "Enchanted creature can't attack or block unless its controller pays {1} for each card in your hand.",
        )
        .expect("Cowed by Wisdom should parse");
    assert_eq!(def.mode, StaticMode::CantAttackOrBlock);
    let (cost, scaling) = extract_unless_pay(&def);
    assert_eq!(cost.mana_value(), 1);
    assert!(matches!(scaling, UnlessPayScaling::PerQuantityRef { .. }));
}

/// CR 118.12a + CR 202.3e: Nils, Discipline Enforcer — counter-gated subject
/// ("Each creature with one or more counters on it") with per-attacker-resolved
/// scaling ({X} = counters on THAT creature). Parses to `PerAffectedWithRef`
/// with `QuantityRef::AnyCountersOnTarget`, using a creature filter with
/// `FilterProp::Counters { CounterMatch::Any, GE, Fixed(1) }`.
#[test]
fn combat_tax_nils_per_affected_with_ref() {
    let def = parse_static_line(
            "Each creature with one or more counters on it can't attack you or planeswalkers you control unless its controller pays {X}, where X is the number of counters on that creature.",
        )
        .expect("Nils, Discipline Enforcer should parse");
    assert_eq!(def.mode, StaticMode::CantAttack);

    // Affected filter gates on counter presence.
    let affected = def.affected.as_ref().expect("affected filter must be set");
    let TargetFilter::Typed(tf) = affected else {
        panic!("expected TypedFilter, got {affected:?}");
    };
    assert!(tf.type_filters.contains(&TypeFilter::Creature));
    assert!(tf.properties.contains(&FilterProp::Counters {
        counters: CounterMatch::Any,
        comparator: Comparator::GE,
        count: QuantityExpr::Fixed { value: 1 },
    }));

    let (_cost, scaling) = extract_unless_pay(&def);
    match scaling {
        UnlessPayScaling::PerAffectedWithRef { quantity } => {
            assert!(matches!(
                quantity,
                QuantityRef::CountersOn {
                    scope: ObjectScope::Target,
                    counter_type: None
                }
            ));
        }
        other => panic!("expected PerAffectedWithRef, got {other:?}"),
    }
}

/// CR 508.1d: Brainwash-class aura form — "Enchanted creature can't attack
/// unless its controller pays {3}." Verifies the aura subject branch emits
/// `FilterProp::EnchantedBy` and flat scaling.
#[test]
fn combat_tax_brainwash_flat_aura() {
    let def = parse_static_line("Enchanted creature can't attack unless its controller pays {3}.")
        .expect("Brainwash-style aura should parse");
    assert_eq!(def.mode, StaticMode::CantAttack);
    let (cost, scaling) = extract_unless_pay(&def);
    assert_eq!(cost.mana_value(), 3);
    assert!(matches!(scaling, UnlessPayScaling::Flat));
    let affected = def.affected.as_ref().expect("affected filter");
    let TargetFilter::Typed(tf) = affected else {
        panic!("expected TypedFilter");
    };
    assert!(tf.properties.contains(&FilterProp::EnchantedBy));
}

/// CR 105.2: Elephant Grass — color-prefixed subject
/// ("Nonblack creatures"). The affected filter gains a `NotColor`
/// predicate while keeping the opponents'-creatures scope and
/// `PerAffectedCreature` scaling.
#[test]
fn combat_tax_color_prefixed_subject_nonblack() {
    let def = parse_static_line(
            "Nonblack creatures can't attack you unless their controller pays {2} for each creature they control that's attacking you.",
        )
        .expect("Elephant Grass combat-tax line should parse");
    assert_eq!(def.mode, StaticMode::CantAttack);
    let affected = def.affected.as_ref().expect("affected filter must be set");
    let TargetFilter::Typed(tf) = affected else {
        panic!("expected TypedFilter, got {affected:?}");
    };
    assert!(tf.type_filters.contains(&TypeFilter::Creature));
    assert_eq!(tf.controller, Some(ControllerRef::Opponent));
    assert!(tf.properties.contains(&FilterProp::NotColor {
        color: ManaColor::Black,
    }));
    let (cost, scaling) = extract_unless_pay(&def);
    assert_eq!(cost.mana_value(), 2);
    assert!(matches!(scaling, UnlessPayScaling::PerAffectedCreature));
}

/// CR 508.1d / CR 509.1c: Myr Prototype — self-referential combat tax
/// ("~ can't attack or block unless you pay {1} for each +1/+1 counter on
/// it"). Parses to `CantAttackOrBlock` + `SelfRef` filter + `PerQuantityRef`
/// scaling against the source's +1/+1 counters.
#[test]
fn combat_tax_self_ref_subject_you_pay_per_counter() {
    let def = parse_static_line(
        "~ can't attack or block unless you pay {1} for each +1/+1 counter on it.",
    )
    .expect("Myr Prototype combat-tax line should parse");
    assert_eq!(def.mode, StaticMode::CantAttackOrBlock);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    let (cost, scaling) = extract_unless_pay(&def);
    assert_eq!(cost.mana_value(), 1);
    match scaling {
        UnlessPayScaling::PerQuantityRef {
            quantity:
                QuantityRef::CountersOn {
                    scope: ObjectScope::Source,
                    ..
                },
        } => {}
        other => panic!("expected PerQuantityRef CountersOn(Source), got {other:?}"),
    }
}

/// CR 508.1d: Phyrexian Marauder — self-referential attack-only tax with
/// the "you pay" payer.
#[test]
fn combat_tax_self_ref_subject_cant_attack_only() {
    let def = parse_static_line("~ can't attack unless you pay {1} for each +1/+1 counter on it.")
        .expect("Phyrexian Marauder combat-tax line should parse");
    assert_eq!(def.mode, StaticMode::CantAttack);
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    let (_cost, scaling) = extract_unless_pay(&def);
    assert!(matches!(scaling, UnlessPayScaling::PerQuantityRef { .. }));
}

/// CR 506.3 + CR 508.1d: Propaganda — `defended` field captures the
/// "you" attack-target scope so the runtime tax only applies to attacks
/// targeting the static's controller. Regression for issue #302
/// (Propaganda taxing attacks against the wrong player).
#[test]
fn combat_tax_propaganda_defended_player_scope() {
    let def = parse_static_line(
            "Creatures can't attack you unless their controller pays {2} for each creature they control that's attacking you.",
        )
        .expect("Propaganda should parse");
    assert_eq!(def.mode, StaticMode::CantAttack);
    let cond = def.condition.as_ref().expect("must carry a condition");
    match cond {
        StaticCondition::UnlessPay { defended, .. } => {
            assert_eq!(
                defended.as_ref(),
                Some(&crate::types::triggers::AttackTargetFilter::Player),
                "Propaganda must capture defended=Player scope",
            );
        }
        other => panic!("expected UnlessPay, got {other:?}"),
    }
}

/// CR 506.3 + CR 508.1d: Sphere of Safety — `defended` field captures
/// "you or planeswalkers you control" → `PlayerOrPlaneswalker`.
#[test]
fn combat_tax_sphere_of_safety_defended_player_or_planeswalker() {
    let def = parse_static_line(
            "Creatures can't attack you or planeswalkers you control unless their controller pays {X} for each of those creatures, where X is the number of enchantments you control.",
        )
        .expect("Sphere of Safety should parse");
    let cond = def.condition.as_ref().expect("must carry a condition");
    match cond {
        StaticCondition::UnlessPay { defended, .. } => {
            assert_eq!(
                defended.as_ref(),
                Some(&crate::types::triggers::AttackTargetFilter::PlayerOrPlaneswalker),
            );
        }
        other => panic!("expected UnlessPay, got {other:?}"),
    }
}

/// CR 509.1c: Block-side restriction — `defended` is `None` because the
/// "defender" of a block restriction is implicit (the static's controller).
#[test]
fn combat_tax_block_side_has_no_defended_scope() {
    // No real card uses pure "Creatures can't block unless...", but the
    // tax-block side of Archangel of Tithes does. Verified via the
    // Archangel test below; here we check the bare grammar in isolation.
    let def = parse_static_line(
        "Creatures can't block unless their controller pays {1} for each of those creatures.",
    )
    .expect("CantBlock with cost should parse");
    assert_eq!(def.mode, StaticMode::CantBlock);
    let cond = def.condition.as_ref().expect("must carry a condition");
    match cond {
        StaticCondition::UnlessPay { defended, .. } => {
            assert!(
                defended.is_none(),
                "block-side tax must have defended=None, got {defended:?}",
            );
        }
        other => panic!("expected UnlessPay, got {other:?}"),
    }
}

/// CR 506.3 + CR 611.3a + CR 118.12a: Archangel of Tithes — first line.
/// "As long as this creature is untapped, creatures can't attack you or
/// planeswalkers you control unless their controller pays {1} for each
/// of those creatures." Must compose `Not(SourceIsTapped)` (the gating
/// condition) AND `UnlessPay { defended=PlayerOrPlaneswalker, ... }`
/// (the tax payload). Regression for issue #309.
#[test]
fn combat_tax_archangel_of_tithes_untapped_attack() {
    let def = parse_static_line(
            "As long as this creature is untapped, creatures can't attack you or planeswalkers you control unless their controller pays {1} for each of those creatures.",
        )
        .expect("Archangel of Tithes attack-tax line should parse");
    assert_eq!(def.mode, StaticMode::CantAttack);

    // Composed condition: gate AND payload.
    let Some(StaticCondition::And { conditions }) = def.condition.as_ref() else {
        panic!("expected And(gate, UnlessPay), got {:?}", def.condition,);
    };
    assert_eq!(conditions.len(), 2, "expected exactly two conjuncts");

    // The gate: Not(SourceIsTapped).
    let has_gate = conditions.iter().any(|c| {
            matches!(
                c,
                StaticCondition::Not { condition } if matches!(**condition, StaticCondition::SourceIsTapped)
            )
        });
    assert!(
        has_gate,
        "missing Not(SourceIsTapped) gate, got {conditions:?}"
    );

    // The payload: UnlessPay {1, PerAffectedCreature, defended=PlayerOrPlaneswalker}.
    let payload = conditions
        .iter()
        .find_map(|c| match c {
            StaticCondition::UnlessPay {
                cost,
                scaling,
                defended,
            } => Some((cost, scaling, defended.as_ref())),
            _ => None,
        })
        .expect("missing UnlessPay payload");
    assert_eq!(payload.0.mana_value(), 1);
    assert!(matches!(payload.1, UnlessPayScaling::PerAffectedCreature));
    assert_eq!(
        payload.2,
        Some(&crate::types::triggers::AttackTargetFilter::PlayerOrPlaneswalker),
    );
}

/// CR 509.1c + CR 611.3a + CR 118.12a: Archangel of Tithes — second line.
/// "As long as this creature is attacking, creatures can't block unless
/// their controller pays {1} for each of those creatures." Composes
/// `SourceIsAttacking` AND `UnlessPay { defended=None, ... }`.
#[test]
fn combat_tax_archangel_of_tithes_attacking_block() {
    let def = parse_static_line(
            "As long as this creature is attacking, creatures can't block unless their controller pays {1} for each of those creatures.",
        )
        .expect("Archangel of Tithes block-tax line should parse");
    assert_eq!(def.mode, StaticMode::CantBlock);

    let Some(StaticCondition::And { conditions }) = def.condition.as_ref() else {
        panic!(
            "expected And(SourceIsAttacking, UnlessPay), got {:?}",
            def.condition,
        );
    };
    let has_gate = conditions
        .iter()
        .any(|c| matches!(c, StaticCondition::SourceIsAttacking));
    assert!(
        has_gate,
        "missing SourceIsAttacking gate, got {conditions:?}"
    );

    let payload = conditions
        .iter()
        .find_map(|c| match c {
            StaticCondition::UnlessPay {
                cost,
                scaling,
                defended,
            } => Some((cost, scaling, defended.as_ref())),
            _ => None,
        })
        .expect("missing UnlessPay payload");
    assert_eq!(payload.0.mana_value(), 1);
    assert!(matches!(payload.1, UnlessPayScaling::PerAffectedCreature));
    // CR 509.1c: block-side has no defender scope.
    assert_eq!(payload.2, None);
}

/// CR 508.1c: Bloodcrazed Goblin — "This creature can't attack unless an
/// opponent has been dealt damage this turn." The `unless`-form must store
/// `Not(condition)`: the restriction is ACTIVE while the inner condition is
/// FALSE. The inner condition is a `DamageDealtThisTurn` quantity comparison
/// targeting an opponent.
#[test]
fn cant_attack_unless_opponent_dealt_damage_stores_not() {
    let def = parse_static_line(
        "This creature can't attack unless an opponent has been dealt damage this turn.",
    )
    .expect("Bloodcrazed Goblin should parse");
    assert_eq!(def.mode, StaticMode::CantAttack);

    let Some(StaticCondition::Not { condition }) = def.condition.as_ref() else {
        panic!("expected Not(QuantityComparison), got {:?}", def.condition);
    };
    let StaticCondition::QuantityComparison { lhs, .. } = condition.as_ref() else {
        panic!("expected QuantityComparison inside Not, got {condition:?}");
    };
    let QuantityExpr::Ref {
        qty: QuantityRef::DamageDealtThisTurn { target, .. },
    } = lhs
    else {
        panic!("expected DamageDealtThisTurn ref, got {lhs:?}");
    };
    // Subject "an opponent" → opponent-controller target filter.
    assert!(
        matches!(
            target.as_ref(),
            TargetFilter::Typed(tf) if tf.controller == Some(ControllerRef::Opponent)
        ),
        "expected opponent-controller target, got {target:?}"
    );
}

/// HAZARD regression — CR 118.12a. A self-referential pay-tax that falls
/// through to the generic `CantAttack` path ("~ can't attack unless their
/// controller pays {2}") must store `UnlessPay` RAW, NOT `Not(UnlessPay)`.
/// `UnlessPay` is inherently negative-polarity; wrapping it would double-
/// negate (the restriction would never be active).
#[test]
fn cant_attack_unless_pay_stores_raw_not_double_negated() {
    let def = parse_static_line("This creature can't attack unless their controller pays {2}.")
        .expect("self-referential pay-tax should parse");
    assert_eq!(def.mode, StaticMode::CantAttack);
    assert!(
        matches!(def.condition, Some(StaticCondition::UnlessPay { .. })),
        "expected raw UnlessPay (not Not-wrapped), got {:?}",
        def.condition,
    );
}

/// CR 508.1c: Regression for committed Unit-5a behavior — a `can't attack IF
/// X` static stores X RAW (convention: `if` => raw, `unless` => `Not`).
#[test]
fn cant_attack_if_condition_stores_raw() {
    let def = parse_static_line(
        "This creature can't attack if an opponent has been dealt damage this turn.",
    )
    .expect("can't-attack-if should parse");
    assert_eq!(def.mode, StaticMode::CantAttack);
    assert!(
        matches!(
            def.condition,
            Some(StaticCondition::QuantityComparison { .. })
        ),
        "`if` condition must be raw (not Not-wrapped), got {:?}",
        def.condition,
    );
}

/// Building-block test for `parse_unless_condition`: `UnlessPay` inner →
/// raw passthrough; any other inner → `Not`-wrapped.
#[test]
fn parse_unless_condition_excludes_unless_pay_from_not_wrap() {
    use crate::parser::oracle_nom::condition as nom_condition;

    // UnlessPay inner → raw.
    let (_, c) = nom_condition::parse_unless_condition("their controller pays {2}")
        .expect("pay clause should parse");
    assert!(
        matches!(c, StaticCondition::UnlessPay { .. }),
        "UnlessPay must pass through raw, got {c:?}"
    );

    // Non-UnlessPay inner → Not-wrapped.
    let (_, c) =
        nom_condition::parse_unless_condition("an opponent has been dealt damage this turn")
            .expect("damage clause should parse");
    assert!(
        matches!(c, StaticCondition::Not { .. }),
        "non-UnlessPay condition must be Not-wrapped, got {c:?}"
    );
}

/// CR 509.1c: Awesome Presence — block tax with defending-player payer and
/// per-blocking-creature scaling.
#[test]
fn awesome_presence_block_tax_unless_pay() {
    let def = parse_static_line(
        "Enchanted creature can't be blocked unless defending player pays {3} for each creature they control that's blocking it.",
    )
    .expect("Awesome Presence should parse");
    assert_eq!(def.mode, StaticMode::CantBeBlocked);
    let Some(StaticCondition::UnlessPay { scaling, .. }) = def.condition.as_ref() else {
        panic!("expected UnlessPay, got {:?}", def.condition);
    };
    assert_eq!(
        *scaling,
        crate::types::ability::UnlessPayScaling::PerAffectedCreature
    );
}

/// Chained Throatseeker's defending-player poison gate is preserved as an
/// undecomposed unless rider until attack legality can evaluate the candidate
/// defending player before attackers are committed.
#[test]
fn chained_throatseeker_defending_player_poisoned_preserved_not_decomposed() {
    let def = parse_static_line("This creature can't attack unless defending player is poisoned.")
        .expect("Chained Throatseeker should parse");
    assert_eq!(def.mode, StaticMode::CantAttack);
    let Some(StaticCondition::Not { condition }) = def.condition.as_ref() else {
        panic!("expected Not(Unrecognized), got {:?}", def.condition);
    };
    let StaticCondition::Unrecognized { text } = condition.as_ref() else {
        panic!("expected preserved unrecognized rider, got {condition:?}");
    };
    assert_eq!(text, "unless defending player is poisoned");
}

/// Arboria's turn-history rider is preserved in the AST, but not yet decomposed
/// into runtime semantics.
#[test]
fn arboria_cant_attack_player_unless_rider_preserved_not_decomposed() {
    let def = parse_static_line(
        "Creatures can't attack a player unless that player cast a spell or put a nontoken permanent onto the battlefield during their last turn.",
    )
    .expect("Arboria should parse");
    assert_eq!(def.mode, StaticMode::CantAttack);
    let Some(StaticCondition::Not { condition }) = def.condition.as_ref() else {
        panic!("expected Not(Unrecognized), got {:?}", def.condition);
    };
    let StaticCondition::Unrecognized { text } = condition.as_ref() else {
        panic!("expected preserved unrecognized rider, got {condition:?}");
    };
    assert_eq!(
        text,
        "unless that player cast a spell or put a nontoken permanent onto the battlefield during their last turn"
    );
}

/// CR 113.6 + CR 113.6b: Anger (Onslaught / Incarnation cycle). The static
/// "As long as this card is in your graveyard and you control a Mountain,
/// creatures you control have haste" must parse with
/// `active_zones = [Graveyard]` so the layers pipeline collects it from
/// the graveyard. Also verifies the compound condition combines
/// `SourceInZone(Graveyard)` AND `IsPresent(Mountain you control)`.
#[test]
fn anger_incarnation_static_declares_graveyard_active_zone() {
    let def = parse_static_line(
        "As long as this card is in your graveyard and you control a Mountain, \
             creatures you control have haste.",
    )
    .expect("Anger static should parse");
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(
        def.active_zones,
        vec![crate::types::zones::Zone::Graveyard],
        "Anger must declare Graveyard in active_zones (CR 113.6b opt-in), got {:?}",
        def.active_zones,
    );
    // Compound condition: source-in-graveyard AND controller-has-Mountain.
    let Some(StaticCondition::And { conditions }) = def.condition.as_ref() else {
        panic!("expected compound And condition, got {:?}", def.condition);
    };
    assert_eq!(conditions.len(), 2);
    assert!(conditions.iter().any(|c| matches!(
        c,
        StaticCondition::SourceInZone { zone } if *zone == crate::types::zones::Zone::Graveyard
    )));
    assert!(conditions
        .iter()
        .any(|c| matches!(c, StaticCondition::IsPresent { .. })));
    // Grants Haste to creatures you control.
    assert!(def.modifications.iter().any(|m| matches!(
        m,
        ContinuousModification::AddKeyword {
            keyword: Keyword::Haste,
        }
    )));
}

/// Statics with no zone-location condition keep `active_zones` empty so
/// they remain battlefield-only (CR 113.6 default).
#[test]
fn ordinary_static_keeps_empty_active_zones() {
    let def =
        parse_static_line("Creatures you control get +1/+1.").expect("anthem static should parse");
    assert!(
        def.active_zones.is_empty(),
        "plain anthem must remain battlefield-default, got {:?}",
        def.active_zones,
    );
}

/// CR 613.4b + CR 107.3m: "have base power and toughness X/X" produces
/// dynamic set-P/T at layer 7b (not static layer 7a CDA, and not pump 7c).
/// Biomass Mutation shape. With no "where X is" clause, X binds to
/// `CostXPaid` (the spell's {X} cost value).
#[test]
fn base_pt_dynamic_x_x_emits_set_power_dynamic() {
    let mods =
        parse_continuous_modifications("have base power and toughness X/X until end of turn");
    let has_p = mods.iter().any(|m| {
        matches!(
            m,
            ContinuousModification::SetPowerDynamic {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::CostXPaid
                }
            }
        )
    });
    let has_t = mods.iter().any(|m| {
        matches!(
            m,
            ContinuousModification::SetToughnessDynamic {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::CostXPaid
                }
            }
        )
    });
    assert!(has_p, "missing SetPowerDynamic(CostXPaid) in {mods:?}");
    assert!(has_t, "missing SetToughnessDynamic(CostXPaid) in {mods:?}");
    assert_eq!(
        mods.iter()
            .filter(|m| matches!(
                m,
                ContinuousModification::SetPower { .. }
                    | ContinuousModification::SetToughness { .. }
            ))
            .count(),
        0,
        "literal SetPower/SetToughness must not be emitted for X/X"
    );
}

#[test]
fn base_pt_equal_to_recipient_mana_value_emits_dynamic_setters() {
    let mods = parse_continuous_modifications(
            "is a creature in addition to its other types and has base power and base toughness each equal to its mana value",
        );
    assert!(mods.iter().any(|m| matches!(
        m,
        ContinuousModification::SetPowerDynamic {
            value: QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Recipient
                }
            }
        }
    )));
    assert!(mods.iter().any(|m| matches!(
        m,
        ContinuousModification::SetToughnessDynamic {
            value: QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Recipient
                }
            }
        }
    )));
}

#[test]
fn static_animation_base_pt_equal_to_mana_value_reaches_line_parser() {
    let def = parse_static_line(
            "Each other non-Aura enchantment is a creature in addition to its other types and has base power and base toughness each equal to its mana value.",
        )
        .expect("mana-value animation static should parse");
    assert!(def.modifications.iter().any(|m| {
        matches!(
            m,
            ContinuousModification::SetPowerDynamic {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::Recipient
                    }
                }
            }
        )
    }));
    assert!(def.modifications.iter().any(|m| {
        matches!(
            m,
            ContinuousModification::SetToughnessDynamic {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::Recipient
                    }
                }
            }
        )
    }));
}

#[test]
fn conditional_static_animation_base_pt_equal_to_mana_value_keeps_condition() {
    let def = parse_static_line(
            "As long as you control five or more enchantments, each other non-Aura enchantment you control is a creature in addition to its other types and has base power and base toughness each equal to its mana value.",
        )
        .expect("conditional mana-value animation static should parse");
    assert!(matches!(
        def.condition,
        Some(StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { .. }
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 5 },
        })
    ));
    assert!(def.modifications.iter().any(|m| {
        matches!(
            m,
            ContinuousModification::SetPowerDynamic {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::Recipient
                    }
                }
            }
        )
    }));
}

// CR 700.9: "Modified creatures you control have <keyword>" class.
// Previously misparsed as Subtype("Modified") (see commit body).
#[test]
fn static_modified_creatures_you_control_have_menace() {
    let def = parse_static_line("Modified creatures you control have menace.").unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    match def.affected {
        Some(TargetFilter::Typed(ref tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(tf.properties.contains(&FilterProp::Modified));
            assert!(
                !tf.type_filters.iter().any(|t| matches!(
                    t,
                    TypeFilter::Subtype(s) if s.eq_ignore_ascii_case("modified")
                )),
                "Modified must not be emitted as a subtype"
            );
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
        }
        _ => panic!("expected TargetFilter::Typed"),
    }
}

// CR 700.9: Ondu Knotmaster-style "other modified creature you control".
#[test]
fn parse_modified_creature_subject_other_variant() {
    let filter = parse_modified_creature_subject_filter("other modified creature you control")
        .expect("other modified creature you control must parse");
    match filter {
        TargetFilter::Typed(tf) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(tf.properties.contains(&FilterProp::Modified));
            assert!(tf.properties.contains(&FilterProp::Another));
        }
        _ => panic!("expected TargetFilter::Typed"),
    }
}

// CR 700.9: Bare "modified creature" with no controller scope.
#[test]
fn parse_modified_creature_subject_unscoped() {
    let filter = parse_modified_creature_subject_filter("modified creature")
        .expect("modified creature must parse");
    match filter {
        TargetFilter::Typed(tf) => {
            assert_eq!(tf.controller, None);
            assert!(tf.properties.contains(&FilterProp::Modified));
            assert!(!tf.properties.contains(&FilterProp::Another));
        }
        _ => panic!("expected TargetFilter::Typed"),
    }
}

// CR 903.3d: "Commanders you control have <keyword>" — Codsworth, Falthis,
// Vexilus Praetor class. Must produce IsCommander, NOT a bogus
// Subtype("Commander") (Commander is not an MTG subtype per CR 903.3).
#[test]
fn parse_commanders_you_control_have_keyword() {
    let def = parse_static_line("Commanders you control have ward {2}.")
        .expect("should parse Commanders-you-control");
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(
                tf.properties.contains(&FilterProp::IsCommander),
                "must carry IsCommander, got {:?}",
                tf.properties
            );
            // Must NOT synthesize a Commander subtype.
            assert!(
                !tf.type_filters
                    .iter()
                    .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "Commander")),
                "must not emit Subtype(\"Commander\") (CR 903.3 — not a subtype)"
            );
        }
        other => panic!("expected Typed filter, got {other:?}"),
    }
}

// CR 903.3d + CR 700.4: "Other commanders you control" — must include Another.
#[test]
fn parse_other_commanders_you_control_have_keyword() {
    let def = parse_static_line("Other commanders you control have menace.")
        .expect("should parse other-commanders-you-control");
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.properties.contains(&FilterProp::IsCommander));
            assert!(tf.properties.contains(&FilterProp::Another));
        }
        other => panic!("expected Typed filter, got {other:?}"),
    }
}

// CR 903.3d: "Commander creatures you control" — Guardian Augmenter class.
// The "Commander" adjective on a creature subject is the commander
// designation, not a subtype.
#[test]
fn parse_commander_creatures_you_control() {
    let def = parse_static_line("Commander creatures you control get +2/+2.")
        .expect("should parse Commander-creatures-you-control");
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(tf.properties.contains(&FilterProp::IsCommander));
            assert!(
                !tf.type_filters
                    .iter()
                    .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "Commander")),
                "must not emit Subtype(\"Commander\")"
            );
        }
        other => panic!("expected Typed filter, got {other:?}"),
    }
}

#[test]
fn parse_commander_creatures_you_own_grant_attack_trigger() {
    use crate::types::ability::{Effect, TriggerCondition};
    use crate::types::triggers::{AttackTargetFilter, TriggerMode};

    let def = parse_static_line(
            "Commander creatures you own have \"Whenever this creature attacks a player, if no opponent has more life than that player, you create two Treasure tokens.\"",
        )
        .expect("Guild Artisan granted trigger should parse");

    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(tf.properties.contains(&FilterProp::IsCommander));
            assert!(tf.properties.contains(&FilterProp::Owned {
                controller: ControllerRef::You,
            }));
            assert_eq!(tf.controller, None);
        }
        other => panic!("expected Typed filter, got {other:?}"),
    }

    match def.modifications.as_slice() {
        [ContinuousModification::GrantTrigger { trigger }] => {
            assert_eq!(trigger.mode, TriggerMode::Attacks);
            assert_eq!(trigger.valid_card, Some(TargetFilter::SelfRef));
            assert_eq!(
                trigger.attack_target_filter,
                Some(AttackTargetFilter::Player)
            );
            match trigger.condition.as_ref() {
                Some(TriggerCondition::QuantityComparison {
                    comparator: Comparator::LE,
                    rhs:
                        QuantityExpr::Ref {
                            qty:
                                QuantityRef::LifeTotal {
                                    player: PlayerScope::DefendingPlayer,
                                },
                        },
                    ..
                }) => {}
                other => panic!("expected defending-player life condition, got {other:?}"),
            }
            let execute = trigger.execute.as_ref().expect("trigger must have effect");
            match execute.effect.as_ref() {
                Effect::Token { name, count, .. } => {
                    assert_eq!(name, "Treasure");
                    assert_eq!(*count, QuantityExpr::Fixed { value: 2 });
                }
                other => panic!("expected Treasure token creation, got {other:?}"),
            }
        }
        other => panic!("expected single GrantTrigger modification, got {other:?}"),
    }
}

#[test]
fn parse_initiative_background_attack_trigger_cluster() {
    use crate::types::ability::{Effect, TriggerCondition};

    let cases = [
            "Commander creatures you own have \"Whenever this creature attacks a player, if no opponent has more life than that player, put a +1/+1 counter on this creature. It gains deathtouch and indestructible until end of turn.\"",
            "Commander creatures you own have \"Whenever this creature attacks a player, if no opponent has more life than that player, you create two Treasure tokens.\"",
            "Commander creatures you own have \"Whenever this creature attacks a player, if no opponent has more life than that player, another target creature you control gets +X/+X until end of turn, where X is this creature's power.\"",
            "Commander creatures you own have \"Whenever this creature attacks a player, if no opponent has more life than that player, this creature can't be blocked this turn.\"",
            "Commander creatures you own have \"Whenever this creature attacks a player, if no opponent has more life than that player, for each opponent, create a 1/1 white Soldier creature token that's tapped and attacking that opponent.\"",
        ];

    for text in cases {
        let def = parse_static_line(text).expect("initiative Background should parse");
        match def.modifications.as_slice() {
            [ContinuousModification::GrantTrigger { trigger }] => {
                assert!(matches!(
                    trigger.condition,
                    Some(TriggerCondition::QuantityComparison {
                        comparator: Comparator::LE,
                        ..
                    })
                ));
                let execute = trigger.execute.as_ref().expect("trigger must have effect");
                assert!(
                    !matches!(execute.effect.as_ref(), Effect::Unimplemented { .. }),
                    "granted trigger effect must be implemented for {text}"
                );
            }
            other => panic!("expected single GrantTrigger modification, got {other:?}"),
        }
    }
}

#[test]
fn parse_quoted_grant_preserves_outer_keyword_only() {
    let def = parse_static_line(
            "Commander creatures you own have menace and \"This creature gets +X/+0, where X is the number of creature cards in your graveyard.\"",
        )
        .expect("Criminal Past-style mixed keyword and quoted ability should parse");

    assert_eq!(def.modifications.len(), 2);
    assert!(def.modifications.iter().any(|modification| matches!(
        modification,
        ContinuousModification::AddKeyword {
            keyword: Keyword::Menace
        }
    )));
    // CR 113.3d + CR 604.1: The inner quoted clause is a `SelfRef`
    // continuous static carrying layered modifications (AddDynamicPower
    // for "+X/+0 where X is..."). Since the new `GrantStaticAbility`
    // primitive landed, this path emits a granted static instead of
    // a generic `GrantAbility` wrapper — the granted static then
    // applies its dynamic P/T mod through the layer system on the
    // recipient. Either is acceptable structurally; assert on the
    // typed primitive that's now produced.
    assert!(def.modifications.iter().any(|modification| matches!(
        modification,
        ContinuousModification::GrantStaticAbility { .. }
    )));
}

// CR 903.3d: parse_commander_subject_filter as a raw subject helper.
// Unblocks subject-continuous-static dispatch (the secondary path).
#[test]
fn parse_commander_subject_filter_basic_variants() {
    let f =
        parse_commander_subject_filter("commanders you control").expect("commanders you control");
    match f {
        TargetFilter::Typed(tf) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(tf.properties.contains(&FilterProp::IsCommander));
            assert!(tf.type_filters.contains(&TypeFilter::Permanent));
        }
        _ => panic!("expected Typed"),
    }

    let f = parse_commander_subject_filter("other commander you control")
        .expect("other commander you control");
    match f {
        TargetFilter::Typed(tf) => {
            assert!(tf.properties.contains(&FilterProp::IsCommander));
            assert!(tf.properties.contains(&FilterProp::Another));
        }
        _ => panic!("expected Typed"),
    }

    // Bare "commander" (no controller) — used by `parse_subject_continuous_static`
    // when an enclosing clause supplies the controller.
    let f = parse_commander_subject_filter("commanders").expect("bare commanders");
    match f {
        TargetFilter::Typed(tf) => {
            assert_eq!(tf.controller, None);
            assert!(tf.properties.contains(&FilterProp::IsCommander));
        }
        _ => panic!("expected Typed"),
    }

    let f = parse_commander_subject_filter("your commander").expect("your commander");
    match f {
        TargetFilter::Typed(tf) => {
            assert_eq!(tf.controller, None);
            assert_eq!(
                tf.properties,
                vec![
                    FilterProp::Owned {
                        controller: ControllerRef::You,
                    },
                    FilterProp::IsCommander,
                ]
            );
        }
        _ => panic!("expected Typed"),
    }

    let f = parse_commander_subject_filter("commander creatures you own")
        .expect("commander creatures you own");
    match f {
        TargetFilter::Typed(tf) => {
            assert_eq!(tf.controller, None);
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(tf.properties.contains(&FilterProp::IsCommander));
            assert!(tf.properties.contains(&FilterProp::Owned {
                controller: ControllerRef::You,
            }));
        }
        _ => panic!("expected Typed"),
    }

    // Negative: must not match subtype-like words.
    assert!(parse_commander_subject_filter("zombies you control").is_none());
    assert!(parse_commander_subject_filter("commander spirits").is_none());
}

/// CR 401.5 + CR 118.9: Realmwalker's "You may cast creature spells of the
/// chosen type from the top of your library." should lower to a
/// `TopOfLibraryCastPermission { play_mode: Cast }` static with the
/// chosen-creature-type filter, NOT to an imperative `Effect::CastFromZone`
/// (which would exile the card via the impulse-draw resolver).
#[test]
fn top_of_library_cast_permission_realmwalker() {
    let text = "You may cast creature spells of the chosen type from the top of your library.";
    let lower = text.to_lowercase();
    let def = try_parse_top_of_library_cast_permission(text, &lower)
        .expect("Realmwalker static must parse");
    match def.mode {
        StaticMode::TopOfLibraryCastPermission {
            play_mode,
            ref alt_cost,
        } => {
            assert_eq!(play_mode, CardPlayMode::Cast);
            assert!(alt_cost.is_none());
        }
        other => panic!("expected TopOfLibraryCastPermission, got {other:?}"),
    }
    // The chosen-creature-type filter must be carried on `affected`.
    let affected = def.affected.expect("affected filter set");
    match affected {
        TargetFilter::Typed(tf) => {
            assert!(tf
                .type_filters
                .iter()
                .any(|t| matches!(t, TypeFilter::Creature)));
            assert!(tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::IsChosenCreatureType)));
        }
        other => panic!("expected Typed filter, got {other:?}"),
    }
}

/// CR 401.5: Future Sight / Magus of the Future — compound "you may play
/// lands and cast spells from the top of your library" collapses to a
/// single `Play` permission with `affected: Any`.
#[test]
fn top_of_library_cast_permission_future_sight_compound() {
    let text = "You may play lands and cast spells from the top of your library.";
    let lower = text.to_lowercase();
    let def = try_parse_top_of_library_cast_permission(text, &lower)
        .expect("Future Sight static must parse");
    match def.mode {
        StaticMode::TopOfLibraryCastPermission {
            play_mode,
            ref alt_cost,
        } => {
            assert_eq!(play_mode, CardPlayMode::Play);
            assert!(alt_cost.is_none());
        }
        other => panic!("expected TopOfLibraryCastPermission, got {other:?}"),
    }
    assert!(matches!(def.affected, Some(TargetFilter::Any)));
}

#[test]
fn top_of_library_cast_permission_keeps_as_long_as_condition() {
    let text = "You may cast creature spells from the top of your library as long as you control three or more creatures with different powers.";
    let lower = text.to_lowercase();
    let def = try_parse_top_of_library_cast_permission(text, &lower)
        .expect("Augur of Autumn static must parse");

    assert!(
        matches!(
            def.condition,
            Some(StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCountDistinct { .. },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            })
        ),
        "expected coven condition, got {:?}",
        def.condition
    );
}

#[test]
fn top_of_library_cast_permission_rejects_partial_as_long_as_condition() {
    let trailing =
        " as long as you control three or more creatures with different powers and a Food.";
    assert!(
        parse_top_of_library_permission_condition(trailing).is_none(),
        "condition parser must not silently accept leftover condition text"
    );
}

/// CR 118.9 + CR 119.4: Bolas's Citadel — compound permission line carrying
/// a same-line alt-cost rider must lower with `alt_cost: Some(PayLife {
/// SelfManaValue })`. Verifies the rider scanner correctly slices into the
/// "If you cast a spell this way, ..." sentence inside the same line.
#[test]
fn top_of_library_cast_permission_bolas_alt_cost() {
    let text = "You may play lands and cast spells from the top of your library. \
                    If you cast a spell this way, pay life equal to its mana value rather \
                    than pay its mana cost.";
    let lower = text.to_lowercase();
    let def = try_parse_top_of_library_cast_permission(text, &lower)
        .expect("Bolas's Citadel static must parse");
    match def.mode {
        StaticMode::TopOfLibraryCastPermission {
            play_mode,
            alt_cost: Some(crate::types::ability::AbilityCost::PayLife { amount }),
        } => {
            assert_eq!(play_mode, CardPlayMode::Play);
            assert_eq!(
                amount,
                crate::types::ability::QuantityExpr::Ref {
                    qty: crate::types::ability::QuantityRef::SelfManaValue
                }
            );
        }
        other => panic!("expected PayLife alt_cost, got {other:?}"),
    }
}

/// Negative: lines without "from the top of your library" must NOT match —
/// the existing impulse-draw / graveyard / hand-permission paths must
/// still own those lines.
#[test]
fn top_of_library_cast_permission_rejects_other_anchors() {
    // Graveyard form — owned by `try_parse_graveyard_cast_permission`.
    assert!(try_parse_top_of_library_cast_permission(
        "You may cast a creature spell from your graveyard.",
        "you may cast a creature spell from your graveyard.",
    )
    .is_none());
    // Hand-free form — owned by `try_parse_cast_free_permission`.
    assert!(try_parse_top_of_library_cast_permission(
        "You may cast spells from your hand without paying their mana costs.",
        "you may cast spells from your hand without paying their mana costs.",
    )
    .is_none());
    // Imperative form (Discover-class) — owned by `try_parse_cast_effect`.
    assert!(try_parse_top_of_library_cast_permission(
        "Cast that card without paying its mana cost.",
        "cast that card without paying its mana cost.",
    )
    .is_none());
}

#[test]
fn subtype_or_list_single() {
    let f = parse_subtype_or_list("Wolf").unwrap();
    assert!(matches!(f, TargetFilter::Typed(ref t) if t.get_subtype() == Some("Wolf")));
}

#[test]
fn subtype_or_list_two_with_article() {
    let f = parse_subtype_or_list("Wolf or a Werewolf").unwrap();
    match f {
        TargetFilter::Or { filters } => {
            assert_eq!(filters.len(), 2);
        }
        other => panic!("expected Or, got {:?}", other),
    }
}

#[test]
fn subtype_or_list_three_with_commas() {
    let f = parse_subtype_or_list("Barbarian, a Warrior, or a Berserker").unwrap();
    match f {
        TargetFilter::Or { filters } => assert_eq!(filters.len(), 3),
        other => panic!("expected Or, got {:?}", other),
    }
}

#[test]
fn subtype_or_list_and_or() {
    let f = parse_subtype_or_list("Cleric, Rogue, Warrior, and/or Wizard").unwrap();
    match f {
        TargetFilter::Or { filters } => assert_eq!(filters.len(), 4),
        other => panic!("expected Or, got {:?}", other),
    }
}

#[test]
fn subtype_or_list_five() {
    let f = parse_subtype_or_list("Cat, Elemental, Nightmare, Dinosaur, or Beast").unwrap();
    match f {
        TargetFilter::Or { filters } => assert_eq!(filters.len(), 5),
        other => panic!("expected Or, got {:?}", other),
    }
}

#[test]
fn thats_a_subject_creature_you_control_two_types() {
    let text = "creature you control that's a Wolf or a Werewolf";
    let lower = text.to_lowercase();
    let f = parse_thats_a_subject_filter(text, &lower).unwrap();
    match f {
        TargetFilter::And { filters } => {
            assert_eq!(filters.len(), 2);
            assert!(
                matches!(&filters[0], TargetFilter::Typed(t) if t.controller == Some(ControllerRef::You))
            );
            assert!(matches!(&filters[1], TargetFilter::Or { filters } if filters.len() == 2));
        }
        other => panic!("expected And, got {:?}", other),
    }
}

#[test]
fn thats_a_subject_no_controller() {
    let text = "creature that's a Barbarian, a Warrior, or a Berserker";
    let lower = text.to_lowercase();
    let f = parse_thats_a_subject_filter(text, &lower).unwrap();
    match f {
        TargetFilter::And { filters } => {
            assert_eq!(filters.len(), 2);
            assert!(matches!(&filters[0], TargetFilter::Typed(t) if t.controller.is_none()));
        }
        other => panic!("expected And, got {:?}", other),
    }
}

#[test]
fn static_line_each_other_wolf_werewolf() {
    let def = parse_static_line(
        "Each other creature you control that's a Wolf or a Werewolf gets +1/+1.",
    )
    .expect("should parse Immerwolf line");
    assert!(matches!(def.mode, StaticMode::Continuous));
    assert_eq!(def.modifications.len(), 2);
}

#[test]
fn static_line_lovisa_coldeyes() {
    let def = parse_static_line(
        "Each creature that's a Barbarian, a Warrior, or a Berserker gets +2/+2 and has haste.",
    )
    .expect("should parse Lovisa Coldeyes line");
    assert!(matches!(def.mode, StaticMode::Continuous));
    assert_eq!(def.modifications.len(), 3);
}

/// CR 614.1c + CR 122.1: Kalain, Reclusive Painter — "Other creatures you
/// control enter with an additional +1/+1 counter on them." parses to the
/// fixed-count `EntersWithAdditionalCounters` static with an Other + you-control
/// creature affected filter.
#[test]
fn enters_with_additional_counter_other_creatures() {
    let def = parse_static_line(
        "Other creatures you control enter with an additional +1/+1 counter on them.",
    )
    .expect("should parse the Kalain enters-with-counter line");
    assert_eq!(
        def.mode,
        StaticMode::EntersWithAdditionalCounters {
            counter_type: CounterType::Plus1Plus1,
            count: 1,
        }
    );
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "affected must be creatures, got {:?}",
                tf.type_filters
            );
            assert!(
                tf.properties.contains(&FilterProp::Another),
                "\"Other\" must add FilterProp::Another, got {:?}",
                tf.properties
            );
        }
        other => panic!("affected must be Typed(other creatures you control), got {other:?}"),
    }
}

/// CR 614.1c + CR 205.4a: Bard Class — "Legendary creatures you control enter
/// with an additional +1/+1 counter on them." parses with a Legendary supertype
/// qualifier on the affected filter (no "Other" exclusion).
#[test]
fn enters_with_additional_counter_legendary_creatures() {
    let def = parse_static_line(
        "Legendary creatures you control enter with an additional +1/+1 counter on them.",
    )
    .expect("should parse the Bard Class enters-with-counter line");
    assert_eq!(
        def.mode,
        StaticMode::EntersWithAdditionalCounters {
            counter_type: CounterType::Plus1Plus1,
            count: 1,
        }
    );
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "affected must be creatures, got {:?}",
                tf.type_filters
            );
            assert!(
                !tf.properties.contains(&FilterProp::Another),
                "non-\"Other\" subject must NOT carry FilterProp::Another, got {:?}",
                tf.properties
            );
        }
        other => panic!("affected must be Typed(legendary creatures you control), got {other:?}"),
    }
}

/// CR 122.1: A dynamic-count "enters with" line (Gev-class, "for each opponent
/// who lost life") has no fixed counter token and must NOT match the fixed-count
/// parser — it stays Unimplemented rather than silently parsing as count 1.
#[test]
fn enters_with_dynamic_count_not_matched_as_fixed() {
    let def = parse_static_line(
        "Other creatures you control enter with an additional +1/+1 counter on them for each opponent who lost life this turn.",
    );
    if let Some(def) = def {
        assert!(
            !matches!(def.mode, StaticMode::EntersWithAdditionalCounters { .. }),
            "dynamic-count line must not lower to the fixed-count EntersWithAdditionalCounters, got {:?}",
            def.mode
        );
    }
}

/// CR 205.3 + CR 604.1 + CR 702.18a: "All Slivers have shroud." (Crystalline
/// Sliver) must land as a TOP-LEVEL continuous static granting Shroud to a
/// `Typed(Subtype:"Sliver")` subject — NOT a spell-resolution GenericEffect.
/// The "all " universal quantifier on the rule-static subject must be stripped
/// and delegated to `parse_type_phrase`.
#[test]
fn static_all_slivers_have_shroud_top_level_typed_subtype() {
    let def =
        parse_static_line("All Slivers have shroud.").expect("All Slivers have shroud must parse");
    assert_eq!(def.mode, StaticMode::Continuous);
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(
                tf.get_subtype(),
                Some("Sliver"),
                "expected Subtype(Sliver), got {:?}",
                tf.type_filters
            );
        }
        other => panic!("expected Typed(Subtype:Sliver), got {other:?}"),
    }
    assert!(
        def.modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Shroud,
            }),
        "expected AddKeyword(Shroud), got {:?}",
        def.modifications
    );
}

/// CR 205.3 + CR 604.1 + CR 702.11a: "All Goblins have hexproof." — same
/// universal-quantifier-strip path, different subtype + keyword, proving the
/// fix covers the whole "all <type> have <keyword>" class, not one card.
#[test]
fn static_all_goblins_have_hexproof_top_level_typed_subtype() {
    let def = parse_static_line("All Goblins have hexproof.")
        .expect("All Goblins have hexproof must parse");
    assert_eq!(def.mode, StaticMode::Continuous);
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.get_subtype(), Some("Goblin"));
        }
        other => panic!("expected Typed(Subtype:Goblin), got {other:?}"),
    }
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Hexproof,
        }));
}

/// CR 205.3 + CR 604.1: "All creatures have shroud." — proves the quantifier
/// strip is GENERAL (works on the core-type subject "creatures", not just
/// subtypes).
#[test]
fn static_all_creatures_have_shroud_top_level() {
    let def = parse_static_line("All creatures have shroud.")
        .expect("All creatures have shroud must parse");
    assert_eq!(def.mode, StaticMode::Continuous);
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "expected Creature type filter, got {:?}",
                tf.type_filters
            );
        }
        other => panic!("expected Typed(creatures), got {other:?}"),
    }
    assert!(def
        .modifications
        .contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::Shroud,
        }));
}

/// CR 509.1b + CR 604.1: A self-referential "~ can't be blocked except by
/// <quality filter>" evasion static must land as a TOP-LEVEL
/// `CantBeBlockedExceptBy { Quality(..) }` on the source — NOT a GenericEffect.
#[test]
fn static_selfref_cant_be_blocked_except_by_quality_top_level() {
    let def = parse_static_line("~ can't be blocked except by creatures with flying.")
        .expect("self-ref except-by-quality must parse");
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    match &def.mode {
        StaticMode::CantBeBlockedExceptBy {
            kind: BlockExceptionKind::Quality(filter),
        } => match filter {
            TargetFilter::Typed(tf) => assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "expected a creature quality filter, got {:?}",
                tf.type_filters
            ),
            other => panic!("expected a Typed creature quality filter, got {other:?}"),
        },
        other => panic!("expected CantBeBlockedExceptBy(Quality), got {other:?}"),
    }
}

/// CR 509.1b: A self-referential "~ can't be blocked except by two or more
/// creatures" (menace-style minimum) must land as a TOP-LEVEL
/// `CantBeBlockedExceptBy { MinBlockers }` static.
#[test]
fn static_selfref_cant_be_blocked_except_by_min_blockers_top_level() {
    let def = parse_static_line("~ can't be blocked except by two or more creatures.")
        .expect("self-ref except-by-min must parse");
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert_eq!(
        def.mode,
        StaticMode::CantBeBlockedExceptBy {
            kind: BlockExceptionKind::MinBlockers { min: 2 }
        }
    );
}

/// CR 509.1b: The Amrou-style disjunction filter ("artifact creatures and/or
/// white creatures") must land as a TOP-LEVEL `CantBeBlockedExceptBy { Quality }`
/// evasion static on the self-referential source.
#[test]
fn static_selfref_cant_be_blocked_except_by_disjunction_top_level() {
    let def = parse_static_line(
        "~ can't be blocked except by artifact creatures and/or white creatures.",
    )
    .expect("self-ref except-by-disjunction must parse");
    assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    assert!(
        matches!(
            def.mode,
            StaticMode::CantBeBlockedExceptBy {
                kind: BlockExceptionKind::Quality(_)
            }
        ),
        "expected CantBeBlockedExceptBy(Quality(disjunction)), got {:?}",
        def.mode
    );
}

/// CR 702.29e + CR 113.6b: Homing Sliver's top-level static grants Typecycling
/// to all Sliver cards in their owner's hand. This asserts the PARSE is correct
/// (affected = Typed(Subtype:Sliver) in the Hand zone; modification =
/// AddKeyword(Typecycling { cost {3}, subtype "Sliver" })). Runtime-granted
/// cycling abilities are surfaced by `game::casting` from the effective
/// off-zone keyword set.
#[test]
fn static_homing_sliver_grants_typecycling_to_slivers_in_hand() {
    // Real Oracle text. The "Each <type> in each player's hand has <keyword>"
    // shape must land as a TOP-LEVEL continuous grant, not a GenericEffect.
    let def = parse_static_line("Each Sliver card in each player's hand has slivercycling {3}.")
        .expect("Homing Sliver slivercycling grant must parse");
    assert_eq!(def.mode, StaticMode::Continuous);
    match &def.affected {
        Some(filter @ TargetFilter::Typed(tf)) => {
            assert_eq!(
                tf.get_subtype(),
                Some("Sliver"),
                "expected Subtype(Sliver), got {:?}",
                tf.type_filters
            );
            // CR 113.6b: the grant functions on cards in the Hand zone.
            assert_eq!(
                filter.extract_in_zone(),
                Some(Zone::Hand),
                "expected InZone(Hand) on the affected filter, got {:?}",
                tf.properties
            );
        }
        other => panic!("expected Typed(Subtype:Sliver in hand), got {other:?}"),
    }
    assert!(
        def.modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddKeyword {
                keyword: Keyword::Typecycling { cost, subtype }
            } if *cost == ManaCost::generic(3) && subtype == "Sliver"
        )),
        "expected AddKeyword(Typecycling {{ {{3}}, Sliver }}), got {:?}",
        def.modifications
    );
}

#[test]
fn eriette_charmed_apple_static_and_trigger_parse() {
    use crate::parser::oracle_trigger::parse_trigger_line;
    use crate::types::ability::{AttachmentKind, PlayerFilter, QuantityRef};

    let static_def = parse_static_line(
        "Each creature that's enchanted by an Aura you control can't attack you or planeswalkers you control.",
    )
    .expect("Eriette attack restriction must parse");
    assert_eq!(static_def.mode, StaticMode::CantAttack);
    assert_eq!(
        static_def.affected,
        Some(TargetFilter::Typed(TypedFilter::creature().properties(
            vec![FilterProp::HasAttachment {
                kind: AttachmentKind::Aura,
                controller: Some(ControllerRef::You),
                exclude_source: crate::types::ability::SourceExclusion::Include,
            }]
        ))),
        "affected must be creatures enchanted by an Aura you control, got {:?}",
        static_def.affected
    );
    assert_eq!(
        static_def.attack_defended.as_ref(),
        Some(&crate::types::triggers::AttackTargetFilter::PlayerOrPlaneswalker),
        "scoped restriction must defend player+planeswalkers, got {:?}",
        static_def.attack_defended
    );
    assert_ne!(
        static_def.affected,
        Some(TargetFilter::SelfRef),
        "must not fall through to SelfRef CantAttack on Eriette herself"
    );

    let trigger = parse_trigger_line(
        "At the beginning of your end step, each opponent loses X life and you gain X life, where X is the number of Auras you control.",
        "Eriette of the Charmed Apple",
    );
    let execute = trigger.execute.expect("end step execute");
    assert_eq!(execute.player_scope, Some(PlayerFilter::Opponent));
    match &*execute.effect {
        Effect::LoseLife { amount, .. } => match amount {
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { filter },
            } => {
                let TargetFilter::Typed(tf) = filter else {
                    panic!("expected Typed aura count filter");
                };
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(
                    tf.type_filters
                        .iter()
                        .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "Aura"))
                        || tf.properties.iter().any(|p| matches!(
                            p,
                            FilterProp::HasAttachment {
                                kind: AttachmentKind::Aura,
                                ..
                            }
                        ))
                );
            }
            other => panic!("expected ObjectCount Auras on LoseLife, got {other:?}"),
        },
        other => panic!("expected LoseLife, got {other:?}"),
    }
    let gain = execute.sub_ability.expect("gain life sibling");
    match &*gain.effect {
        Effect::GainLife { amount, .. } => {
            assert!(matches!(amount, QuantityExpr::Ref { .. }));
        }
        other => panic!("expected GainLife, got {other:?}"),
    }
}

/// CR 702.122c / 702.171a / 702.184a: crew/saddle/station power-contribution
/// modifiers (Reckoner Bankbuster, the "Roads" cycle, Giant Ox, Stoic
/// Star-Captain) parse into a `CrewContribution` static carrying the kind and
/// the named action list.
#[test]
fn crew_contribution_power_and_toughness_modifiers_parse() {
    let defs = parse_static_line_multi("~ crews Vehicles as though its power were 2 greater.");
    assert_eq!(
        defs.len(),
        1,
        "got {:?}",
        defs.iter().map(|d| &d.mode).collect::<Vec<_>>()
    );
    assert_eq!(
        defs[0].mode,
        StaticMode::CrewContribution {
            kind: CrewContributionKind::PowerDelta { delta: 2 },
            actions: vec![CrewAction::Crew],
        }
    );
    assert_eq!(defs[0].affected, Some(TargetFilter::SelfRef));

    let defs = parse_static_line_multi(
        "~ saddles Mounts and crews Vehicles as though its power were 2 greater.",
    );
    assert_eq!(
        defs[0].mode,
        StaticMode::CrewContribution {
            kind: CrewContributionKind::PowerDelta { delta: 2 },
            actions: vec![CrewAction::Saddle, CrewAction::Crew],
        }
    );

    let defs =
        parse_static_line_multi("~ crews Vehicles using its toughness rather than its power.");
    assert_eq!(
        defs[0].mode,
        StaticMode::CrewContribution {
            kind: CrewContributionKind::ToughnessInsteadOfPower,
            actions: vec![CrewAction::Crew],
        }
    );

    // Anthem form (Stoic Star-Captain): granted to a group, so it is emitted as a
    // Continuous static carrying AddStaticMode(CrewContribution) for propagation
    // onto each affected creature, rather than a bare group-affected mode.
    let defs = parse_static_line_multi(
        "Each creature you control crews Vehicles and stations permanents as though its power were 2 greater.",
    );
    assert_eq!(defs[0].mode, StaticMode::Continuous);
    assert!(
        defs[0]
            .modifications
            .contains(&ContinuousModification::AddStaticMode {
                mode: StaticMode::CrewContribution {
                    kind: CrewContributionKind::PowerDelta { delta: 2 },
                    actions: vec![CrewAction::Crew, CrewAction::Station],
                }
            }),
        "anthem form must propagate via AddStaticMode, got {:?}",
        defs[0].modifications
    );
}

/// CR 611.3 + CR 613.1 + CR 205.4a: A multi-sentence static ability whose
/// sentences are independent continuous effects (the dual-subject anthem class)
/// must emit one `StaticDefinition` per sentence, each carrying its own affected
/// filter and modifications. Regression for Flowering of the White Tree, whose
/// first-sentence parse previously swallowed the period, mangled the ward cost,
/// and dropped the entire second sentence.
///
/// Discriminating: the legendary half gets +2/+1 AND `ward {1}` (generic 1, NOT
/// the empty `ward {0}` the swallowed-period bug produced); the nonlegendary
/// half gets +1/+1 with NO keyword; both halves are restricted to `You`-
/// controlled creatures so an opponent's legendary creature is unaffected.
#[test]
fn multi_sentence_dual_subject_anthem_emits_both_statics() {
    use crate::types::keywords::WardCost;
    use crate::types::mana::ManaCost;

    let defs = parse_static_line_multi(
        "Legendary creatures you control get +2/+1 and have ward {1}. \
         Nonlegendary creatures you control get +1/+1.",
    );
    assert_eq!(
        defs.len(),
        2,
        "dual-subject anthem must emit both sentences as separate statics, got {defs:?}"
    );

    // Legendary half: +2/+1, ward {1}, controller=You, supertype=Legendary.
    let legendary = defs
        .iter()
        .find(|d| {
            matches!(
                &d.affected,
                Some(TargetFilter::Typed(tf))
                    if tf.properties.contains(&FilterProp::HasSupertype {
                        value: Supertype::Legendary,
                    })
            )
        })
        .expect("must emit a Legendary-supertype static");
    assert_eq!(legendary.mode, StaticMode::Continuous);
    assert!(matches!(
        &legendary.affected,
        Some(TargetFilter::Typed(tf))
            if tf.controller == Some(ControllerRef::You)
                && tf.type_filters.contains(&TypeFilter::Creature)
    ));
    assert!(legendary
        .modifications
        .contains(&ContinuousModification::AddPower { value: 2 }));
    assert!(legendary
        .modifications
        .contains(&ContinuousModification::AddToughness { value: 1 }));
    // Ward cost must be {1} — the swallowed-period bug produced an empty {0}.
    let ward = legendary
        .modifications
        .iter()
        .find_map(|m| match m {
            ContinuousModification::AddKeyword {
                keyword: Keyword::Ward(WardCost::Mana(cost)),
            } => Some(cost),
            _ => None,
        })
        .expect("legendary half must grant a mana ward keyword");
    assert_eq!(
        ward.mana_value(),
        1,
        "ward cost must be {{1}}, got {ward:?}"
    );
    assert_ne!(*ward, ManaCost::zero(), "ward cost must not be empty {{0}}");

    // Nonlegendary half: +1/+1, controller=You, NotSupertype(Legendary), no keyword.
    let nonlegendary = defs
        .iter()
        .find(|d| {
            matches!(
                &d.affected,
                Some(TargetFilter::Typed(tf))
                    if tf.properties.contains(&FilterProp::NotSupertype {
                        value: Supertype::Legendary,
                    })
            )
        })
        .expect("must emit a NotSupertype(Legendary) static");
    assert_eq!(nonlegendary.mode, StaticMode::Continuous);
    assert!(matches!(
        &nonlegendary.affected,
        Some(TargetFilter::Typed(tf)) if tf.controller == Some(ControllerRef::You)
    ));
    assert!(nonlegendary
        .modifications
        .contains(&ContinuousModification::AddPower { value: 1 }));
    assert!(nonlegendary
        .modifications
        .contains(&ContinuousModification::AddToughness { value: 1 }));
    assert!(
        !nonlegendary
            .modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::AddKeyword { .. })),
        "nonlegendary half must NOT grant a keyword, got {:?}",
        nonlegendary.modifications
    );
}

/// Regression guard for the multi-sentence splitter: a single-sentence anthem
/// must still emit exactly ONE static (the splitter only fires for 2+ sentences
/// that each parse as statics), and trailing non-static prose must not cause a
/// partial emit.
#[test]
fn single_sentence_anthem_unaffected_by_multi_sentence_split() {
    let defs = parse_static_line_multi("Creatures you control get +1/+1.");
    assert_eq!(
        defs.len(),
        1,
        "single-sentence anthem must remain one static, got {defs:?}"
    );
    assert!(defs[0]
        .modifications
        .contains(&ContinuousModification::AddPower { value: 1 }));
}

/// CR 611.3a + CR 613.1f: Compound static where the keyword-grant conjunct has
/// a different subject ("creatures you control") than the P/T conjunct ("~").
/// Angelic Field Marshal: "As long as you control your commander, ~ gets +2/+2
/// and creatures you control have vigilance."
/// Must decompose into TWO StaticDefinitions sharing the same condition:
///   - def[0]: SelfRef subject, [AddPower(2), AddToughness(2)], condition present
///   - def[1]: Typed(creatures you control) subject, [AddKeyword(Vigilance)], condition present
#[test]
fn compound_static_foreign_keyword_grant_splits_angelic_field_marshal() {
    let defs = parse_static_line_multi(
        "As long as you control your commander, ~ gets +2/+2 and creatures you control have vigilance.",
    );
    assert_eq!(
        defs.len(),
        2,
        "expected 2 StaticDefinitions (self P/T + foreign keyword), got {defs:?}"
    );

    // Primary def: self-ref P/T boost with condition.
    let primary = &defs[0];
    assert_eq!(primary.mode, StaticMode::Continuous);
    assert!(
        matches!(&primary.affected, Some(TargetFilter::SelfRef)),
        "primary must target SelfRef, got {:?}",
        primary.affected
    );
    assert!(
        primary
            .modifications
            .contains(&ContinuousModification::AddPower { value: 2 }),
        "primary must add +2 power, got {:?}",
        primary.modifications
    );
    assert!(
        primary
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 2 }),
        "primary must add +2 toughness, got {:?}",
        primary.modifications
    );
    assert!(
        primary.condition.is_some(),
        "primary must carry the as-long-as condition"
    );

    // Companion def: creatures you control get vigilance with same condition.
    let companion = &defs[1];
    assert_eq!(companion.mode, StaticMode::Continuous);
    match &companion.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "companion must affect creatures you control, got {:?}",
                tf.type_filters
            );
        }
        other => panic!("companion affected must be Typed(creatures you control), got {other:?}"),
    }
    assert!(
        companion.modifications.iter().any(|m| matches!(m, ContinuousModification::AddKeyword { keyword } if *keyword == Keyword::Vigilance)),
        "companion must add Vigilance, got {:?}",
        companion.modifications
    );
    assert!(
        companion.condition.is_some(),
        "companion must carry the as-long-as condition"
    );
}

/// CR 702 + CR 613.1f: Kaldra Compleat — keyword list preceding a quoted
/// trigger ability must not drop the last bare keyword. `strip_quoted_segments`
/// removes the `, and "Whenever..."` tail but leaves a trailing comma; all five
/// keywords (first strike, trample, indestructible, haste, and the trigger)
/// must survive.
#[test]
fn static_keyword_list_before_quoted_trigger_keeps_last_keyword() {
    let defs = parse_static_line_multi(
        "Equipped creature gets +5/+5 and has first strike, trample, indestructible, haste, and \"Whenever this creature deals combat damage to a creature, exile that creature.\"",
    );
    assert_eq!(defs.len(), 1);
    let mods = &defs[0].modifications;
    for kw in [
        Keyword::FirstStrike,
        Keyword::Trample,
        Keyword::Indestructible,
        Keyword::Haste,
    ] {
        assert!(
            mods.iter().any(
                |m| matches!(m, ContinuousModification::AddKeyword { keyword } if *keyword == kw)
            ),
            "keyword {kw:?} missing from mods: {mods:?}"
        );
    }
    assert!(
        mods.iter()
            .any(|m| matches!(m, ContinuousModification::GrantTrigger { .. })),
        "GrantTrigger missing: {mods:?}"
    );
}

#[test]
fn static_graveyard_cards_have_retrace_during_your_turn() {
    // Six: "During your turn, nonland permanent cards in your graveyard have retrace."
    // CR 702.81a: Retrace is a casting keyword granted to cards in a specific zone.
    // Emits a Continuous static with AddKeyword so the off-zone keyword-grant
    // path sees the grant and the card becomes castable from the graveyard.
    let text = "During your turn, nonland permanent cards in your graveyard have retrace.";
    assert!(
        parse_spells_have_keyword_for_test(text).is_some(),
        "parse_spells_have_keyword should handle graveyard-zone keyword grants (Six)"
    );
    let def = parse_static_line(text).unwrap();
    assert_eq!(def.mode, StaticMode::Continuous);
    assert_eq!(def.condition, Some(StaticCondition::DuringYourTurn));
    assert!(
        def.modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Retrace,
            }),
        "Expected AddKeyword(Retrace), got {:?}",
        def.modifications
    );
    match &def.affected {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(
                tf.properties.contains(&FilterProp::InZone {
                    zone: Zone::Graveyard
                }),
                "Expected InZone(Graveyard), got {:?}",
                tf.properties
            );
            assert!(
                tf.type_filters.contains(&TypeFilter::Permanent),
                "Expected Permanent type filter, got {:?}",
                tf.type_filters
            );
            assert!(
                tf.type_filters
                    .contains(&TypeFilter::Non(Box::new(TypeFilter::Land))),
                "Expected Non(Land) type filter, got {:?}",
                tf.type_filters
            );
        }
        other => panic!("Expected Some(Typed filter), got {other:?}"),
    }
}
