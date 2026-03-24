use crate::types::ability::MultiTargetSpec;
use crate::types::ability::{
    AbilityDefinition, CastingPermission, Duration, Effect, ManaProduction, ManaSpendRestriction,
    PaymentCost, QuantityExpr, StaticDefinition, TargetFilter, UnlessCost,
};
use crate::types::game_state::DistributionUnit;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;
use crate::types::mana::ManaCost;
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SubjectApplication {
    pub(super) affected: TargetFilter,
    pub(super) target: Option<TargetFilter>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TokenDescription {
    pub(super) name: String,
    pub(super) power: Option<crate::types::ability::PtValue>,
    pub(super) toughness: Option<crate::types::ability::PtValue>,
    pub(super) types: Vec<String>,
    pub(super) colors: Vec<ManaColor>,
    pub(super) keywords: Vec<Keyword>,
    pub(super) tapped: bool,
    pub(super) count: QuantityExpr,
    pub(super) attach_to: Option<TargetFilter>,
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
    pub(super) count: u32,
    pub(super) reveal: bool,
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
        clause: Box<ClauseAst>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SubjectPhraseAst {
    pub(super) affected: TargetFilter,
    pub(super) target: Option<TargetFilter>,
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
        /// When true, the found card enters "attached to" the search source.
        /// Adds forward_result on the ChangeZone and chains an Attach sub_ability.
        attach_to_source: bool,
    },
    RevealHandFilter {
        card_filter: TargetFilter,
    },
    ManaRestriction {
        restriction: ManaSpendRestriction,
    },
    CounterSourceStatic {
        source_static: Box<StaticDefinition>,
    },
    /// "create a ... token and suspect it" → chain Suspect { target: LastCreated }
    SuspectLastCreated,
    /// CR 701.15: "It can't be regenerated" / "They can't be regenerated" — sets
    /// `cant_regenerate: true` on the preceding Destroy/DestroyAll effect.
    CantRegenerate,
    /// "Choose one/N of them" / "An opponent chooses one/N of those cards" after a ChangeZone
    /// to exile → ChooseFromZone { count, zone: Exile, chooser }.
    ChooseFromExile {
        count: u32,
        chooser: crate::types::ability::Chooser,
    },
    /// CR 401.5: "Put them back in any order" / "put the rest on top in any order"
    /// after Dig/RevealTop — the remaining cards go back to the library top.
    /// The ordering choice is handled by the engine's WaitingFor flow.
    PutBackInAnyOrder,
    /// "Put up to N [filter] from among them onto the battlefield/into your hand"
    /// after Dig — patches the Dig's keep_count, filter, destination, and rest_destination.
    DigFromAmong {
        count: u32,
        up_to: bool,
        filter: TargetFilter,
        destination: Zone,
    },
    /// CR 508.4 / CR 614.1: "It/The token enters tapped and attacking [that player]"
    /// Absorbs into preceding CopyTokenOf, Token, or ChangeZone by setting
    /// enters_attacking and tapped/enter_tapped flags.
    EntersTappedAttacking,
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
    /// CR 702.26a: Phase out.
    PhaseOut,
    /// CR 509.1g: Block this turn if able.
    ForceBlock,
    /// CR 701.15a: Goad target creature.
    Goad,
    /// CR 701.12a: Exchange control of two target permanents.
    ExchangeControl,
    /// CR 509.1c: Must be blocked this turn if able.
    MustBeBlocked,
    Investigate,
    /// CR 701.62a: Manifest dread.
    ManifestDread,
    BecomeMonarch,
    Proliferate,
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
    Shuffle(ShuffleImperativeAst),
    Put(PutImperativeAst),
    YouMay {
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum NumericImperativeAst {
    Draw {
        count: u32,
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
        count: u32,
    },
    Surveil {
        count: u32,
    },
    Mill {
        count: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TargetedImperativeAst {
    Tap {
        target: TargetFilter,
    },
    Untap {
        target: TargetFilter,
    },
    Sacrifice {
        target: TargetFilter,
    },
    Discard {
        count: u32,
    },
    /// CR 701.3: Return to hand (bounce).
    Return {
        target: TargetFilter,
    },
    /// CR 400.7: Return to the battlefield (zone change, not bounce).
    ReturnToBattlefield {
        target: TargetFilter,
        /// CR 711.8: "return ... transformed"
        enter_transformed: bool,
    },
    Fight {
        target: TargetFilter,
    },
    GainControl {
        target: TargetFilter,
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
        count: u32,
        reveal: bool,
    },
    Dig {
        count: u32,
    },
    CopyTokenOf {
        target: TargetFilter,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum UtilityImperativeAst {
    Prevent { text: String },
    Regenerate { text: String },
    Copy { target: TargetFilter },
    Transform { target: TargetFilter },
    Attach { target: TargetFilter },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum HandRevealImperativeAst {
    LookAtHand {
        target: TargetFilter,
    },
    RevealHand,
    /// "reveals a number of cards from their hand equal to X" (CR 701.16a).
    RevealPartialHand {
        count: crate::types::ability::QuantityExpr,
    },
    RevealTop {
        count: u32,
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
    Counter {
        target: TargetFilter,
        source_static: Option<Box<StaticDefinition>>,
        unless_payment: Option<UnlessCost>,
    },
    PutCounter {
        counter_type: String,
        count: i32,
        target: TargetFilter,
    },
    RemoveCounter {
        counter_type: String,
        count: i32,
        target: TargetFilter,
    },
    /// CR 121.5: "Put its counters on [target]" — copy all counters from source to target.
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

pub(super) fn parsed_clause(effect: Effect) -> ParsedEffectClause {
    ParsedEffectClause {
        effect,
        duration: None,
        sub_ability: None,
        distribute: None,
        multi_target: None,
    }
}

pub(super) fn with_clause_duration(
    mut clause: ParsedEffectClause,
    duration: Duration,
) -> ParsedEffectClause {
    if clause.duration.is_none() {
        clause.duration = Some(duration.clone());
    }
    match &mut clause.effect {
        Effect::GenericEffect {
            duration: ref mut effect_duration @ None,
            ..
        } => {
            *effect_duration = Some(duration);
        }
        Effect::GrantCastingPermission {
            permission: CastingPermission::PlayFromExile { duration: perm_dur },
            ..
        } => {
            *perm_dur = duration;
        }
        _ => {}
    }
    clause
}
