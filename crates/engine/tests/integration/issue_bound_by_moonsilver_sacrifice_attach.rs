//! Bound by Moonsilver — sacrifice-another-permanent relocate activated ability.

use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityCost, AbilityKind, ActivationRestriction, Effect, FilterProp, TargetFilter, TypedFilter,
};
use engine::types::statics::StaticMode;

const BOUND_BY_MOONSILVER_ORACLE: &str = "Enchant creature\n\
    Enchanted creature can't attack, block, or transform.\n\
    Sacrifice another permanent: Attach this Aura to target creature. Activate only as a sorcery and only once each turn.";

#[test]
fn bound_by_moonsilver_parser_sacrifice_another_attach_activated() {
    let parsed = parse_oracle_text(
        BOUND_BY_MOONSILVER_ORACLE,
        "Bound by Moonsilver",
        &[],
        &["Enchantment".to_string()],
        &["Aura".to_string()],
    );

    assert_eq!(parsed.abilities.len(), 1);
    let ability = &parsed.abilities[0];
    assert_eq!(ability.kind, AbilityKind::Activated);

    let Some(AbilityCost::Sacrifice(sac)) = ability.cost.as_ref() else {
        panic!("expected Sacrifice cost, got {:?}", ability.cost);
    };
    let TargetFilter::Typed(tf) = &sac.target else {
        panic!("expected typed sacrifice filter, got {:?}", sac.target);
    };
    assert!(
        tf.properties.contains(&FilterProp::Another),
        "Sacrifice another permanent must exclude the Aura source: {:?}",
        tf.properties
    );

    assert!(
        matches!(ability.effect.as_ref(), Effect::Attach { .. }),
        "expected Attach effect, got {:?}",
        ability.effect
    );
    let Effect::Attach { attachment, target } = ability.effect.as_ref() else {
        unreachable!();
    };
    assert_eq!(*attachment, TargetFilter::SelfRef);
    let TargetFilter::Typed(attach_target) = target else {
        panic!("expected creature attach target, got {target:?}");
    };
    assert!(
        attach_target.properties.is_empty()
            || !attach_target.properties.contains(&FilterProp::Another),
        "attach destination must not inherit Another from the cost"
    );

    assert!(ability
        .activation_restrictions
        .contains(&ActivationRestriction::AsSorcery));
    assert!(ability
        .activation_restrictions
        .contains(&ActivationRestriction::OnlyOnceEachTurn));

    assert!(
        parsed.statics.iter().any(|s| {
            s.mode == StaticMode::CantAttack
                && s.affected
                    == Some(TargetFilter::Typed(
                        TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
                    ))
        }),
        "expected enchanted-host CantAttack static"
    );
}
