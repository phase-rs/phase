//! Issue #3272 — Lightstall Inquisitor. The ETB compound "each opponent exiles
//! a card from their hand and may play that card for as long as it remains
//! exiled" must split into a player-scoped exile plus an owner-binding
//! `PlayFromExile` grant, and the two rider sentences ("Each spell cast this way
//! costs {1} more to cast." / "Each land played this way enters tapped.") must
//! fold into that grant's `cast_cost_raise` / `land_enter_tapped` rather than
//! emitting a board-wide cost static or ETB-tapped replacement.

use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, CastingPermission, Duration, Effect, PermissionGrantee, TargetFilter,
    TypeFilter,
};
use engine::types::mana::ManaCost;
use engine::types::zones::EtbTapState;

const LIGHTSTALL_ORACLE: &str = "Vigilance\nWhen this creature enters, each opponent exiles a \
card from their hand and may play that card for as long as it remains exiled. Each spell cast \
this way costs {1} more to cast. Each land played this way enters tapped.";

const GOBAKHAN_ORACLE: &str = "When this Siege enters, look at target opponent's hand. You may \
exile a nonland card from it. For as long as that card remains exiled, its owner may play it. A \
spell cast this way costs {2} more to cast.";

/// Walk an ability definition and its `sub_ability` / `else_ability` chain,
/// returning the first `PlayFromExile` grant together with its grantee.
fn find_play_from_exile_grant(
    def: &AbilityDefinition,
) -> Option<(&CastingPermission, &PermissionGrantee)> {
    if let Effect::GrantCastingPermission {
        permission: permission @ CastingPermission::PlayFromExile { .. },
        grantee,
        ..
    } = &*def.effect
    {
        return Some((permission, grantee));
    }
    def.sub_ability
        .as_deref()
        .and_then(find_play_from_exile_grant)
        .or_else(|| {
            def.else_ability
                .as_deref()
                .and_then(find_play_from_exile_grant)
        })
}

#[test]
fn lightstall_etb_grants_owner_play_with_cost_raise_and_land_tapped() {
    let keywords = vec!["Vigilance".to_string()];
    let types = vec!["Creature".to_string()];
    let subtypes = vec!["Bird".to_string(), "Cleric".to_string()];
    let parsed = parse_oracle_text(
        LIGHTSTALL_ORACLE,
        "Lightstall Inquisitor",
        &keywords,
        &types,
        &subtypes,
    );

    assert_eq!(
        parsed.triggers.len(),
        1,
        "Lightstall has exactly one ETB trigger, got {:?}",
        parsed.triggers
    );
    let execute = parsed.triggers[0]
        .execute
        .as_deref()
        .expect("the ETB trigger must carry an execute body");

    let (permission, grantee) = find_play_from_exile_grant(execute).unwrap_or_else(|| {
        panic!("ETB body must chain a PlayFromExile grant; body was:\n{execute:#?}")
    });

    assert_eq!(
        *grantee,
        PermissionGrantee::ObjectOwner,
        "the subject-elided \"may play that card\" binds the grant per-card to the exiling \
         player (the card's owner), not to the Lightstall controller"
    );

    let CastingPermission::PlayFromExile {
        duration,
        cast_cost_raise,
        land_enter_tapped,
        ..
    } = permission
    else {
        unreachable!("matched PlayFromExile above")
    };

    // CR 400.7i + CR 611.2a: "for as long as it remains exiled" → Permanent
    // (cleared on zone exit), not the impulse-draw UntilEndOfTurn default.
    assert_eq!(
        *duration,
        Duration::Permanent,
        "\"for as long as it remains exiled\" must encode a Permanent (exile-scoped) duration"
    );

    // CR 601.2f: "Each spell cast this way costs {1} more to cast." folds into the grant.
    assert_eq!(
        *cast_cost_raise,
        Some(ManaCost::Cost {
            shards: vec![],
            generic: 1,
        }),
        "the cost-raise rider must fold into the grant's cast_cost_raise as {{1}}"
    );

    // CR 614.1c: "Each land played this way enters tapped." folds into the grant.
    assert_eq!(
        *land_enter_tapped,
        EtbTapState::Tapped,
        "the land-tapped rider must fold into the grant's land_enter_tapped"
    );
}

#[test]
fn gobakhan_exiles_only_nonlands_and_raises_the_exiled_spell_cost() {
    let parsed = parse_oracle_text(
        GOBAKHAN_ORACLE,
        "Invasion of Gobakhan",
        &[],
        &["Battle".to_string()],
        &["Siege".to_string()],
    );
    let execute = parsed
        .triggers
        .iter()
        .filter_map(|trigger| trigger.execute.as_deref())
        .find(|execute| matches!(execute.effect.as_ref(), Effect::RevealHand { .. }))
        .expect("Gobakhan must have an ETB hand-reveal trigger");

    let Effect::RevealHand {
        card_filter,
        choice_optional,
        ..
    } = execute.effect.as_ref()
    else {
        unreachable!("matched RevealHand above")
    };
    assert!(*choice_optional);
    assert!(
        matches!(
            card_filter,
            TargetFilter::Typed(filter)
                if filter.type_filters.iter().any(|kind| matches!(
                    kind,
                    TypeFilter::Non(inner) if **inner == TypeFilter::Land
                ))
        ),
        "Gobakhan's exile choice must exclude lands, got {card_filter:?}"
    );

    let (permission, grantee) = find_play_from_exile_grant(execute)
        .expect("Gobakhan must grant the exiled card's owner permission to play it");
    assert_eq!(*grantee, PermissionGrantee::ObjectOwner);
    let CastingPermission::PlayFromExile {
        duration,
        cast_cost_raise,
        ..
    } = permission
    else {
        unreachable!("matched PlayFromExile above")
    };
    assert_eq!(*duration, Duration::Permanent);
    assert_eq!(
        *cast_cost_raise,
        Some(ManaCost::Cost {
            shards: vec![],
            generic: 2,
        })
    );
}
