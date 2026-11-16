//! Explains a finished [`BracketEstimate`]: the reasons that hold the deck at its
//! estimated tier ("Why") and a ladder of steps down to the base floor ("Bring it
//! down"), each reason carrying the typed cuts that clear it.
//!
//! The deck builder's bracket panel renders this verbatim. Every grouping, count and
//! cut is decided here, because the frontend is a display layer.
//!
//! Steps describe the deck as it is. They do not simulate the cuts of the steps above
//! them, so one card can appear in more than one step.
//!
//! Pure: no game state, no I/O, no randomness. Same estimate → identical explanation.
//!
//! Bracket policy is **not** part of the Comprehensive Rules — it is WotC's
//! Commander Format Panel guidance. No Comprehensive Rules annotations apply.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::database::{CardDatabase, ComboMatch, ComboOutcome, ComboPiece};
use crate::game::bracket_estimate::{
    cards_until_cleared, combo_contributions, combo_declaration_rule, AxisReading, BracketAxis,
    BracketCheck, BracketCheckOutcome, BracketEstimate, ComboCheck, ComboDeclaration,
    ComboFloorTrigger, CommanderBracketTier, BASE_FLOOR,
};
use crate::types::ability::Comparator;

/// A card the explanation names, tagged with where it sits in the deck.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BracketCard {
    pub name: String,
    pub role: BracketCardRole,
}

/// Where a named card sits. The commander is fixed: a cut never names one while the
/// deck's other cards can clear the reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BracketCardRole {
    Commander,
    Deck,
}

/// A matched two-card combo, with its pieces tagged by role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplainedCombo {
    pub pieces: [BracketCard; 2],
    pub outcomes: BTreeSet<ComboOutcome>,
    pub assemble_cost: u16,
}

/// What raised a reason's floor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BracketReasonSource {
    /// A `FLOOR_RULES` row fired on a card count.
    CardRule {
        axis: BracketAxis,
        comparator: Comparator,
        threshold: u8,
        observed: u8,
        /// The tier just below `floor`, which this rule rules out.
        tier_below: CommanderBracketTier,
        /// The most `tier_below` permits: `observed - cards_until_cleared`.
        allowed_below: u8,
        /// Cards counted by name, in deck order (commander first).
        cards: Vec<BracketCard>,
        /// Combo pairs counted on this axis (`combo_contributions`).
        combos: Vec<ExplainedCombo>,
    },
    /// A `COMBO_FLOOR_RULES` row fired. In `why`, every combo it fired on. In `steps`, only
    /// the combos no earlier reason shows, so each combo appears at one step: the highest
    /// that holds it.
    ComboFloor {
        trigger: ComboFloorTrigger,
        combos: Vec<ExplainedCombo>,
    },
    /// The owner's own answer to the two-card-combo barometer.
    OwnerComboAnswer { declaration: ComboDeclaration },
}

/// One thing holding the deck at `floor`: its source, the official line it rests on,
/// and the cuts that clear it. Wire-visible, so every provenance field is owned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BracketReason {
    pub floor: CommanderBracketTier,
    pub source: BracketReasonSource,
    /// The official sentence the rule encodes, quoted verbatim.
    pub official_line: String,
    pub source_document: String,
    /// ISO-8601.
    pub source_published: String,
    pub source_url: String,
    /// Together with the cuts of the other reasons at its step and at the steps above,
    /// enough to clear this reason.
    pub cuts: Vec<BracketCut>,
}

/// One change toward clearing its reason; see `BracketReason::cuts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BracketCut {
    /// Remove `count` of `from`. `from` holds deck cards only; `count == from.len()` means all.
    RemoveCards { count: u8, from: Vec<BracketCard> },
    /// Break `combo` by removing any one of `remove_one_of` (deck cards only, never empty).
    BreakCombo {
        combo: ExplainedCombo,
        remove_one_of: Vec<BracketCard>,
    },
    /// No deck-card cut is enough: `count` of the commanders in `of` (never empty) must
    /// change. A combo whose pieces are both commanders is cleared this way.
    ReplaceCommander { count: u8, of: Vec<BracketCard> },
    /// The reason is the owner's combo answer.
    ChangeComboAnswer,
}

/// One rung of the ladder down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BracketLadderStep {
    /// The tier the deck lands on once this step and every step above it are done.
    pub lands_on: CommanderBracketTier,
    pub reasons: Vec<BracketReason>,
}

/// Why the deck sits at its estimated tier, and the ladder of steps down.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BracketExplanation {
    /// Every reason whose floor is the estimated tier, each listing every combo it fired
    /// on. At the base floor: the base-floor rules that fired (they rule out declaring
    /// Exhibition), possibly none.
    pub why: Vec<BracketReason>,
    /// Highest first; empty exactly when the estimate is at the base floor. Each combo
    /// appears at one step only, so a combo floor in the first step can list fewer combos
    /// than it does in `why`, or be left out.
    pub steps: Vec<BracketLadderStep>,
}

/// Tags card names with their [`BracketCardRole`] against the deck's commanders.
pub(crate) struct CardRoles<'a> {
    /// The commanders' `CardDatabase::lookup_key`s (`ResolvedComboDeck::commanders`).
    commanders: &'a BTreeSet<String>,
    db: &'a CardDatabase,
}

impl<'a> CardRoles<'a> {
    pub(crate) fn new(commanders: &'a BTreeSet<String>, db: &'a CardDatabase) -> Self {
        Self { commanders, db }
    }

    /// The card `name` names: a commander when its lookup key is a commander's.
    pub(crate) fn card(&self, name: &str) -> BracketCard {
        BracketCard {
            name: name.to_owned(),
            role: self.role_of(&self.db.lookup_key(name)),
        }
    }

    /// A combo piece, named as the table prints it: a commander when its key is a commander's.
    pub(crate) fn piece(&self, piece: &ComboPiece) -> BracketCard {
        BracketCard {
            name: piece.display.clone(),
            role: self.role_of(&piece.key),
        }
    }

    /// A matched combo, with its pieces tagged by role.
    fn combo(&self, combo: &ComboMatch) -> ExplainedCombo {
        ExplainedCombo {
            pieces: combo.pieces.each_ref().map(|piece| self.piece(piece)),
            outcomes: combo.outcomes.clone(),
            assemble_cost: combo.assemble_cost,
        }
    }

    fn role_of(&self, key: &str) -> BracketCardRole {
        if self.commanders.contains(key) {
            BracketCardRole::Commander
        } else {
            BracketCardRole::Deck
        }
    }
}

/// Explains `estimate`. `card_readings` are its axis readings before combo pairs were
/// added, so a reason lists the cards counted by name apart from the pairs.
///
/// Every fired card rule, every fired combo floor and the owner's combo answer become
/// reasons; within a tier, card rules come first in `checks` order, then combo floors in
/// `combo_checks` order, then the owner's answer.
///
/// `why` lists every reason whose floor is the estimated tier, each with every combo it
/// fired on. At the base floor these are the base-floor reasons, possibly none.
///
/// The ladder groups the reasons above the base floor by tier, highest first, and shows
/// each combo once, at the highest tier that holds it. A card rule keeps every pair it
/// counted, because they count toward its `observed`. A combo floor lists only the
/// combos no earlier reason shows, and a floor left with none is not a step's reason. So
/// a pair that both a card rule and a combo floor at the estimated tier count is under
/// both reasons in `why`, and under the card rule alone in the first step.
///
/// Cuts are sufficient per step: once a step's cuts and those of every step above it are
/// done, the deck lands on that step's `lands_on`. One reason's cuts need not clear it on
/// their own: a combo floor whose pairs a card rule at its tier partly shows keeps firing
/// until that rule's cut is done. Cuts never name a commander while deck cards or pairs
/// suffice.
pub(crate) fn explain(
    estimate: &BracketEstimate,
    card_readings: &BTreeMap<BracketAxis, AxisReading>,
    roles: &CardRoles<'_>,
) -> BracketExplanation {
    let card_rules = estimate
        .checks
        .iter()
        .filter(|check| check.outcome == BracketCheckOutcome::Fired)
        .map(FiredSource::CardRule);
    let combo_floors = estimate
        .combo_checks
        .iter()
        .filter(|check| check.outcome == BracketCheckOutcome::Fired)
        .map(FiredSource::ComboFloor);
    let owner_answer = owner_combo_answer_reason(estimate.combo_barometer.declaration)
        .map(FiredSource::OwnerComboAnswer);
    let mut sources: Vec<FiredSource<'_>> =
        card_rules.chain(combo_floors).chain(owner_answer).collect();
    // Highest floor first, so a combo is shown at the highest tier that holds it. The sort
    // is stable: within a tier, sources keep the order above.
    sources.sort_by_key(|source| Reverse(source.floor().as_u8()));

    let why = sources
        .iter()
        .filter(|source| source.floor() == estimate.tier)
        .map(|source| source.reason(&source.combos(estimate), card_readings, roles))
        .collect();

    let mut shown: BTreeSet<[&str; 2]> = BTreeSet::new();
    let ladder_reasons = sources.iter().filter_map(|source| {
        let combos = source.combos(estimate);
        let listed = match source {
            // A combo floor at this tier or below leaves out the pairs a card rule shows,
            // so "cuts are sufficient per step" holds only while that rule's cuts break
            // every pair it lists. Today they do: the one rule that counts pairs, mass
            // land denial at >= 1, clears only at zero, so its cuts take its whole count,
            // every pair included. A pair-counted rule that could clear with pairs left
            // would strand them; the `debug_assert!` below catches that.
            FiredSource::CardRule(_) | FiredSource::OwnerComboAnswer(_) => {
                shown.extend(combos.iter().map(|combo| combo_identity(combo)));
                combos
            }
            FiredSource::ComboFloor(_) => {
                let unshown: Vec<&ComboMatch> = combos
                    .into_iter()
                    .filter(|combo| shown.insert(combo_identity(combo)))
                    .collect();
                if unshown.is_empty() {
                    return None;
                }
                unshown
            }
        };
        let reason = source.reason(&listed, card_readings, roles);
        debug_assert!(
            breaks_every_listed_pair(&reason),
            "a ladder reason's pairs are marked shown, so its own cuts must break them: {reason:?}"
        );
        Some(reason)
    });
    let steps = ladder(estimate.tier, group_by_tier(ladder_reasons));

    BracketExplanation { why, steps }
}

/// A fired source, before it becomes a reason.
enum FiredSource<'a> {
    /// Lists every pair it counted, since they count toward its `observed`.
    CardRule(&'a BracketCheck),
    /// In the ladder, lists only the combos no earlier reason shows.
    ComboFloor(&'a ComboCheck),
    /// Names no combo, so its reason is final.
    OwnerComboAnswer(BracketReason),
}

impl<'a> FiredSource<'a> {
    fn floor(&self) -> CommanderBracketTier {
        match self {
            Self::CardRule(check) => check.floor,
            Self::ComboFloor(check) => check.floor,
            Self::OwnerComboAnswer(reason) => reason.floor,
        }
    }

    /// Every combo this source fired on: the pairs a card rule counted, or a combo
    /// floor's evidence.
    fn combos(&self, estimate: &'a BracketEstimate) -> Vec<&'a ComboMatch> {
        match self {
            Self::CardRule(check) => combo_contributions(&estimate.combos, check.axis),
            Self::ComboFloor(check) => check.evidence.iter().collect(),
            Self::OwnerComboAnswer(_) => Vec::new(),
        }
    }

    /// This source's reason, listing `combos`: all of [`Self::combos`] for a card rule,
    /// whose count includes them, and any non-empty subset of them for a combo floor.
    fn reason(
        &self,
        combos: &[&ComboMatch],
        card_readings: &BTreeMap<BracketAxis, AxisReading>,
        roles: &CardRoles<'_>,
    ) -> BracketReason {
        match self {
            Self::CardRule(check) => card_rule_reason(check, card_readings, combos, roles),
            Self::ComboFloor(check) => combo_floor_reason(check, combos, roles),
            Self::OwnerComboAnswer(reason) => reason.clone(),
        }
    }
}

/// Whether `reason`'s cuts include the cut that breaks each pair it lists.
fn breaks_every_listed_pair(reason: &BracketReason) -> bool {
    let listed: &[ExplainedCombo] = match &reason.source {
        BracketReasonSource::CardRule { combos, .. }
        | BracketReasonSource::ComboFloor { combos, .. } => combos,
        BracketReasonSource::OwnerComboAnswer { .. } => &[],
    };
    listed
        .iter()
        .all(|combo| reason.cuts.contains(&combo_cut(combo.clone())))
}

/// A combo's identity: its two piece keys, sorted, so either order names the same pair.
fn combo_identity(combo: &ComboMatch) -> [&str; 2] {
    let mut keys = combo.pieces.each_ref().map(|piece| piece.key.as_str());
    keys.sort_unstable();
    keys
}

/// A fired card rule as a reason: the cards and combo pairs it counted, the most the
/// tier below allows, and the fewest removals that clear it.
fn card_rule_reason(
    check: &BracketCheck,
    card_readings: &BTreeMap<BracketAxis, AxisReading>,
    combos: &[&ComboMatch],
    roles: &CardRoles<'_>,
) -> BracketReason {
    // Both invariants hold for every `FLOOR_RULES` row; the bracket_estimate test
    // `every_floor_rule_has_a_tier_below_and_clears_by_removing_cards` pins them.
    let need = cards_until_cleared(check.comparator, check.threshold, check.observed)
        .expect("every fired floor rule clears by removing cards");
    let tier_below = check
        .floor
        .below()
        .expect("every fired floor is at least Core, which has a tier below it");
    let cards: Vec<BracketCard> = card_readings
        .get(&check.axis)
        .map_or_else(Vec::new, |reading| {
            reading
                .contributing
                .iter()
                .map(|name| roles.card(name))
                .collect()
        });
    let combos: Vec<ExplainedCombo> = combos.iter().map(|combo| roles.combo(combo)).collect();
    let cuts = card_rule_cuts(&cards, &combos, need);
    BracketReason {
        floor: check.floor,
        source: BracketReasonSource::CardRule {
            axis: check.axis,
            comparator: check.comparator,
            threshold: check.threshold,
            observed: check.observed,
            tier_below,
            allowed_below: check.observed - need,
            cards,
            combos,
        },
        official_line: check.official_line.clone(),
        source_document: check.source_document.clone(),
        source_published: check.source_published.clone(),
        source_url: check.source_url.clone(),
        cuts,
    }
}

/// The cuts that take `need` off a card rule's count of `cards` and `combos`: deck cards
/// first, then pairs with a deck piece, then commander-only pairs, each in order, and
/// commanders only for what those cannot cover.
fn card_rule_cuts(cards: &[BracketCard], combos: &[ExplainedCombo], need: u8) -> Vec<BracketCut> {
    let mut deck_cards = Vec::new();
    let mut commanders = Vec::new();
    for card in cards {
        match card.role {
            BracketCardRole::Deck => deck_cards.push(card.clone()),
            BracketCardRole::Commander => commanders.push(card.clone()),
        }
    }

    let mut cuts = Vec::new();
    let removed = need.min(u8::try_from(deck_cards.len()).unwrap_or(u8::MAX));
    if removed > 0 {
        cuts.push(BracketCut::RemoveCards {
            count: removed,
            from: deck_cards,
        });
    }
    let broken = (need - removed).min(u8::try_from(combos.len()).unwrap_or(u8::MAX));
    // A pair with a deck piece breaks without touching a commander, so those go first.
    let (breakable, commander_only): (Vec<&ExplainedCombo>, Vec<&ExplainedCombo>) =
        combos.iter().partition(|combo| {
            combo
                .pieces
                .iter()
                .any(|piece| piece.role == BracketCardRole::Deck)
        });
    cuts.extend(
        breakable
            .into_iter()
            .chain(commander_only)
            .take(usize::from(broken))
            .cloned()
            .map(combo_cut),
    );
    let remaining = need - removed - broken;
    if remaining > 0 {
        debug_assert!(
            !commanders.is_empty(),
            "a rule's count is its cards plus its pairs, so commanders cover what is left"
        );
        cuts.push(BracketCut::ReplaceCommander {
            count: remaining,
            of: commanders,
        });
    }
    cuts
}

/// A fired combo floor as a reason: the combos it shows, and one cut per combo.
fn combo_floor_reason(
    check: &ComboCheck,
    combos: &[&ComboMatch],
    roles: &CardRoles<'_>,
) -> BracketReason {
    let combos: Vec<ExplainedCombo> = combos.iter().map(|combo| roles.combo(combo)).collect();
    let cuts = combos.iter().cloned().map(combo_cut).collect();
    BracketReason {
        floor: check.floor,
        source: BracketReasonSource::ComboFloor {
            trigger: check.trigger,
            combos,
        },
        official_line: check.official_line.clone(),
        source_document: check.source_document.clone(),
        source_published: check.source_published.clone(),
        source_url: check.source_url.clone(),
        cuts,
    }
}

/// The cut that breaks `combo`: remove either deck piece, or, when both pieces are
/// commanders, replace one of them.
fn combo_cut(combo: ExplainedCombo) -> BracketCut {
    let deck_pieces: Vec<BracketCard> = combo
        .pieces
        .iter()
        .filter(|piece| piece.role == BracketCardRole::Deck)
        .cloned()
        .collect();
    if deck_pieces.is_empty() {
        BracketCut::ReplaceCommander {
            count: 1,
            of: Vec::from(combo.pieces),
        }
    } else {
        BracketCut::BreakCombo {
            combo,
            remove_one_of: deck_pieces,
        }
    }
}

/// The owner's answer to the combo barometer, cited against the published combo line
/// it falls under. `None` when the owner claimed no combo.
fn owner_combo_answer_reason(declaration: ComboDeclaration) -> Option<BracketReason> {
    combo_declaration_rule(declaration).map(|rule| BracketReason {
        floor: rule.floor,
        source: BracketReasonSource::OwnerComboAnswer { declaration },
        official_line: rule.official_line.to_owned(),
        source_document: rule.source.document.to_owned(),
        source_published: rule.source.published.to_owned(),
        source_url: rule.source.url.to_owned(),
        cuts: vec![BracketCut::ChangeComboAnswer],
    })
}

/// The reasons whose floor is `tier`, in the order they were produced.
struct TierReasons {
    tier: CommanderBracketTier,
    reasons: Vec<BracketReason>,
}

/// Groups reasons by floor, highest tier first, keeping each tier's reasons in the
/// order given. Tiers order through `as_u8()`: `CommanderBracketTier` derives no `Ord`.
fn group_by_tier(reasons: impl IntoIterator<Item = BracketReason>) -> Vec<TierReasons> {
    let mut tiers: BTreeMap<Reverse<u8>, TierReasons> = BTreeMap::new();
    for reason in reasons {
        tiers
            .entry(Reverse(reason.floor.as_u8()))
            .or_insert_with(|| TierReasons {
                tier: reason.floor,
                reasons: Vec::new(),
            })
            .reasons
            .push(reason);
    }
    tiers.into_values().collect()
}

/// Builds the ladder from tier groups ordered highest first: one step per group above
/// the base floor. Each step lands on the next group's tier, and the last lands on the
/// base floor.
fn ladder(estimated: CommanderBracketTier, tiers: Vec<TierReasons>) -> Vec<BracketLadderStep> {
    let above: Vec<TierReasons> = tiers
        .into_iter()
        .filter(|group| group.tier.as_u8() > BASE_FLOOR.as_u8())
        .collect();

    debug_assert_eq!(
        above.first().map_or(BASE_FLOOR, |group| group.tier),
        estimated,
        "the estimated tier is the highest reason's floor, or the base floor with none above it"
    );

    let landings: Vec<CommanderBracketTier> = above
        .iter()
        .skip(1)
        .map(|group| group.tier)
        .chain(std::iter::once(BASE_FLOOR))
        .collect();
    above
        .into_iter()
        .zip(landings)
        .map(|(group, lands_on)| BracketLadderStep {
            lands_on,
            reasons: group.reasons,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(name: &str, role: BracketCardRole) -> BracketCard {
        BracketCard {
            name: name.to_owned(),
            role,
        }
    }

    fn pair(first: BracketCard, second: BracketCard) -> ExplainedCombo {
        ExplainedCombo {
            pieces: [first, second],
            outcomes: BTreeSet::from([ComboOutcome::Wins]),
            assemble_cost: 4,
        }
    }

    #[test]
    fn a_pair_with_a_deck_piece_is_cut_before_a_commander_only_pair() {
        let partner_a = card("Partner A", BracketCardRole::Commander);
        let partner_b = card("Partner B", BracketCardRole::Commander);
        let piece = card("Piece", BracketCardRole::Deck);
        let commander_only = pair(partner_a.clone(), partner_b);
        let breakable = pair(partner_a, piece.clone());

        assert_eq!(
            card_rule_cuts(&[], &[commander_only, breakable.clone()], 1),
            [BracketCut::BreakCombo {
                combo: breakable,
                remove_one_of: vec![piece],
            }]
        );
    }
}
