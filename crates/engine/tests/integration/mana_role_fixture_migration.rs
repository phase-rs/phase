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
//! Carpet of Flowers carries its CR 603.4 "added mana with this ability" guard
//! and the count-source role its "target opponent" fills.

use engine::types::ability::{
    AbilityDefinition, ControllerRef, Effect, ManaTargetRole, TargetFilter, TriggerCondition,
    TypedFilter,
};

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

/// Every `Effect::Unimplemented` in a definition's resolution chain.
fn unimplemented_effects_in(def: &AbilityDefinition) -> usize {
    let own = usize::from(matches!(def.effect.as_ref(), Effect::Unimplemented { .. }));
    let chained: usize = def
        .sub_ability
        .as_deref()
        .into_iter()
        .chain(def.else_ability.as_deref())
        .map(unimplemented_effects_in)
        .sum();
    own + chained
}

#[test]
fn carpet_of_flowers_fixture_carries_its_guard_and_count_source_role() {
    let Some(db) = shared_card_db() else {
        eprintln!("card fixture unavailable; skipping");
        return;
    };
    // Reach guard: Carpet's face is present in the fixture.
    let face = db
        .get_face_by_name("Carpet of Flowers")
        .expect("Carpet of Flowers must be present in the fixture");
    assert_eq!(face.triggers.len(), 1);
    let trigger = &face.triggers[0];
    // CR 603.4 + CR 607.1c: the intervening-if gates both triggering and
    // resolution, and is about this ability's own history.
    assert_eq!(
        trigger.condition,
        Some(TriggerCondition::Not {
            condition: Box::new(TriggerCondition::AddedManaWithThisAbilityThisTurn),
        })
    );
    let execute = trigger
        .execute
        .as_deref()
        .expect("Carpet's trigger must carry its mana effect");
    assert_eq!(unimplemented_effects_in(execute), 0);
    // CR 115.1d + CR 106.4: "target opponent" is read by the count; the mana
    // goes to Carpet's controller.
    assert_eq!(
        roles_for("Carpet of Flowers"),
        vec![ManaTargetRole::CountSource {
            count_source: TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent)
            ),
        }]
    );
}
