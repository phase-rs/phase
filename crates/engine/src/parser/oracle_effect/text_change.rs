//! CR 612: recognizes "Change the text of <target> by replacing all instances of one <word class> with another" for the color-word and basic-land-type classes only, so any other class stays an honest `Effect::unimplemented`.

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::combinator::{eof, opt, value, verify};
use nom::sequence::{preceded, terminated};
use nom::Parser;

use crate::parser::oracle_ir::ast::ParsedEffectClause;
use crate::parser::oracle_nom::bridge::nom_on_lower;
use crate::parser::oracle_nom::duration::parse_duration;
use crate::parser::oracle_nom::error::OracleResult;
use crate::parser::oracle_target::parse_target;
use crate::parser::oracle_util::TextPair;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, ChoiceType, ContinuousModification, Duration, Effect,
    StaticDefinition, TargetFilter, TargetSelectionMode, TextSubstitution, TextSubstitutionSpec,
    TextWordDomain,
};

/// CR 612.2: the word class named by "one <class> with another".
fn parse_word_class(input: &str) -> OracleResult<'_, TextWordDomain> {
    terminated(
        preceded(
            tag("one "),
            alt((
                value(TextWordDomain::ColorWord, tag("color word")),
                value(TextWordDomain::BasicLandType, tag("basic land type")),
            )),
        ),
        tag(" with another"),
    )
    .parse(input)
}

struct TextChange {
    target: TargetFilter,
    domains: Vec<TextWordDomain>,
    duration: Option<Duration>,
}

fn parse_text_change(input: &str) -> OracleResult<'_, TextChange> {
    let (rest, _) = tag("change the text of ").parse(input)?;
    let (target, rest) = parse_target(rest);
    let (rest, _) = tag(" by replacing all instances of ").parse(rest)?;
    let (rest, first) = parse_word_class(rest)?;
    let (rest, second) = opt(preceded(
        tag(" or "),
        verify(parse_word_class, |second| *second != first),
    ))
    .parse(rest)?;
    let (rest, duration) = opt(preceded(tag(" "), parse_duration)).parse(rest)?;
    let (rest, _) = opt(tag(".")).parse(rest)?;
    let (rest, _) = eof.parse(rest)?;
    let domains = std::iter::once(first).chain(second).collect();
    Ok((
        rest,
        TextChange {
            target,
            domains,
            duration,
        },
    ))
}

/// CR 612.1 + CR 608.2d: the controller names the two words as the effect resolves, then a `GenericEffect` installs the Layer 3 substitution on the target.
pub(super) fn try_parse_text_change_clause(tp: TextPair<'_>) -> Option<ParsedEffectClause> {
    let (change, _) = nom_on_lower(tp.original, tp.lower, parse_text_change)?;

    // CR 611.2a: no stated duration means the effect lasts until end of game.
    let duration = change.duration.unwrap_or(Duration::Permanent);
    let options = TextSubstitution::options(&change.domains);
    let apply = Effect::GenericEffect {
        static_abilities: vec![StaticDefinition::continuous()
            .affected(TargetFilter::ParentTarget)
            .modifications(vec![ContinuousModification::SubstituteTextWord {
                substitution: TextSubstitutionSpec::Chosen {
                    domains: change.domains,
                },
            }])
            .description(tp.original.trim().to_string())],
        duration: Some(duration.clone()),
        target: Some(change.target),
        end_cost: None,
    };
    Some(ParsedEffectClause {
        unlowered_guard: None,
        effect: Effect::Choose {
            choice_type: ChoiceType::Labeled { options },
            persist: false,
            selection: TargetSelectionMode::Chosen,
        },
        duration: Some(duration),
        sub_ability: Some(Box::new(AbilityDefinition::new(AbilityKind::Spell, apply))),
        distribute: None,
        multi_target: None,
        condition: None,
        optional: false,
        unless_pay: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_oracle_text;
    use crate::types::ability::{AbilityCost, TypeFilter};

    fn parse(name: &str, text: &str, keywords: &[&str]) -> Vec<AbilityDefinition> {
        let keywords: Vec<String> = keywords.iter().map(|k| (*k).to_string()).collect();
        parse_oracle_text(text, name, &keywords, &["Instant".to_string()], &[]).abilities
    }

    /// True when `def` or any node chained from it satisfies `pred`.
    fn node_matches(def: &AbilityDefinition, pred: &impl Fn(&AbilityDefinition) -> bool) -> bool {
        pred(def)
            || def
                .sub_ability
                .as_deref()
                .is_some_and(|d| node_matches(d, pred))
            || def
                .else_ability
                .as_deref()
                .is_some_and(|d| node_matches(d, pred))
            || def.mode_abilities.iter().any(|d| node_matches(d, pred))
    }

    fn any_node(
        abilities: &[AbilityDefinition],
        pred: &impl Fn(&AbilityDefinition) -> bool,
    ) -> bool {
        abilities.iter().any(|def| node_matches(def, pred))
    }

    fn has_unimplemented(abilities: &[AbilityDefinition]) -> bool {
        any_node(abilities, &|def| {
            matches!(def.effect.as_ref(), Effect::Unimplemented { .. })
                || def
                    .cost
                    .as_ref()
                    .is_some_and(AbilityCost::contains_unimplemented)
        })
    }

    fn has_word_substitution(abilities: &[AbilityDefinition]) -> bool {
        any_node(abilities, &|def| {
            let Effect::GenericEffect {
                static_abilities, ..
            } = def.effect.as_ref()
            else {
                return false;
            };
            static_abilities.iter().any(|s| {
                s.modifications
                    .iter()
                    .any(|m| matches!(m, ContinuousModification::SubstituteTextWord { .. }))
            })
        })
    }

    /// The chain every text change lowers to: the word prompt, then the install.
    fn prompt_and_install(
        def: &AbilityDefinition,
    ) -> (Vec<String>, Vec<TextWordDomain>, Duration, TargetFilter) {
        let Effect::Choose {
            choice_type: ChoiceType::Labeled { options },
            ..
        } = def.effect.as_ref()
        else {
            panic!("expected the word prompt, got {:?}", def.effect);
        };
        let install = def.sub_ability.as_deref().expect("the install step");
        let Effect::GenericEffect {
            static_abilities,
            duration,
            target,
            ..
        } = install.effect.as_ref()
        else {
            panic!("expected the install step, got {:?}", install.effect);
        };
        let [ContinuousModification::SubstituteTextWord {
            substitution: TextSubstitutionSpec::Chosen { domains },
        }] = static_abilities[0].modifications.as_slice()
        else {
            panic!("expected one pending substitution");
        };
        (
            options.clone(),
            domains.clone(),
            duration.clone().expect("explicit duration"),
            target.clone().expect("a target"),
        )
    }

    fn spell_or_permanent() -> TargetFilter {
        TargetFilter::Or {
            filters: vec![
                TargetFilter::StackSpell,
                TargetFilter::Typed(crate::types::ability::TypedFilter::permanent()),
            ],
        }
    }

    /// "One basic land type with another" with no stated duration lowers to a land-pair prompt and a permanent install.
    #[test]
    fn magical_hack_lowers_to_a_land_pair_prompt_and_a_permanent_install() {
        let abilities = parse(
            "Magical Hack",
            "Change the text of target spell or permanent by replacing all instances of one basic land type with another. (For example, you may change \"swampwalk\" to \"plainswalk.\" This effect lasts indefinitely.)",
            &[],
        );
        let (options, domains, duration, target) = prompt_and_install(&abilities[0]);
        assert_eq!(
            options,
            TextSubstitution::options(&[TextWordDomain::BasicLandType])
        );
        assert_eq!(domains, [TextWordDomain::BasicLandType]);
        assert_eq!(duration, Duration::Permanent, "CR 611.2a");
        assert_eq!(target, spell_or_permanent());
    }

    /// Two word domains with "until end of turn" lower to a both-domain prompt, an until-end-of-turn install, then the draw.
    #[test]
    fn crystal_spray_lowers_to_both_domains_until_end_of_turn_then_draws() {
        let abilities = parse(
            "Crystal Spray",
            "Change the text of target spell or permanent by replacing all instances of one color word with another or one basic land type with another until end of turn.\nDraw a card.",
            &[],
        );
        let (options, domains, duration, _) = prompt_and_install(&abilities[0]);
        assert_eq!(options.len(), 40);
        assert_eq!(
            domains,
            [TextWordDomain::ColorWord, TextWordDomain::BasicLandType]
        );
        assert_eq!(duration, Duration::UntilEndOfTurn);
        let install = abilities[0].sub_ability.as_deref().expect("install");
        assert!(
            matches!(
                install.sub_ability.as_deref().map(|d| d.effect.as_ref()),
                Some(Effect::Draw { .. })
            ),
            "the draw follows the change"
        );
    }

    /// The target phrase is a parameter, so "target permanent" excludes spells.
    #[test]
    fn mind_bend_targets_permanents_only() {
        let abilities = parse(
            "Mind Bend",
            "Change the text of target permanent by replacing all instances of one color word with another or one basic land type with another. (For example, you may change \"nonblack creature\" to \"nongreen creature\" or \"forestwalk\" to \"islandwalk.\" This effect lasts indefinitely.)",
            &[],
        );
        let (_, domains, duration, target) = prompt_and_install(&abilities[0]);
        assert_eq!(
            domains,
            [TextWordDomain::ColorWord, TextWordDomain::BasicLandType]
        );
        assert_eq!(duration, Duration::Permanent);
        assert_eq!(
            target,
            TargetFilter::Typed(crate::types::ability::TypedFilter::new(
                TypeFilter::Permanent
            ))
        );
    }

    /// The keyworded and modal members of the word class parse with no `Unimplemented`.
    #[test]
    fn keyworded_and_modal_class_members_parse_completely() {
        for (name, text, keywords) in [
            (
                "Sleight of Mind",
                "Change the text of target spell or permanent by replacing all instances of one color word with another. (For example, you may change \"target black spell\" to \"target blue spell.\" This effect lasts indefinitely.)",
                &[][..],
            ),
            (
                "Alter Reality",
                "Change the text of target spell or permanent by replacing all instances of one color word with another. (This effect lasts indefinitely.)\nFlashback {1}{U} (You may cast this card from your graveyard for its flashback cost. Then exile it.)",
                &["Flashback"][..],
            ),
            (
                "Glamerdye",
                "Change the text of target spell or permanent by replacing all instances of one color word with another.\nRetrace (You may cast this card from your graveyard by discarding a land card in addition to paying its other costs.)",
                &["Retrace"][..],
            ),
            (
                "Whim of Volrath",
                "Buyback {2} (You may pay an additional {2} as you cast this spell. If you do, put this card into your hand as it resolves.)\nChange the text of target permanent by replacing all instances of one color word with another or one basic land type with another until end of turn. (For example, you may change \"nonred creature\" to \"nongreen creature\" or \"plainswalk\" to \"swampwalk.\")",
                &["Buyback"][..],
            ),
            (
                "Trait Doctoring",
                "Change the text of target permanent by replacing all instances of one color word with another or one basic land type with another until end of turn.\nCipher (Then you may exile this spell card encoded on a creature you control. Whenever that creature deals combat damage to a player, its controller may cast a copy of the encoded card without paying its mana cost.)",
                &["Cipher"][..],
            ),
            (
                "Spectral Shift",
                "Choose one —\n• Change the text of target spell or permanent by replacing all instances of one basic land type with another. (This effect lasts indefinitely.)\n• Change the text of target spell or permanent by replacing all instances of one color word with another. (This effect lasts indefinitely.)\nEntwine {2} (Choose both if you pay the entwine cost.)",
                &["Entwine"][..],
            ),
        ] {
            let abilities = parse(name, text, keywords);
            assert!(has_word_substitution(&abilities), "{name}: reach-guard");
            assert!(!has_unimplemented(&abilities), "{name}: {abilities:?}");
        }
    }

    /// Fail-closed: every other word class stays an honest `Unimplemented`.
    #[test]
    fn other_word_classes_stay_unimplemented() {
        for (name, text) in [
            (
                "Artificial Evolution",
                "Change the text of target spell or permanent by replacing all instances of one creature type with another. The new creature type can't be Wall. (This effect lasts indefinitely.)",
            ),
            (
                "New Blood",
                "As an additional cost to cast this spell, tap an untapped Vampire you control.\nGain control of target creature. Change the text of that creature by replacing all instances of one creature type with Vampire.",
            ),
            (
                "Magical Hacker",
                "{U}: Change the text of target spell or permanent by replacing all instances of + with -, and vice versa, until end of turn.",
            ),
        ] {
            let abilities = parse(name, text, &[]);
            assert!(has_unimplemented(&abilities), "{name} must stay unimplemented");
            assert!(
                !has_word_substitution(&abilities),
                "{name} must not lower to a word substitution"
            );
        }
    }
}
