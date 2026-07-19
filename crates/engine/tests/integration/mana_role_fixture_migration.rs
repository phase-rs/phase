//! Matrix row 11 — the migrated card fixture must carry the CORRECT mana roles,
//! not merely deserialize.
//!
//! CR 601.2c: a mana sentence's player target is either the RECIPIENT whose pool
//! receives the mana (CR 106.4) or the COUNT SOURCE the production's quantity
//! reads (CR 115.1). The role is NOT recoverable from a bare `TargetFilter`'s
//! shape — Carpet of Flowers (count source) encodes as
//! `Typed{controller: Opponent}` and Spectral Searchlight (recipient) as
//! `Typed{controller: ChosenPlayer(0)}`: both `Typed`, opposite roles. A
//! "legacy-tolerant `Deserialize`" would therefore have to re-introduce exactly
//! the quantity-shape inference this change deletes, so regeneration from the
//! parser (which knows the role by construction) is the only correct migration.
//!
//! Carpet of Flowers is the canary: a careless bulk rewrite silently flips it to
//! `Recipient`, which at runtime would redirect its mana to a targeted opponent
//! instead of its controller. This test is the durable guard the one-shot
//! migration script is not.

use engine::types::ability::{AbilityDefinition, Effect, ManaTargetRole};

use crate::support::shared_card_db;

/// Collect every mana role reachable from an ability definition, including the
/// sub-ability chain (Carpet of Flowers and Belbe both reach their
/// `Effect::Mana` through a TRIGGER's execute chain, not a bare spell ability).
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
fn carpet_of_flowers_stays_a_count_source_and_belbe_stays_a_recipient() {
    if shared_card_db().is_none() {
        eprintln!("card fixture unavailable; skipping");
        return;
    }

    let carpet = roles_for("Carpet of Flowers");
    let belbe = roles_for("Belbe, Corrupted Observer");

    // Reach guard: both cards must actually carry a mana role, or every
    // assertion below is vacuously satisfied by an empty vector.
    assert!(
        !carpet.is_empty(),
        "Carpet of Flowers must carry a mana role in the fixture"
    );
    assert!(
        !belbe.is_empty(),
        "Belbe, Corrupted Observer must carry a mana role in the fixture"
    );

    assert!(
        carpet
            .iter()
            .all(|r| matches!(r, ManaTargetRole::CountSource { .. })),
        "CANARY: Carpet of Flowers' target is a COUNT SOURCE (\"where X is the \
         number of Islands target opponent controls\") — its mana goes to its \
         CONTROLLER. Got {carpet:?}"
    );
    assert!(
        belbe
            .iter()
            .all(|r| matches!(r, ManaTargetRole::Recipient { .. })),
        "Belbe's subject-led target is the mana RECIPIENT. Got {belbe:?}"
    );
}
