use serde::Serialize;

use crate::types::ability::MultiTargetSpec;
use crate::types::ability::{
    AbilityCondition, AbilityDefinition, ActivationRestriction, BounceSelection, CastingPermission,
    ControllerRef, CounterSourceRider, Duration, Effect, FaceDownProfile, LibraryPosition,
    ManaProduction, ManaSpendRestriction, ModalSelectionConstraint, OutsideGameSourcePool,
    PaymentCost, PlayerFilter, PtStat, PtValue, QuantityExpr, SearchDestinationSplit,
    SearchSelectionConstraint, StaticDefinition, TargetFilter,
};
use crate::types::card_type::Supertype;
use crate::types::counter::CounterType;
use crate::types::game_state::DistributionUnit;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;
use crate::types::mana::ManaCost;
use crate::types::player::PlayerCounterKind;
use crate::types::zones::Zone;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ParsedEffectClause {
    pub(crate) effect: Effect,
    pub(crate) duration: Option<Duration>,
    /// Compound "and" remainder parsed into a sub_ability chain.
    pub(crate) sub_ability: Option<Box<AbilityDefinition>>,
    /// CR 601.2d: When set, this effect requires distribution among targets at cast time.
    pub(crate) distribute: Option<DistributionUnit>,
    /// CR 115.1d: Multi-target spec for "any number of" / "up to N" / fixed-count targeting.
    pub(crate) multi_target: Option<MultiTargetSpec>,
    /// CR 608.2c: Leading conditional guard from "if X, Y" clause structure.
    /// Set when `parse_clause_ast` detects a leading conditional and the condition
    /// text is parseable by the nom condition combinator pipeline.
    pub(crate) condition: Option<AbilityCondition>,
    /// CR 608.2c + CR 117.3a: Set when the parsed subject phrase carried a "may"
    /// modal (e.g., "its controller may search their library"). Lowered into
    /// `AbilityDefinition.optional` so the resolver prompts the acting player.
    pub(crate) optional: bool,
    /// CR 118.12: When set, the parsed effect carries an "unless [player] pays
    /// [cost]" modifier (e.g., "Counter target spell unless its controller
    /// pays {2}"). Lowered into `AbilityDefinition.unless_pay` so the
    /// resolution-time runtime owns the payment choice via the unified
    /// `unless_pay` pipeline (rather than a per-effect bespoke path).
    pub(crate) unless_pay: Option<crate::types::ability::UnlessPayModifier>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SubjectApplication {
    pub(crate) affected: TargetFilter,
    pub(crate) target: Option<TargetFilter>,
    pub(crate) multi_target: Option<MultiTargetSpec>,
    pub(crate) inherits_parent: bool,
    /// CR 608.2c: Set when the subject phrase includes a "may" modal
    /// (e.g., "its controller may search their library"). Lowered into
    /// `AbilityDefinition.optional` so the resolver treats the sub-ability
    /// as a player choice.
    pub(crate) is_optional: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TokenDescription {
    pub(crate) name: String,
    pub(crate) power: Option<crate::types::ability::PtValue>,
    pub(crate) toughness: Option<crate::types::ability::PtValue>,
    pub(crate) types: Vec<String>,
    /// CR 205.4a: Supertypes parsed from the inline token grammar (e.g. the
    /// "legendary" in "a legendary 20/20 black Avatar creature token"). Captured
    /// rather than discarded so legendary/snow tokens (Marit Lage, etc.) carry
    /// their supertype — load-bearing for the legend rule (CR 704.5j).
    pub(crate) supertypes: Vec<Supertype>,
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

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub(crate) struct AnimationSpec {
    pub(crate) power: Option<i32>,
    pub(crate) toughness: Option<i32>,
    pub(crate) dynamic_power: Option<crate::types::ability::QuantityExpr>,
    pub(crate) dynamic_toughness: Option<crate::types::ability::QuantityExpr>,
    pub(crate) colors: Option<Vec<ManaColor>>,
    pub(crate) keywords: Vec<Keyword>,
    pub(crate) types: Vec<String>,
    pub(crate) remove_all_abilities: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SearchLibraryDetails {
    pub(crate) filter: TargetFilter,
    pub(crate) count: QuantityExpr,
    pub(crate) reveal: bool,
    /// CR 701.23a: When set, search this player's library instead of controller's.
    pub(crate) target_player: Option<TargetFilter>,
    /// CR 107.1c + CR 701.23d: "any number of" / "up to N" allow 0..=count picks.
    pub(crate) up_to: bool,
    /// CR 608.2c: Printed-text restriction on the chosen set ("with different
    /// names"). Defaults to `None`; set by the parser when the corresponding
    /// suffix is detected.
    pub(crate) selection_constraint: SearchSelectionConstraint,
    /// CR 115.1c + CR 608.2c: Printed target used only as a reference for
    /// search filters like "with the same name as target creature".
    pub(crate) reference_target: Option<TargetFilter>,
    /// CR 701.23a + CR 107.1: "a X card and a Y card" — additional filters, each
    /// producing its own independent search. The primary filter is `filter`;
    /// each `extra_filters` entry becomes a chained `SearchLibrary` sub-ability.
    /// Empty for the common single-filter case.
    pub(crate) extra_filters: Vec<TargetFilter>,
    /// CR 701.23a + CR 701.18a: Destination zone scanned from the imperative
    /// text. Populated only when `extra_filters` is non-empty — used by the
    /// multi-filter lowering to splice a `ChangeZone` between each search in
    /// the chain. Single-filter searches get their destination from the
    /// sequence-level continuation machinery and ignore this field.
    pub(crate) multi_destination: Zone,
    /// CR 701.23a: Whether the interleaved `ChangeZone`s in a multi-filter
    /// chain should enter tapped ("put them onto the battlefield tapped").
    pub(crate) multi_enter_tapped: bool,
    /// CR 701.23a + CR 608.2c: When set, the found set is partitioned across two
    /// destinations (cultivate-class "put one onto the battlefield tapped and
    /// the other into your hand"). Lowered to `Effect::SearchLibrary.split`.
    pub(crate) split: Option<SearchDestinationSplit>,
    /// CR 701.23a: Zones the search looks through. Defaults to `[Library]`;
    /// God-Pharaoh's-Gift-class cards set `[Graveyard, Hand, Library]`. Lowered
    /// to `Effect::SearchLibrary.source_zones`.
    pub(crate) source_zones: Vec<Zone>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SeekDetails {
    pub(crate) filter: TargetFilter,
    pub(crate) count: QuantityExpr,
    pub(crate) from_top: Option<usize>,
    pub(crate) destination: Zone,
    pub(crate) enter_tapped: bool,
    /// Alchemy digital-only analogue to search multi-filters: "seek a X card
    /// and a Y card" performs one independent seek per filter.
    pub(crate) extra_filters: Vec<TargetFilter>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum ClauseAst {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SubjectPhraseAst {
    pub(crate) affected: TargetFilter,
    pub(crate) target: Option<TargetFilter>,
    pub(crate) multi_target: Option<MultiTargetSpec>,
    pub(crate) inherits_parent: bool,
    /// CR 608.2c: Propagated from `SubjectApplication.is_optional`.
    pub(crate) is_optional: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum PredicateAst {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum ContinuationAst {
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
        card_filter: Option<TargetFilter>,
        choice_optional: bool,
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
    /// CR 701.8: "If a permanent's ability is countered this way, destroy that
    /// permanent." — patches `source_rider = Some(CounterSourceRider::Destroy)`
    /// on the preceding `Effect::Counter` (Teferi's Response, Green Slime).
    CounterSourceRiderDestroy,
    /// CR 707.10c: "You may choose new targets for the copy/copies." after a
    /// CopySpell (possibly wrapped in a CreateDelayedTrigger) — patches
    /// `retarget = MayChooseNewTargets` on the inner Effect::CopySpell.
    CopyMayRetarget,
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
    /// "reveal it" immediately after a SearchLibrary whose destination is handled
    /// by a later conditional branch. Patches SearchLibrary.reveal without adding
    /// a default ChangeZone.
    SearchRevealResult,
    /// "Put the rest on the bottom of your library ..." after a tracked-set choice that
    /// already moved chosen cards out of the library. Appends a library-bottom placement
    /// step onto the preceding ChangeZone so the unchosen cards are handled by that chain.
    PutChoiceRemainderOnBottom,
    /// "Put/shuffle the chosen cards into <zone> and put the rest into <zone>"
    /// after a tracked-set choice. The choice resolver injects chosen cards into
    /// the first continuation and unchosen cards into its immediate sub-ability.
    ChoicePartitionDestinations {
        chosen_destination: Zone,
        rest_destination: Zone,
    },
    /// "Put those cards on top ..." after a search/dig/choice producer.
    /// Count is supplied by the already-selected target set.
    PutChosenCardsAtLibraryPosition { position: LibraryPosition },
    /// CR 702.170c-d: "It/that card/they become plotted" after an exile effect.
    BecomesPlotted,
    /// "Put the rest on the bottom/into your graveyard" after Dig/RevealTop —
    /// sets `rest_destination` on the preceding Dig effect. The destination is
    /// parsed from the text (bottom of library, graveyard, hand, etc.).
    ///
    /// `reorder_all` covers "put them back in any order": all looked-at cards
    /// stay in the library, and the submitted selection order becomes top order.
    PutRest {
        destination: Zone,
        reorder_all: bool,
    },
    /// CR 701.20e + CR 608.2c: "Put up to N [filter] from among them onto the battlefield/into
    /// your hand" after Dig — patches the Dig's keep_count, filter, destination, and rest_destination.
    ///
    /// `destination: None` is the reveal-only form where the kept cards are
    /// NOT routed to a fixed destination; subsequent sub_abilities route them
    /// by type via `TargetFilter::TrackedSetFiltered` (Zimone's Experiment).
    DigFromAmong {
        /// CR 701.20e / CR 701.17c: How many of the from-among set are taken.
        /// `All` is the mass quantifier ("put all creature cards milled this
        /// way ..."); `Up(n)` / `Exactly(n)` are the bounded singular forms.
        quantity: PutCount,
        filter: TargetFilter,
        destination: Option<Zone>,
        /// Set when the same clause encodes both kept and rest destinations, e.g.,
        /// "put two of them into your hand and the rest on the bottom of your library".
        /// When None, a subsequent PutRest continuation handles rest_destination.
        rest_destination: Option<Zone>,
        /// CR 110.2a: Controller override for the kept cards' battlefield entry
        /// ("... onto the battlefield ... under your control"). `None` leaves
        /// them under their owner's control.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enters_under: Option<ControllerRef>,
        /// CR 708.2a + CR 708.3: When `Some`, the kept cards enter the battlefield
        /// face down with these characteristics ("... face down ... They're 2/2
        /// Cyberman artifact creatures."). `None` = normal face-up entry.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        face_down_profile: Option<FaceDownProfile>,
        /// CR 614.1 / CR 110.5b: "onto the battlefield tapped" on the
        /// from-among put-step.
        #[serde(default)]
        enter_tapped: bool,
    },
    /// CR 708.2a + CR 205.1a: "They're N/M [types] [subtypes] creatures." after a
    /// put-face-down clause — refines the preceding face-down move's profile.
    FaceDownProfileSpec { profile: FaceDownProfile },
    /// CR 508.4 / CR 614.1: "It/The token enters tapped and attacking [that player]"
    /// Absorbs into preceding CopyTokenOf, Token, or ChangeZone by setting
    /// enters_attacking and tapped/enter_tapped flags.
    EntersTappedAttacking,
    /// CR 122.6a: "The token enters with X +1/+1 counters on it, where X is ..."
    /// Absorbs into the preceding Token effect by populating `enter_with_counters`.
    TokenEntersWithCounters {
        counter_type: CounterType,
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
        /// CR 508.4: the kept card enters the battlefield attacking
        /// ("tapped and attacking"). Absorbs into `enters_attacking`.
        enters_attacking: bool,
        rest_destination: Option<Zone>,
        /// CR 701.20a + CR 608.2c: `Some(decline_zone)` when the kept clause is
        /// optional ("you may put that card onto the battlefield"). `destination`
        /// is then the accept zone and `decline_zone` is where the kept card
        /// goes if the controller declines (the explicit "if you don't, put it
        /// into your hand" zone, or the bottom-of-library rest pile by default).
        /// `None` → mandatory kept destination (absorbs into `kept_destination`).
        optional_decline: Option<Zone>,
    },
    /// CR 701.20a: "puts those cards into [zone]" after RevealUntil — the entire
    /// revealed pile (the matching card AND everything revealed before it) goes
    /// to the same zone. Distinct from `PutRest`, which only overrides
    /// `rest_destination`. Used by cards like Balustrade Spy, Consuming Aberration,
    /// and Destroy the Evidence where "those cards" refers to all cards revealed
    /// during the RevealUntil resolution, not only the non-matching ones.
    RevealUntilAllToZone { destination: Zone },
    /// CR 406.3 + CR 701.16a: "[then] exile it/them [face down]" after a private
    /// `Dig` (the "look at the top N cards of <player>'s library" look step).
    /// Rewrites the preceding `Dig` into an `Effect::ExileTop` so the looked-at
    /// card(s) actually leave the library — the Gonti, Canny Acquisitor impulse
    /// idiom ("look at the top card of that player's library, then exile it face
    /// down. You may play that card ..."). `player`/`count` are lifted from the
    /// `Dig` (with `ParentTarget` re-bound to the triggering player via
    /// `that_player_library_filter`); `face_down` reflects the explicit
    /// hidden-information suffix.
    ExileLookedAtCard {
        player: TargetFilter,
        count: QuantityExpr,
        face_down: bool,
    },
    /// CR 608.2c + CR 701.21a: absorbs the explicit/bare sacrifice-rest clause
    /// following a choose-and-sacrifice-rest effect, optionally narrowing the
    /// final sacrifice sweep ("all other nonland permanents they control").
    ChooseAndSacrificeRestFilter {
        sacrifice_filter: Option<TargetFilter>,
    },
}

/// CR 701.20e / CR 701.17c: How many cards a "from among [set]" continuation
/// takes. `All` is the mass quantifier ("put all creature cards milled this
/// way ...") that lowers to a `ChangeZoneAll`; `AnyNumber` is an unbounded
/// player choice ("put any number of ..."), and the bounded forms lower to a
/// singular `ChangeZone` (`Up` → up_to, `Exactly` → fixed count).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum PutCount {
    All,
    AnyNumber,
    Up(u32),
    Exactly(u32),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum ImperativeAst {
    Numeric(NumericImperativeAst),
    Targeted(TargetedImperativeAst),
    SearchCreation(SearchCreationImperativeAst),
    HandReveal(HandRevealImperativeAst),
    Choose(ChooseImperativeAst),
    Utility(UtilityImperativeAst),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum ImperativeFamilyAst {
    Structured(ImperativeAst),
    CostResource(CostResourceImperativeAst),
    ZoneCounter(ZoneCounterImperativeAst),
    Explore,
    /// CR 702.162a: Connive.
    Connive,
    /// CR 509.1g: Block this turn if able.
    ForceBlock,
    /// CR 508.1d: Attack a required player this turn/combat if able. The
    /// `required_player` filter selects whom the forced attacker must attack —
    /// `TargetFilter::Controller` for "attacks you", or
    /// `ControllerRef::ChosenPlayer { index }` for "attacks that player" (the
    /// opponent chosen by a preceding "choose an opponent" instruction in the
    /// same resolution, e.g. Ruhan of the Fomori).
    ForceAttack {
        duration: Duration,
        required_player: TargetFilter,
    },
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
    /// CR 701.12a: Exchange a player's life total with the source's power or
    /// toughness (Tree of Perdition, Tree of Redemption, Evra). `player` is the
    /// player whose life is exchanged (`Controller` for "your", an opponent
    /// filter for "target opponent's"); `stat` selects which source stat.
    ExchangeLifeWithStat {
        player: TargetFilter,
        stat: PtStat,
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
        target: TargetFilter,
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
    /// CR 701.51b: "open N Attractions"
    OpenAttractions {
        count: u32,
    },
    /// CR 701.52: "roll to visit your Attractions"
    RollToVisitAttractions,
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
    /// CR 706.1: `count` is how many dice of this kind to roll ("roll two
    /// six-sided dice", "roll X d12"). Emitted for the multi-dice form;
    /// the single-die path lowers with `count = Fixed(1)`.
    /// CR 706.2: Optional additive/subtractive modifier applied to the natural
    /// result before result-table lookup ("Roll a d20 and add the number of
    /// cards in your hand").
    RollDie {
        count: crate::types::ability::QuantityExpr,
        sides: u8,
        modifier: Option<crate::types::ability::DieRollModifier>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum NumericImperativeAst {
    Draw {
        count: QuantityExpr,
        /// CR 121.1 + CR 608.2d: "Draw up to N cards" — drawing player picks
        /// any 0..count. Mirrors NumericImperativeAst::Sacrifice's up_to.
        up_to: bool,
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
pub(crate) fn replace_fixed_quantity(fixed: QuantityExpr, for_each: QuantityExpr) -> QuantityExpr {
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
    pub(crate) fn with_for_each_quantity(self, quantity: QuantityExpr) -> Self {
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
            Self::Draw { count, up_to } => Self::Draw {
                count: replace_fixed_quantity(count, quantity),
                up_to,
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

impl TargetedImperativeAst {
    /// Replace fixed counts with a dynamic for-each quantity expression.
    /// Targeted action verbs keep their parsed target/filter data; only count
    /// fields that represent "N objects/cards" are rewritten.
    pub(crate) fn with_for_each_quantity(self, quantity: QuantityExpr) -> Self {
        match self {
            Self::Sacrifice {
                target,
                count,
                min_count,
            } => Self::Sacrifice {
                target,
                count: replace_fixed_quantity(count, quantity),
                min_count,
            },
            Self::Discard {
                count,
                random,
                up_to,
                unless_filter,
                filter,
            } => Self::Discard {
                count: replace_fixed_quantity(count, quantity),
                random,
                up_to,
                unless_filter,
                filter,
            },
            other => other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum TargetedImperativeAst {
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
    Goad {
        target: TargetFilter,
    },
    GoadAll {
        target: TargetFilter,
    },
    Sacrifice {
        target: TargetFilter,
        /// CR 701.16a: Number of permanents to sacrifice. Defaults to
        /// `QuantityExpr::Fixed { value: 1 }` for the common "sacrifice a X"
        /// case; "sacrifice N X" / "sacrifice half the permanents they
        /// control" carry the parsed dynamic count.
        count: QuantityExpr,
        /// Minimum number of permanents the player must choose when `count` is
        /// an up-to/ranged quantity. Used for "one or more" choices.
        min_count: usize,
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
    /// CR 701.9a: Back-reference discard — "discard that card" / "discard those
    /// cards" — discards a specific card identified by the parent effect's
    /// affected IDs (Seek, Conjure, Reveal-Choose). Distinct from `Discard`
    /// which is player-choice-from-hand. Lowers to `Effect::DiscardCard`.
    DiscardCard {
        target: TargetFilter,
    },
    /// CR 701.3: Return to hand (bounce).
    Return {
        target: TargetFilter,
        /// CR 115.1 + Whitemane Lion ruling: Captured at parse time from the
        /// `TargetSyntax` discriminator. `Descriptor` Oracle text without
        /// "target" (e.g. "return a creature you control to its owner's hand")
        /// becomes `BounceSelection::AtResolution`; the resolver picks the
        /// eligible permanent at resolution via `EffectZoneChoice` rather than
        /// the targeting pipeline.
        selection: BounceSelection,
    },
    /// CR 400.7 + CR 611.2c: Mass return-to-hand. Mirrors `TapAll`/`UntapAll`
    /// for "return all/each [filter] to their owners' hands" Oracle text.
    /// Lowers to `Effect::BounceAll`, not `Effect::Bounce`, so the runtime
    /// resolver iterates every matching permanent instead of prompting for one.
    ReturnAll {
        target: TargetFilter,
        /// CR 107.1a + CR 608.2d: Optional counted subset for phrases such as
        /// "return half the creatures they control, rounded up." `None`
        /// preserves all/each mass-bounce semantics.
        count: Option<QuantityExpr>,
    },
    /// CR 400.7: Return to the battlefield (zone change, not bounce).
    ReturnToBattlefield {
        target: TargetFilter,
        origin: Option<Zone>,
        /// CR 712.2: "return ... transformed" (DFC entering with back face up)
        enter_transformed: bool,
        /// CR 110.2a: Controller override on ETB. `Some(ref)` routes the object
        /// to the player resolved from `ref`. `None` leaves the object under
        /// its owner's control. Lowered 1:1 onto `Effect::ChangeZone.enters_under`.
        enters_under: Option<ControllerRef>,
        /// CR 614.1: "tapped" — enters tapped.
        enter_tapped: bool,
        /// CR 508.4: "tapped and attacking" — enters attacking.
        enters_attacking: bool,
        /// CR 122.1 + CR 122.6: Counters placed on the returned object as it
        /// enters the battlefield.
        enter_with_counters: Vec<(CounterType, QuantityExpr)>,
    },
    /// CR 400.6: Return to a specific non-hand, non-battlefield zone (zone change).
    ReturnToZone {
        target: TargetFilter,
        origin: Option<Zone>,
        destination: Zone,
    },
    /// CR 400.7 + CR 608.2c: Mass return to a non-default zone. Lowers to
    /// `ChangeZoneAll` so the resolver scans every matching object instead of
    /// requiring player target slots.
    ReturnAllToZone {
        target: TargetFilter,
        origin: Option<Zone>,
        destination: Zone,
        /// CR 110.2a: Controller override for mass returns to the battlefield.
        /// `None` preserves default controller assignment.
        enters_under: Option<ControllerRef>,
        enter_tapped: bool,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum SearchCreationImperativeAst {
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
        /// CR 115.1c + CR 608.2c: Printed target used only as a reference for
        /// search filters like "with the same name as target creature".
        reference_target: Option<TargetFilter>,
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
        /// CR 701.23a + CR 608.2c: cultivate-class split destination ("put one
        /// onto the battlefield tapped and the other into your hand"). Lowered
        /// to `Effect::SearchLibrary.split`.
        split: Option<SearchDestinationSplit>,
        /// CR 701.23a: Zones searched. `[Library]` for ordinary tutors;
        /// `[Graveyard, Hand, Library]` for God-Pharaoh's-Gift-class cards.
        source_zones: Vec<Zone>,
    },
    SearchOutsideGame {
        filter: TargetFilter,
        count: QuantityExpr,
        reveal: bool,
        destination: Zone,
        up_to: bool,
        /// CR 400.11 + CR 406.3: Which source pool the outside-game search uses.
        source_pool: OutsideGameSourcePool,
    },
    Dig {
        count: QuantityExpr,
        /// CR 701.20a vs CR 701.16a: True = revealed (public), false = looked at (private).
        reveal: bool,
        player: TargetFilter,
    },
    CopyTokenOf {
        target: TargetFilter,
        /// CR 107.1 + CR 707.2: Number of copy tokens to create.
        count: QuantityExpr,
        /// CR 115.10: Non-targeted "for each <object>, create a token that's a
        /// copy of it" source set. Lowered to `Effect::CopyTokenOf::source_filter`.
        source_filter: Option<TargetFilter>,
        /// CR 508.4: Whether the copy token enters attacking.
        enters_attacking: bool,
        /// CR 110.5a: Status is not copied; this captures printed token-entry
        /// status from the creating effect.
        tapped: bool,
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
        from_top: Option<usize>,
        destination: Zone,
        enter_tapped: bool,
        /// Alchemy digital-only analogue to search multi-filters: "seek a X card
        /// and a Y card" performs one independent seek per filter.
        extra_filters: Vec<TargetFilter>,
    },
    /// CR 400.7 + CR 701.23 + CR 701.24: "Search [possessive] graveyard, hand,
    /// and library for any number of cards with that name and exile them."
    /// Lowered to `Effect::ChangeZoneAll` with multi-zone origin
    /// (`InAnyZone[Graveyard, Hand, Library]`) + `SameNameAsParentTarget` filter,
    /// scoped to the owner of the parent target's exiled card. Used by
    /// Deadly Cover-Up.
    MultiZoneSameNameExile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum UtilityImperativeAst {
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
        attachment: TargetFilter,
        target: TargetFilter,
    },
    UnattachAll {
        attachment: TargetFilter,
        target: TargetFilter,
    },
    /// CR 613.4d: Switch power and toughness.
    SwitchPT {
        target: TargetFilter,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum HandRevealImperativeAst {
    LookAt {
        target: TargetFilter,
        count: Option<crate::types::ability::QuantityExpr>,
        random: bool,
    },
    RevealAll {
        target: TargetFilter,
        card_filter: TargetFilter,
    },
    /// "reveals a number of cards from their hand equal to X" (CR 701.20a).
    RevealPartial {
        count: crate::types::ability::QuantityExpr,
    },
    /// CR 701.20a: Back-reference reveal — "reveal it" / "reveal that card" /
    /// "reveal those cards" — reveals a specific card identified by the parent
    /// effect's affected IDs (e.g. "look at top → reveal it" patterns).
    /// Lowers to `Effect::Reveal { target: ParentTarget }`.
    RevealBackRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum ChooseImperativeAst {
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
        choice_optional: bool,
    },
    /// "choose N of them/those [cards]" — anaphoric reference to a previously
    /// revealed/exiled set of cards. Lowered to `Effect::ChooseFromZone`.
    FromTrackedSet {
        count: u32,
        chooser: crate::types::ability::Chooser,
    },
    /// "choose a [filter] card in/from [player's] [zone]" — direct selection
    /// from visible/resolution-scoped zone contents. Lowered to `Effect::ChooseFromZone`.
    FromZone {
        count: u32,
        zones: Vec<crate::types::zones::Zone>,
        zone_owner: crate::types::ability::ZoneOwner,
        filter: crate::types::ability::TargetFilter,
        chooser: crate::types::ability::Chooser,
        up_to: bool,
    },
    /// "choose from among the permanents ... an artifact, a creature, ..." —
    /// multi-category selection where each player keeps one per type, then sacrifices the rest.
    /// Lowered to `Effect::ChooseAndSacrificeRest`.
    CategoryAndSacrificeRest {
        categories: Vec<crate::types::card_type::CoreType>,
        chooser_scope: crate::types::ability::CategoryChooserScope,
        choose_filter: crate::types::ability::TargetFilter,
        sacrifice_filter: crate::types::ability::TargetFilter,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum PutImperativeAst {
    Mill {
        count: u32,
    },
    ZoneChange {
        origin: Option<Zone>,
        destination: Zone,
        target: TargetFilter,
        /// CR 110.2a: Controller override on ETB. `Some(ref)` routes the object
        /// to the player resolved from `ref`. `None` leaves the object under
        /// its owner's control. Lowered 1:1 onto `Effect::ChangeZone.enters_under`.
        enters_under: Option<ControllerRef>,
        /// CR 603.6d: "enters tapped" — enters the battlefield tapped.
        enter_tapped: bool,
        /// CR 701.28c: "transformed" — enters with its back face up.
        enter_transformed: bool,
        /// CR 508.4: "tapped and attacking [<player_phrase>]" — the moved
        /// object enters the battlefield as an attacking creature (without
        /// having been declared as one). Set by the inline-tail patcher in
        /// `try_parse_put_zone_change` for the Kaalia / Ilharg class.
        enters_attacking: bool,
        /// "Up to one" resolution-choice zone changes may move zero matching objects.
        up_to: bool,
        /// CR 107.1c + CR 608.2c: Cardinality for non-targeted zone-change
        /// choices made during resolution, e.g. "put any number of creature
        /// cards from your hand onto the battlefield."
        choice_count: Option<Box<MultiTargetSpec>>,
        /// CR 122.1 + CR 614.1c: Counters granted as the moved object enters
        /// (e.g., "with two additional +1/+1 counters on it"). Each entry is
        /// `(counter_type, count)`.
        enter_with_counters: Vec<(CounterType, QuantityExpr)>,
    },
    /// CR 400.7 + CR 110.2a: Mass put effects ("put all creature cards from all
    /// graveyards onto the battlefield") lower to `Effect::ChangeZoneAll`.
    ZoneChangeAll {
        origin: Option<Zone>,
        destination: Zone,
        target: TargetFilter,
        enters_under: Option<ControllerRef>,
        enter_tapped: bool,
    },
    TopOfLibrary,
    BottomOfLibrary,
    NthFromTop {
        n: u32,
    },
    /// CR 121.5: "put that many cards from the top of your library into your
    /// hand" moves library cards without drawing them (Scroll Rack).
    PutTopCardsIntoHandMatchingExileCount,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum ShuffleImperativeAst {
    ShuffleLibrary {
        target: TargetFilter,
    },
    /// CR 701.24a + CR 400.3: "shuffle <pronoun> into <possessive> library".
    /// Examples: "shuffle it into its owner's library" (Cavalier of Gales),
    /// "shuffle that card into its owner's library" (search-then-shuffle
    /// tutors), "shuffle them into their owners' libraries" (compound
    /// subject).
    ///
    /// `target` carries the pronoun resolution — `SelfRef` for "it" / "~",
    /// `ParentTarget` for "them" / "that card" / "those cards".
    /// `owner_library` is `true` when the possessive resolves unambiguously
    /// to the moving card's owner ("its owner's", "their owner's", "their
    /// owners'") and `false` for "your library". Bare "their library" is
    /// intentionally not treated as owner-routing because the antecedent is
    /// ambiguous.
    ///
    /// Lowered to `Effect::ChangeZone { destination: Library, target,
    /// owner_library, … }` + a `Shuffle` sub_ability via
    /// `with_shuffle_sub_ability`.
    ChangeZoneToLibrary {
        target: TargetFilter,
        owner_library: bool,
    },
    ChangeZoneAllToLibrary {
        origins: Vec<Zone>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum CostResourceImperativeAst {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum ZoneCounterImperativeAst {
    Destroy {
        target: TargetFilter,
        all: bool,
    },
    Exile {
        origin: Option<Zone>,
        target: TargetFilter,
        all: bool,
        /// CR 122.1 + CR 614.1c: counters the exiled object enters Exile with
        /// ("exile a card … with N <type> counters on it"). Empty for the
        /// common no-counter case. Mirrors `Effect::ChangeZone.enter_with_counters`.
        enter_with_counters: Vec<(CounterType, QuantityExpr)>,
    },
    ExileTop {
        player: TargetFilter,
        count: QuantityExpr,
        /// CR 406.3: Mirrors `Effect::ExileTop.face_down` — set when the
        /// Oracle text terminates with "face down" (Necropotence / Bomat
        /// Courier / Asmodeus class).
        face_down: bool,
    },
    Counter {
        target: TargetFilter,
        /// CR 701.6 + CR 608.2c: Follow-up instruction acting on the countered
        /// ability's source permanent. Mirrors `Effect::Counter.source_rider`.
        source_rider: Option<CounterSourceRider>,
        /// CR 118.12: "Counter target spell unless its controller pays {X}"
        /// modifier. Lowered to `ParsedEffectClause.unless_pay` and ultimately
        /// to `AbilityDefinition.unless_pay`, so the runtime resolves the
        /// payment via the unified `unless_pay` pipeline rather than a
        /// counter-specific branch.
        unless_pay: Option<crate::types::ability::UnlessPayModifier>,
        /// CR 701.6 + CR 405.1: When `true`, lower to `Effect::CounterAll`
        /// (mass counter) instead of `Effect::Counter`. Mirrors the
        /// `Destroy { all }` and `Exile { all }` flags above. Triggered by
        /// the "counter all "/"counter each " precheck in `parse_counter_ast`.
        all: bool,
    },
    PutCounter {
        counter_type: CounterType,
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
        entries: Vec<(CounterType, QuantityExpr)>,
        target: TargetFilter,
        multi_target: Option<MultiTargetSpec>,
    },
    /// CR 122.1: "Put counters on each/all" — mass counter placement without targeting.
    PutCounterAll {
        counter_type: CounterType,
        count: QuantityExpr,
        target: TargetFilter,
    },
    RemoveCounter {
        counter_type: Option<CounterType>,
        count: i32,
        target: TargetFilter,
    },
    /// CR 122.5 / CR 122.8: Transfer counters from source to target.
    MoveCounters {
        source: TargetFilter,
        counter_type: Option<CounterType>,
        count: Option<QuantityExpr>,
        mode: crate::types::ability::CounterTransferMode,
        selection: crate::types::ability::CounterMoveSelection,
        target: TargetFilter,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum ClauseBoundary {
    Sentence,
    Then,
    Comma,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ClauseChunk {
    pub(crate) text: String,
    pub(crate) boundary_after: Option<ClauseBoundary>,
}

/// Debug-only assertion that a `parse_target` remainder doesn't contain a compound
/// connector (` and <verb>`). Used as a safety net at call sites that discard
/// remainders — compound detection runs first, so these should never fire for
/// production paths. `and put ...` is exempt because targeted compound actions
/// intentionally preserve that continuation for the higher-level clause parser.
#[cfg(debug_assertions)]
pub(crate) fn assert_no_compound_remainder(rem: &str, context: &str) {
    assert!(
        rem.is_empty()
            // allow-noncombinator: debug assertion on pre-parsed remainder, not parsing dispatch
            || !rem.strip_prefix(" and ").is_some_and(|after| {
                let after = after.trim();
                !after.starts_with("put ") // allow-noncombinator: debug assertion guard, not parsing dispatch
                    && crate::parser::oracle_effect::sequence::starts_bare_and_clause(after)
            }),
        "silent remainder drop: {rem:?} from: {context:?}"
    );
}

pub(crate) fn parsed_clause(effect: Effect) -> ParsedEffectClause {
    ParsedEffectClause {
        effect,
        duration: None,
        sub_ability: None,
        distribute: None,
        multi_target: None,
        condition: None,
        optional: false,
        unless_pay: None,
    }
}

pub(crate) fn with_clause_duration(
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
        Effect::CastFromZone {
            duration: ref mut effect_duration,
            ..
        } => {
            *effect_duration = Some(duration);
        }
        Effect::BecomeCopy {
            duration: ref mut effect_duration,
            ..
        } => {
            *effect_duration = Some(duration);
        }
        _ => {}
    }
    clause
}

// --- Modal types (moved from oracle_modal.rs) ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum OracleBlockAst {
    ActivatedModal {
        cost_text: String,
        header: ModalHeaderAst,
        modes: Vec<ModeAst>,
        constraints: ActivatedConstraintAst,
    },
    Modal {
        header: ModalHeaderAst,
        modes: Vec<ModeAst>,
    },
    TriggeredModal {
        trigger_line: String,
        header: ModalHeaderAst,
        modes: Vec<ModeAst>,
    },
    /// CR 614.12c + CR 607.2d: "As [this permanent] enters, choose <A> or
    /// <B>. \n • <A> — <linked ability>. \n • <B> — <linked ability>." The
    /// header text is the original "As ~ enters, choose <A> or <B>" sentence
    /// and the modes' `label` fields hold the anchor words. Lowered to:
    ///   - One `ReplacementDefinition` (Moved → `Choose { ChoiceType::Labeled,
    ///     persist: true }`) that records the chosen anchor word as a
    ///     `ChosenAttribute::Label` on the entering permanent.
    ///   - One `TriggerDefinition` or `StaticDefinition` per mode, gated on
    ///     `ChosenLabelIs { label: <anchor word> }` so the linked ability
    ///     only functions while its anchor word was chosen.
    AsEntersAnchorWordModal {
        /// Original "As ~ enters, choose <A> or <B>" sentence text used as
        /// the description on the synthesized replacement.
        header_text: String,
        /// Anchor-word labels in declaration order (matches `modes[i].label`).
        labels: Vec<String>,
        /// The bullet-prefixed linked-ability bodies, one per anchor word.
        modes: Vec<ModeAst>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ModeAst {
    pub(crate) raw: String,
    pub(crate) label: Option<String>,
    pub(crate) body: String,
    /// Per-mode additional cost (Spree). None for standard `\u{2022}` modes.
    pub(crate) mode_cost: Option<crate::types::mana::ManaCost>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ModalHeaderAst {
    pub(crate) raw: String,
    pub(crate) min_choices: usize,
    pub(crate) max_choices: usize,
    pub(crate) allow_repeat_modes: bool,
    pub(crate) constraints: Vec<ModalSelectionConstraint>,
    /// CR 700.2e: The player who chooses the mode(s). `Controller` (CR 700.2a)
    /// for standard `Choose one —` headers and the `you choose —` alias.
    pub(crate) chooser: PlayerFilter,
}

// --- ActivatedConstraintAst (moved from oracle.rs) ---

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct ActivatedConstraintAst {
    pub(crate) restrictions: Vec<ActivationRestriction>,
    /// CR 602.2: "Any player may activate this ability." — annotation recognized
    /// during parsing. Runtime enforcement is a future item; currently stripped
    /// so the sentence does not produce an `Unimplemented` fallback.
    pub(crate) any_player_may_activate: bool,
}

impl ActivatedConstraintAst {
    pub(crate) fn sorcery_speed(&self) -> bool {
        self.restrictions
            .contains(&ActivationRestriction::AsSorcery)
    }
}
