// CR 613.3 — mana transformation static abilities.

#[allow(unused_imports)]
use super::prelude::*;
#[allow(unused_imports)]
use super::support::*;

/// CR 614.1a + CR 703.4q: Parse "If you would lose unspent mana, that mana
/// becomes [type] instead." — Horizon Stone / Kruphix / Omnath / Ozai class.
/// Emits the unified `StepEndUnspentMana { filter: None, action: Transform(to) }`
/// bound to the source's controller.
pub(crate) fn try_parse_transform_unspent_mana_static(
    text: &str,
    lower: &str,
) -> Option<StaticDefinition> {
    use crate::types::mana::StepEndManaAction;

    nom_on_lower(text, lower, |input| {
        let (input, _) =
            tag::<_, _, OracleError<'_>>("if you would lose unspent mana, that mana becomes ")
                .parse(input)?;
        let (input, to) = alt((
            value(ManaType::Colorless, tag("colorless")),
            map(nom_primitives::parse_color, ManaType::from),
        ))
        .parse(input)?;
        let (input, _) = tag(" instead").parse(input)?;
        let (input, _) = opt(tag(".")).parse(input)?;
        eof(input)?;
        Ok((input, to))
    })
    .map(|(to, _)| {
        let mode = StaticMode::StepEndUnspentMana {
            filter: None,
            action: StepEndManaAction::Transform(to),
        };
        StaticDefinition::new(mode.clone())
            .affected(TargetFilter::Controller)
            .modifications(vec![ContinuousModification::AddStaticMode { mode }])
            .description(text.to_string())
    })
}

pub(crate) fn try_parse_retain_unspent_mana_static(
    text: &str,
    lower: &str,
) -> Option<StaticDefinition> {
    use crate::types::mana::StepEndManaAction;

    nom_on_lower(text, lower, |input| {
        // CR 703.4q: Subject parameterizes the affected scope.
        // "You" → controller (Electro); "Players" → every player (Upwelling).
        let (input, affected) = alt((
            value(
                TargetFilter::Controller,
                tag::<_, _, OracleError<'_>>("you "),
            ),
            value(TargetFilter::Player, tag("players ")),
        ))
        .parse(input)?;
        let (input, _) = alt((tag("don't lose "), tag("don\u{2019}t lose "))).parse(input)?;
        let (input, color) = alt((
            value(None, tag("unspent mana")),
            map(
                preceded(
                    tag("unspent "),
                    terminated(nom_primitives::parse_color, tag(" mana")),
                ),
                Some,
            ),
        ))
        .parse(input)?;
        let (input, _) = tag(" as steps and phases end").parse(input)?;
        let (input, _) = opt(tag(".")).parse(input)?;
        eof(input)?;
        Ok((input, (affected, color)))
    })
    .map(|((affected, color), _)| {
        // CR 611.2b: `modifications` carries the same mode so transient-effect
        // installation (spells like The Last Agni Kai that emit this via
        // `Effect::GenericEffect`) propagates the retention rule through
        // `register_transient_effect` → `add_transient_continuous_effect`.
        // Printed-static callers (Upwelling, Electro) reach this via the
        // source's `static_definitions` scan and ignore `modifications`.
        let mode = StaticMode::StepEndUnspentMana {
            filter: color,
            action: StepEndManaAction::Retain,
        };
        StaticDefinition::new(mode.clone())
            .affected(affected)
            .modifications(vec![ContinuousModification::AddStaticMode { mode }])
            .description(text.to_string())
    })
}
