use crate::types::ability::{
    AbilityDefinition, CountValue, DamageAmount, Duration, Effect, ManaProduction,
    ManaSpendRestriction, PaymentCost, QuantityExpr, QuantityRef, StaticDefinition, TargetFilter,
};
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;
use crate::types::mana::ManaCost;
use crate::types::zones::Zone;

/// Convert a DamageAmount to a QuantityExpr for the DealDamage effect.
pub(super) fn damage_amount_to_quantity(da: &DamageAmount) -> QuantityExpr {
    match da {
        DamageAmount::Fixed(n) => QuantityExpr::Fixed { value: *n },
        DamageAmount::Variable(s) => QuantityExpr::Ref {
            qty: QuantityRef::Variable { name: s.clone() },
        },
    }
}

/// Convert a QuantityExpr back to DamageAmount for DamageAll (which still uses DamageAmount).
pub(super) fn quantity_to_damage_amount(q: &QuantityExpr) -> DamageAmount {
    match q {
        QuantityExpr::Fixed { value } => DamageAmount::Fixed(*value),
        QuantityExpr::Ref {
            qty: QuantityRef::Variable { name: ref s },
        } => DamageAmount::Variable(s.clone()),
        _ => DamageAmount::Variable(format!("{q:?}")),
    }
}

/// Convert a LifeAmount to a QuantityExpr for the GainLife effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedEffectClause {
    pub(super) effect: Effect,
    pub(super) duration: Option<Duration>,
    /// Compound "and" remainder parsed into a sub_ability chain.
    pub(super) sub_ability: Option<Box<AbilityDefinition>>,
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
    pub(super) count: CountValue,
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
pub(super) enum ClauseAst {
    Imperative {
        text: String,
    },
    SubjectPredicate {
        subject: SubjectPhraseAst,
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
    },
    RevealHandFilter {
        card_filter: TargetFilter,
    },
    ManaRestriction {
        restriction: ManaSpendRestriction,
    },
    CounterSourceStatic {
        source_static: StaticDefinition,
    },
    /// "create a ... token and suspect it" → chain Suspect { target: LastCreated }
    SuspectLastCreated,
    /// CR 701.15: "It can't be regenerated" / "They can't be regenerated" — sets
    /// `cant_regenerate: true` on the preceding Destroy/DestroyAll effect.
    CantRegenerate,
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
    Investigate,
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
        amount: i32,
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
    },
    Fight {
        target: TargetFilter,
    },
    GainControl {
        target: TargetFilter,
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
        token: TokenDescription,
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
    LookAtHand { target: TargetFilter },
    RevealHand,
    RevealTop { count: u32 },
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
    },
    TopOfLibrary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ShuffleImperativeAst {
    ShuffleLibrary {
        target: TargetFilter,
    },
    /// "shuffle and put that card on top" — shuffle, then place the parent target on top.
    ShuffleAndPutOnTop,
    ChangeZoneToLibrary,
    ChangeZoneAllToLibrary {
        origin: Zone,
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
        amount: DamageAmount,
        target: TargetFilter,
        all: bool,
    },
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
        unless_payment: Option<ManaCost>,
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
    }
}

pub(super) fn with_clause_duration(
    mut clause: ParsedEffectClause,
    duration: Duration,
) -> ParsedEffectClause {
    if clause.duration.is_none() {
        clause.duration = Some(duration.clone());
    }
    if let Effect::GenericEffect {
        duration: effect_duration,
        ..
    } = &mut clause.effect
    {
        if effect_duration.is_none() {
            *effect_duration = Some(duration);
        }
    }
    clause
}
