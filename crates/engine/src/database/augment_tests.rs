use crate::database::augment::synthesize_augment;
use crate::types::ability::{AbilityDefinition, AbilityKind, Effect};
use crate::types::card::CardFace;
use crate::types::keywords::Keyword;

#[test]
fn synthesize_augment_adds_hand_activated_combine_ability() {
    let mut face = CardFace {
        name: "Monkey-".to_string(),
        oracle_text: Some(
            "Whenever a nontoken creature you control dies,\nAugment {2}{G} ({2}{G}, Reveal this card from your hand: Combine it with target host. Augment only as a sorcery.)"
                .to_string(),
        ),
        ..CardFace::default()
    };
    face.keywords.push(Keyword::Augment);
    face.abilities.push(
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Unimplemented {
                name: "unknown".to_string(),
                description: Some("Augment {2}{G}".to_string()),
            },
        )
        .description("Augment {2}{G}".to_string()),
    );

    synthesize_augment(&mut face);

    assert!(face.abilities.iter().any(|ability| {
        ability.activation_zone == Some(crate::types::zones::Zone::Hand)
            && matches!(ability.effect.as_ref(), Effect::CombineHost { .. })
    }));
}
