use serde::Serialize;

use crate::types::ability::MultiTargetSpec;
use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, ActivationRestriction, BounceSelection,
    CastingPermission, ControlWindow, ControllerRef, CopyRetargetPermission, CounterAdjustment,
    CounterSourceRider, DoorLockOp, Duration, Effect, FaceDownProfile, LibraryPosition,
    ManaProduction, ManaSpendRestriction, ModalSelectionConstraint, OutsideGameSourcePool,
    PlayerFilter, PtStat, PtValue, QuantityExpr, SearchDestinationSplit, SearchSelectionConstraint,
    SpellStackToGraveyardReplacement, StaticCondition, StaticDefinition, TargetFilter,
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
    ///
    /// **#13 / gate-9 residual (DEFERRED, not closed by Unit 5).** This holds a
    /// fully-lowered `AbilityDefinition` with NO clause identity of its own. It is
    /// constructed deep in imperative/subject parsing (e.g. `imperative.rs`), long
    /// before the enclosing clause reaches `ClauseIrBuilder`, so no `ClauseId` is
    /// in scope at construction. The enclosing `ClauseIr` IS addressed (it carries
    /// a `ClauseId` + `OracleUnitSource`), so this nested def is covered
    /// transitively by its parent clause's id — but it has no INDEPENDENT clause
    /// identity. Minting one requires U6's `ClauseId`-keyed assembly arena
    /// (Plan 01 §6, line 823), which owns the nested sub-clause id-space; a bespoke
    /// nested id-space here would preempt that decision. The `rg 'ClauseIr {'`
    /// removal gate does not cover this field (it holds an `AbilityDefinition`, not
    /// a `ClauseIr`), so this doc comment is the honest marker: independent
    /// sub-clause identity upgrades in U6.
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
    pub(crate) supertypes: Vec<crate::types::card_type::Supertype>,
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
        /// CR 509.1b + CR 611.2c: A conjoined-subject evasion grant ("<source>
        /// and up to N other target creature(s) can't be blocked this turn",
        /// Martha Jones) carries the SECOND conjunct's grant as a sub_ability
        /// continuation, mirroring `Become`/`Continuous`. `None` for the common
        /// single-subject restriction.
        sub_ability: Option<Box<AbilityDefinition>>,
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
        /// CR 110.2a: Some(You) when the card enters "under your control"; None keeps the ChangeZone default (owner's control).
        enters_under: Option<ControllerRef>,
        /// CR 701.23a: When true, the searched card is revealed before it moves.
        reveal: bool,
        /// When `Some`, the found card enters attached to this host filter.
        /// Adds `forward_result` on the ChangeZone and chains an Attach sub_ability.
        attach_host: Option<TargetFilter>,
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
    /// CR 701.6a + CR 614.1a: "If that spell is countered this way, put it
    /// <zone> instead of into that player's graveyard." — patches
    /// `countered_spell_zone = Some(destination)` on the preceding
    /// `Effect::Counter` (Memory Lapse, Remand, Spell Crumple).
    CounterSpellZoneRedirect {
        destination: SpellStackToGraveyardReplacement,
    },
    /// CR 707.10c: "You may choose new targets for the copy/copies." after a
    /// CopySpell (possibly wrapped in a CreateDelayedTrigger) — patches
    /// `retarget = MayChooseNewTargets` on the inner Effect::CopySpell.
    /// `all_copies` is the plural "the copies" form: it patches every copy the
    /// source ability makes (Increasing Vengeance's primary + conditional
    /// second copy), where the singular "the copy" form binds only the nearest.
    CopyMayRetarget { all_copies: bool },
    /// "create a ... token and suspect it" → chain Suspect { target: LastCreated }
    SuspectLastCreated,
    /// CR 701.15a + CR 701.15b: "The token(s) (is|are) goaded [duration]" after token
    /// creation — grants `StaticMode::Goaded` on `TargetFilter::LastCreated`.
    GoadLastCreated { duration: Option<Duration> },
    /// CR 702.34a / CR 702.128a / CR 702.180a: "The/Its [flashback|embalm|harmonize]
    /// cost is equal to its/that card's mana cost." after a self-cost graveyard
    /// keyword grant. Redundant reminder text — the grant already carries
    /// `ManaCost::SelfManaCost`, so this continuation is absorbed as a no-op
    /// rather than lowering to `Effect::Unimplemented`.
    SelfCostKeywordCostClarification,
    /// CR 701.19c: "It can't be regenerated" / "They can't be regenerated" — sets
    /// `cant_regenerate: true` on the preceding Destroy/DestroyAll effect.
    CantRegenerate,
    /// CR 120.4a + CR 608.2c + CR 702: "Excess damage is dealt to that
    /// creature's controller instead" patches the preceding `DealDamage`; an
    /// optional source-keyword gate covers Ram Through's "If the creature you
    /// control has trample" prefix without making the damage itself conditional.
    ExcessDamageToController {
        source_keyword_condition: Option<crate::types::keywords::KeywordKind>,
    },
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
    /// CR 702.143d: "It/that card/they become foretold" after an exile effect.
    BecomesForetold,
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
        /// CR 701.20a vs 701.20e: True when the from-among clause's stripped verb
        /// was "reveal" (a public action) rather than "put"/"choose" (a private
        /// look). Promotes the patched Dig to `reveal: true` even when the kept
        /// cards route to a fixed library position (Fertile Thicket).
        #[serde(default)]
        reveal_verb: bool,
    },
    /// CR 708.2a + CR 205.1a: "They're N/M [types] [subtypes] creatures." after a
    /// put-face-down clause — refines the preceding face-down move's profile.
    FaceDownProfileSpec { profile: FaceDownProfile },
    /// CR 508.4 / CR 614.1: "It/The token enters tapped and attacking [that player]"
    /// Absorbs into preceding CopyTokenOf, Token, or ChangeZone by setting
    /// enters_attacking and tapped/enter_tapped flags.
    ///
    /// CR 614.12: `moved_filter` carries an optional leading moved-object
    /// type condition ("If that card is an enchantment card, it enters
    /// tapped and attacking" — Summoner's Grimoire). When `Some`, the
    /// absorbed ChangeZone gates the riders on the moved object via
    /// `Effect::ChangeZone.enters_modified_if`. `None` = unconditional
    /// (Stangg / Shark Shredder). Only ChangeZone honors the gate;
    /// CopyTokenOf / Token always enter unconditionally.
    EntersTappedAttacking { moved_filter: Option<TargetFilter> },
    /// CR 122.6a: "The token enters with X +1/+1 counters on it, where X is ..."
    /// Absorbs into the preceding Token effect by populating `enter_with_counters`.
    TokenEntersWithCounters {
        counter_type: CounterType,
        count: QuantityExpr,
    },
    /// CR 608.2h + CR 111.3: "Its power is equal to this creature's power and
    /// its toughness is equal to this creature's toughness" after a token
    /// creation clause — source-defined token P/T printed as a separate
    /// sentence.
    TokenSourcePowerToughness { power: PtValue, toughness: PtValue },
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
        /// CR 701.20a + CR 608.2c: `true` when the disposition is "put any number
        /// of those [filter] cards onto [destination]" over the *set* of matched
        /// cards (Aurora Awakener), absorbing into
        /// `RevealUntilDisposition::ChooseAnyNumber`. `false` is the single-hit
        /// "put that card …" form (`KeepEach`).
        any_number: bool,
        rest_destination: Option<Zone>,
        /// CR 110.2a: "under your control" on the kept-card clause.
        enters_under: Option<ControllerRef>,
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
    /// CR 406.3 + CR 701.20e: "[then] exile it/them [face down]" after a private
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
    /// CR 702.75a + CR 406.3: "exile one of them face down" after a private
    /// `Dig` (the "look at the top N cards of <player>'s library" look step) —
    /// the Gonti, Lord of Luxury class. Unlike `ExileLookedAtCard` (which exiles
    /// the looked-at card(s) wholesale via `ExileTop`), this is a player choice
    /// of ONE card from among the N looked at. It patches the preceding `Dig`
    /// into the Hideaway shape (`keep_count: Some(1)`, `destination: Exile`) so
    /// the dug card is player-selected and routed to exile by the `DigChoice`
    /// flow, then chains a `HideawayConceal` sub-ability to turn the chosen card
    /// face down and link it to the source. Gated on the exile-the-dug-card
    /// continuation, so genuine pure-peek Digs (Delver of Secrets) are untouched.
    ExileOneOfThemFaceDown {
        /// CR 122.1: A "... face down with <count> <type> counter(s) on it"
        /// rider (The Dragon-Kami Reborn: "Exile one of them face down with a
        /// hatching counter on it"). Threaded into the fused exile as a
        /// `PutCounter { target: ParentTarget }` sub-ability chain appended
        /// after the conceal, so the player-selected dug card — NOT the trigger
        /// source — receives the counters. Empty for the bare "exile one of
        /// them face down" form (Gonti, Lord of Luxury).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        enter_with_counters: Vec<(CounterType, QuantityExpr)>,
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
    /// "up to N" — the bound is a `QuantityExpr` so dynamic keep counts
    /// ("put up to X cards ...") carry through to `Effect::Dig.keep_count_expr`.
    Up(QuantityExpr),
    /// "exactly N" — dynamic form ("put X cards from among them", Stargaze).
    Exactly(QuantityExpr),
}

impl PutCount {
    /// `Up` with a literal bound — the common fixed-count call site.
    pub(crate) fn up(n: u32) -> Self {
        Self::Up(QuantityExpr::Fixed { value: n as i32 })
    }

    /// `Exactly` with a literal bound.
    pub(crate) fn exactly(n: u32) -> Self {
        Self::Exactly(QuantityExpr::Fixed { value: n as i32 })
    }

    /// CR 701.20e: Lower a `PutCount` to an `Effect::Dig` keep specification:
    /// `(keep_count, keep_count_expr, up_to)`. Fixed bounds stay on the
    /// `keep_count` u32 path (identical lowering to the pre-widen code); a
    /// dynamic bound routes to `keep_count_expr` and leaves `keep_count` None
    /// so the resolver reads the expression. `u32::MAX` is the unbounded
    /// sentinel the resolver clamps to the number of seen cards.
    pub(crate) fn to_dig_keep(&self) -> (Option<u32>, Option<QuantityExpr>, bool) {
        match self {
            PutCount::All => (Some(u32::MAX), None, false),
            PutCount::AnyNumber => (Some(u32::MAX), None, true),
            PutCount::Up(QuantityExpr::Fixed { value }) => (Some(*value as u32), None, true),
            PutCount::Up(e) => (None, Some(e.clone()), true),
            PutCount::Exactly(QuantityExpr::Fixed { value }) => (Some(*value as u32), None, false),
            PutCount::Exactly(e) => (None, Some(e.clone()), false),
        }
    }
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
    /// CR 701.12a: Two players exchange life totals (Soul Conduit, Axis of
    /// Mortality, Magus of the Mirror, Mirror Universe). `player_a`/`player_b`
    /// select each player (`Controller` for "you", an opponent filter for "target
    /// opponent", `Player` for "target player").
    ExchangeLifeTotals {
        player_a: TargetFilter,
        player_b: TargetFilter,
    },
    /// CR 119.7 + CR 119.8: The controller redistributes any number of players' life
    /// totals (Reverse the Sands, The Doctor's Tomb). Field-less: "any number of
    /// players" is self-gathered at resolution, so there are no target slots.
    RedistributeLifeTotals,
    /// CR 509.1c: Must be blocked this turn if able.
    MustBeBlocked,
    Investigate,
    /// CR 701.36a: Populate.
    Populate,
    /// CR 701.30: Clash with an opponent.
    Clash,
    /// CR 701.4a: Behold a [quality] — reveal-or-choose keyword action. Carries
    /// the beheld quality as a subtype/type filter.
    Behold(TargetFilter),
    /// CR 701.48a: Learn.
    Learn,
    /// CR 701.40a: Manifest the top card(s) of library.
    Manifest {
        target: TargetFilter,
        count: QuantityExpr,
        /// CR 110.2a: Direct imperative manifest defaults to the instruction's
        /// controller; subject-predicate forms leave this unset so the subject's
        /// library owner controls the manifested card.
        enters_under: Option<ControllerRef>,
    },
    /// CR 701.62a: Manifest dread.
    ManifestDread,
    /// CR 701.58a: Cloak card(s) — face-down 2/2 with ward {2}, turnable face up
    /// for its mana cost if it's a creature card. `from_zone` is the source
    /// discriminant: `None` cloaks the top `count` cards of `target`'s library
    /// (Cryptic Coat, Ransom Note); `Some(zone)` cloaks a card the controller
    /// chooses from that zone (Vannifar's "cloak a card from your hand"), which
    /// lowers to a `ChooseFromZone` parent + `Cloak { object_source }` sub-chain.
    Cloak {
        target: TargetFilter,
        count: QuantityExpr,
        from_zone: Option<Zone>,
        /// CR 110.2a: The instruction subject who cloaks — the controller on
        /// entry for the cloaked card(s). Mirrors `Manifest.enters_under`.
        enters_under: Option<ControllerRef>,
    },
    /// CR 406.3 + CR 701.20a: Turn an exiled face-down card face up via a
    /// resolving effect (not the morph special action). The Imprint "flip"
    /// cards — Clone Shell, Summoner's Egg, Compleated Clone Shell, The Creation
    /// of Avacyn — say "turn the exiled card(s) face up"; `target` references
    /// the card(s) the source exiled.
    TurnFaceUp {
        target: TargetFilter,
    },
    /// CR 708.2a: "Turn target [permanent] face down" — turns the targeted
    /// face-up permanent(s) face down via a resolving effect (Cyber Conversion).
    /// `profile` is seeded with `Some(vanilla_2_2())` at the verb arm so a
    /// trailing "It's a 2/2 Cyberman artifact creature." `FaceDownProfileSpec`
    /// continuation can refine the face-down body (CR 205.1a).
    ///
    /// CR 115.1d: `multi_target` carries the target-count quantifier when the
    /// subject is plural ("turn any number of target tapped nontoken creatures
    /// face down" — Illithid Harvester; "turn N target … face down"). It is
    /// stamped onto the lowered `ParsedEffectClause` so the cast surfaces the
    /// correct number of target slots rather than collapsing to one. `None` for
    /// the single-subject form (Cyber Conversion, Backslide).
    TurnFaceDown {
        target: TargetFilter,
        profile: Option<FaceDownProfile>,
        multi_target: Option<MultiTargetSpec>,
    },
    BecomeMonarch,
    /// CR 701.49: "venture into the dungeon"
    VentureIntoDungeon,
    /// CR 701.49d: "venture into the Undercity"
    VentureIntoUndercity,
    /// CR 725: "take the initiative"
    TakeTheInitiative,
    /// CR 701.31c: An ability instructs a player to planeswalk (TARDIS, Start
    /// the TARDIS, TARDIS Bay). Resolves to a no-op outside a Planechase game
    /// (CR 701.31a).
    Planeswalk,
    /// CR 701.51b: "open N Attractions"
    OpenAttractions {
        count: u32,
    },
    /// CR 701.52: "roll to visit your Attractions"
    RollToVisitAttractions,
    /// Unstable Contraptions: assemble one or more Contraptions from the top of
    /// your Contraption deck.
    AssembleContraptions {
        count: crate::types::ability::QuantityExpr,
    },
    /// Unstable Contraptions: assemble a number of Contraptions equal to the
    /// difference between the two most recent die-roll results.
    AssembleContraptionsFromRollDifference,
    /// Unstable Contraptions: move a Contraption onto a sprocket, optionally
    /// gaining control of it first.
    ReassembleContraption {
        target: crate::types::ability::TargetFilter,
        control_mode: crate::types::ability::ReassembleControlMode,
    },
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

/// CR 107.1: Scale a *fixed* base count by a per-each `for_each` quantity.
/// Fixed(0) is preserved as-is (zero effect regardless of for-each count).
/// Fixed(1) is replaced directly with the for-each quantity.
/// Fixed(N>1) wraps in Multiply { factor: N, inner: for_each }.
///
/// A non-`Fixed` base (e.g. `EventContextAmount` from "that many", a `Ref`, or
/// a nested `Multiply` from "twice X") is returned **unchanged**: there is no
/// `QuantityExpr` variant for the product of two arbitrary dynamic quantities
/// (`Multiply` takes a constant `factor`, not a second dynamic operand), so the
/// only rules-safe choice is to keep the parsed base rather than silently
/// discard it in favor of the bare for-each. Callers must therefore only reach
/// the for-each-attach path with a `Fixed` base; if a future card pairs a
/// dynamic base with a for-each multiplier, a general product variant is the
/// correct extension (gated through `add-engine-variant`).
pub(crate) fn replace_fixed_quantity(fixed: QuantityExpr, for_each: QuantityExpr) -> QuantityExpr {
    match fixed {
        QuantityExpr::Fixed { value: 0 } => QuantityExpr::Fixed { value: 0 },
        QuantityExpr::Fixed { value: 1 } => for_each,
        QuantityExpr::Fixed { value } if value > 1 => QuantityExpr::Multiply {
            factor: value,
            inner: Box::new(for_each),
        },
        // Non-`Fixed` base (or a negative Fixed, which a draw/counter count never
        // produces): keep the parsed base rather than dropping it for `for_each`.
        base => base,
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
        /// CR 115.1d + CR 701.26a: Variable target count for "tap up to N target
        /// creatures" (Nyssa of Traken's "tap up to that many target creatures",
        /// N = `EventContextAmount`). `None` for the common single-target
        /// "tap target creature". Carried onto `ParsedEffectClause.multi_target`
        /// at lowering so the targeting system surfaces the right number of slots.
        multi_target: Option<MultiTargetSpec>,
    },
    Untap {
        target: TargetFilter,
        /// CR 115.1d + CR 701.26b: Variable target count for "untap up to N target
        /// creatures", mirroring [`TargetedImperativeAst::Tap`].
        multi_target: Option<MultiTargetSpec>,
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
    /// CR 709.5f-g + CR 709.5j: "lock"/"unlock"/"lock or unlock" a door of a
    /// target Room permanent. The eligible half is chosen at resolution from the
    /// Room's runtime unlock state, so only the operation and the target Room
    /// filter are captured here. Lowers to `Effect::SetRoomDoorLock`.
    SetRoomDoorLock {
        op: DoorLockOp,
        target: TargetFilter,
    },
    Sacrifice {
        target: TargetFilter,
        /// CR 701.21a: Number of permanents to sacrifice. Defaults to
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
        /// CR 115.1d + CR 601.2c: Variable object count for a non-targeted
        /// "return up to N cards from your graveyard to your hand" (Ill-Gotten
        /// Gains) or "return that many cards from your graveyard to your hand"
        /// (dynamic count) — mirrors [`TargetedImperativeAst::Tap`]. `None` for
        /// the common single-object "return a card from your graveyard to your
        /// hand". Carried onto `ParsedEffectClause.multi_target` at lowering;
        /// the runtime's `EffectZoneChoice` picker already resolves bounded
        /// at-resolution bounce counts generically (Wrenn and Six precedent),
        /// so no game-logic change is needed — only the parser previously
        /// dropped the quantifier before it ever reached that machinery.
        multi_target: Option<MultiTargetSpec>,
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
        /// CR 708.2a + CR 708.3: "face down" — the returned object is turned
        /// face down before it enters (Yedora's "return it ... face down ... It's
        /// a Forest land."). Lowered to a default vanilla-2/2 `face_down_profile`,
        /// refined by a trailing "It's a <type>" `FaceDownProfileSpec`.
        face_down: bool,
    },
    /// CR 400.6: Return to a specific non-hand, non-battlefield zone (zone change).
    ReturnToZone {
        target: TargetFilter,
        origin: Option<Zone>,
        destination: Zone,
        /// CR 107.1c: "return any number of [filter] cards" — zero-or-more
        /// resolution-time zone selection (Grave Sifter class).
        up_to: bool,
        /// CR 115.1d + CR 601.2c: Variable object count for a non-targeted
        /// "return up to N cards from your graveyard to your hand"
        /// (Ill-Gotten Gains) or "return that many cards …" (dynamic count).
        /// `None` for the common single-object "return a card from your
        /// graveyard to your hand" and for the unrelated unbounded `up_to`
        /// shape above (Grave Sifter class), which keeps using its own flag.
        /// Carried onto `ParsedEffectClause.multi_target` at lowering,
        /// mirroring `ZoneCounterImperativeAst::Exile`'s `multi_target` (#5649
        /// / Forage precedent) — `Effect::ChangeZone` has no count slot of its
        /// own, so the quantity rides the clause's `MultiTargetSpec` instead.
        multi_target: Option<MultiTargetSpec>,
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
        /// CR 122.1 + CR 122.1h: Counters placed on each returned object as it
        /// enters the battlefield (e.g. "return each creature card from your
        /// graveyard to the battlefield. They enter with a finality counter").
        /// Threaded onto `Effect::ChangeZoneAll.enter_with_counters`. Empty for
        /// returns that carry no counters.
        enter_with_counters: Vec<(CounterType, QuantityExpr)>,
    },
    Fight {
        target: TargetFilter,
        /// CR 115.6: "up to N target …" cardinality (min=0) preserved from
        /// `strip_optional_target_prefix`; `None` for the mandatory "fights
        /// target …" form. Lowered onto `ParsedEffectClause.multi_target` in
        /// `lower_imperative_family_ast`, never onto `Effect::Fight` (the spec
        /// is an ability-level target-count axis, not an effect field).
        multi_target: Option<MultiTargetSpec>,
    },
    GainControl {
        target: TargetFilter,
        /// True for the untargeted mass form ("gain control of all/each …"),
        /// lowered to `Effect::GainControlAll`; false for targeted GainControl.
        all: bool,
        /// CR 115.1d + CR 601.2c: Variable target count for "gain control of up to N target
        /// …" (The Super Hero Civil War's "up to two target creatures with total
        /// mana value 6 or less"; Jace, Ingenious Mind-Mage's "up to three target
        /// creatures"). `None` for the common single-target "gain control of
        /// target creature". Carried onto `ParsedEffectClause.multi_target` at
        /// lowering so the targeting system surfaces N optional slots — without
        /// it the count collapsed to one and only a single creature could be
        /// chosen (issue #6205). Mirrors the same field on `Tap`/`Untap`.
        multi_target: Option<MultiTargetSpec>,
    },
    ControlNextTurn {
        target: TargetFilter,
        grant_extra_turn_after: bool,
        /// CR 723.1 / CR 723.2: full-turn vs next-combat-phase control window.
        window: ControlWindow,
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
        /// CR 701.20a vs CR 701.20e: True = revealed (public), false = looked at (private).
        reveal: bool,
        player: TargetFilter,
    },
    /// CR 701.20e + CR 701.13a + CR 406.3: Fused "look at the top N ... and exiles it face down".
    ExileTopLookedAt {
        player: TargetFilter,
        count: QuantityExpr,
        face_down: bool,
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
    /// and library for `<quantifier>` cards with that name and exile them."
    /// The `quantifier` axis selects the lowering:
    /// - `All` → `Effect::ChangeZoneAll` (mandatory mass exile) with multi-zone
    ///   origin (`InAnyZone[Graveyard, Hand, Library]`) + `SameNameAsParentTarget`.
    /// - `AnyNumber` / `UpTo(n)` → interactive `Effect::SearchLibrary` (CR 701.23b:
    ///   the searcher may fail to find), `count: UpTo`, `SameNameAsParentTarget`.
    ///
    /// Both are scoped to the player named by the possessive zone phrase (`owner`).
    MultiZoneSameNameExile {
        owner: ControllerRef,
        quantifier: MultiZoneExileQuantifier,
    },
}

/// CR 107.1c + CR 701.23b: How many name-matched cards a multi-zone same-name
/// exile removes. `All` is the mandatory mass-exile form ("all cards");
/// `AnyNumber` ("any number of cards") and `UpTo(n)` ("up to N cards") are the
/// interactive forms where the searcher chooses a subset (and may find none).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum MultiZoneExileQuantifier {
    All,
    AnyNumber,
    UpTo(u32),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum UtilityImperativeAst {
    Prevent {
        text: String,
        /// CR 608.2c: true when an earlier clause in the same effect chain
        /// selected a target this clause's anaphor can bind to. Captured from
        /// `ctx.parent_target_available` at the one construction site where
        /// `ParseContext` is live (issue #1094).
        parent_target_available: bool,
    },
    Regenerate {
        text: String,
    },
    Copy {
        target: TargetFilter,
        /// CR 707.10c: set when the imperative remainder is a copy-retarget grant.
        retarget: CopyRetargetPermission,
    },
    Transform {
        target: TargetFilter,
    },
    Attach {
        attachment: TargetFilter,
        target: TargetFilter,
        /// CR 115.1d: "attach up to N target ..." / "attach any number of
        /// target ..." cardinality belongs to the ability's target selection,
        /// not the `Effect::Attach` payload.
        multi_target: Option<MultiTargetSpec>,
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
    /// CR 701.20: Reveal a specific object selected by a target phrase —
    /// "Reveal target face-down permanent" (Hauntwoods Shrieker). Lowers to
    /// `Effect::Reveal { target }`. Distinct from `RevealBackRef` (anaphoric
    /// "it"/"that card") and `RevealAll`/`RevealPartial` (hand reveals): this
    /// reveals a battlefield/zone object chosen via the targeting pipeline.
    RevealObject { target: TargetFilter },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum ChooseImperativeAst {
    /// CR 609.7a: "choose a source [you control|...]" — interactive damage-source
    /// selection, distinct from permanent targeting (`TargetOnly`).
    DamageSource {
        source_filter: TargetFilter,
    },
    TargetOnly {
        target: TargetFilter,
    },
    Reparse {
        text: String,
    },
    NamedChoice {
        choice_type: crate::types::ability::ChoiceType,
        /// CR 608.2d (override): `Random` for "choose a player at random".
        selection: crate::types::ability::TargetSelectionMode,
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
        /// CR 608.2d (override): `Random` for "choose one of them at random".
        selection: crate::types::ability::CardSelectionMode,
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
        /// CR 608.2d (override): `Random` for "choose ... at random".
        selection: crate::types::ability::CardSelectionMode,
    },
    /// "choose from among the permanents ... an artifact, a creature, ..." —
    /// multi-category selection where each player keeps one per type, then sacrifices the rest.
    /// Lowered to `Effect::ChooseAndSacrificeRest`.
    CategoryAndSacrificeRest {
        categories: Vec<crate::types::card_type::CoreType>,
        chooser_scope: crate::types::ability::CategoryChooserScope,
        choose_filter: crate::types::ability::TargetFilter,
        sacrifice_filter: crate::types::ability::TargetFilter,
        /// Slaughter the Strong: keep ANY number of `choose_filter` permanents
        /// whose combined power is at most this cap, instead of one per category.
        total_power_cap: Option<crate::types::ability::QuantityExpr>,
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
    /// CR 608.2d + CR 122.1: "choose a counter on it / that permanent" — pick one
    /// of the distinct counter kinds present on the anaphoric object (The Caves
    /// of Androzani II/III). Lowered to `Effect::ChooseCounterKind`. `target` is
    /// the anaphor (`ParentTarget` for the per-iteration object).
    CounterKind {
        target: TargetFilter,
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
        /// CR 401.4: Specific library placement for mass library moves.
        /// `Some` suppresses the default library shuffle and places each moved
        /// object at that position.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        library_position: Option<LibraryPosition>,
        /// CR 401.4: The owner may randomize/arrange simultaneous library
        /// placement for mass moves.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        random_order: bool,
        /// CR 608.2c: "and the rest into <zone>" complement for a tracked-set
        /// partition ("Put all <filter> revealed this way into your hand and
        /// the rest into your graveyard" — Winding Way). The primary move sends
        /// the chosen subset to `destination`; the lowering emits a sibling
        /// `ChangeZoneAll { target: TrackedSet, destination: rest }` so the
        /// still-tracked cards left in the producer's zone (the rest) move to
        /// the rest zone. `None` for non-partition forms.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rest_destination: Option<Zone>,
        /// CR 401.4: Library placement for the "rest" pile when the complement
        /// returns to the library ("… and the rest on the bottom of your
        /// library in a random order" — The Fourteenth Doctor — or "… in any
        /// order" — Garruk, Caller of Beasts / Goblin Ringleader et al.).
        /// Independent of `library_position` (which governs the PRIMARY move)
        /// because the two piles may target different positions. `None` when the
        /// rest does not go to a specific library position.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rest_library_position: Option<LibraryPosition>,
    },
    TopOfLibrary,
    BottomOfLibrary,
    NthFromTop {
        n: u32,
    },
    /// CR 401.7 (Unexpectedly Absent class): "into its owner's library just
    /// beneath the top N cards of that library." The placed object ends with
    /// exactly `depth` cards above it (0-based insertion index = resolved
    /// `depth`). `depth` is a `QuantityExpr` so the count can be the spell's
    /// announced `{X}` resolved at resolution time.
    BeneathTop {
        depth: QuantityExpr,
    },
    /// CR 121.5: "put that many cards from the top of your library into your
    /// hand" moves library cards without drawing them (Scroll Rack).
    PutTopCardsIntoHandMatchingExileCount,
    /// CR 701.40a + CR 708.2a + CR 110.2a: "put the top N cards of [a player]'s
    /// library onto the battlefield face down [under your control]." This is the
    /// put-clause surface form of manifest (CR 701.40a): the cards are turned
    /// face down before entry (CR 708.3) and become 2/2 creatures by default.
    /// `target` selects whose library is the source. `count` is N. `profile`
    /// seeds the effect-specified face-down characteristics (CR 708.2a) — set to
    /// `Some(vanilla_2_2())` when "face down" is present so a trailing "They're
    /// 2/2 Cyberman artifact creatures." continuation has a profile to refine.
    /// `enters_under` carries the CR 110.2a controller override ("under your
    /// control"). Lowered 1:1 onto `Effect::Manifest`.
    Manifest {
        target: TargetFilter,
        count: QuantityExpr,
        profile: Option<FaceDownProfile>,
        enters_under: Option<ControllerRef>,
    },
    /// CR 401.4 + CR 608.2c: "put the cards {in|from} <possessive> hand on the
    /// bottom/top of <possessive> library [in any order]" — the whole-hand
    /// reposition (Teferi's Puzzle Box). The mover's entire hand moves to the
    /// named library `position` at once; CR 401.4 lets the owner arrange the
    /// simultaneously-placed cards in any order. Lowered to
    /// `Effect::ChangeZoneAll { origin: Hand, destination: Library,
    /// library_position: Some(position) }` with NO trailing shuffle — a shuffle
    /// would scatter the cards the effect just placed on the bottom/top.
    HandToLibraryPosition {
        position: LibraryPosition,
    },
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
    ///
    /// `all` distinguishes a single-target move ("shuffle target card from your
    /// graveyard into your library", `false`) from a filtered mass move
    /// ("shuffle all nonland cards from your graveyard into your library",
    /// `true`). When `true`, the lowering emits `Effect::ChangeZoneAll` so every
    /// eligible object moves with no interactive choice (CR 400.6) and the move
    /// stamps `last_effect_count`; when `false` it emits a single
    /// `Effect::ChangeZone`.
    ///
    /// CR 115.1d: `multi_target` carries an "up to N target" count ("shuffle up
    /// to three target cards from your graveyard into your library" — Memory's
    /// Journey) so the lowering surfaces N target slots instead of one. `None`
    /// for the single-target form; only meaningful when `all` is `false`.
    TargetedChangeZoneToLibrary {
        target: TargetFilter,
        origin: Option<Zone>,
        all: bool,
        multi_target: Option<MultiTargetSpec>,
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
    /// CR 118.1: "pay {cost}" as an effect verb (mana, life, energy, …).
    /// Carries the unified `AbilityCost` taxonomy directly (lowered to
    /// `Effect::PayCost { cost, scale: None, .. }`); this IR path never emits a
    /// per-object scaled mana cost.
    Pay {
        cost: AbilityCost,
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
        /// CR 700.4 (#5649): a counted graveyard exile — "exile <N> cards from
        /// your graveyard" (Nefarious Lich: "exile that many cards … instead").
        /// `Effect::ChangeZone` carries no count, so the quantity rides the
        /// clause's `MultiTargetSpec` (mirroring Forage), threaded at lowering by
        /// `lower_imperative_family_ast`. `None` for the ordinary single-object
        /// exile.
        multi_target: Option<MultiTargetSpec>,
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
    /// CR 122.1 + CR 122.6: "put an additional counter of that kind on <anaphor>"
    /// — add `count` counters of the kind chosen by a preceding
    /// `ChooseCounterKind` (The Caves of Androzani II/III). Lowered to
    /// `Effect::PutChosenCounter`.
    PutChosenCounter {
        target: TargetFilter,
        count: QuantityExpr,
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
        count: QuantityExpr,
        target: TargetFilter,
    },
    /// CR 122.1 + CR 608.2d (Clockspinning sentence 2): "Remove that counter ...
    /// or put another of those counters on it." The single target object is
    /// established by the preceding `TargetOnly` clause; this clause only records
    /// the operation set the controller may choose among at resolution. Lowers to
    /// `Effect::ChooseCounterAdjustment` (which has no target slot of its own).
    ChooseCounterAdjustment {
        adjustment: CounterAdjustment,
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

/// A `.parsed` placeholder for a clause whose real semantics live in its
/// `ClauseDisposition` (e.g. a `Special` action), not in its effect. Distinct
/// from `Effect::unimplemented`: it carries no fragment (`description: None`)
/// because it marks an intentional structural non-effect, not an unparsed gap —
/// a `Some(fragment)` here would leak into the coverage report as a false parse
/// gap. `name` is a stable snake_case pattern-class key.
pub(crate) fn placeholder_parsed_clause(name: &str) -> ParsedEffectClause {
    // Structural placeholder for a disposition-carried clause; the None fragment is
    // load-bearing (a Some(_) would surface as a false parse gap in coverage).
    // allow-noncombinator: intentional Effect::Unimplemented placeholder, not parse dispatch.
    parsed_clause(Effect::Unimplemented {
        name: name.to_string(),
        description: None,
    })
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
            *perm_dur = normalize_play_from_exile_duration(duration);
        }
        Effect::CastFromZone {
            duration: ref mut effect_duration,
            ..
        } => {
            *effect_duration = Some(duration);
        }
        Effect::BecomeCopy {
            duration: ref mut effect_duration,
            recipient,
            ..
        } => {
            // CR 611.2b + CR 301.5: a leading "for as long as ~ remains
            // attached to it" binds a singular become-copy to the attachment
            // host. The duration is stripped before the body is parsed, so
            // this is the first point where both the copy and its final
            // duration are available.
            if matches!(
                &duration,
                Duration::ForAsLongAs {
                    condition: StaticCondition::RecipientMatchesFilter {
                        filter: TargetFilter::AttachedTo,
                    },
                }
            ) && *recipient == TargetFilter::SelfRef
            {
                *recipient = TargetFilter::AttachedTo;
            }
            *effect_duration = Some(duration);
        }
        _ => {}
    }
    clause
}

fn normalize_play_from_exile_duration(duration: Duration) -> Duration {
    match duration {
        Duration::ForAsLongAs {
            condition: StaticCondition::Unrecognized { text },
        } if matches!(
            text.as_str(),
            "it remains exiled"
                | "that card remains exiled"
                | "those cards remain exiled"
                | "they remain exiled"
        ) =>
        {
            // CR 400.7i + CR 611.2a: exile-play permissions persist until the
            // referenced object leaves exile; zone-exit cleanup removes the
            // object-tagged permission.
            Duration::Permanent
        }
        other => other,
    }
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
        /// CR 603.12 + CR 700.2b: When the trigger gates its modal choice behind
        /// an optional reflexive cost ("Whenever you attack, you may sacrifice
        /// another creature. When you do, choose ..."), this holds the cost
        /// effect text (e.g. "Sacrifice another creature"). The lowering builds
        /// an `Effect::Sacrifice { optional }` whose `WhenYouDo` sub_ability
        /// carries the modal, so the modes fire only after the cost is paid.
        /// `None` for a plain triggered modal (Pip-Boy), where the modal attaches
        /// directly as the trigger's execute.
        optional_cost: Option<String>,
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
    /// CR 700.2i: pawprint weight for this mode ("{P}" runs). None for bullet/Spree modes.
    pub(crate) mode_pawprint: Option<u8>,
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
    /// CR 700.2b (override) + CR 701.9b (analogous): `Random` for "choose one at
    /// random" headers (Cult of Skaro) — the game selects the mode(s), not the
    /// chooser. `Chosen` for all standard modal headers.
    pub(crate) selection: crate::types::ability::TargetSelectionMode,
    /// CR 700.2 + CR 107.3m: Dynamic max ("choose up to X —") — `Some` carries
    /// the cost {X} reference resolved live at runtime; `None` for fixed caps.
    pub(crate) dynamic_max_choices: Option<crate::types::ability::QuantityExpr>,
    /// CR 608.2c: Triggered modal headers of the form "you may choose N"
    /// (Shadrix Silverquill) make the entire triggered ability optional — the
    /// controller may decline to choose any modes. Distinct from "you may choose
    /// up to N", which only lowers `min_choices` to 0 while the trigger remains
    /// mandatory.
    pub(crate) optionality: ModalOptionality,
}

/// CR 608.2c: Whether a modal header makes its enclosing triggered ability
/// optional. This remains distinct from a modal's `min_choices`, which models
/// how many modes a mandatory ability may select.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum ModalOptionality {
    Mandatory,
    MayDecline,
}

// --- ActivatedConstraintAst (moved from oracle.rs) ---

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct ActivatedConstraintAst {
    pub(crate) restrictions: Vec<ActivationRestriction>,
    /// CR 602.2a: Who may begin to activate this ability.
    pub(crate) activator_filter: Option<PlayerFilter>,
    /// CR 602.2: "Any player may activate this ability." — annotation recognized
    /// during parsing. Lowered to `activator_filter = All` on `AbilityDefinition`.
    pub(crate) any_player_may_activate: bool,
}
