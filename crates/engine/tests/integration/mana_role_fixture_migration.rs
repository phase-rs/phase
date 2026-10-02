//! SHAPE: Matrix row 11 — the migrated card fixture must carry the correct
//! mana roles, not merely deserialize.
//!
//! CR 601.2c: a mana sentence's player target is either the RECIPIENT whose pool
//! receives the mana (CR 106.4) or the COUNT SOURCE the production's quantity
//! reads (CR 115.1). Jeska's Will's first mode persists a CountSource, while
//! Belbe persists a Recipient. The role is not recoverable from a bare
//! `TargetFilter`, so regeneration from the parser, which knows the role by
//! construction, is the correct migration.
//!
//! Carpet of Flowers currently fails closed on its unparsed intervening-if.
//! Preserve that explicit gap alongside the nonempty positive role canaries.

use engine::parser::oracle_ir::diagnostic::ClauseGapKind;
use engine::types::ability::{AbilityDefinition, Effect, ManaTargetRole};

use crate::support::shared_card_db;

/// Collect every mana role through ability, sub-ability and else-ability chains;
/// `roles_for` also visits trigger execution chains.
fn collect_roles(def: &AbilityDefinition, out: &mut Vec<ManaTargetRole>) {
    if let Effect::Mana {
        target: Some(role), ..
    } = &*def.effect
    {
        out.push(role.clone());
    }
    for sub in def
        .sub_ability
        .as_deref()
        .into_iter()
        .chain(def.else_ability.as_deref())
    {
        collect_roles(sub, out);
    }
}

fn roles_for(card: &str) -> Vec<ManaTargetRole> {
    let Some(db) = shared_card_db() else {
        return Vec::new();
    };
    let Some(face) = db.get_face_by_name(card) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ability in &face.abilities {
        collect_roles(ability, &mut out);
    }
    for trigger in &face.triggers {
        if let Some(execute) = trigger.execute.as_deref() {
            collect_roles(execute, &mut out);
        }
    }
    out
}

#[test]
fn jeskas_will_stays_a_count_source_and_belbe_stays_a_recipient() {
    if shared_card_db().is_none() {
        eprintln!("card fixture unavailable; skipping");
        return;
    }

    let jeska = roles_for("Jeska's Will");
    let belbe = roles_for("Belbe, Corrupted Observer");

    // Reach guard: both cards must actually carry a mana role, or every
    // assertion below is vacuously satisfied by an empty vector.
    assert!(
        !jeska.is_empty(),
        "Jeska's Will must carry a mana role in the fixture"
    );
    assert!(
        !belbe.is_empty(),
        "Belbe, Corrupted Observer must carry a mana role in the fixture"
    );

    assert!(
        jeska
            .iter()
            .all(|r| matches!(r, ManaTargetRole::CountSource { .. })),
        "CANARY: Jeska's Will's target is a COUNT SOURCE (\"for each card in \
         target opponent's hand\") — its mana goes to its CONTROLLER. Got {jeska:?}"
    );
    assert!(
        belbe
            .iter()
            .all(|r| matches!(r, ManaTargetRole::Recipient { .. })),
        "Belbe's subject-led target is the mana RECIPIENT. Got {belbe:?}"
    );
}

#[test]
fn carpet_of_flowers_unparsed_intervening_if_stays_explicitly_unsupported() {
    let Some(db) = shared_card_db() else {
        eprintln!("card fixture unavailable; skipping");
        return;
    };
    let face = db
        .get_face_by_name("Carpet of Flowers")
        .expect("Carpet of Flowers must be present in the fixture");
    assert_eq!(face.triggers.len(), 1);
    let execute = face.triggers[0]
        .execute
        .as_deref()
        .expect("Carpet's trigger must retain its explicit execution gap");
    let Effect::Unimplemented { name, description } = execute.effect.as_ref() else {
        panic!(
            "Carpet must retain its conditional gap: {:?}",
            execute.effect
        );
    };
    // CR 603.4: the intervening-if must gate both triggering and resolution.
    assert_eq!(name, ClauseGapKind::Condition.unimplemented_name());
    let description = description
        .as_deref()
        .expect("Carpet's conditional gap must retain its actual intervening-if guard");
    assert!(description.starts_with("if you haven't added mana with this ability this turn,"));
    assert!(roles_for("Carpet of Flowers").is_empty());
}
