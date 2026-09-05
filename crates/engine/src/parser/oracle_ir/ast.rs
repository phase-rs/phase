use serde::Serialize;

use crate::parser::oracle_nom::enters_under::ControlClausePossessor;
use crate::types::ability::MultiTargetSpec;
use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, ActivationRestriction, BounceSelection,
    CastingPermission, ChosenCounterCountCondition, ControlWindow, ControllerRef,
    CopyRetargetPermission, CounterAdjustment, CounterKindChooser, CounterKindDomain,
    CounterSourceRider, DigRestOrder, DoorLockOp, Duration, Effect, EffectScope, FaceDownProfile,
    ForceBlockAttackerRef, LibraryPosition, ManaProduction, ManaSpendRestriction, ManaTargetRole,
    ModalSelectionConstraint, OutsideGameSourcePool, PlayerFilter, PtStat, PtValue, QuantityExpr,
    SearchDestinationSplit, SearchSelectionConstraint, SpellStackToGraveyardReplacement,
    StaticCondition, StaticDefinition, SubAbilityLink, TargetFilter, ThisWayCause,
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
    /// CR 608.2c ("read the whole text and apply the rules of English to the
    /// text"): the subject the predicate applies to, or `None` when the
    /// sentence printed a subject the subject grammar could not bind.
    ///
    /// **`Option`, not a permissive default (issue #6965).** Both sites that
    /// re-derive a subject phrase used to substitute `TargetFilter::Any` when
    /// [`super::SubjectApplication`] could not be produced. `TargetFilter::Any`
    /// matches unconditionally (`game/filter.rs`), so a parse FAILURE emitted a
    /// BOARD-WIDE effect — the grant landed on every permanent, lands and
    /// artifacts included, while coverage still reported the card as supported.
    /// Encoding the unbound state in the type makes that fail-open
    /// unrepresentable: every consumer must say what it does with `None`.
    /// `lower_subject_predicate_ast`'s `ImperativeFallback` arm — the only
    /// predicate kind that applies the subject filter — is the only consumer
    /// that treats `None` as a coverage GAP, failing closed to
    /// `Effect::unimplemented`. The other readers
    /// (`sync_subject_into_nested_shuffle_sub`, `inject_subject_target`) reach
    /// it through `target.or(affected)` and treat `None` as "nothing to
    /// rebind", returning early. `None` is therefore reachable in all three —
    /// do not assume otherwise when editing them. Same shape, same reason, as
    /// [`EntersUnderSpec::UnboundAnaphor`].
    pub(crate) affected: Option<TargetFilter>,
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

/// CR 110.2a: the resolved battlefield-entry
/// controller for a zone change, as the IR carries it.
///
/// Three states, not two: `Default` (no explicit controller override in the
/// IR; lowering carries it as `None` to the existing resolver), `Override` (a
/// bound controller), and `UnboundAnaphor` — a control clause that WAS printed
/// but whose antecedent the parser cannot name. The third state is what keeps a
/// dropped `"under their control"` from silently degrading into the existing
/// fallback representation; the lowering sites turn it into an honest
/// `Effect::unimplemented` instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub(crate) enum EntersUnderSpec {
    #[default]
    Default,
    Override(ControllerRef),
    /// Carries the possessor so a lowering site with no text in scope
    /// (`lower_put_ast`) can still emit a verbatim printed fragment.
    UnboundAnaphor(ControlClausePossessor),
}

impl EntersUnderSpec {
    /// The bound controller, or `None` for both no-override and an unbound
    /// anaphor. Callers that can fail closed MUST check
    /// [`Self::unbound_possessor`] first — this method deliberately collapses
    /// the two `None` cases so the guard has to be written explicitly.
    pub(crate) fn as_controller_ref(&self) -> Option<ControllerRef> {
        match self {
            EntersUnderSpec::Override(r) => Some(r.clone()),
            EntersUnderSpec::Default | EntersUnderSpec::UnboundAnaphor(_) => None,
        }
    }

    /// `Some(possessor)` exactly when a control clause was printed but could not
    /// be bound — the fail-closed signal.
    pub(crate) fn unbound_possessor(&self) -> Option<ControlClausePossessor> {
        match self {
            EntersUnderSpec::UnboundAnaphor(p) => Some(*p),
            EntersUnderSpec::Default | EntersUnderSpec::Override(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum ContinuationAst {
    SearchDestination {
        destination: Zone,
        /// CR 701.23a: When true, the searched card enters the battlefield tapped.
        enter_tapped: bool,
        /// CR 110.2a: the battlefield-entry
        /// controller for the searched card. `Default` lowers through the
        /// existing no-override carrier; `UnboundAnaphor` fails closed.
        enters_under: EntersUnderSpec,
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
        restrictions: Vec<ManaSpendRestriction>,
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
    /// CR 116.2c + CR 608.2c: "You may pay {W} to end this effect." — later text
    /// modifying the continuous effect an EARLIER clause of the same chain
    /// created (CR 608.2c: "later text may modify earlier text"). Stamps
    /// `end_cost` on the bound `Effect::GenericEffect` antecedent.
    ///
    /// Fully absorbed: it emits no def of its own, because CR 116.2c grants a
    /// later SPECIAL ACTION rather than performing anything on resolution. The
    /// mandatory `Effect::PayCost` this replaces was flatly wrong at runtime —
    /// it force-paid the cost the moment the ability resolved.
    EndEffectCost { cost: crate::types::mana::ManaCost },
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
    /// "Put those cards/the chosen cards on top ..." after a search/dig/choice
    /// producer.
    /// Count is supplied by the already-selected target set.
    PutChosenCardsAtLibraryPosition { position: LibraryPosition },
    /// CR 701.23a + CR 608.2c: "exile the rest" after a multi-zone search.
    /// The searched player's cards in the searched zones, excluding the cards
    /// selected by the SearchLibrary choice, are moved to exile.
    ExileSearchRemainder,
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
        /// CR 400.5 + CR 608.2c: Only exact "in a random order" text
        /// randomizes the unchosen library remainder.
        #[serde(default, skip_serializing_if = "DigRestOrder::is_preserve")]
        rest_order: DigRestOrder,
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
        /// CR 400.5 + CR 608.2c: Only exact "in a random order" text sets
        /// `Random`; every other accepted form preserves existing behavior.
        #[serde(default)]
        rest_order: DigRestOrder,
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
        /// CR 508.4: Kept cards enter attacking when the from-among clause
        /// says "onto the battlefield ... attacking".
        #[serde(default)]
        enters_attacking: bool,
        /// CR 701.20a vs 701.20e: True when the from-among clause's stripped verb
        /// was "reveal" (a public action) rather than "put"/"choose" (a private
        /// look). Promotes the patched Dig to `reveal: true` even when the kept
        /// cards route to a fixed library position (Fertile Thicket).
        #[serde(default)]
        reveal_verb: bool,
        /// CR 608.2c: The producer action named by an explicit tracked-set suffix
        /// such as "milled this way". Generic "from among" selection remains
        /// action-agnostic (`None`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caused_by: Option<ThisWayCause>,
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
    /// CR 202.3 + CR 608.2c: "If its mana value is <comparator> <dynamic
    /// quantity>, put it onto <zone>[. Otherwise, put it into <zone>]." after
    /// RevealUntil — a card-property branch on the hit card's own mana value
    /// (Part in Friendship), distinct from the player-choice `RevealUntilKept
    /// { optional_decline }` shape. Absorbs into `kept_destination_if`
    /// (the `if_true` branch) and, when the trailing "otherwise" clause is
    /// present in the same sentence, `kept_destination` (the "otherwise"
    /// branch) as well.
    RevealUntilConditionalKept {
        filter: Box<TargetFilter>,
        if_true_destination: Zone,
        otherwise_destination: Option<Zone>,
    },
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
    /// CR 701.70a + CR 608.2c: Recruit — draw, discard, then create the
    /// contingent Soldier token when the card discarded by this instruction was
    /// nonland. This remains a parser IR node because lowering must build the
    /// three-step, direct-child chain rather than introduce a card-specific
    /// runtime effect.
    Recruit,
    /// CR 205.1a + CR 205.1b + CR 613.1d + CR 110.2a + CR 122.1: `assimilate
    /// <target phrase>` (Borg Queen, Perfection Manifest — Star Trek Commander).
    ///
    /// The keyword action's definition is supplied ONLY by reminder text, which
    /// is stripped before the parser runs, so it is encoded here rather than
    /// parsed: "Put it onto the battlefield under your control with a +1/+1
    /// counter. It's a Borg artifact creature and loses all other creature
    /// types."
    ///
    /// Like `Recruit`, this remains a parser IR node because lowering must build
    /// a two-step, direct-child chain (`ChangeZone` + a `Duration::Permanent`
    /// `GenericEffect` bound to the parent target) rather than introduce a
    /// card-specific runtime effect. The chain it lowers to is the SAME shape
    /// the reanimate-then-retype class already produces from spelled-out Oracle
    /// text (Ashen Powder's move + Rise from the Grave's type rider).
    ///
    /// CR 205.1b: "becomes a '[creature type or types] artifact creature'"
    /// RETAINS all prior card types, supertypes, and non-creature subtypes and
    /// REPLACES only the creature types — so the lowering emits additive
    /// `AddType`s plus a creature-set-scoped subtype replacement, never
    /// `SetCardTypes`.
    ///
    /// CR 611.2e: because the definition uses the "is [characteristic]" form
    /// ("It's a Borg artifact creature"), the rule requires the type change to
    /// apply SIMULTANEOUSLY with the permanent entering the battlefield. The
    /// engine installs it after entry via the shared `ChangeZone` +
    /// `GenericEffect` continuation, so an ETB trigger keying on the new
    /// characteristics does not see them. This is a PRE-EXISTING, CLASS-WIDE
    /// deviation shared with Rise from the Grave and Grave Betrayal (both also
    /// the "is" form); Puppeteer Clique's "It gains haste" is the "gains" form
    /// and is correct. Not introduced here, and out of scope to fix.
    ///
    /// `assimilate` has NO CR 701.x keyword-action number: the set is
    /// unreleased and `docs/MagicCompRules.txt` has zero matches for it. Do not
    /// invent one, and do not reuse Recruit's `CR 701.70a` — that number is
    /// Recruit's, not assimilate's.
    Assimilate {
        /// The card the keyword action moves — "target creature card from an
        /// opponent's graveyard" (CR 115.2: a non-battlefield-zone target).
        target: TargetFilter,
    },
    /// CR 509.1c: Block this turn/combat if able.
    ForceBlock {
        attacker: Option<ForceBlockAttackerRef>,
        duration: Duration,
    },
    /// CR 508.1d + CR 506.3: Attack a required DEFENDER this turn/combat if
    /// able. The `required_defender` filter selects whom the forced attacker
    /// must attack — `TargetFilter::Controller` for "attacks you",
    /// `ControllerRef::ChosenPlayer { index }` for "attacks that player" (the
    /// opponent chosen by a preceding "choose an opponent" instruction in the
    /// same resolution, e.g. Ruhan of the Fomori), or `TargetFilter::SelfRef`
    /// for a permanent defender ("attack ~ if able" — Gideon Jura, whose
    /// required defender is the planeswalker itself).
    ForceAttack {
        /// `None` is the WINDOWLESS form ("attack ~ if able" — Gideon Jura),
        /// whose span is stated by an enclosing clause ("During target
        /// opponent's next turn, …") and applied by the clause machinery.
        /// `Some` carries the window the predicate states for itself.
        duration: Option<Duration>,
        required_defender: TargetFilter,
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
    /// CR 106.1b + CR 602.2b: "note the type of mana spent to pay this
    /// activation cost" (Jeweled Amulet). Field-less: there is nothing to
    /// select — the payment already happened, so the effect is a pure
    /// readback recorded at resolution. Scoped to the singular-type wording;
    /// Ice Cauldron's "note the type AND AMOUNT..." sibling is intentionally
    /// left unmatched (see `parse_imperative_family_ast`).
    NoteManaSpent,
    /// CR 701.40a: Manifest the top card(s) of library.
    Manifest {
        target: TargetFilter,
        count: QuantityExpr,
        /// CR 701.40a: Source discriminant, mirroring `Cloak.from_zone`:
        /// `None` manifests the top `count` cards of `target`'s library;
        /// `Some(zone)` manifests a card the controller chooses from that zone
        /// (Scroll of Fate's "manifest a card from your hand"), which lowers
        /// to a `ChooseFromZone` parent + `Manifest { object_source }`
        /// sub-chain.
        from_zone: Option<Zone>,
        /// CR 701.40a + CR 608.2c: when `Some`, manifest these SPECIFIC
        /// objects directly with no further selection — "manifest them" /
        /// "manifest those cards" (Ghastly Conscription, Jeskai Infiltrator),
        /// which read the chain's already-formed face-down pile (an earlier
        /// "exile ... in a face-down pile" step's published tracked set).
        /// Mutually exclusive with `from_zone` (which still requires an
        /// interactive choice); `None` preserves the two source forms above.
        object_source: Option<TargetFilter>,
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
        /// CR 110.2a: the battlefield-entry
        /// controller. `Override(r)` routes the object to the player resolved
        /// from `r`; `Default` lowers through the existing no-override carrier;
        /// `UnboundAnaphor` marks a printed control clause whose antecedent
        /// could not be named, and the lowering site turns it into an honest
        /// `Effect::unimplemented` rather than a silently-wrong controller.
        enters_under: EntersUnderSpec,
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
        /// CR 701.3a + CR 303.4f/i: Optional "attached to <host>" rider on the
        /// return (Gift of Immortality, Next of Kin, Lynde). When set, lowering
        /// nests `Effect::Attach { SelfRef → host }` under the ChangeZone with
        /// `forward_result` so the Aura enters attached and skips the CR 303.4f
        /// host-choice consult.
        attach_host: Option<TargetFilter>,
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
        /// CR 110.2a: the battlefield-entry
        /// controller for mass returns. `Default` preserves default controller
        /// assignment; `UnboundAnaphor` fails closed at the lowering site.
        enters_under: EntersUnderSpec,
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
        /// CR 701.27a vs CR 115.10a: `Single` is the legacy targeted/anaphoric
        /// transform; `All` is the non-targeting mass transform ("transform all
        /// Humans"). Mirrors `Effect::Transform`'s `scope` axis.
        scope: EffectScope,
    },
    /// CR 710.4: the Kamigawa flip-card instruction ("flip this creature" /
    /// "flip it" / "flip <name>"). A sibling of [`UtilityImperativeAst::Transform`]
    /// rather than a parameterization of it because CR 701.27a and CR 710 are
    /// different game actions with different copiable-value semantics
    /// (CR 710.1c holds color and mana cost fixed).
    FlipPermanent {
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
        target_a_multi_target: Option<MultiTargetSpec>,
        target_b: Box<TargetFilter>,
        target_b_multi_target: Option<MultiTargetSpec>,
    },
    /// CR 608.2d + CR 122.1: "choose a counter on it / that permanent" — pick one
    /// of the distinct counter kinds present on the anaphoric object (The Caves
    /// of Androzani II/III). Lowered to `Effect::ChooseCounterKind`. `target` is
    /// the anaphor (`ParentTarget` for the per-iteration object).
    ///
    /// `domain` and `chooser` carry the second surface form of the same
    /// instruction — "a kind of counter at random ... from among <list>"
    /// (Crystalline Giant), whose population is printed on the card and whose
    /// pick is made by the game. Both default to the on-target/controller
    /// reading, so the anaphoric form above is unchanged.
    CounterKind {
        target: TargetFilter,
        domain: CounterKindDomain,
        chooser: CounterKindChooser,
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
        /// CR 110.2a: the battlefield-entry
        /// controller. `Override(r)` routes the object to the player resolved
        /// from `r`; `Default` lowers through the existing no-override carrier;
        /// `UnboundAnaphor` marks a printed control clause whose antecedent
        /// could not be named, and the lowering site turns it into an honest
        /// `Effect::unimplemented` rather than a silently-wrong controller.
        enters_under: EntersUnderSpec,
        /// CR 603.6d: "enters tapped" — enters the battlefield tapped.
        enter_tapped: bool,
        /// CR 712.14a: "transformed" — enters with its back face up.
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
        /// CR 110.2a: the battlefield-entry
        /// controller for the moved population. `UnboundAnaphor` fails closed.
        enters_under: EntersUnderSpec,
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
    /// CR 701.24a: Lowers straight to `Effect::Shuffle { target }`. `target`
    /// is usually a player-resolving filter ("shuffle your library"), but
    /// CR 608.2c's "shuffle that pile" / "shuffle those piles" reuses this
    /// same variant with a `TrackedSet` sentinel target — the resolver's
    /// `Effect::Shuffle` arm already dispatches on whether `target` names a
    /// player or a tracked object set.
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
        /// CR 601.2c: Role-scoped player targets for this mana production
        /// (recipient and/or count source). This is a TRANSPORT field on the
        /// cost-resource intermediate AST — it carries whatever role
        /// `try_parse_add_mana_effect_with_context` stamped, unchanged, and
        /// `lower_cost_resource_ast` puts it back on `Effect::Mana`. It must
        /// mirror `Effect::Mana::target`'s type exactly: re-deciding or
        /// flattening the role here would silently drop a count source on the
        /// cost-resource path.
        target: Option<ManaTargetRole>,
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
        /// CR 401.2 + CR 701.13a: the library's ordered top or bottom edge
        /// selects the cards this exile instruction moves.
        position: LibraryPosition,
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
        target_condition: Option<ChosenCounterCountCondition>,
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
        /// CR 115.1d + CR 122.1: A fixed `each of N <permanents>` phrase is
        /// an untargeted resolution-time selection, not N target slots.
        exact_selection: Option<u32>,
        /// CR 601.2c: A fixed "each of N target <objects>" phrase keeps its
        /// announced target cardinality instead of becoming an untargeted
        /// resolution-time selection.
        multi_target: Option<MultiTargetSpec>,
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
    /// CR 122.1 + CR 603.2c: "put the same number and kind of counters" / "put
    /// one of each of those kinds of counters" — reproduce the triggering
    /// event's counters onto `target`. Lowered to `Effect::ReproduceEventCounters`.
    ReproduceEventCounters {
        target: TargetFilter,
        per_kind_count: crate::types::ability::EventCounterReproductionCount,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum ClauseBoundary {
    Sentence,
    Then,
    Comma,
}

/// CR 608.2c: the SINGLE translation from the printed boundary that precedes a
/// clause to the link the clause carries to the one before it. A `Sentence`
/// boundary marks the next printed instruction (`SequentialSibling`); a
/// `Comma`/`Then`/absent boundary marks a within-clause `ContinuationStep`.
///
/// Two passes need this mapping and MUST agree: `assemble_effect_chain` stamps
/// `AbilityDefinition::sub_link` with it, and the referent walk
/// (`parser::oracle_effect::chain_prior_referent_is_created_token`) predicts,
/// while still in `ClauseIr` space, whether the clauses it walks past will be
/// continuation steps. Keeping the match here is what makes those two the same
/// rule rather than two copies of it.
pub(crate) fn sub_link_after_boundary(boundary: Option<ClauseBoundary>) -> SubAbilityLink {
    match boundary {
        Some(ClauseBoundary::Sentence) => SubAbilityLink::SequentialSibling,
        Some(ClauseBoundary::Then) | Some(ClauseBoundary::Comma) | None => {
            SubAbilityLink::ContinuationStep
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ClauseChunk {
    pub(crate) text: String,
    pub(crate) boundary_after: Option<ClauseBoundary>,
    /// CR 611.2a + CR 608.2c: a duration stated on the sentence this chunk was split
    /// out of, whose PHRASE this chunk's text does not itself carry. `None` = the chunk
    /// carries its own duration text, or states none. Set only by
    /// `sequence::expand_leading_duration_chunks`; consumed by the chunk loop's pending
    /// stamp. Typed rather than re-synthesized as prose so every chunk stays a contiguous
    /// substring of the printed text and `ClauseIrBuilder::locate` keeps honest spans.
    pub(crate) leading_duration: Option<Duration>,
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

/// CR 611.2a: "A continuous effect generated by the
/// resolution of a spell or ability lasts as long as stated by the spell or ability
/// creating it." A duration field that already holds a value the parser deliberately
/// wrote IS a stated window, and a governing prefix must not overwrite it.
///
/// THE RULE: a duration carrier is "unset" iff it holds `None` or
/// `Some(Duration::Permanent)`. Every other value is treated as explicitly written
/// and is preserved.
///
/// The single authority for that question. It applies to BOTH carriers of a governed
/// node — `AbilityDefinition.duration` (the enclosing clause's window) and the
/// effect's OWN embedded `duration` field — so the clause path
/// (`with_clause_duration`), the `sub_ability`-chain path
/// (`with_clause_chain_duration`) and the trailing-duration peel in
/// `oracle_effect/mod.rs` cannot drift apart. Ask the question through this function;
/// do not spell the expression out inline again.
///
/// SCOPE, so the authority is not overstated: it governs the `apply_duration_to_effect`
/// writers whose embedded field is `Option<Duration>` (`GenericEffect`,
/// `CastFromZone`, `BecomeCopy`, `GainActivatedAbilitiesOfTarget`) plus the two
/// carrier gates. `GrantCastingPermission { PlayFromExile }` is DELIBERATELY
/// unguarded: its `duration` is a NON-`Option` `Duration` (`types/ability.rs`), so it
/// has no unset sentinel to test — the same reason `ForceAttack` and `ForceBlock` get
/// no arm at all. Widening those three fields to `Option<Duration>` would bring every
/// writer under one rule; that is a serialized-`Effect`-shape change and is deferred.
/// `duration_arms_match_governed_set` pins the writer/non-writer split.
///
/// # Why the unset set is exactly these two values
///
/// * `None` — no recognizer wrote the field, so nothing was stated.
///
///   THIS HALF IS LOAD-BEARING, not a formality. Xanathar, Guild Kingpin's
///   `CastFromZone` link reaches this predicate with BOTH its carriers unset, and it
///   is the leading "Until end of turn," that must reach it: with no stated duration
///   the play permission would otherwise last for the rest of the game (CR 611.2a's
///   second sentence). A guard narrowed to `Some(Permanent)` alone would silently
///   drop it. `xanathar_unset_cast_window_still_takes_the_leading_duration`
///   (`tests/integration/leading_duration_distribution_7923.rs`) is the row that
///   turns red if anyone narrows it.
///
/// * `Some(Duration::Permanent)` — the sub-parser sentinel MEANING "no duration
///   stated", written by `build_become_clause` in `oracle_effect/subject.rs` as
///   `let duration = duration.or(Some(Duration::Permanent));` when its own trailing
///   peel finds no window. It is already the sentinel named by
///   `with_clause_duration`'s comment and by every gate this function replaces, so
///   including it is consistency, not novelty. It is also the value
///   `game/effects/become_copy.rs` resolves an absent embedded window to
///   (`.unwrap_or(Duration::Permanent)`), which is why for `BecomeCopy` the two
///   members of this set are one runtime value.
///
/// A future `Duration` variant is therefore treated as EXPLICIT by default — the safe
/// side. Do NOT turn this into an exhaustive `match`.
///
/// # `Some(Duration::UntilEndOfTurn)` is DELIBERATELY NOT in the unset set
///
/// Some recognizers inject it as a default that is byte-identical to a printed "until
/// end of turn". Enumerate those sites with
/// `rg 'duration: Some\(Duration::UntilEndOfTurn\)' crates/engine/src/parser/ | grep -v tests`.
/// Distinguishing an injected default from a printed window is
/// <https://github.com/phase-rs/phase/issues/7962> and is OUT OF SCOPE here: this
/// predicate NEITHER FIXES NOR CLAIMS TO FIX that class.
///
/// The consequence, stated as the rule's own cost: a node whose recognizer injected
/// `UntilEndOfTurn` will decline a governing prefix of a different window. Whether
/// that cost is paid anywhere in the corpus today is a MEASUREMENT, and it lives in
/// the PR body for #7959 — not here, where it would go stale silently. The intended
/// treatment is written down as the `ge_ueot` row in
/// `duration_distribution_tests_7923::narrower_printed_window_survives_a_wider_outer_duration`;
/// that row DOCUMENTS the boundary and discriminates nothing (its outer window is
/// itself `UntilEndOfTurn`), and it is the row to flip when #7962 lands.
///
/// The established remedy, when such a site DOES become load-bearing, is site-local
/// default removal — as this change's sibling already did at
/// `try_parse_gain_all_activated_abilities_of_target`.
///
/// Returns `bool`, matching its neighbour `duration_governs`: this is a predicate, not
/// a data field, and a two-state enum here would be a bool in a costume.
pub(crate) fn duration_is_unset_sentinel(duration: &Option<Duration>) -> bool {
    duration.is_none() || matches!(duration, Some(Duration::Permanent))
}

/// CR 608.2c + CR 611.2a: The honest gap a duration reconciliation emits when it
/// would otherwise drop a printed batch bound.
///
/// Every reconciliation seam (`with_clause_duration`, `reconcile_coordinated_cast`,
/// and the trailing-duration fixup in `oracle_effect/mod.rs`) builds its refusal
/// here, so all three name the same gap and carry the same diagnostic. The
/// description records the bound that was about to be lost rather than an Oracle
/// fragment, because these seams run after lowering and no longer hold the source
/// text — and the bound is the load-bearing fact for anyone auditing the gap.
pub(crate) fn cast_bound_lost_to_duration_gap(
    bounds: crate::types::ability::ResolutionCastWindow,
) -> Effect {
    Effect::unimplemented(
        crate::types::ability::CAST_BOUND_LOST_TO_DURATION_GAP,
        format!(
            "cast bound the duration-scoped lingering permission cannot carry: {}",
            bounds.describe_bound()
        ),
    )
}

/// CR 611.2a + CR 608.2g + CR 608.2c: Carry a sentence's LEADING duration onto a
/// later coordinated clause of that same sentence.
///
/// A leading duration scopes the whole coordinated predicate it introduces, not
/// just its first conjunct. Magus of the Mind prints "Until end of turn, you may
/// play lands **and** cast spells from among cards exiled this way without paying
/// their mana costs": one duration, two coordinated instructions.
/// `split_clause_sequence` cuts that sentence into the chunks
/// `"Until end of turn, you may play lands"` (boundary `Comma`) and
/// `"cast spells from among cards exiled this way without paying their mana
/// costs"`, so `with_clause_duration` — which only ever sees the chunk the
/// duration was printed on — reconciles the FIRST of the two and nothing else.
/// The cast half was then lowered from a fragment in which no duration token is
/// visible at any position, so `from_among_batch_cast_driver`'s in-clause scan
/// could not see one either, and the card became a resolution-scoped window
/// although its own rulings state the opposite ("You must follow the normal
/// timing permissions and restrictions for cards you cast this way" / "Any of the
/// cards you don't play will remain in exile") — the lingering signature.
///
/// The sentence-scoped grouping is the same one CR 107.3i's `where X is` binding
/// uses (`compute_sentence_leading_duration` / `compute_sentence_where_x`), so
/// the two cannot drift apart on what "the same sentence" means.
///
/// Scoped narrowly to `Effect::CastFromZone`, because that is the only effect
/// whose *mechanism* (`CastFromZoneDriver`) is decided from the local clause
/// fragment and therefore cannot see the sentence head. Every other effect either
/// carries its own duration slot (stamped by `with_clause_duration` on the chunk
/// that printed the duration) or is instantaneous, and blanket-stamping a
/// duration onto arbitrary later chunks would change effects the coordination
/// does not scope.
pub(crate) fn apply_sentence_duration_to_coordinated_casts(
    clause: &mut ParsedEffectClause,
    duration: &Duration,
) {
    reconcile_coordinated_cast(&mut clause.effect, &mut clause.duration, duration);
    if let Some(sub) = clause.sub_ability.as_deref_mut() {
        apply_sentence_duration_to_coordinated_cast_defs(sub, duration);
    }
}

/// The `AbilityDefinition` half of the walk: a clause's compound-"and" remainder
/// belongs to the same printed sentence, so the same duration scopes it.
fn apply_sentence_duration_to_coordinated_cast_defs(
    def: &mut AbilityDefinition,
    duration: &Duration,
) {
    let (effect, def_duration) = (def.effect.as_mut(), &mut def.duration);
    reconcile_coordinated_cast(effect, def_duration, duration);
    if let Some(next) = def.sub_ability.as_deref_mut() {
        apply_sentence_duration_to_coordinated_cast_defs(next, duration);
    }
    if let Some(alt) = def.else_ability.as_deref_mut() {
        apply_sentence_duration_to_coordinated_cast_defs(alt, duration);
    }
}

/// CR 611.2a: stamp the sentence's duration on one cast grant and reconcile its
/// mechanism with it. A duration the clause stated for ITSELF always wins.
fn reconcile_coordinated_cast(
    effect: &mut Effect,
    node_duration: &mut Option<Duration>,
    duration: &Duration,
) {
    let Effect::CastFromZone {
        duration: effect_duration,
        driver,
        ..
    } = effect
    else {
        return;
    };
    // CR 611.2a: the permission expires with the stated duration rather than
    // standing indefinitely (`duration: None` on this grant means "castable until
    // the cards leave exile").
    if effect_duration.is_none() {
        *effect_duration = Some(duration.clone());
    }
    if node_duration.is_none() {
        *node_duration = Some(duration.clone());
    }
    // CR 611.2a + CR 608.2g: a stated duration means the controller casts at a
    // LATER priority window, which is the defining property of a lingering
    // permission — so a resolution-scoped window degrades back to one. Same
    // authority `with_clause_duration` uses for the single-chunk case, including
    // its refusal: a sentence-leading duration that reaches a coordinated cast
    // conjunct whose window printed a cast cap or a CR 202.3 running-total
    // budget cannot silently drop it into a countless per-object permission.
    match driver.with_lingering_duration() {
        Some(reconciled) => *driver = reconciled,
        None => {
            let bounds = driver.window_bounds().unwrap_or_default();
            *effect = cast_bound_lost_to_duration_gap(bounds);
        }
    }
}

pub(crate) fn with_clause_duration(
    mut clause: ParsedEffectClause,
    duration: Duration,
) -> ParsedEffectClause {
    // CR 611.2a: a leading duration from Oracle text (e.g. "Until end of turn, ...")
    // is authoritative for `clause.duration` — the CARRIER is written unconditionally here.
    // On the effect's OWN embedded duration field it overrides only the parser's unset
    // sentinels (`None`, and `build_become_clause`'s `Some(Permanent)`): since #7959,
    // `apply_duration_to_effect` decides that second carrier per arm through
    // `duration_is_unset_sentinel`, and any other value was written deliberately and survives.
    clause.duration = Some(duration.clone());
    apply_duration_to_effect(&mut clause.effect, &duration);
    clause
}

/// CR 611.2a: "A continuous effect generated by the
/// resolution of a spell or ability lasts as long as stated by the spell or ability
/// creating it (such as 'until end of turn')."
///
/// The single authority for writing a stated duration into an effect's OWN embedded
/// duration field. Extracted from `with_clause_duration` so the clause path and the
/// `sub_ability`-chain path (`with_clause_chain_duration`) can never drift.
///
/// # Deliberate exclusions (read before adding an arm)
///
/// FOUR members of `duration_governs` deliberately have NO arm here, for THREE
/// different reasons. Note that this function is called from `with_clause_duration`,
/// i.e. from EVERY existing call site of that function — not only from
/// `with_clause_chain_duration`. An arm added here takes effect corpus-wide.
///
/// * `ForceAttack` / `ForceBlock` — NO UNSET SENTINEL. Both carry
///   `duration: Duration` (NON-Option), so a printed window ("each combat if able")
///   and the `default_duration_until_end_of_turn` serde fallback are
///   indistinguishable here, and writing them would clobber the printed window
///   (Silver Surfer, Galactus's Herald). They ARE in `duration_governs`, which
///   stamps `AbilityDefinition.duration` — but the two are NOT symmetric downstream:
///   - `ForceAttack`: `force_attack::resolve` applies
///     `ability.duration.clone().unwrap_or_else(|| duration.clone())`, so the
///     enclosing clause's duration already wins at runtime.
///   - `ForceBlock`: `force_block::resolve` reads the duration ONLY out of
///     `ability.effect` and never consults `ability.duration`. Its membership is
///     therefore a KNOWINGLY INERT STAMP today, kept for set-completeness and to
///     make the asymmetry visible. Repair (teach it `force_attack`'s precedence, or
///     widen both fields to `Option<Duration>`) is a separate change; blast radius
///     measured at 5 cards, all currently UntilEndOfTurn-under-UntilEndOfTurn.
///
/// * `PreventDamage` — REDUNDANT **AND** UNSAFE. `prevent_damage.rs` already
///   implements the yield-to-explicit precedence under a CR 611.2a comment naming
///   both carriers: it reads `prevention_duration` first and falls back to
///   `ability.duration` — the `.or_else` arm the comment itself calls out as the one
///   that makes Suppressor Skyguard correct. Membership in `duration_governs` stamps
///   exactly that carrier, so an arm here would be redundant (measured: membership
///   ALONE corrects the exported AST of Dovin, Hand of Control and Kiora, the
///   Crashing Wave). And it would be unsafe: this function's arms overwrite
///   UNCONDITIONALLY unless the arm carries an unset-sentinel guard, and
///   `PreventDamage` has no sentinel to guard on — its `prevention_duration` is
///   `None` when unstated, which is indistinguishable at this seam from "governed
///   but not yet stamped". (The `GainActivatedAbilitiesOfTarget` arm IS guarded,
///   on `duration_is_unset_sentinel`; see it for the shape a guard takes when
///   the variant does have a distinguishable sentinel.) Regenerate the population with the census command on
///   `duration_governs`; at the time of writing it is 47 nodes across 46 cards with a
///   printed `prevention_duration`, of which 46 are `UntilEndOfTurn` and exactly ONE
///   — `sewers of estark`, "prevent all combat damage that would be dealt THIS
///   COMBAT ..." — is `UntilEndOfCombat`. Do NOT read that as "the one node an added
///   arm would visibly clobber": measured, its `UntilEndOfTurn` ancestor comes from a
///   TRAILING peel on a different printed sentence, and this function's chain-walk
///   caller reaches only LEADING-duration seams, so ZERO printed-window nodes in the
///   corpus are reachable by an added arm today. `sewers of estark` is a named
///   BYTE-IDENTITY control in the corpus gate; the arm is DISCRIMINATED by the
///   directly-constructed hostile row in `duration_arms_match_governed_set`. Writing
///   would also permanently disable the `.or_else` arm for that node.
///
/// * `AddRestriction` — NO EMBEDDED FIELD. Its expiry derives from the enclosing
///   `AbilityDefinition.duration` via `add_restriction::fill_runtime_fields`
///   (CR 514.2).
///
/// The catch-all covers every effect whose only duration lives on the enclosing
/// `AbilityDefinition`. A future effect with an embedded duration field MUST be
/// classified into `duration_governs` and, if it has an unset sentinel AND no
/// runtime reader of `ability.duration`, given an arm here. ENUMERATE WITH THE
/// COMMAND ON `duration_governs`, DO NOT ENUMERATE BY EYE — that is how
/// `PreventDamage` was missed once already.
/// See `duration_arms_match_governed_set`.
///
/// Four of the five writers above are guarded on `duration_is_unset_sentinel`, the
/// single authority for "this duration carrier is unset" (CR 611.2a): an
/// explicitly written embedded window survives a governing prefix. The fifth,
/// `GrantCastingPermission { PlayFromExile }`, is unguarded because its `duration`
/// is a non-`Option` `Duration` and therefore has no unset sentinel to test. A new
/// duration-bearing variant whose field DOES have a distinguishable sentinel must
/// be guarded the same way. (`duration_arms_match_governed_set` pins the 9/5 split.)
fn apply_duration_to_effect(effect: &mut Effect, duration: &Duration) {
    // Both gap outcomes are recorded here and applied AFTER the match, so the
    // borrow of `*effect` taken by the arms has ended before it is replaced.
    // CR 608.2c (#8174): the printed cast bound the selected driver cannot carry.
    let mut refused_bound: Option<crate::types::ability::ResolutionCastWindow> = None;
    // CR 611.2a (#7959): the inner lifetime condition the engine cannot evaluate.
    let mut unevaluable_lifetime: Option<String> = None;
    match effect {
        // CR 611.2a: yield to an explicitly written
        // inner duration. The two parser-default sentinels (`None`,
        // `Some(Permanent)`) still take the governing prefix; any other value was
        // deliberately written by the recognizer that built this effect and IS a
        // stated window. `duration_is_unset_sentinel` is the single authority for
        // that distinction — see its doc.
        //
        // WHY EACH ARM IS GUARDED, and why the three are NOT one guarantee. The
        // precedence below was read at each resolver, not inferred:
        //
        //   * `GenericEffect` — `game/effects/effect.rs` resolves the window as
        //     `ability.duration.or(embedded)`, so the CARRIER wins, and
        //     `with_clause_chain_duration` writes that carrier on the very node whose
        //     embedded write this guard declines. The guard therefore corrects the
        //     EXPORTED PROVENANCE (card-data, the coverage report, the semantic
        //     audit, the client's parse overlay) and does NOT yet change the
        //     installed window. Closing that half needs the chain walk to decline the
        //     CARRIER too, which is blocked on
        //     <https://github.com/phase-rs/phase/issues/7962>. Do NOT read this arm as
        //     a runtime guarantee, and do NOT delete it as inert: it is what makes the
        //     exported AST honest, and it is what the #7962 fix will build on.
        //   * `CastFromZone` — `cast_from_zone::resolve` takes the window ONLY out of
        //     `ability.effect` and never consults `ability.duration`, and that same
        //     field gates its driver selection. RUNTIME-LOAD-BEARING: without the
        //     guard, a governing prefix silently rewrites the play window AND the
        //     driver choice.
        //   * `BecomeCopy` — `game/effects/become_copy.rs` resolves the window as
        //     `embedded.or(ability.duration).unwrap_or(Permanent)`, so the EMBEDDED
        //     field wins. RUNTIME-LOAD-BEARING for the same reason.
        //
        // Do NOT restate a declined write as "a no-op under an `UntilEndOfTurn`
        // head": that is FALSE unless the declined value is itself `UntilEndOfTurn`.
        //
        // The rows that pin each arm are in
        // `duration_distribution_tests_7923::narrower_printed_window_survives_a_wider_outer_duration`
        // and, at the chain-walk level, in
        // `chain_duration_walks_governed_links_and_yields_to_explicit`. The measured
        // corpus blast radius for this change is in the PR body for #7959; it is
        // deliberately not restated here, where it would go stale unnoticed.
        Effect::GenericEffect {
            duration: ref mut effect_duration,
            ..
        } if duration_is_unset_sentinel(effect_duration) => {
            *effect_duration = Some(duration.clone());
        }
        Effect::GrantCastingPermission {
            permission:
                CastingPermission::PlayFromExile {
                    duration: perm_dur, ..
                },
            ..
        } => {
            *perm_dur = normalize_play_from_exile_duration(duration.clone());
        }
        // RECONCILED SEAM (#7959 x #8174). Both rules live here and neither is
        // allowed to shadow the other; the ORDER below is the reconciliation:
        //
        //  1. CR 608.2g driver reconciliation (current main, #8174) runs
        //     UNCONDITIONALLY and FIRST. A leading duration states that the
        //     permission is exercised at a later priority window, which is the
        //     defining property of a lingering permission — that judgement is about
        //     the DRIVER and is independent of whatever this PR's guard later
        //     decides about the embedded window. Running it first preserves #8174's
        //     behaviour exactly, including its CR 608.2c refusal.
        //  2. CR 611.2a strict-fail (#7959) for an inner lifetime the engine cannot
        //     evaluate — see `masked_outer_bound_fragment`.
        //  3. CR 611.2a guarded write (#7959): an explicitly printed embedded window
        //     survives; only the unset sentinels take the governing prefix.
        //
        // PRECEDENCE when both 1 and 2 would gap the node: main's bound-refusal
        // wins. It names the specific bound that was lost, which strictly dominates
        // this PR's "lifetime not understood" as a diagnostic.
        Effect::CastFromZone {
            duration: ref mut effect_duration,
            ref mut driver,
            ..
        } => {
            match driver.with_lingering_duration() {
                Some(reconciled) => *driver = reconciled,
                None => refused_bound = Some(driver.window_bounds().unwrap_or_default()),
            }
            if refused_bound.is_none() {
                if let Some(fragment) = masked_outer_bound_fragment(effect_duration) {
                    unevaluable_lifetime = Some(fragment);
                } else if duration_is_unset_sentinel(effect_duration) {
                    *effect_duration = Some(duration.clone());
                }
            }
        }
        // Same rule as the siblings above, asked through the shared authority: see
        // `duration_is_unset_sentinel`.
        // CR 611.2a: yield to an explicitly stated inner duration, exactly as the
        // `sub_ability` walk in `with_clause_chain_duration` and the trailing-duration
        // peel in `oracle_effect/mod.rs` do. TWO parser sites construct this variant
        // and they differ: `imperative.rs`'s Symbiote Spider-Man arm sets
        // `Some(Duration::Permanent)` to MEAN "no duration stated", which a stated
        // outer duration must replace; but `imperative.rs`'s "gain all activated
        // abilities of" arm emits the printed trailing window recovered by
        // `strip_trailing_duration` verbatim — a genuinely PRINTED inner window that
        // an outer duration must NOT clobber, and `None` when nothing was printed.
        // Gating on the unset sentinels distinguishes the two.
        // NOTE: unit-covered only. No card in the corpus reaches this arm today;
        // Mondo Gecko and Navigator's Compass, which LOOK like this case, are
        // `Effect::GenericEffect` from `build_become_clause` and take the
        // GenericEffect arm above.
        // A failed guard falls through to the `_ => {}` arm below, i.e. the node is
        // left exactly as printed — which is the intended yield-to-explicit result.
        Effect::GainActivatedAbilitiesOfTarget {
            duration: ref mut effect_duration,
            ..
        } if duration_is_unset_sentinel(effect_duration) => {
            *effect_duration = Some(duration.clone());
        }
        Effect::BecomeCopy {
            duration: ref mut effect_duration,
            recipient,
            ..
        } => {
            // CR 611.2b + CR 301.5: a leading "for as long as ~
            // remains attached to it" binds a singular become-copy to the
            // attachment host. UNCONDITIONAL — this rewrite must run whether or not
            // the duration write below is declined, which is exactly why the guard
            // is on the ASSIGNMENT and not on the match arm. Normalizing this into
            // an arm guard silently loses the binding;
            // `become_copy_recipient_rewrite_survives_a_declined_duration_write`
            // turns red if anyone does.
            // The duration is stripped before the body is parsed, so this is the
            // first point where both the copy and its final duration are available.
            if matches!(
                duration,
                Duration::ForAsLongAs {
                    condition: StaticCondition::RecipientMatchesFilter {
                        filter: TargetFilter::AttachedTo,
                    },
                }
            ) && *recipient == TargetFilter::SelfRef
            {
                *recipient = TargetFilter::AttachedTo;
            }
            // CR 611.2a: yield to an explicitly written window, same rule as the
            // siblings above. `become_copy::resolve` reads this field FIRST, so an
            // unguarded write here would install the wrong copy window, not merely
            // mislabel the export.
            if duration_is_unset_sentinel(effect_duration) {
                *effect_duration = Some(duration.clone());
            }
        }
        _ => {}
    }
    // Applied in precedence order: main's specific bound-refusal dominates this
    // PR's generic "lifetime not understood" gap when both fire.
    if let Some(bounds) = refused_bound {
        *effect = cast_bound_lost_to_duration_gap(bounds);
    } else if let Some(fragment) = unevaluable_lifetime {
        *effect = Effect::unimplemented("cast_from_zone_unevaluable_lifetime", fragment);
    }
}

/// CR 611.2a: a stated duration governs the lifetime of the effect it
/// prefixes. This is the COMPLETE set. DERIVE IT FROM THE COMMAND, NOT BY EYE:
///
/// ```text
/// awk '/^pub enum Effect \{/,/^\}/' crates/engine/src/types/ability.rs \
///   | rg -n 'duration: (Option<)?Duration'
/// ```
///
/// At the time of writing that returns SEVEN duration-bearing variants —
/// GenericEffect, CastFromZone, BecomeCopy, GainActivatedAbilitiesOfTarget,
/// ForceAttack, ForceBlock, and PreventDamage (whose field is named
/// `prevention_duration`, which is exactly why an eye-enumeration missed it).
/// All seven are members. Two more are members without an embedded field:
/// `GrantCastingPermission { permission: CastingPermission::PlayFromExile { .. } }`,
/// and `AddRestriction`, whose expiry derives from `AbilityDefinition.duration`
/// in `add_restriction::fill_runtime_fields` (CR 514.2).
///
/// FOUR of the nine deliberately get NO `apply_duration_to_effect` arm —
/// `AddRestriction`, `ForceAttack`, `ForceBlock`, `PreventDamage`; four of the
/// five writers are guarded on `duration_is_unset_sentinel`. Read that
/// function's doc for the three distinct reasons before changing either set.
/// (CR 615 is the prevention-effects section `PreventDamage` implements.)
///
/// It does NOT include one-shot instructions: a duration does not govern them, and
/// an unrestricted walk was measured to write `AbilityDefinition.duration` onto
/// `CreateDelayedTrigger` (37 cards), `PutCounter` (36) and `RegisterBending` (36).
pub(crate) fn duration_governs(effect: &Effect) -> bool {
    // EXHAUSTIVE BY CONSTRUCTION — no `_` arm. A new `Effect` variant does not
    // compile until someone decides, here, whether a printed duration governs it.
    // That decision is the whole point: the silent-loss class this distribution
    // authority exists to prevent is a duration-bearing variant that no table
    // mentions, which a wildcard would answer `false` for without anyone noticing.
    // The cost is a long ungoverned list; the list is the guard.
    match effect {
        // GOVERNED. Read `apply_duration_to_effect`'s doc for which of these take the
        // window on their EMBEDDED field and which take it only on the carrier.
        Effect::GenericEffect { .. }
        | Effect::CastFromZone { .. }
        | Effect::BecomeCopy { .. }
        | Effect::GainActivatedAbilitiesOfTarget { .. }
        | Effect::ForceAttack { .. }
        | Effect::ForceBlock { .. }
        | Effect::PreventDamage { .. }
        | Effect::AddRestriction { .. } => true,

        // Governed only in its `PlayFromExile` shape: that permission carries its own
        // `duration`, which `normalize_play_from_exile_duration` maps (CR 400.7i).
        // The other permissions have no window to place.
        Effect::GrantCastingPermission { permission, .. } => {
            matches!(permission, CastingPermission::PlayFromExile { .. })
        }

        // UNGOVERNED — every remaining variant, named rather than swept into `_`.
        Effect::StartYourEngines { .. }
        | Effect::ChangeSpeed { .. }
        | Effect::DealDamage { .. }
        | Effect::ApplyPostReplacementDamage { .. }
        | Effect::EachDealsDamageEqualToPower { .. }
        | Effect::EachSourceDealsDamage { .. }
        | Effect::Draw { .. }
        | Effect::Pump { .. }
        | Effect::PairWith { .. }
        | Effect::Destroy { .. }
        | Effect::Regenerate { .. }
        | Effect::RemoveAllDamage { .. }
        | Effect::Counter { .. }
        | Effect::CounterAll { .. }
        | Effect::Token { .. }
        | Effect::GainLife { .. }
        | Effect::LoseLife { .. }
        | Effect::SetTapState { .. }
        | Effect::RemoveCounter { .. }
        | Effect::Sacrifice { .. }
        | Effect::DiscardCard { .. }
        | Effect::Mill { .. }
        | Effect::Scry { .. }
        | Effect::PumpAll { .. }
        | Effect::DamageAll { .. }
        | Effect::DamageEachPlayer { .. }
        | Effect::DestroyAll { .. }
        | Effect::ChangeZone { .. }
        | Effect::ChangeZoneAll { .. }
        | Effect::Dig { .. }
        | Effect::GainControl { .. }
        | Effect::GainControlAll { .. }
        | Effect::ControlNextTurn { .. }
        | Effect::Attach { .. }
        | Effect::UnattachAll { .. }
        | Effect::Surveil { .. }
        | Effect::Fight { .. }
        | Effect::Bounce { .. }
        | Effect::BounceAll { .. }
        | Effect::Explore
        | Effect::ExploreAll { .. }
        | Effect::Investigate
        | Effect::Tribute { .. }
        | Effect::TimeTravel
        | Effect::BecomeMonarch { .. }
        | Effect::NoOp
        | Effect::Proliferate
        | Effect::ProliferateTarget { .. }
        | Effect::Populate
        | Effect::Clash
        | Effect::Behold { .. }
        | Effect::EndTheTurn
        | Effect::EndCombatPhase
        | Effect::Vote { .. }
        | Effect::SeparateIntoPiles { .. }
        | Effect::SwitchPT { .. }
        | Effect::CopySpell { .. }
        | Effect::EpicCopy { .. }
        | Effect::CastCopyOfCard { .. }
        | Effect::CopyTokenOf { .. }
        | Effect::CreateTokenCopyFromPool { .. }
        | Effect::Myriad
        | Effect::Encore
        | Effect::CombineHost { .. }
        | Effect::ChooseAugmentAndCombineWithHost { .. }
        | Effect::Meld { .. }
        | Effect::ExileHaunting { .. }
        | Effect::HideawayConceal { .. }
        | Effect::CopyTokenBlockingAttacker { .. }
        | Effect::ChoosePermanent { .. }
        | Effect::ChooseCard { .. }
        | Effect::PutCounter { .. }
        | Effect::ChooseCounterKind { .. }
        | Effect::PutChosenCounter { .. }
        | Effect::PutCounterAll { .. }
        | Effect::MultiplyCounter { .. }
        | Effect::ChooseCounterAdjustment { .. }
        | Effect::DoublePT { .. }
        | Effect::DoublePTAll { .. }
        | Effect::MoveCounters { .. }
        | Effect::ReproduceEventCounters { .. }
        | Effect::Animate { .. }
        | Effect::ReturnAsAura { .. }
        | Effect::RegisterBending { .. }
        | Effect::Cleanup { .. }
        | Effect::Mana { .. }
        | Effect::Discard { .. }
        | Effect::Shuffle { .. }
        | Effect::Transform { .. }
        | Effect::FlipPermanent { .. }
        | Effect::SearchLibrary { .. }
        | Effect::SearchOutsideGame { .. }
        | Effect::OpenBoosterPack { .. }
        | Effect::RevealHand { .. }
        | Effect::RevealFromHand { .. }
        | Effect::Reveal { .. }
        | Effect::RevealChosenNumbers { .. }
        | Effect::RevealTop { .. }
        | Effect::ExileTop { .. }
        | Effect::ExileFaceDownPile { .. }
        | Effect::TargetOnly { .. }
        | Effect::Choose { .. }
        | Effect::OpponentGuess { .. }
        | Effect::SwapChosenLabels { .. }
        | Effect::ChooseDamageSource { .. }
        | Effect::Suspect { .. }
        | Effect::Unsuspect { .. }
        | Effect::Connive { .. }
        | Effect::PhaseOut { .. }
        | Effect::PhaseIn { .. }
        | Effect::SolveCase
        | Effect::BecomePrepared { .. }
        | Effect::BecomeUnprepared { .. }
        | Effect::BecomeSaddled { .. }
        | Effect::SetClassLevel { .. }
        | Effect::CreateDelayedTrigger { .. }
        | Effect::AddTargetReplacement { .. }
        | Effect::ReduceNextSpellCost { .. }
        | Effect::GrantNextSpellAbility { .. }
        | Effect::AddPendingETBCounters { .. }
        | Effect::AddPendingEntersModifications { .. }
        | Effect::CreateEmblem { .. }
        | Effect::PayCost { .. }
        | Effect::FreeCastFromZones { .. }
        | Effect::ExileResolvingSpellInsteadOfGraveyard { .. }
        | Effect::CreateDamageReplacement { .. }
        | Effect::CreateDrawReplacement { .. }
        | Effect::CreatePlaneswalkReplacement { .. }
        | Effect::LoseTheGame { .. }
        | Effect::WinTheGame { .. }
        | Effect::RollDie { .. }
        | Effect::FlipCoin { .. }
        | Effect::FlipCoins { .. }
        | Effect::FlipCoinUntilLose { .. }
        | Effect::RingTemptsYou
        | Effect::VentureIntoDungeon
        | Effect::VentureInto { .. }
        | Effect::TakeTheInitiative
        | Effect::ArrangePlanarDeckTop { .. }
        | Effect::Planeswalk
        | Effect::ChaosEnsues
        | Effect::ReverseTurnOrder
        | Effect::RedistributeLifeTotals
        | Effect::OpenAttractions { .. }
        | Effect::RollToVisitAttractions
        | Effect::AssembleContraptions { .. }
        | Effect::AssembleContraptionsFromRollDifference
        | Effect::CrankContraptions { .. }
        | Effect::ReassembleContraption { .. }
        | Effect::AssembleContraptionOnSprocket { .. }
        | Effect::ReassembleContraptionOnSprocket { .. }
        | Effect::PutSticker { .. }
        | Effect::ApplySticker { .. }
        | Effect::ProcessRadCounters
        | Effect::ChooseFromZone { .. }
        | Effect::RememberCard { .. }
        | Effect::NoteManaSpent
        | Effect::ForEachCategory { .. }
        | Effect::ChooseObjectsIntoTrackedSet { .. }
        | Effect::ChooseAndSacrificeRest { .. }
        | Effect::EachPlayerCopyChosen { .. }
        | Effect::Exploit { .. }
        | Effect::GainEnergy { .. }
        | Effect::GivePlayerCounter { .. }
        | Effect::LoseAllPlayerCounters { .. }
        | Effect::ExileFromTopUntil { .. }
        | Effect::RevealUntil { .. }
        | Effect::Discover { .. }
        | Effect::Heist { .. }
        | Effect::HeistExile
        | Effect::Cascade
        | Effect::Ripple { .. }
        | Effect::MiracleCast { .. }
        | Effect::MadnessCast { .. }
        | Effect::PutAtLibraryPosition { .. }
        | Effect::ChooseDrawnThisTurnPayOrTopdeck { .. }
        | Effect::PutOnTopOrBottom { .. }
        | Effect::GiftDelivery { .. }
        | Effect::Goad { .. }
        | Effect::GoadAll { .. }
        | Effect::Detain { .. }
        | Effect::SetRoomDoorLock { .. }
        | Effect::ExchangeControl { .. }
        | Effect::ChangeTargets { .. }
        | Effect::Manifest { .. }
        | Effect::ManifestDread
        | Effect::Cloak { .. }
        | Effect::TurnFaceUp { .. }
        | Effect::TurnFaceDown { .. }
        | Effect::ExtraTurn { .. }
        | Effect::GrantExtraLoyaltyActivations { .. }
        | Effect::SkipNextTurn { .. }
        | Effect::SkipNextStep { .. }
        | Effect::AdditionalPhase { .. }
        | Effect::Double { .. }
        | Effect::RuntimeHandled { .. }
        | Effect::Incubate { .. }
        | Effect::Amass { .. }
        | Effect::Monstrosity { .. }
        | Effect::Specialize
        | Effect::Renown { .. }
        | Effect::Bolster { .. }
        | Effect::Adapt { .. }
        | Effect::Learn
        | Effect::Forage
        | Effect::CompletePlayerAction { .. }
        | Effect::Harness
        | Effect::CollectEvidence { .. }
        | Effect::Endure { .. }
        | Effect::BlightEffect { .. }
        | Effect::Seek { .. }
        | Effect::SetLifeTotal { .. }
        | Effect::ExchangeLifeWithStat { .. }
        | Effect::ExchangeLifeTotals { .. }
        | Effect::SetDayNight { .. }
        | Effect::GiveControl { .. }
        | Effect::RemoveFromCombat { .. }
        | Effect::BecomeBlocked { .. }
        | Effect::Conjure { .. }
        | Effect::ApplyPerpetual { .. }
        | Effect::Intensify { .. }
        | Effect::DraftFromSpellbook { .. }
        | Effect::ChooseOneOf { .. }
        | Effect::Unimplemented { .. } => false,
    }
}

/// CR 611.2a + CR 608.2c: one stated duration governs the WHOLE
/// instruction it prefixes — "read the whole text and apply the rules of English".
/// When a clause recognizer builds its own sequential sibling chain (Xanathar,
/// Guild Kingpin; Abeyance; Kiora, the Crashing Wave), the duration must reach every
/// governed link, not only the head. Without this, Xanathar's `CastFromZone`
/// play-permission is installed with `duration: None` — a permission that is never
/// pruned (CR 611.2a: "If no duration is stated, it lasts until the end of the
/// game") — and Kiora's "and dealt by" prevention shield is CREATED with the
/// engine's end-of-turn `is_shield` default instead of the printed "until your next
/// turn". (Whether that corrected window is ever OBSERVED is a separate,
/// pre-existing defect: a resolution-created prevention shield hosted on an object
/// is discarded by the next layer pass — CR 613.1's top-of-pass reset — before any
/// damage event consults it. Measured; see the scope-boundary note and its
/// follow-up. This function puts the right value on the right carrier; it does not
/// and cannot fix the flush.)
///
/// Yields to an explicitly stated narrower duration: a link already carrying
/// `Some(d)` with `d != Permanent` had that duration deliberately attached by its
/// own recognizer. `Permanent` is the known sub-parser default sentinel — see
/// `with_clause_duration`'s own comment naming `build_become_clause` — and yields
/// to the printed duration, the same convention the trailing-duration peel uses.
///
/// A governed node has TWO duration carriers: `AbilityDefinition.duration` and the
/// effect's own embedded `duration` field. This walk gates on the FIRST, via
/// `duration_is_unset_sentinel(&def.duration)`. The SECOND is decided per arm inside
/// `apply_duration_to_effect`, by the same predicate — so the two carriers now share
/// one definition of "unset" instead of two inline copies. Declining the CARRIER for
/// a `GenericEffect` whose embedded window is written is the remaining half, and it
/// is blocked on <https://github.com/phase-rs/phase/issues/7962>.
///
/// Walks ONLY `sub_ability` — CR 608.2c: "The controller of the spell or ability
/// follows its instructions in the order written." `else_ability` and
/// `mode_abilities` are deliberately NOT walked: a mode is one of several options
/// of which only the chosen one applies (CR 700.2; CR 700.2c), and an
/// "otherwise" branch is separately-printed alternative text whose own duration, if
/// any, is stated in that text (CR 608.2c's "read the whole text").
pub(crate) fn with_clause_chain_duration(
    clause: ParsedEffectClause,
    duration: Duration,
) -> ParsedEffectClause {
    let mut clause = with_clause_duration(clause, duration.clone());
    let mut link = clause.sub_ability.as_deref_mut();
    while let Some(def) = link {
        // The gate is on the CARRIER only, deliberately. Do NOT add an
        // embedded-duration conjunct here: `apply_duration_to_effect` already decides
        // the embedded field per arm (CR 611.2a), and declining the carrier as well is
        // the `GenericEffect` half that is blocked on
        // <https://github.com/phase-rs/phase/issues/7962> — see
        // `duration_is_unset_sentinel`'s doc. "Completing" this gate here would widen
        // this change's scope silently.
        if duration_governs(&def.effect) && duration_is_unset_sentinel(&def.duration) {
            def.duration = Some(duration.clone());
            apply_duration_to_effect(&mut def.effect, &duration);
        }
        link = def.sub_ability.as_deref_mut();
    }
    clause
}

pub(crate) fn is_play_from_exile_lifetime_duration(duration: &Duration) -> bool {
    matches!(
        duration,
        Duration::ForAsLongAs {
            condition: StaticCondition::Unrecognized { text },
        } if matches!(
            text.as_str(),
            "it remains exiled"
                | "that card remains exiled"
                | "those cards remain exiled"
                | "they remain exiled"
        )
    )
}

/// The first `Unrecognized` condition text inside a lifetime condition, or `None`
/// when every leaf is understood.
///
/// A `StaticCondition::Unrecognized` is a PARSE-FAILURE MARKER, not a stated
/// window: nothing downstream can evaluate it. Recurses through `And`/`Or`/`Not`
/// because the marker nests — `dead man's chest` carries one inside an `And`.
fn unrecognized_condition_text(condition: &StaticCondition) -> Option<&str> {
    match condition {
        StaticCondition::Unrecognized { text } => Some(text.as_str()),
        StaticCondition::And { conditions } | StaticCondition::Or { conditions } => {
            conditions.iter().find_map(unrecognized_condition_text)
        }
        StaticCondition::Not { condition } => unrecognized_condition_text(condition),
        _ => None,
    }
}

/// CR 611.2a: the fragment naming an inner lifetime
/// that MASKS a printed outer window, or `None` when the inner lifetime is either
/// understood or absent.
///
/// "Masks" is literal for an effect whose EMBEDDED duration is the sole runtime
/// authority: if the inner condition cannot be evaluated, nothing ever ends the
/// effect at the outer bound the Oracle text printed, so the permission can outlive
/// it. That is strictly worse than having no inner condition at all, which is why
/// the shape is strict-failed rather than accepted.
///
/// A RECOGNIZED play-from-exile lifetime is NOT masking: it is understood, and
/// `normalize_play_from_exile_duration` maps it to a real window (CR 400.7i).
fn masked_outer_bound_fragment(duration: &Option<Duration>) -> Option<String> {
    let inner = duration.as_ref()?;
    if is_play_from_exile_lifetime_duration(inner) {
        return None;
    }
    match inner {
        Duration::ForAsLongAs { condition } => {
            unrecognized_condition_text(condition).map(str::to_owned)
        }
        _ => None,
    }
}

fn normalize_play_from_exile_duration(duration: Duration) -> Duration {
    match duration {
        duration if is_play_from_exile_lifetime_duration(&duration) => {
            // CR 400.7i + CR 611.2a: exile-play permissions persist until the
            // referenced object leaves exile; zone-exit cleanup removes the
            // object-tagged permission.
            Duration::Permanent
        }
        other => other,
    }
}

// --- Modal types (moved from oracle_modal.rs) ---

/// CR 603.12: The printed instruction a triggered modal's reflexive
/// connector rides on — `"<trigger>, <instruction>. When you do, choose …"`.
///
/// The `"When you do"` connector creates the reflexive triggered ability. The
/// `"you may "` marker only makes its parent instruction optional, so it cannot
/// decide whether a reflexive exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum ReflexiveModalParent {
    /// `"…, you may <instruction>. When you do, choose …"` — a declinable
    /// resolution-time instruction (Caesar, Legion's Emperor). Carries the
    /// printed instruction text with the `"you may "` marker and connector stripped;
    /// `trigger_line` is reduced to the bare trigger condition alongside it.
    MayPay(String),
    /// `"…, <instruction>. When you do, choose …"` — a mandatory instruction
    /// (Cemetery Desecrator). No text is carried: the
    /// instruction stays in `trigger_line`, where the ordinary trigger parser
    /// lowers it as it already does for every non-modal reflexive (Bone
    /// Rattler, Diregraf Horde). Lowering then attaches the modal as that
    /// chain's `WhenYouDo` sub instead of replacing it.
    Mandatory,
}

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
        /// CR 603.12 + CR 700.2b: How the modal choice is introduced.
        ///
        /// `None` is a plain triggered modal (Pip-Boy 3000), where the modal
        /// attaches directly as the trigger's execute. Anything else means a
        /// reflexive connector stands between the trigger and the mode list,
        /// and the modal must ride on the printed instruction before it —
        /// see `ReflexiveModalParent`.
        reflexive_parent: Option<ReflexiveModalParent>,
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
    /// Verbatim mode text before any structural distribution rewrite.
    pub(crate) source_text: String,
    /// Absolute Oracle line for a collected block bullet. Inline `; or` modes
    /// have no independent printed line.
    pub(crate) source_line: Option<usize>,
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

#[cfg(test)]
mod duration_distribution_tests_7923 {
    use super::*;
    use crate::types::ability::{
        AbilityKind, CardPlayMode, CastFromZoneDriver, EffectScope, GameRestriction,
        GrantedAbilityScope, PermissionGrantee, PlayerScope, PreventionAmount, PreventionScope,
        ProhibitedActivity, RestrictionExpiry, RestrictionPlayerScope,
    };
    use crate::types::identifiers::ObjectId;

    /// The stated outer duration every row is stamped with.
    fn outer() -> Duration {
        Duration::UntilEndOfTurn
    }

    fn generic_with(duration: Option<Duration>) -> Effect {
        Effect::GenericEffect {
            static_abilities: Vec::new(),
            duration,
            target: None,
            end_cost: None,
        }
    }

    fn play_from_exile_permission(duration: Duration) -> CastingPermission {
        CastingPermission::PlayFromExile {
            duration,
            // #8180 widened this variant. A duration fixture models a plain cast
            // permission with no alternative cost, matching the parser's own
            // play-from-exile construction in `oracle_effect/mod.rs`.
            mode: CardPlayMode::Cast,
            alt_ability_cost: None,
            granted_to: crate::types::player::PlayerId(0),
            frequency: crate::types::statics::CastFrequency::Unlimited,
            source_id: None,
            invalidation: None,
            exiled_by_ability_controller: None,
            mana_spend_permission: None,
            card_filter: None,
            single_use_group: None,
            single_use: false,
            cast_cost_raise: None,
            land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            // #7948: a self-standing play permission with full cast authority,
            // which is what this duration fixture models — NOT the
            // `LandLookCompanion` half of an alternative-cost grant.
            provenance: crate::types::ability::PlayFromExileProvenance::Impulse,
        }
    }

    fn grant_play_from_exile(duration: Duration) -> Effect {
        Effect::GrantCastingPermission {
            permission: play_from_exile_permission(duration),
            target: TargetFilter::SelfRef,
            grantee: PermissionGrantee::AbilityController,
        }
    }

    fn cast_from_zone(duration: Option<Duration>) -> Effect {
        Effect::CastFromZone {
            target: TargetFilter::Any,
            without_paying_mana_cost: false,
            mode: CardPlayMode::Cast,
            cast_transformed: false,
            alt_ability_cost: None,
            constraint: None,
            duration,
            driver: CastFromZoneDriver::LingeringPermission,
            mana_spend_permission: None,
        }
    }

    /// `cast_from_zone` with an explicit driver, for the reconciled-seam rows.
    fn cast_from_zone_driven(
        duration: Option<Duration>,
        driver: crate::types::ability::CastFromZoneDriver,
    ) -> Effect {
        match cast_from_zone(duration) {
            Effect::CastFromZone {
                target,
                without_paying_mana_cost,
                mode,
                cast_transformed,
                alt_ability_cost,
                constraint,
                duration,
                mana_spend_permission,
                ..
            } => Effect::CastFromZone {
                target,
                without_paying_mana_cost,
                mode,
                cast_transformed,
                alt_ability_cost,
                constraint,
                duration,
                driver,
                mana_spend_permission,
            },
            other => other,
        }
    }

    /// The gap name a clause was replaced by, or `None` if it is not a gap.
    /// Destructuring is permitted by `check-parser-combinators.sh`; only
    /// hand-CONSTRUCTED `Effect::Unimplemented` literals are forbidden.
    fn gap_name(effect: &Effect) -> Option<&str> {
        match effect {
            Effect::Unimplemented { name, .. } => Some(name.as_str()),
            _ => None,
        }
    }

    fn become_copy(duration: Option<Duration>) -> Effect {
        Effect::BecomeCopy {
            target: TargetFilter::Any,
            recipient: TargetFilter::SelfRef,
            duration,
            mana_value_limit: None,
            additional_modifications: Vec::new(),
        }
    }

    fn gain_activated(duration: Option<Duration>) -> Effect {
        Effect::GainActivatedAbilitiesOfTarget {
            target: TargetFilter::Any,
            recipient: TargetFilter::SelfRef,
            scope: GrantedAbilityScope::ActivatedOnly,
            duration,
        }
    }

    fn force_attack(duration: Duration) -> Effect {
        Effect::ForceAttack {
            target: TargetFilter::Any,
            required_defender: TargetFilter::SelfRef,
            duration,
            scope: EffectScope::Single,
        }
    }

    fn force_block(duration: Duration) -> Effect {
        Effect::ForceBlock {
            target: TargetFilter::Any,
            attacker: None,
            duration,
        }
    }

    fn prevent_damage(prevention_duration: Option<Duration>) -> Effect {
        Effect::PreventDamage {
            amount: PreventionAmount::All,
            amount_dynamic: None,
            target: TargetFilter::Any,
            scope: PreventionScope::AllDamage,
            damage_source_filter: None,
            prevention_duration,
        }
    }

    fn add_restriction() -> Effect {
        Effect::AddRestriction {
            restriction: GameRestriction::ProhibitActivity {
                source: ObjectId(0),
                affected_players: RestrictionPlayerScope::AllPlayers,
                expiry: RestrictionExpiry::EndOfTurn,
                activity: ProhibitedActivity::CastSpells { spell_filter: None },
            },
        }
    }

    /// Apply a stated duration to a clone and report whether the EMBEDDED field moved.
    fn stamped_with(effect: &Effect, duration: &Duration) -> Effect {
        let mut e = effect.clone();
        apply_duration_to_effect(&mut e, duration);
        e
    }

    /// Apply the stamp to a clone and report whether the EMBEDDED field moved.
    fn stamped(effect: &Effect) -> Effect {
        stamped_with(effect, &outer())
    }

    /// **RECONCILED SEAM (#7959 x #8174) — `[NEW-UNIT]`, COMPOSITION.**
    /// CR 608.2c + CR 611.2a.
    ///
    /// The `CastFromZone` arm now carries TWO independent gap rules that can fire on
    /// the same node, and the ORDER between them is a decision, not an accident. This
    /// row is the only thing that pins it: every other test in this file and in
    /// `invoke_calamity_free_cast` passes under EITHER order, because each exercises
    /// just one of the two.
    ///
    /// REVERT-FAILING IN THREE DIRECTIONS. Measured, not asserted — each mutation was
    /// applied to `apply_duration_to_effect` and the row that actually went red
    /// recorded, because a "revert-failing" annotation that does not match measured
    /// behaviour is the defect this file's header says was found three times already:
    ///
    /// * SWAP the two steps  -> ROW 1 red. The node is still gapped, and the whole
    ///   rest of the suite stays green; it is gapped under the WRONG NAME, losing the
    ///   specific bound that was dropped. Nothing else in the tree catches this.
    /// * DELETE step 2 (this PR's strict-fail) -> ROW 2 red. Row 1 stays green,
    ///   because the refusal still wins there and still names the bound.
    /// * DELETE step 1 (current main's refusal) -> ROW 1 red, NOT row 3. The
    ///   both-fire node gaps as `cast_from_zone_unevaluable_lifetime`, and row 1
    ///   asserts before row 3 is ever reached — so row 3's status under that mutation
    ///   was NOT observed, and this comment does not claim it.
    ///
    /// PRECEDENCE, and why: current-main's bound-refusal wins. It names the bound the
    /// selected mechanism could not carry, which strictly dominates this PR's generic
    /// "inner lifetime not understood" as a diagnostic for the same node.
    ///
    /// Rows are constructed directly because NO CARD supplies both shapes at once — a
    /// refusable batch bound AND an unevaluable inner lifetime. That is exactly why
    /// the composition needs a unit row: the corpus cannot discriminate it.
    #[test]
    fn reconciled_cast_seam_refuses_the_bound_before_gapping_the_lifetime() {
        use crate::types::ability::{
            CastFromZoneDriver, ResolutionCastWindow, CAST_BOUND_LOST_TO_DURATION_GAP,
        };

        let refusable = CastFromZoneDriver::ResolutionWindow {
            bounds: ResolutionCastWindow {
                max_casts: Some(1),
                max_total_mv: None,
            },
        };
        let unevaluable = Some(Duration::ForAsLongAs {
            condition: StaticCondition::Unrecognized {
                text: "that card remains on top of your library".to_string(),
            },
        });

        // ROW 1 — BOTH rules fire. The ordering assertion.
        let both = stamped_with(
            &cast_from_zone_driven(unevaluable.clone(), refusable),
            &outer(),
        );
        assert_eq!(
            gap_name(&both),
            Some(CAST_BOUND_LOST_TO_DURATION_GAP),
            "CR 608.2c: the bound-refusal runs FIRST and names the lost bound; \
             reordering gaps this node as an unevaluable lifetime and loses that fact"
        );

        // ROW 2 — only this PR's rule applies: the driver reconciles cleanly, so the
        // unevaluable inner lifetime is what gaps the node.
        let lifetime_only = stamped_with(
            &cast_from_zone_driven(unevaluable, CastFromZoneDriver::LingeringPermission),
            &outer(),
        );
        assert_eq!(
            gap_name(&lifetime_only),
            Some("cast_from_zone_unevaluable_lifetime"),
            "CR 611.2a: an inner lifetime the engine cannot evaluate still gaps the node \
             when the driver had no bound to refuse"
        );

        // ROW 3 — only current-main's rule applies: unset inner window, refusable bound.
        let bound_only = stamped_with(&cast_from_zone_driven(None, refusable), &outer());
        assert_eq!(
            gap_name(&bound_only),
            Some(CAST_BOUND_LOST_TO_DURATION_GAP),
            "CR 608.2c: the refusal does not depend on this PR's guard declining"
        );

        // ROW 4 — PAIRED POSITIVE REACH GUARD: neither rule fires, so the node is NOT
        // gapped, the driver is reconciled, and the governing window is written.
        // Without this the three rows above would pass on a seam that gaps everything.
        let ok = stamped_with(
            &cast_from_zone_driven(None, CastFromZoneDriver::LingeringPermission),
            &outer(),
        );
        assert_eq!(
            gap_name(&ok),
            None,
            "reach guard: a clean node is not gapped"
        );
        match &ok {
            Effect::CastFromZone {
                duration, driver, ..
            } => {
                assert_eq!(duration.as_ref(), Some(&outer()), "the window is written");
                assert_eq!(
                    driver,
                    &CastFromZoneDriver::LingeringPermission,
                    "CR 608.2g: the driver is reconciled, not refused"
                );
            }
            other => panic!("expected CastFromZone, got {other:?}"),
        }
    }

    /// **V-U1e — `[NEW-UNIT]`.** CR 611.2a.
    ///
    /// ANCHORED AGAINST MISCLASSIFICATION OF THE NEW HELPERS, NOT AGAINST BASE_SHA:
    /// neither `duration_governs` nor `apply_duration_to_effect` exists at BASE, so
    /// this test cannot "fail at BASE". What it pins is the SPLIT between the FIVE
    /// members that get an `apply_duration_to_effect` arm and the FOUR that
    /// deliberately do not (`AddRestriction`, `ForceAttack`, `ForceBlock`,
    /// `PreventDamage` — see `apply_duration_to_effect`'s doc for the three reasons).
    ///
    /// STATED LIMITATION: this is a HAND-WRITTEN TABLE, not a compile error. A future
    /// `Effect` variant that gains a duration field and is added to NEITHER set is
    /// exactly the case it cannot catch — which is how `PreventDamage` was missed
    /// once already. Enumerate with the command in `duration_governs`'s doc.
    #[test]
    fn duration_arms_match_governed_set() {
        // (label, effect, is_writer)
        let table: Vec<(&str, Effect, bool)> = vec![
            ("GenericEffect", generic_with(None), true),
            (
                "GrantCastingPermission{PlayFromExile}",
                grant_play_from_exile(Duration::Permanent),
                true,
            ),
            ("CastFromZone", cast_from_zone(None), true),
            ("BecomeCopy", become_copy(None), true),
            ("GainActivatedAbilitiesOfTarget", gain_activated(None), true),
            (
                "ForceAttack",
                force_attack(Duration::UntilEndOfCombat),
                false,
            ),
            ("ForceBlock", force_block(Duration::UntilEndOfCombat), false),
            ("PreventDamage", prevent_damage(None), false),
            ("AddRestriction", add_restriction(), false),
        ];

        // Positive reach guard: the table is non-empty and EVERY `duration_governs`
        // member appears in it. A member missing from the table fails here.
        assert_eq!(table.len(), 9, "the governed set has nine members");
        for (label, effect, _) in &table {
            assert!(
                duration_governs(effect),
                "{label} must be a member of duration_governs"
            );
        }
        assert_eq!(
            table.iter().filter(|(_, _, w)| *w).count(),
            5,
            "exactly five writers"
        );

        for (label, effect, is_writer) in &table {
            let after = stamped(effect);
            if *is_writer {
                assert_ne!(
                    &after, effect,
                    "{label} is a writer: apply_duration_to_effect must change its embedded field"
                );
            } else {
                assert_eq!(
                    &after, effect,
                    "{label} deliberately has NO arm: apply_duration_to_effect must leave it alone"
                );
            }
        }

        // A non-member is untouched too (the catch-all's other side).
        let non_member = Effect::Draw {
            count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        };
        assert!(!duration_governs(&non_member));
        assert_eq!(stamped(&non_member), non_member);
    }

    /// **V-U1e's MANDATORY hostile rows.** A link carrying its OWN, NARROWER printed
    /// window under a WIDER outer stated duration must keep the printed one
    /// (CR 611.2a — yield to explicit).
    ///
    /// The `PreventDamage` row is THE discriminating one and is constructed DIRECTLY,
    /// not drawn from the corpus: measured, ZERO printed-window `PreventDamage` nodes
    /// in the corpus are reached by any leading-duration path, so no card supplies
    /// this shape (`sewers of estark`'s `UntilEndOfCombat` sits under a TRAILING peel
    /// on a different printed sentence, which the chain walk never reaches). If an
    /// `apply_duration_to_effect` arm for `PreventDamage` is ever added, THIS row
    /// turns red.
    #[test]
    fn narrower_printed_window_survives_a_wider_outer_duration() {
        // THE discriminating row — the `sewers of estark` shape, built directly.
        let pd = prevent_damage(Some(Duration::UntilEndOfCombat));
        assert_eq!(
            stamped(&pd),
            pd,
            "an added PreventDamage arm would clobber a narrower printed \
             prevention_duration and permanently disable prevent_damage.rs's \
             `.or_else(ability.duration)` fallback for that node"
        );

        // Silver Surfer's printed "each combat if able" — the ForceAttack analogue.
        let fa = force_attack(Duration::UntilEndOfCombat);
        assert_eq!(
            stamped(&fa),
            fa,
            "ForceAttack has no unset sentinel; an arm would clobber the printed window"
        );
        let fb = force_block(Duration::UntilEndOfCombat);
        assert_eq!(stamped(&fb), fb, "same for ForceBlock");

        // Retained but EXPLICITLY NOT the discriminating row: under
        // `UntilEndOfTurn` an added arm would write `UntilEndOfTurn` over
        // `UntilEndOfTurn` and nothing would move.
        let pd_same = prevent_damage(Some(Duration::UntilEndOfTurn));
        assert_eq!(stamped(&pd_same), pd_same);

        // `GainActivatedAbilitiesOfTarget` DOES have an arm, and that arm gates on
        // the unset sentinels. Both sides are asserted here because the two parser
        // construction sites in `oracle_effect/imperative.rs` differ: the "gain all
        // activated abilities of" arm emits the printed trailing window recovered by
        // `strip_trailing_duration` verbatim — a genuinely PRINTED window that must
        // survive — while the Symbiote Spider-Man arm sets `Some(Permanent)` to MEAN
        // "no duration stated", which the outer duration must replace.
        let ga_printed = gain_activated(Some(Duration::UntilEndOfCombat));
        assert_eq!(
            stamped(&ga_printed),
            ga_printed,
            "a printed inner window on GainActivatedAbilitiesOfTarget must survive a \
             wider outer duration — ungating this arm silently widens the grant"
        );
        let ga_sentinel = gain_activated(Some(Duration::Permanent));
        assert_eq!(
            stamped(&ga_sentinel),
            gain_activated(Some(outer())),
            "the Permanent sentinel MEANS unset and must be overwritten — gating this \
             arm on `is_none()` alone would make it dead code"
        );
        let ga_unset = gain_activated(None);
        assert_eq!(
            stamped(&ga_unset),
            gain_activated(Some(outer())),
            "an unset inner duration takes the outer stated duration"
        );

        // NEW — the three arms this change guards. These rows are constructed
        // DIRECTLY rather than drawn from a card, for the same reason the
        // `PreventDamage` row above is: what they pin — a governed node whose embedded
        // window is WRITTEN and DIFFERS from the window governing it — is the shape the
        // guard exists to protect, and a unit row states it without depending on any
        // card continuing to print it. (CR 611.2a: an explicitly written window
        // is a stated one.) The corpus survey behind that choice, including which of
        // these three types a card can supply today, is in the PR body for #7959; do
        // not re-derive it from a comment.
        let ge_printed = generic_with(Some(Duration::UntilEndOfCombat));
        assert_eq!(
            stamped(&ge_printed),
            ge_printed,
            "a printed inner window on GenericEffect must survive a wider outer duration"
        );
        let cfz_printed = cast_from_zone(Some(Duration::UntilEndOfCombat));
        assert_eq!(
            stamped(&cfz_printed),
            cfz_printed,
            "a printed inner window on CastFromZone must survive — cast_from_zone::resolve \
             reads ONLY this field, so clobbering it silently rewrites the play window"
        );
        let bc_printed = become_copy(Some(Duration::UntilEndOfCombat));
        assert_eq!(
            stamped(&bc_printed),
            bc_printed,
            "a printed inner window on BecomeCopy must survive — become_copy::resolve reads \
             `embedded.or(carrier)`, so clobbering it silently rewrites the copy window"
        );
        // PAIRED POSITIVE REACH GUARDS: the stamp still fires on both unset
        // sentinels, on all three arms. Without these, the three rows above pass if
        // the arms stopped writing at all.
        assert_eq!(stamped(&generic_with(None)), generic_with(Some(outer())));
        assert_eq!(
            stamped(&generic_with(Some(Duration::Permanent))),
            generic_with(Some(outer())),
            "the Permanent sentinel MEANS unset — gating on is_none() alone makes this dead code"
        );
        assert_eq!(
            stamped(&cast_from_zone(None)),
            cast_from_zone(Some(outer()))
        );
        assert_eq!(
            stamped(&cast_from_zone(Some(Duration::Permanent))),
            cast_from_zone(Some(outer()))
        );
        assert_eq!(stamped(&become_copy(None)), become_copy(Some(outer())));
        assert_eq!(
            stamped(&become_copy(Some(Duration::Permanent))),
            become_copy(Some(outer())),
            "`become_copy::resolve` resolves an absent window with `.unwrap_or(Permanent)`, \
             so `Some(Permanent)` here is the same runtime value as `None` and must be admitted"
        );
        // #7962 BOUNDARY — DOCUMENTATION OF INTENT, NOT EVIDENCE. An injected
        // `UntilEndOfTurn` is indistinguishable from a printed one at this seam and is
        // therefore treated as printed. NOTE: `outer()` is ITSELF `UntilEndOfTurn`, so
        // this row holds identically with and without the guard — it DISCRIMINATES
        // NOTHING and must never be cited as evidence that the guard works. Its only
        // job is to make the scope live in the test file, and to be the row to flip
        // when #7962 removes the injections.
        let ge_ueot = generic_with(Some(Duration::UntilEndOfTurn));
        assert_eq!(stamped(&ge_ueot), ge_ueot);
    }

    /// CR 611.2a + CR 611.2b + CR 301.5.
    ///
    /// The `BecomeCopy` arm carries TWO obligations under one governing window: the
    /// unconditional attachment-host rewrite, and the duration write that must yield
    /// to an explicitly printed inner window. The guard therefore sits on the
    /// ASSIGNMENT, not on the match arm.
    ///
    /// REVERT-FAILING BOTH WAYS: with the guard on the arm instead, `recipient` stays
    /// `SelfRef` and the attachment binding is silently lost; with no guard at all,
    /// the printed `UntilEndOfCombat` is overwritten by the `ForAsLongAs` window and
    /// `become_copy::resolve` — which reads `embedded.or(carrier)` — installs it.
    #[test]
    fn become_copy_recipient_rewrite_survives_a_declined_duration_write() {
        let attach = Duration::ForAsLongAs {
            condition: StaticCondition::RecipientMatchesFilter {
                filter: TargetFilter::AttachedTo,
            },
        };

        // PAIRED POSITIVE REACH GUARD, first: on an unset embedded field the arm
        // does BOTH things. Without this the row below could pass on a dead arm.
        match stamped_with(&become_copy(None), &attach) {
            Effect::BecomeCopy {
                recipient,
                duration,
                ..
            } => {
                assert_eq!(
                    recipient,
                    TargetFilter::AttachedTo,
                    "CR 611.2b rewrite fires"
                );
                assert_eq!(
                    duration,
                    Some(attach.clone()),
                    "an unset window takes the outer one"
                );
            }
            other => panic!("expected BecomeCopy, got {other:?}"),
        }

        // THE DISCRIMINATING ROW: a printed inner window declines the write, and the
        // rewrite must STILL run.
        match stamped_with(&become_copy(Some(Duration::UntilEndOfCombat)), &attach) {
            Effect::BecomeCopy {
                recipient,
                duration,
                ..
            } => {
                assert_eq!(
                    recipient,
                    TargetFilter::AttachedTo,
                    "the CR 611.2b attachment rewrite is UNCONDITIONAL — moving the guard onto \
                     the match arm silently drops it"
                );
                assert_eq!(
                    duration,
                    Some(Duration::UntilEndOfCombat),
                    "the printed inner window survives the governing ForAsLongAs prefix"
                );
            }
            other => panic!("expected BecomeCopy, got {other:?}"),
        }

        // MULTI-AUTHORITY NEGATIVE: under a window that is NOT the attachment
        // duration, the rewrite must NOT fire, and the printed window still survives.
        match stamped_with(&become_copy(Some(Duration::UntilEndOfCombat)), &outer()) {
            Effect::BecomeCopy {
                recipient,
                duration,
                ..
            } => {
                assert_eq!(
                    recipient,
                    TargetFilter::SelfRef,
                    "no attachment window, no rewrite"
                );
                assert_eq!(duration, Some(Duration::UntilEndOfCombat));
            }
            other => panic!("expected BecomeCopy, got {other:?}"),
        }
    }

    /// CR 611.2a: `with_clause_chain_duration` walks ONLY `sub_ability`, stamps only
    /// GOVERNED links, and YIELDS to a link's own explicitly stated narrower duration
    /// while overwriting the `Permanent` sub-parser sentinel.
    #[test]
    fn chain_duration_walks_governed_links_and_yields_to_explicit() {
        let outer = Duration::UntilEndOfTurn;
        let narrower = Duration::UntilNextTurnOf {
            player: PlayerScope::Controller,
        };

        // leaf: a one-shot the duration does NOT govern.
        let leaf = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        // link 5: carrier unset, embedded window WRITTEN — the shape
        // `oracle_effect/subject.rs::try_parse_become_choice` builds.
        let mut written_ge = AbilityDefinition::new(
            AbilityKind::Spell,
            generic_with(Some(Duration::UntilEndOfCombat)),
        );
        written_ge.sub_ability = Some(Box::new(leaf));
        // link 4: the same shape on a `CastFromZone` carrier.
        let mut written_cfz = AbilityDefinition::new(
            AbilityKind::Spell,
            cast_from_zone(Some(Duration::UntilEndOfCombat)),
        );
        written_cfz.sub_ability = Some(Box::new(written_ge));
        // link 3: already carries its own, narrower, stated duration -> untouched.
        let mut explicit = AbilityDefinition::new(AbilityKind::Spell, cast_from_zone(None));
        explicit.duration = Some(narrower.clone());
        explicit.sub_ability = Some(Box::new(written_cfz));
        // link 2: carries the `Permanent` sub-parser sentinel -> overwritten.
        let mut sentinel = AbilityDefinition::new(AbilityKind::Spell, generic_with(None));
        sentinel.duration = Some(Duration::Permanent);
        sentinel.sub_ability = Some(Box::new(explicit));
        // link 1: unset -> stamped.
        let mut first = AbilityDefinition::new(AbilityKind::Spell, cast_from_zone(None));
        first.sub_ability = Some(Box::new(sentinel));

        let head = parsed_clause(add_restriction());
        let mut clause = head;
        clause.sub_ability = Some(Box::new(first));
        let out = with_clause_chain_duration(clause, outer.clone());

        assert_eq!(out.duration, Some(outer.clone()), "head is stamped");
        let l1 = out.sub_ability.as_deref().expect("link 1");
        assert_eq!(
            l1.duration,
            Some(outer.clone()),
            "unset governed link stamped"
        );
        assert_eq!(
            *l1.effect,
            cast_from_zone(Some(outer.clone())),
            "the embedded CastFromZone duration is stamped too"
        );
        let l2 = l1.sub_ability.as_deref().expect("link 2");
        assert_eq!(
            l2.duration,
            Some(outer.clone()),
            "the Permanent sentinel yields to the printed duration"
        );
        let l3 = l2.sub_ability.as_deref().expect("link 3");
        assert_eq!(
            l3.duration,
            Some(narrower),
            "a link with its OWN stated narrower duration is NOT overwritten"
        );
        // link 4 — the shape this change exists for, on a `CastFromZone` carrier: the
        // carrier is unset (so the walk's gate ADMITS the link and stamps it) while the
        // embedded field already holds a written window. The carrier IS stamped; the
        // embedded window SURVIVES (CR 611.2a). This link sits AFTER the
        // declined `explicit` link, so it also pins that the walk ADVANCES past a
        // decline instead of stopping — `link = def.sub_ability.as_deref_mut();` must
        // stay OUTSIDE the gate `if` in `with_clause_chain_duration`.
        let l4 = l3.sub_ability.as_deref().expect("link 4");
        assert_eq!(
            l4.duration,
            Some(outer.clone()),
            "carrier is stamped — the walk reached this link"
        );
        assert_eq!(
            *l4.effect,
            cast_from_zone(Some(Duration::UntilEndOfCombat)),
            "the written embedded window survives; reverting the arm guard writes UntilEndOfTurn here"
        );
        // link 5 — the SAME shape on a `GenericEffect` carrier, which is what
        // `oracle_effect/subject.rs::try_parse_become_choice` builds: an
        // `Effect::GenericEffect` holding its own duration inside an
        // `AbilityDefinition::new(..)` that sets no carrier. Without this link the
        // walk-level evidence would cover only `CastFromZone`.
        let l5 = l4.sub_ability.as_deref().expect("link 5");
        assert_eq!(
            l5.duration,
            Some(outer.clone()),
            "carrier is stamped — the walk advanced past link 4 too"
        );
        assert_eq!(
            *l5.effect,
            generic_with(Some(Duration::UntilEndOfCombat)),
            "the written embedded window survives on GenericEffect; reverting the arm guard \
             writes UntilEndOfTurn here"
        );
        let l6 = l5.sub_ability.as_deref().expect("leaf");
        assert_eq!(
            l6.duration, None,
            "a one-shot the duration does not govern is never stamped"
        );
    }

    /// CR 700.2 + CR 700.2c + CR 608.2c:
    /// `else_ability` and `mode_abilities` are deliberately NOT walked.
    #[test]
    fn chain_duration_does_not_walk_else_or_modes() {
        let else_link = AbilityDefinition::new(AbilityKind::Spell, cast_from_zone(None));
        let mode_link = AbilityDefinition::new(AbilityKind::Spell, cast_from_zone(None));
        let mut sub = AbilityDefinition::new(AbilityKind::Spell, cast_from_zone(None));
        sub.else_ability = Some(Box::new(else_link));
        sub.mode_abilities = vec![mode_link];

        let mut clause = parsed_clause(add_restriction());
        clause.sub_ability = Some(Box::new(sub));
        let out = with_clause_chain_duration(clause, Duration::UntilEndOfTurn);

        let l1 = out.sub_ability.as_deref().expect("sub");
        // Positive reach guard: the sub_ability walk DID reach this link.
        assert_eq!(l1.duration, Some(Duration::UntilEndOfTurn));
        assert_eq!(
            l1.else_ability.as_deref().expect("else").duration,
            None,
            "else_ability is separately-printed alternative text (CR 608.2c)"
        );
        assert_eq!(
            l1.mode_abilities[0].duration, None,
            "only the CHOSEN mode applies (CR 700.2c)"
        );
    }
}
