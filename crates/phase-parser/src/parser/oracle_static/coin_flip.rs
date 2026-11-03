// CR 705.3 — coin-flip result statics.

#[allow(unused_imports)]
use super::prelude::*;
#[allow(unused_imports)]
use super::support::*;

/// CR 705.3 + CR 109.5: Edgar, King of Figaro's Two-Headed Coin — "The first
/// time you flip one or more coins each turn, those coins come up heads and you
/// win those flips." The ability-word label is peeled before this runs.
///
/// Exact grammar on purpose: only the printed window ("the first time … each
/// turn"), subject ("you") and indicated result ("heads and you win") are
/// supported, so any other shape keeps its honest `Unimplemented`.
fn parse_win_first_coin_flip_set_each_turn(input: &str) -> OracleResult<'_, TargetFilter> {
    let (input, _) = tag("the first time ").parse(input)?;
    let (input, affected) = value(TargetFilter::Controller, tag("you")).parse(input)?;
    let (input, _) = tag(" flip one or more coins each turn, ").parse(input)?;
    let (input, _) = tag("those coins come up heads").parse(input)?;
    let (input, _) = tag(" and you win those flips").parse(input)?;
    let (input, _) = opt(tag(".")).parse(input)?;
    eof(input)?;
    Ok((input, affected))
}

pub(crate) fn is_win_first_coin_flip_set_each_turn_static(lower: &str) -> bool {
    parse_win_first_coin_flip_set_each_turn(lower).is_ok()
}

pub(crate) fn try_parse_win_first_coin_flip_set_each_turn_static(
    text: &str,
    lower: &str,
) -> Option<StaticDefinition> {
    nom_on_lower(text, lower, parse_win_first_coin_flip_set_each_turn).map(|(affected, _)| {
        StaticDefinition::new(StaticMode::WinFirstCoinFlipSetEachTurn)
            .affected(affected)
            .description(text.to_string())
    })
}
