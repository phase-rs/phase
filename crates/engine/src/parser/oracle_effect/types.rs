use crate::types::ability::MultiTargetSpec;
use crate::types::ability::{
    AbilityCondition, AbilityDefinition, CastingPermission, Duration, Effect, ManaProduction,
    ManaSpendRestriction, PaymentCost, PtValue, QuantityExpr, SearchSelectionConstraint,
    StaticDefinition, TargetFilter, UnlessCost,
};
use crate::types::game_state::DistributionUnit;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;
use crate::types::mana::ManaCost;
use crate::types::player::PlayerCounterKind;
use crate::types::zones::Zone;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedEffectClause {
    pub(super) effect: Effect,
    pub(super) duration: Option<Duration>,
    /// Compound "and" remainder parsed into a sub_ability chain.
    pub(super) sub_ability: Option<Box<AbilityDefinition>>,
    /// CR 601.2d: When set, this effect requires distribution among targets at cast time.
    pub(super) distribute: Option<DistributionUnit>,
    /// CR 115.1d: Multi-target spec for "any number of" / "up to N" / fixed-count targeting.
    pub(super) multi_target: Option<MultiTargetSpec>,
    /// CR 608.2c: Leading conditional guard from "if X, Y" clause structure.
    /// Set when `parse_clause_ast` detects a leading conditional and the condition
    /// text is parseable by the nom condition combinator pipeline.
    pub(super) condition: Option<AbilityCondition>,
    /// CR 608.2c + CR 117.3a: Set when the parsed subject phrase carried a "may"
    /// modal (e.g., "its controller may search their library"). Lowered into
    /// `AbilityDefinition.optional` so the resolver prompts the acting player.
    pub(super) optional: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SubjectApplication {
    pub(super) affected: TargetFilter,
    pub(super) target: Option<TargetFilter>,
    pub(super) multi_target: Option<MultiTargetSpec>,
    pub(super) inherits_parent: bool,
    /// CR 608.2c: Set when the subject phrase includes a "may" modal
    /// (e.g., "its controller may search their library"). Lowered into
    /// `AbilityDefinition.optional` so the resolver treats the sub-ability
    /// as a player choice.
    pub(super) is_optional: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TokenDescription {
    pub(crate) name: String,
    pub(crate) power: Option<crate::types::ability::PtValue>,
    pub(crate) toughness: Option<crate::types::ability::PtValue>,
    pub(crate) types: Vec<String>,
    pub(crate) colors: Vec<ManaColor>,
    pub(crate) keywords: Vec<Keyword>,
    pub(crate) tapped: bool,
    pub(crate) count: QuantityExpr,
    pub(crate) attach_to: Option<TargetFilter>,
    pub(crate) static_abilities: Vec<StaticDefinition>,
    /// CR 508.4: Inline "that's tapped and attacking" clause inside the token
    /// description phrase (e.g., "a 1/1 Goblin creature token that's tapped
    /// and attacking"). Distinct from a trailing "It enters tapped and
    /// attacking" continuation sentence, which is patched onto the preceding
    /// `Effect::Token` by the sequence-level continuation handler.
    pub(crate) enters_attacking: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct AnimationSpec {
    pub(super) power: Option<i32>,
    pub(super) toughness: Option<i32>,
    pub(super) colors: Option<Vec<ManaColor>>,
    pub(super) keywords: Vec<Keyword>,
    pub(super) types: Vec<String>,
    pub(super) remove_all_abilities: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SearchLibraryDetails {
    pub(super) filter: TargetFilter,
    pub(super) count: QuantityExpr,
    pub(super) reveal: bool,
    /// CR 701.23a: When set, search this player's library instead of controller's.
    pub(super) target_player: Option<TargetFilter>,
    /// CR 107.1c + CR 701.23d: "any number of" / "up to N" allow 0..=count picks.
    pub(super) up_to: bool,
    /// CR 608.2c: Printed-text restriction on the chosen set ("with different
    /// names"). Defaults to `None`; set by the parser when the corresponding
    /// suffix is detected.
    pub(super) selection_constraint: SearchSelectionConstraint,
    /// CR 701.23a + CR 107.1: "a X card and a Y card" — additional filters, each
    /// producing its own independent search. The primary filter is `filter`;
    /// each `extra_filters` entry becomes a chained `SearchLibrary` sub-ability.
    /// Empty for the common single-filter case.
    pub(super) extra_filters: Vec<TargetFilter>,
    /// CR 701.23a + CR 701.18a: Destination zone scanned from the imperative
    /// text. Populated only when `extra_filters` is non-empty — used by the
    /// multi-filter lowering to splice a `ChangeZone` between each search in
    /// the chain. Single-filter searches get their destination from the
    /// sequence-level continuation machinery and ignore this field.
    pub(super) multi_destination: Zone,
    /// CR 701.23a: Whether the interleaved `ChangeZone`s in a multi-filter
    /// chain should enter tapped ("put them onto the battlefield tapped").
    pub(super) multi_enter_tapped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SeekDetails {
    pub(super) filter: TargetFilter,
    pub(super) count: QuantityExpr,
    pub(super) destination: Zone,
    pub(super) enter_tapped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ClauseAst {
    Imperative {
        text: String,
    },
    SubjectPredicate {
        subject: Box<SubjectPhraseAst>,
        predicate: Box<PredicateAst>,
    },
    Conditional {
        /// CR 608.2c: Parsed leading "if" guard, when recognized by the condition pipeline.
        condition: Option<AbilityCondition>,
        clause: Box<ClauseAst>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SubjectPhraseAst {
    pub(super) affected: TargetFilter,
    pub(super) target: Option<TargetFilter>,
    pub(super) multi_target: Option<MultiTargetSpec>,
    pub(super) inherits_parent: bool,
    /// CR 608.2c: Propagated from `SubjectApplication.is_optional`.
    pub(super) is_optional: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PredicateAst {
    Continuous {
        effect: Effect,
        duration: Option<Duration>,
        sub_ability: Option<Box<AbilityDefinition>>,
    },
    Become {
        effect: Effect,
        duration: Option<Duration>,
        sub_ability: Option<Box<AbilityDefinition>>,
    },
    Restriction {
        effect: Effect,
        duration: Option<Duration>,
    },
    ImperativeFallback {
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ContinuationAst {
    SearchDestination {
        destination: Zone,
        /// CR 701.23a: When true, the searched card enters the battlefield tapped.
        enter_tapped: bool,
        /// CR 701.23a: When true, the searched card is revealed before it moves.
        reveal: bool,
        /// When true, the found card enters "attached to" the search source.
        /// Adds forward_result on the ChangeZone and chains an Attach sub_ability.
        attach_to_source: bool,
    },
    RevealHandFilter {
        card_filter: TargetFilter,
    },
    ManaRestriction {
        restriction: ManaSpendRestriction,
        grants: Vec<crate::types::mana::ManaSpellGrant>,
    },
    /// CR 106.6: "that spell can't be countered" — adds grants to the preceding
    /// mana effect without a new restriction (the restriction was already parsed).
    ManaGrant {
        grants: Vec<crate::types::mana::ManaSpellGrant>,
    },
    CounterSourceStatic {
        source_static: Box<StaticDefinition>,
    },
    /// "create a ... token and suspect it" → chain Suspect { target: LastCreated }
    SuspectLastCreated,
    /// "The flashback cost is equal to its mana cost." after a flashback grant.
    FlashbackCostEqualsManaCost,
    /// CR 701.19c: "It can't be regenerated" / "They can't be regenerated" — sets
    /// `cant_regenerate: true` on the preceding Destroy/DestroyAll effect.
    CantRegenerate,
    /// "Choose one/N of them" / "An opponent chooses one/N of those cards" after a ChangeZone
    /// to exile → ChooseFromZone { count, zone: Exile, chooser }.
    ChooseFromExile {
        count: u32,
        chooser: crate::types::ability::Chooser,
    },
    /// Clauses like "reveal that card" / "put it into your hand" immediately after a
    /// library-to-hand search continuation are already represented by the intrinsic
    /// SearchDestination + reveal flag and should be absorbed.
    SearchResultClauseHandled,
    /// "Put the rest on the bottom of your library ..." after a tracked-set choice that
    /// already moved chosen cards out of the library. Appends a library-bottom placement
    /// step onto the preceding ChangeZone so the unchosen cards are handled by that chain.
    PutChoiceRemainderOnBottom,
    /// "Put the rest on the bottom/into your graveyard" after Dig/RevealTop —
    /// sets `rest_destination` on the preceding Dig effect. The destination is
    /// parsed from the text (bottom of library, graveyard, hand, etc.).
    PutRest {
        destination: Zone,
    },
    /// CR 701.20e + CR 608.2c: "Put up to N [filter] from among them onto the battlefield/into
    /// your hand" after Dig — patches the Dig's keep_count, filter, destination, and rest_destination.
    ///
    /// `destination: None` is the reveal-only form where the kept cards are
    /// NOT routed to a fixed destination; subsequent sub_abilities route them
    /// by type via `TargetFilter::TrackedSetFiltered` (Zimone's Experiment).
    DigFromAmong {
        count: u32,
        up_to: bool,
        filter: TargetFilter,
        destination: Option<Zone>,
        /// Set when the same clause encodes both kept and rest destinations, e.g.,
        /// "put two of them into your hand and the rest on the bottom of your library".
        /// When None, a subsequent PutRest continuation handles rest_destination.
        rest_destination: Option<Zone>,
    },
    /// CR 508.4 / CR 614.1: "It/The token enters tapped and attacking [that player]"
    /// Absorbs into preceding CopyTokenOf, Token, or ChangeZone by setting
    /// enters_attacking and tapped/enter_tapped flags.
    EntersTappedAttacking,
    /// CR 122.6a: "The token enters with X +1/+1 counters on it, where X is ..."
    /// Absorbs into the preceding Token effect by populating `enter_with_counters`.
    TokenEntersWithCounters {
        counter_type: String,
        count: QuantityExpr,
    },
    /// "After that turn, that player takes an extra turn." after a controlled-turn effect.
    GrantExtraTurnAfterControlledTurn,
    /// CR 701.20a: "Put that card [onto the battlefield / into your hand]" after RevealUntil —
    /// overrides kept_destination on the preceding RevealUntil effect.
    /// When the compound sentence also includes "and the rest [into zone]",
    /// `rest_destination` is extracted from the same clause.
    RevealUntilKept {
        destination: Zone,
        enter_tapped: bool,
        rest_destination: Option<Zone>,
    },
    /// CR 701.20a: "puts those cards into [zone]" after RevealUntil — the entire
    /// revealed pile (the matching card AND everything revealed before it) goes
    /// to the same zone. Distinct from `PutRest`, which only overrides
    /// `rest_destination`. Used by cards like Balustrade Spy, Consuming Aberration,
    /// and Destroy the Evidence where "those cards" refers to all cards revealed
    /// during the RevealUntil resolution, not only the non-matching ones.
    RevealUntilAllToZone {
        destination: Zone,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ImperativeAst {
    Numeric(NumericImperativeAst),
    Targeted(TargetedImperativeAst),
    SearchCreation(SearchCreationImperativeAst),
    HandReveal(HandRevealImperativeAst),
    Choose(ChooseImperativeAst),
    Utility(UtilityImperativeAst),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ImperativeFamilyAst {
    Structured(ImperativeAst),
    CostResource(CostResourceImperativeAst),
    ZoneCounter(ZoneCounterImperativeAst),
    Explore,
    /// CR 702.162a: Connive.
    Connive,
    /// CR 509.1g: Block this turn if able.
    ForceBlock,
    /// CR 701.15a: Goad target creature.
    Goad,
    /// CR 701.12a: Exchange control of two target permanents. Carries a distinct
    /// filter per slot so patterns like "target X you control and target Y an
    /// opponent controls" preserve per-slot legality, while "two target X" reuses
    /// the same filter for both slots.
    ExchangeControl {
        target_a: TargetFilter,
        target_b: TargetFilter,
    },
    /// CR 509.1c: Must be blocked this turn if able.
    MustBeBlocked,
    Investigate,
    /// CR 701.36a: Populate.
    Populate,
    /// CR 701.30: Clash with an opponent.
    Clash,
    /// CR 701.48a: Learn.
    Learn,
    /// CR 701.40a: Manifest the top card(s) of library.
    Manifest {
        count: QuantityExpr,
    },
    /// CR 701.62a: Manifest dread.
    ManifestDread,
    BecomeMonarch,
    /// CR 701.49: "venture into the dungeon"
    VentureIntoDungeon,
    /// CR 701.49d: "venture into the Undercity"
    VentureIntoUndercity,
    /// CR 725: "take the initiative"
    TakeTheInitiative,
    Proliferate,
    /// CR 701.56a: Time travel — add or remove time counters.
    TimeTravel,
    GainKeyword(Effect),
    LoseKeyword(Effect),
    /// CR 104.3a: "[target player] lose(s) the game"
    LoseTheGame,
    /// CR 104.3a: "[you/target player] win(s) the game"
    WinTheGame,
    /// CR 706: Roll a die with N sides.
    RollDie {
        sides: u8,
    },
    /// CR 705: Flip a coin.
    FlipCoin,
    /// CR 705: Flip N coins. `count` is the number of flips; consolidation
    /// passes may attach `win_effect`/`lose_effect` from a following sentence
    /// (e.g., "for each heads …"). Emitted for "flip N coins" / "flip X coins"
    /// where N > 1.
    FlipCoins {
        count: crate::types::ability::QuantityExpr,
    },
    /// CR 705: Flip a coin until you lose a flip.
    FlipCoinUntilLose,
    /// CR 506.4: Remove a creature from combat.
    RemoveFromCombat(TargetFilter),
    Shuffle(ShuffleImperativeAst),
    Put(PutImperativeAst),
    YouMay {
        text: String,
    },
    /// CR 122.1: Give a player counters of a named type (poison, experience, rad, ticket, etc.).
    GivePlayerCounter {
        counter_kind: PlayerCounterKind,
        count: QuantityExpr,
    },
    /// CR 701.41a: Support N — put a +1/+1 counter on each of up to N target creatures.
    /// `is_other` is true on permanents (targets "other" creatures), false on spells.
    Support {
        count: u32,
        is_other: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum NumericImperativeAst {
    Draw {
        count: QuantityExpr,
    },
    GainLife {
        amount: QuantityExpr,
    },
    LoseLife {
        amount: QuantityExpr,
    },
    Pump {
        power: crate::types::ability::PtValue,
        toughness: crate::types::ability::PtValue,
    },
    Scry {
        count: QuantityExpr,
    },
    Surveil {
        count: QuantityExpr,
    },
    Mill {
        count: QuantityExpr,
    },
}

/// Replace a fixed quantity with a for-each quantity, preserving multipliers.
/// Fixed(0) is preserved as-is (zero effect regardless of for-each count).
/// Fixed(1) is replaced directly with the for-each quantity.
/// Fixed(N>1) wraps in Multiply { factor: N, inner: for_each }.
pub(super) fn replace_fixed_quantity(fixed: QuantityExpr, for_each: QuantityExpr) -> QuantityExpr {
    match fixed {
        QuantityExpr::Fixed { value: 0 } => QuantityExpr::Fixed { value: 0 },
        QuantityExpr::Fixed { value } if value > 1 => QuantityExpr::Multiply {
            factor: value,
            inner: Box::new(for_each),
        },
        _ => for_each,
    }
}

impl NumericImperativeAst {
    /// Replace fixed counts/amounts with a dynamic for-each quantity expression.
    /// For draw/life/scry/surveil/mill: a fixed multiplier > 1 wraps the quantity in Multiply.
    /// For pump: each P/T component is converted from Fixed(N) to Quantity(N * for_each).
    pub(super) fn with_for_each_quantity(self, quantity: QuantityExpr) -> Self {
        /// Convert a P/T value from Fixed(N) to Quantity(N * for_each).
        fn pt_to_quantity(pt: PtValue, quantity: &QuantityExpr) -> PtValue {
            match pt {
                PtValue::Fixed(0) => PtValue::Fixed(0),
                PtValue::Fixed(n) if n == 1 || n == -1 => {
                    let q = if n < 0 {
                        QuantityExpr::Multiply {
                            factor: -1,
                            inner: Box::new(quantity.clone()),
                        }
                    } else {
                        quantity.clone()
                    };
                    PtValue::Quantity(q)
                }
                PtValue::Fixed(n) => PtValue::Quantity(QuantityExpr::Multiply {
                    factor: n,
                    inner: Box::new(quantity.clone()),
                }),
                other => other,
            }
        }
        match self {
            Self::Draw { count } => Self::Draw {
                count: replace_fixed_quantity(count, quantity),
            },
            Self::GainLife { amount } => Self::GainLife {
                amount: replace_fixed_quantity(amount, quantity),
            },
            Self::LoseLife { amount } => Self::LoseLife {
                amount: replace_fixed_quantity(amount, quantity),
            },
            Self::Scry { count } => Self::Scry {
                count: replace_fixed_quantity(count, quantity),
            },
            Self::Surveil { count } => Self::Surveil {
                count: replace_fixed_quantity(count, quantity),
            },
            Self::Mill { count } => Self::Mill {
                count: replace_fixed_quantity(count, quantity),
            },
            Self::Pump { power, toughness } => Self::Pump {
                power: pt_to_quantity(power, &quantity),
                toughness: pt_to_quantity(toughness, &quantity),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TargetedImperativeAst {
    Tap {
        target: TargetFilter,
    },
    Untap {
        target: TargetFilter,
    },
    TapAll {
        target: TargetFilter,
    },
    UntapAll {
        target: TargetFilter,
    },
    Sacrifice {
        target: TargetFilter,
        /// CR 701.16a: Number of permanents to sacrifice. Defaults to
        /// `QuantityExpr::Fixed { value: 1 }` for the common "sacrifice a X"
        /// case; "sacrifice N X" / "sacrifice half the permanents they
        /// control" carry the parsed dynamic count.
        count: QuantityExpr,
    },
    Discard {
        count: QuantityExpr,
        /// CR 701.9a: When true, the discard is random.
        random: bool,
        /// CR 701.9b: When true, the player may discard 0..=count cards.
        up_to: bool,
        /// CR 608.2c: "discard N unless you discard a [type]" — type filter for
        /// the alternative 1-card discard.
        unless_filter: Option<TargetFilter>,
        /// CR 701.9a + CR 608.2c: Restricts which cards are legal to discard
        /// (e.g., "discard a creature card" — Dokuchi Silencer). `None` means
        /// any card in the discarding player's hand is legal.
        filter: Option<TargetFilter>,
    },
    /// CR 701.3: Return to hand (bounce).
    Return {
        target: TargetFilter,
    },
    /// CR 400.7 + CR 611.2c: Mass return-to-hand. Mirrors `TapAll`/`UntapAll`
    /// for "return all/each [filter] to their owners' hands" Oracle text.
    /// Lowers to `Effect::BounceAll`, not `Effect::Bounce`, so the runtime
    /// resolver iterates every matching permanent instead of prompting for one.
    ReturnAll {
        target: TargetFilter,
    },
    /// CR 400.7: Return to the battlefield (zone change, not bounce).
    ReturnToBattlefield {
        target: TargetFilter,
        origin: Option<Zone>,
        /// CR 712.2: "return ... transformed" (DFC entering with back face up)
        enter_transformed: bool,
        /// CR 110.2: "under your control" — controller override.
        under_your_control: bool,
        /// CR 614.1: "tapped" — enters tapped.
        enter_tapped: bool,
    },
    /// CR 400.6: Return to a specific non-hand, non-battlefield zone (zone change).
    ReturnToZone {
        target: TargetFilter,
        origin: Option<Zone>,
        destination: Zone,
    },
    Fight {
        target: TargetFilter,
    },
    GainControl {
        target: TargetFilter,
    },
    ControlNextTurn {
        target: TargetFilter,
        grant_extra_turn_after: bool,
    },
    /// Earthbend: animate target land into a creature with haste (emits Earthbend event).
    Earthbend {
        target: TargetFilter,
        power: i32,
        toughness: i32,
    },
    /// Airbend: exile target and grant cast-from-exile permission at specified cost.
    Airbend {
        target: TargetFilter,
        cost: ManaCost,
    },
    /// Proxy for zone-counter family (destroy/exile/put counter) used during
    /// compound splitting to unify targeted and zone-counter parsing.
    ZoneCounterProxy(Box<ZoneCounterImperativeAst>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SearchCreationImperativeAst {
    SearchLibrary {
        filter: TargetFilter,
        count: QuantityExpr,
        reveal: bool,
        /// CR 701.23a: When set, search this player's library instead of controller's.
        target_player: Option<TargetFilter>,
        /// CR 107.1c + CR 701.23d: "any number of" / "up to N" allow 0..=count picks.
        up_to: bool,
        /// CR 608.2c: Printed-text restriction on the chosen set ("with
        /// different names").
        selection_constraint: SearchSelectionConstraint,
        /// CR 701.23a + CR 107.1: Dual/N-way search — "a X card and a Y card".
        /// Each entry is an additional independent library search chained after
        /// the primary `filter`. Empty for the common single-filter case.
        extra_filters: Vec<TargetFilter>,
        /// CR 701.23a + CR 701.18a: Destination zone for each found card in a
        /// multi-filter chain. Ignored when `extra_filters` is empty.
        multi_destination: Zone,
        /// CR 701.23a: "put them onto the battlefield tapped" — enters-tapped
        /// flag for multi-filter chains. Ignored when `extra_filters` is empty.
        multi_enter_tapped: bool,
    },
    Dig {
        count: QuantityExpr,
        /// CR 701.20a vs CR 701.16a: True = revealed (public), false = looked at (private).
        reveal: bool,
    },
    CopyTokenOf {
        target: TargetFilter,
        /// CR 707.2 + CR 702: "except it has [keyword]" — extra keywords granted
        /// to each created copy token. See `Effect::CopyTokenOf::extra_keywords`.
        extra_keywords: Vec<crate::types::keywords::Keyword>,
        /// CR 707.9 + CR 707.2: "except <body>" non-keyword modifications
        /// (e.g., `RemoveSupertype` for Miirym's "isn't legendary"). See
        /// `Effect::CopyTokenOf::additional_modifications`.
        additional_modifications: Vec<crate::types::ability::ContinuousModification>,
    },
    Token {
        token: Box<TokenDescription>,
    },
    /// Alchemy digital-only: seek card(s) from library matching filter.
    Seek {
        filter: TargetFilter,
        count: QuantityExpr,
        destination: Zone,
        enter_tapped: bool,
    },
    /// CR 400.7 + CR 701.23 + CR 701.24: "Search [possessive] graveyard, hand,
    /// and library for any number of cards with that name and exile them."
    /// Lowered to `Effect::ChangeZoneAll` with multi-zone origin
    /// (`InAnyZone[Graveyard, Hand, Library]`) + `SameNameAsParentTarget` filter,
    /// scoped to the owner of the parent target's exiled card. Used by
    /// Deadly Cover-Up.
    MultiZoneSameNameExile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum UtilityImperativeAst {
    Prevent {
        text: String,
    },
    Regenerate {
        text: String,
    },
    Copy {
        target: TargetFilter,
    },
    Transform {
        target: TargetFilter,
    },
    Attach {
        target: TargetFilter,
    },
    /// CR 613.4d: Switch power and toughness.
    SwitchPT {
        target: TargetFilter,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum HandRevealImperativeAst {
    LookAt {
        target: TargetFilter,
    },
    RevealAll,
    /// "reveals a number of cards from their hand equal to X" (CR 701.20a).
    RevealPartial {
        count: crate::types::ability::QuantityExpr,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ChooseImperativeAst {
    TargetOnly {
        target: TargetFilter,
    },
    Reparse {
        text: String,
    },
    NamedChoice {
        choice_type: crate::types::ability::ChoiceType,
    },
    RevealHandFilter {
        card_filter: TargetFilter,
    },
    /// "choose N of them/those [cards]" — anaphoric reference to a previously
    /// revealed/exiled set of cards. Lowered to `Effect::ChooseFromZone`.
    FromTrackedSet {
        count: u32,
        chooser: crate::types::ability::Chooser,
    },
    /// "choose from among the permanents ... an artifact, a creature, ..." —
    /// multi-category selection where each player keeps one per type, then sacrifices the rest.
    /// Lowered to `Effect::ChooseAndSacrificeRest`.
    CategoryAndSacrificeRest {
        categories: Vec<crate::types::card_type::CoreType>,
        chooser_scope: crate::types::ability::CategoryChooserScope,
    },
    /// CR 115.1c + CR 601.2c: "choose target X and target Y" — two independent
    /// target slots declared in a single targeting clause (Goblin Welder shape).
    /// Each `target` becomes its own `Effect::TargetOnly` slot so that the
    /// caster announces both targets at activation time per CR 601.2c. The
    /// later sub_ability sentence ("If both targets are still legal …")
    /// references them via `TargetFilter::ParentTarget` chained through the
    /// sub_ability lattice.
    TwoTargets {
        target_a: TargetFilter,
        target_b: TargetFilter,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PutImperativeAst {
    Mill {
        count: u32,
    },
    ZoneChange {
        origin: Option<Zone>,
        destination: Zone,
        target: TargetFilter,
        /// CR 110.2: "under your control" — controller override on ETB.
        under_your_control: bool,
        /// CR 603.6d: "enters tapped" — enters the battlefield tapped.
        enter_tapped: bool,
        /// CR 508.4: "tapped and attacking [<player_phrase>]" — the moved
        /// object enters the battlefield as an attacking creature (without
        /// having been declared as one). Set by the inline-tail patcher in
        /// `try_parse_put_zone_change` for the Kaalia / Ilharg class.
        enters_attacking: bool,
        /// CR 122.1 + CR 614.1c: Counters granted as the moved object enters
        /// (e.g., "with two additional +1/+1 counters on it"). Each entry is
        /// `(counter_type, count)`.
        enter_with_counters: Vec<(String, QuantityExpr)>,
    },
    TopOfLibrary,
    BottomOfLibrary,
    NthFromTop {
        n: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ShuffleImperativeAst {
    ShuffleLibrary {
        target: TargetFilter,
    },
    ChangeZoneToLibrary,
    ChangeZoneAllToLibrary {
        origin: Zone,
    },
    /// "shuffle target card from {origin} into {owner}'s library" —
    /// targeted zone change + shuffle composition.
    TargetedChangeZoneToLibrary {
        target: TargetFilter,
        origin: Option<Zone>,
    },
    Unimplemented {
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CostResourceImperativeAst {
    ActivateOnlyIfControlsLandSubtypeAny {
        subtypes: Vec<String>,
    },
    Mana {
        produced: ManaProduction,
        restrictions: Vec<ManaSpendRestriction>,
        /// CR 115.1 + CR 115.7: Player target for mana effects whose count
        /// references a target player (e.g. Jeska's Will mode 1 — "Add {R} for
        /// each card in target opponent's hand"). `None` for the common case.
        target: Option<TargetFilter>,
    },
    Damage {
        amount: QuantityExpr,
        target: TargetFilter,
        all: bool,
    },
    /// Passthrough for damage effects that carry additional fields not representable
    /// in the CostResource AST (DamageSource, DamageEachPlayer, etc.).
    /// The Effect is already fully constructed by try_parse_damage.
    DamageEffect(Box<Effect>),
    /// CR 118.1: "pay {cost}" as an effect verb (mana or life).
    Pay {
        cost: PaymentCost,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ZoneCounterImperativeAst {
    Destroy {
        target: TargetFilter,
        all: bool,
    },
    Exile {
        origin: Option<Zone>,
        target: TargetFilter,
        all: bool,
    },
    ExileTop {
        player: TargetFilter,
        count: QuantityExpr,
    },
    Counter {
        target: TargetFilter,
        source_static: Option<Box<StaticDefinition>>,
        unless_payment: Option<UnlessCost>,
        /// CR 701.6 + CR 405.1: When `true`, lower to `Effect::CounterAll`
        /// (mass counter) instead of `Effect::Counter`. Mirrors the
        /// `Destroy { all }` and `Exile { all }` flags above. Triggered by
        /// the "counter all "/"counter each " precheck in `parse_counter_ast`.
        all: bool,
    },
    PutCounter {
        counter_type: String,
        count: QuantityExpr,
        target: TargetFilter,
    },
    /// CR 122.1: "Put a X counter, a Y counter[, and a Z counter] on TARGET" —
    /// a list of typed counters placed on one shared target. Lowered to a
    /// `PutCounter` chain where the first entry carries the resolved target
    /// and each remaining entry uses `TargetFilter::ParentTarget` so the
    /// target is chosen once and reused. Covers Abigale, Unexpected Fangs,
    /// Gift of the Viper, Qarsi Revenant, Nezumi Prowler, Arwen, Champion of
    /// Dusan, Quicksilver.
    PutCounterList {
        entries: Vec<(String, QuantityExpr)>,
        target: TargetFilter,
        multi_target: Option<MultiTargetSpec>,
    },
    /// CR 122.1: "Put counters on each/all" — mass counter placement without targeting.
    PutCounterAll {
        counter_type: String,
        count: QuantityExpr,
        target: TargetFilter,
    },
    RemoveCounter {
        counter_type: String,
        count: i32,
        target: TargetFilter,
    },
    /// CR 122.8: "Put its counters on [target]" — copy all counters from source to target.
    MoveCounters {
        source: TargetFilter,
        counter_type: Option<String>,
        target: TargetFilter,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClauseBoundary {
    Sentence,
    Then,
    Comma,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ClauseChunk {
    pub(super) text: String,
    pub(super) boundary_after: Option<ClauseBoundary>,
}

/// Debug-only assertion that a `parse_target` remainder doesn't contain a compound
/// connector (` and <verb>`). Used as a safety net at call sites that discard
/// remainders — compound detection runs first, so these should never fire for
/// production paths. `and put ...` is exempt because targeted compound actions
/// intentionally preserve that continuation for the higher-level clause parser.
#[cfg(debug_assertions)]
pub(super) fn assert_no_compound_remainder(rem: &str, context: &str) {
    assert!(
        rem.is_empty()
            || !rem.strip_prefix(" and ").is_some_and(|after| {
                let after = after.trim();
                !after.starts_with("put ") && super::sequence::starts_bare_and_clause(after)
            }),
        "silent remainder drop: {rem:?} from: {context:?}"
    );
}

pub(super) fn parsed_clause(effect: Effect) -> ParsedEffectClause {
    ParsedEffectClause {
        effect,
        duration: None,
        sub_ability: None,
        distribute: None,
        multi_target: None,
        condition: None,
        optional: false,
    }
}

pub(super) fn with_clause_duration(
    mut clause: ParsedEffectClause,
    duration: Duration,
) -> ParsedEffectClause {
    // Leading duration from Oracle text (e.g., "Until end of turn, ...") is authoritative —
    // it overrides any default injected by sub-parsers (e.g., build_become_clause's Permanent).
    clause.duration = Some(duration.clone());
    match &mut clause.effect {
        Effect::GenericEffect {
            duration: ref mut effect_duration,
            ..
        } => {
            *effect_duration = Some(duration);
        }
        Effect::GrantCastingPermission {
            permission:
                CastingPermission::PlayFromExile {
                    duration: perm_dur, ..
                },
            ..
        } => {
            *perm_dur = duration;
        }
        _ => {}
    }
    clause
}
