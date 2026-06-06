// CR 702.122c + CR 702.171 + CR 702.184 — crew/saddle/station contribution statics.

#[allow(unused_imports)]
use super::prelude::*;
#[allow(unused_imports)]
use super::support::*;

/// Shared tail for "as though its power were N greater" and toughness substitution.
fn parse_crew_contribution_modifier(input: &str) -> OracleResult<'_, CrewContributionKind> {
    alt((
        map(
            (
                tag("as though its power were "),
                nom_primitives::parse_number,
                tag(" greater"),
            ),
            |(_, delta, _)| CrewContributionKind::PowerDelta {
                delta: i32::try_from(delta).unwrap_or(i32::MAX),
            },
        ),
        value(
            CrewContributionKind::ToughnessInsteadOfPower,
            tag("using its toughness rather than its power"),
        ),
    ))
    .parse(input)
}

/// CR 702.122 / CR 702.171 / CR 702.184: Keyword-action targets this creature
/// may crew, saddle, or station — composed as one axis, not enumerated per card.
fn parse_crew_contribution_actions(input: &str) -> OracleResult<'_, Vec<CrewAction>> {
    let (input, actions) = alt((
        value(vec![CrewAction::Crew], tag("crews vehicles")),
        value(
            vec![CrewAction::Saddle, CrewAction::Crew],
            tag("saddles mounts and crews vehicles"),
        ),
        value(
            vec![CrewAction::Crew, CrewAction::Station],
            tag("crews vehicles and stations permanents"),
        ),
        value(
            vec![CrewAction::Crew, CrewAction::Station],
            tag("crews vehicles and station permanents"),
        ),
    ))
    .parse(input)?;
    Ok((input, actions))
}

fn finish_crew_contribution(
    text: &str,
    kind: CrewContributionKind,
    actions: Vec<CrewAction>,
    affected: TargetFilter,
) -> StaticDefinition {
    StaticDefinition::new(StaticMode::CrewContribution { kind, actions })
        .affected(affected)
        .description(text.to_string())
}

/// Parse "~ crews Vehicles as though its power were 2 greater." and the
/// controlled-creature grant class (Stoic Star-Captain).
pub(crate) fn parse_crew_contribution(tp: &TextPair<'_>, text: &str) -> Option<StaticDefinition> {
    if let Some(rest) = nom_tag_tp(tp, "each creature you control ") {
        let rest_lower = rest.lower;
        let ((kind, actions, affected), _) = nom_on_lower(rest.original, rest_lower, |i| {
            let (i, actions) = parse_crew_contribution_actions(i)?;
            let (i, _) = space1.parse(i)?;
            let (i, kind) = parse_crew_contribution_modifier(i)?;
            let (i, _) = opt(tag(".")).parse(i)?;
            let (i, _) = eof(i)?;
            let affected =
                TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You));
            Ok((i, (kind, actions, affected)))
        })?;
        return Some(finish_crew_contribution(text, kind, actions, affected));
    }

    let ((kind, actions, affected), _) = nom_on_lower(tp.original, tp.lower, |i| {
        let (i, _) = alt((tag("~ "), tag("this creature "))).parse(i)?;
        let (i, actions) = parse_crew_contribution_actions(i)?;
        let (i, _) = space1.parse(i)?;
        let (i, kind) = parse_crew_contribution_modifier(i)?;
        let (i, _) = opt(tag(".")).parse(i)?;
        let (i, _) = eof(i)?;
        Ok((i, (kind, actions, TargetFilter::SelfRef)))
    })?;

    Some(finish_crew_contribution(text, kind, actions, affected))
}
