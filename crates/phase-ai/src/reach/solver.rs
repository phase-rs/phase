//! Combination search over priced reach sources.
//!
//! The question is joint, not per-spell: which SUBSET of the available sources
//! takes the opponent to 0 within the mana on hand? Evaluating spells one at a
//! time in hand order is exactly how "Seal + Seal" (4 damage) beats
//! "Seal + Flame Rift" (6 damage) — each Seal is cheaper in isolation.

use super::sources::{Gain, LifeLoss, Linear, PricedSource};
use super::LineStep;

/// Above this many sources only the most mana-efficient are searched; the
/// subset enumeration is `2^n`.
const MAX_SEARCHED_SOURCES: usize = 14;

/// What a line has to work with, read from the state the sources were priced
/// in.
pub(super) struct Budget {
    pub(super) opponent_life: u32,
    pub(super) controller_life: u32,
    pub(super) mana: u32,
}

/// A lethal subset of the priced sources and the X its X-scaling member
/// announces, if it has one (CR 107.3a).
pub(super) struct Combination {
    pub(super) members: Vec<usize>,
    pub(super) x: Option<u32>,
}

/// Every combination of `sources` that meets the opponent's life total while
/// the AI survives, cheapest first: least mana, then least life paid, then
/// fewest cards. Mana is checked only as a total here — colours, restrictions
/// and real payment are the reducer's to certify.
///
/// At most one source in a combination may scale with X (CR 107.3a). X is the
/// most the mana the rest of the combination leaves over can pay for, unless
/// that X would cost the AI the game — damage to each player — in which case
/// it is the least X that is still lethal.
///
/// Survival is checked against the AI's life plus the life the combination
/// gains it (CR 119.3), and not at all when that gain is unread: the order
/// things resolve in decides whether a gain lands before a loss, and the
/// reducer reads that order when it certifies the line.
pub(super) fn lethal_combinations(sources: &[PricedSource], budget: &Budget) -> Vec<Combination> {
    let searched = searched_indices(sources, budget.mana);
    let ceiling: u32 = searched
        .iter()
        .map(|&index| sources[index].loss.opponent.at(budget.mana))
        .sum();
    if ceiling < budget.opponent_life {
        return Vec::new();
    }

    let mut lethal: Vec<(u32, u32, u32, u32, Combination)> = Vec::new();
    for mask in 1u32..(1u32 << searched.len()) {
        let members: Vec<usize> = searched
            .iter()
            .enumerate()
            .filter(|(bit, _)| mask & (1 << bit) != 0)
            .map(|(_, &index)| index)
            .collect();
        let mut scaling = members
            .iter()
            .filter(|&&index| sources[index].loss.opponent.per_x > 0);
        let x_member = scaling.next();
        if scaling.next().is_some() {
            continue;
        }
        let mana_value: u32 = members
            .iter()
            .map(|&index| sources[index].mana.mana_value())
            .sum();
        if mana_value > budget.mana {
            continue;
        }
        let total = |side: fn(&LifeLoss) -> Linear| {
            members.iter().fold(Linear::default(), |sum, &index| {
                sum.plus(side(&sources[index].loss))
            })
        };
        let damage = total(|loss| loss.opponent);
        let paid = total(|loss| loss.controller);
        let gain = members.iter().fold(Gain::default(), |sum, &index| {
            sum.plus(sources[index].loss.controller_gain)
        });
        // CR 104.4a: if both players hit 0 together the game is a draw, so the
        // AI must stay above 0 for the line to win.
        let survives = |x: u32| match gain {
            Gain::Read(gained) => paid.at(x) < budget.controller_life + gained.at(x),
            Gain::Unread => true,
        };
        let lethal_at = |x: u32| damage.at(x) >= budget.opponent_life;
        let x = match x_member {
            None => None,
            Some(&index) => {
                let most = (budget.mana - mana_value) / sources[index].x_shards().max(1);
                if lethal_at(most) && survives(most) {
                    Some(most)
                } else {
                    // Damage and life paid both grow with X, so the least
                    // lethal X is the one the AI is likeliest to survive.
                    least_lethal_x(damage, budget.opponent_life).filter(|&x| x <= most)
                }
            }
        };
        let at = x.unwrap_or(0);
        if (x_member.is_none() || x.is_some()) && lethal_at(at) && survives(at) {
            lethal.push((
                mana_value,
                paid.at(at),
                members.len() as u32,
                mask,
                Combination { members, x },
            ));
        }
    }
    lethal.sort_by_key(|(mana_value, life_paid, count, mask, _)| {
        (*mana_value, *life_paid, *count, *mask)
    });
    lethal
        .into_iter()
        .map(|(.., combination)| combination)
        .collect()
}

/// Every affordable combination of `sources`, for responses to work already on
/// the stack whose damage this pricing does not read (CR 608.2h): nothing here
/// is filtered on reaching the opponent's life or on the AI surviving, since
/// the pending work moves both and only the reducer's simulation reads where
/// they end up. Most damage first, then least mana, fewest cards.
///
/// At most one source scales with X, which takes the most the rest of the
/// combination leaves over (CR 107.3a).
pub(super) fn response_combinations(sources: &[PricedSource], mana: u32) -> Vec<Combination> {
    let searched = searched_indices(sources, mana);
    let mut affordable: Vec<(u32, u32, u32, u32, Combination)> = Vec::new();
    for mask in 1u32..(1u32 << searched.len()) {
        let members: Vec<usize> = searched
            .iter()
            .enumerate()
            .filter(|(bit, _)| mask & (1 << bit) != 0)
            .map(|(_, &index)| index)
            .collect();
        let mut scaling = members
            .iter()
            .filter(|&&index| sources[index].loss.opponent.per_x > 0);
        let x_member = scaling.next();
        if scaling.next().is_some() {
            continue;
        }
        let mana_value: u32 = members
            .iter()
            .map(|&index| sources[index].mana.mana_value())
            .sum();
        if mana_value > mana {
            continue;
        }
        let x = x_member.map(|&index| (mana - mana_value) / sources[index].x_shards().max(1));
        let damage: u32 = members
            .iter()
            .map(|&index| sources[index].loss.opponent.at(x.unwrap_or(0)))
            .sum();
        affordable.push((
            damage,
            mana_value,
            members.len() as u32,
            mask,
            Combination { members, x },
        ));
    }
    affordable.sort_by_key(|(damage, mana_value, count, mask, _)| {
        (std::cmp::Reverse(*damage), *mana_value, *count, *mask)
    });
    affordable
        .into_iter()
        .map(|(.., combination)| combination)
        .collect()
}

/// The least X at which `damage` reaches `life`, or `None` when no X does.
fn least_lethal_x(damage: Linear, life: u32) -> Option<u32> {
    match life.checked_sub(damage.fixed) {
        None | Some(0) => Some(0),
        Some(_) if damage.per_x == 0 => None,
        Some(missing) => Some(missing.div_ceil(damage.per_x)),
    }
}

/// The order a combination is played in: each source's first step by
/// [`super::sources::StepOrder`], then the follow-up activations of permanents
/// those steps cast, with an X-scaling source last of all so it absorbs every
/// mana the rest leave over. The X-scaling source's steps carry the X the
/// combination announces.
pub(super) fn line_steps(sources: &[PricedSource], combination: &Combination) -> Vec<LineStep> {
    let mut ordered: Vec<&PricedSource> = combination
        .members
        .iter()
        .map(|&index| &sources[index])
        .collect();
    ordered.sort_by_key(|source| source.order);
    let (scaling, fixed): (Vec<&PricedSource>, Vec<&PricedSource>) = ordered
        .into_iter()
        .partition(|source| source.loss.opponent.per_x > 0);
    let first_steps = fixed
        .iter()
        .filter_map(|source| source.steps.first())
        .cloned();
    let follow_ups = fixed
        .iter()
        .flat_map(|source| source.steps.iter().skip(1))
        .cloned();
    let scaling_steps = scaling.iter().flat_map(|source| {
        source.steps.iter().map(|step| LineStep {
            x: combination.x,
            ..step.clone()
        })
    });
    first_steps.chain(follow_ups).chain(scaling_steps).collect()
}

/// The sources the subset search covers: all of them when few, otherwise the
/// most damage per mana.
fn searched_indices(sources: &[PricedSource], mana: u32) -> Vec<usize> {
    let mut indices: Vec<usize> = (0..sources.len()).collect();
    if indices.len() > MAX_SEARCHED_SOURCES {
        indices.sort_by(|&left, &right| {
            let efficiency = |index: usize| {
                let source = &sources[index];
                let damage = source.loss.opponent.at(mana) as f64;
                damage / (source.mana.mana_value() + 1) as f64
            };
            efficiency(right)
                .partial_cmp(&efficiency(left))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.cmp(&right))
        });
        indices.truncate(MAX_SEARCHED_SOURCES);
        indices.sort_unstable();
    }
    indices
}
