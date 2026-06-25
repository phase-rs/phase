use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::ability::{
    AbilityCost, CardPlayMode, CastTimingPermission, CostCategory, QuantityExpr, QuantityRef,
    TargetFilter,
};
use super::identifiers::ObjectId;
use super::keywords::{Keyword, KeywordKind};
use super::mana::{ManaColor, ManaCost, StepEndManaAction};
use super::phase::Phase;
use super::player::PlayerId;
use super::zones::Zone;

/// CR 109.5 + CR 102.1: The "who" axis of a continuous prohibition static.
///
/// Shared across the prohibition family (casting, drawing, searching, activating).
/// CR 109.5: The words "you" and "your" on an object refer to the object's controller.
/// CR 102.1: "opponent" is defined relative to a given player's controller.
/// Wire format (`Display` / `FromStr`) is preserved: `"opponents"`, `"all_players"`,
/// `"controller"`, `"enchanted_creature_controller"` — do NOT change these strings,
/// they are serialized into card-data JSON.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProhibitionScope {
    /// "your opponents" — only the controller's opponents are prohibited.
    Opponents,
    /// "players" / "each player" — all players are prohibited.
    AllPlayers,
    /// "you" — only the controller is prohibited.
    Controller,
    /// "enchanted creature's controller" — the controller of the creature this aura enchants.
    /// CR 303.4e: Used by auras that restrict the enchanted creature's controller.
    EnchantedCreatureController,
}

/// Legacy name retained as a type alias during the codebase-wide rename.
/// Prefer `ProhibitionScope` in new code.
pub type CastingProhibitionScope = ProhibitionScope;

impl fmt::Display for ProhibitionScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProhibitionScope::Opponents => write!(f, "opponents"),
            ProhibitionScope::AllPlayers => write!(f, "all_players"),
            ProhibitionScope::Controller => write!(f, "controller"),
            ProhibitionScope::EnchantedCreatureController => {
                write!(f, "enchanted_creature_controller")
            }
        }
    }
}

impl FromStr for ProhibitionScope {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "opponents" => Ok(ProhibitionScope::Opponents),
            "all_players" => Ok(ProhibitionScope::AllPlayers),
            "controller" => Ok(ProhibitionScope::Controller),
            "enchanted_creature_controller" => Ok(ProhibitionScope::EnchantedCreatureController),
            other => Err(format!("unknown ProhibitionScope: {other}")),
        }
    }
}

/// CR 101.2: When the casting prohibition applies.
///
/// The "pronoun-binding" axis is encoded by the choice of `NotDuringYourTurn`
/// vs `NotDuringAffectedPlayersTurn`. CR 109.5 binds the "you/your" pronoun to
/// the static's source controller, while the distributive "their own" reading
/// (from CR 102.1 plus the template structure of "[every player] can [action]
/// only during their own [time]") binds the predicate per-affected caster.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CastingProhibitionCondition {
    /// "during your turn" — prohibition active on controller's turn.
    DuringYourTurn,
    /// "during combat" — prohibition active during any combat phase.
    DuringCombat,
    /// CR 117.1a + CR 604.1: "only during your turn" ≡ "can't cast when it's not your turn"
    /// — prohibition active when it is NOT the controller's turn.
    /// E.g., Fires of Invention: "You can cast spells only during your turn."
    NotDuringYourTurn,
    /// CR 102.1 + CR 117.1a + CR 604.1: "only during their own turn(s)" —
    /// distributive per-affected-player binding. The prohibition is active
    /// whenever it is NOT the *affected* player's turn.
    ///
    /// Contrasts with `NotDuringYourTurn` which binds to the static's source
    /// controller (CR 109.5 — "your turn" on Fires of Invention).
    ///
    /// **Why a separate variant, not a re-use of `NotDuringYourTurn`:**
    /// `NotDuringYourTurn` says "blocked when it is NOT the source-controller's
    /// turn" (CR 109.5). For Dosan ("Players can cast spells only during their
    /// own turns.") the binding is per-affected-caster, not per-source: when
    /// Alice has Dosan on the battlefield and Bob has priority on Alice's turn,
    /// Bob is blocked because it's not *Bob's* turn — Alice's possessive doesn't
    /// reach. The CompRules don't carve out a specific pronoun-binding rule for
    /// "their" the way CR 109.5 governs "you/your"; the distributive reading
    /// follows from CR 102.1 (active player definition) + the template structure
    /// of "[every player] can [action] only during their own [time]".
    NotDuringAffectedPlayersTurn,
    /// CR 117.1: "only any time they could cast a sorcery" — prohibition active when it is
    /// not sorcery speed (main phase + active player's turn + empty stack).
    /// E.g., Teferi, Time Raveler: "Each opponent can cast spells only any time they could
    /// cast a sorcery."
    NotSorcerySpeed,
}

impl fmt::Display for CastingProhibitionCondition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CastingProhibitionCondition::DuringYourTurn => write!(f, "your_turn"),
            CastingProhibitionCondition::DuringCombat => write!(f, "combat"),
            CastingProhibitionCondition::NotDuringYourTurn => write!(f, "not_your_turn"),
            CastingProhibitionCondition::NotDuringAffectedPlayersTurn => {
                write!(f, "not_their_own_turn")
            }
            CastingProhibitionCondition::NotSorcerySpeed => write!(f, "not_sorcery_speed"),
        }
    }
}

impl FromStr for CastingProhibitionCondition {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "your_turn" => Ok(CastingProhibitionCondition::DuringYourTurn),
            "combat" => Ok(CastingProhibitionCondition::DuringCombat),
            "not_your_turn" => Ok(CastingProhibitionCondition::NotDuringYourTurn),
            "not_their_own_turn" => Ok(CastingProhibitionCondition::NotDuringAffectedPlayersTurn),
            "not_sorcery_speed" => Ok(CastingProhibitionCondition::NotSorcerySpeed),
            other => Err(format!("unknown CastingProhibitionCondition: {other}")),
        }
    }
}

/// CR 603.2g + CR 603.6a + CR 700.4: A trigger event whose triggered-ability
/// firing can be suppressed by a `StaticMode::SuppressTriggers` effect.
///
/// Distinct from `GameEvent` (the raw engine event) — this is the narrow set of
/// events for which the MTG rules recognize "enters"/"dies" as a bound category.
/// Other zone-change events (leaves-battlefield, exile, bounce) are not expressed
/// here because no printed card prohibits those specifically in this shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SuppressedTriggerEvent {
    /// CR 603.6a: Enters-the-battlefield triggered abilities.
    /// Does NOT include CR 603.6d static "enters tapped" / "enters with counters"
    /// / "as X enters" effects — those are static, not triggered.
    EntersBattlefield,
    /// CR 700.4: "Dies" means moving from the battlefield to the graveyard.
    /// Narrower than "leaves the battlefield" — does not catch exile or bounce.
    Dies,
}

impl fmt::Display for SuppressedTriggerEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SuppressedTriggerEvent::EntersBattlefield => write!(f, "EntersBattlefield"),
            SuppressedTriggerEvent::Dies => write!(f, "Dies"),
        }
    }
}

/// CR 402.2: How a static ability modifies the maximum hand size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HandSizeModification {
    /// "Your maximum hand size is N." — overrides the base hand size.
    SetTo(u32),
    /// "Your maximum hand size is increased/reduced by N." — adjusts the base hand size.
    AdjustedBy(i32),
    /// "Your maximum hand size is equal to [quantity]." — dynamic quantity from game state.
    EqualTo(QuantityExpr),
}

impl fmt::Display for HandSizeModification {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HandSizeModification::SetTo(n) => write!(f, "SetTo({n})"),
            HandSizeModification::AdjustedBy(n) => write!(f, "AdjustedBy({n})"),
            HandSizeModification::EqualTo(_) => write!(f, "EqualTo(qty)"),
        }
    }
}

/// CR 605.1a: Exemption applied to a `CantBeActivated` prohibition.
///
/// Encodes the "unless they're mana abilities" suffix that appears on
/// activation prohibitions like Pithing Needle. Modeled as a typed enum
/// (not a bool) so the design space is self-documenting and extensible if
/// a future card introduces a new exemption kind — do not add variants
/// until a real card needs them.
///
/// CR 605.1a: A mana ability is an activated ability that has no target, could
/// add mana to a player's mana pool when it resolves, and is not a loyalty
/// ability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum ActivationExemption {
    /// No exemption — every matching activated ability is prohibited.
    #[default]
    None,
    /// "unless they're mana abilities" — activations classified as mana abilities
    /// (CR 605.1a) bypass the prohibition.
    ManaAbilities,
}

impl fmt::Display for ActivationExemption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ActivationExemption::None => write!(f, "none"),
            ActivationExemption::ManaAbilities => write!(f, "mana"),
        }
    }
}

/// CR 118.3 + CR 119.4b + CR 601.2h + CR 602.2b: A non-mana cost payment
/// category prohibited by a static ability.
///
/// This is intentionally cost-scoped. `PayLife` blocks paying life as a cost
/// without preventing damage or other life loss, unlike `CantLoseLife`.
/// `Sacrifice` carries the object filter for the permanents that can't be
/// sacrificed as costs, allowing "sacrifice a permanent" costs to remain
/// payable with legal permanents outside the forbidden filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CostPaymentProhibition {
    PayLife,
    Sacrifice { filter: TargetFilter },
}

impl fmt::Display for CostPaymentProhibition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CostPaymentProhibition::PayLife => write!(f, "PayLife"),
            CostPaymentProhibition::Sacrifice { .. } => write!(f, "Sacrifice"),
        }
    }
}

/// CR 601.2a + CR 601.2b: How often a casting-permission static may be used per turn.
///
/// Replaces the older `once_per_turn: bool` flag on `GraveyardCastPermission` and
/// parameterizes `CastFromHandFree` so every "cast from zone X for free / via alt
/// cost" permission shares a single frequency axis.
///
/// - `Unlimited` — any number of casts per turn from this source (Conduit of Worlds,
///   Crucible of Worlds, Omniscience).
/// - `OncePerTurn` — at most one cast per turn from this source, tracked by the
///   source's `ObjectId` in the corresponding per-turn used-set. CR 400.7: zone
///   change creates a new `ObjectId`, so the permission naturally resets when the
///   source leaves and returns.
/// - `OncePerTurnPerPermanentType` — at most one cast/play per turn from this
///   source **for each permanent type** the consumed card has (CR 110.4 lists
///   the six permanent types). Tracked by the source's `ObjectId` plus the
///   `CoreType` slot consumed in `state.graveyard_cast_permissions_used_per_type`.
///   Muldrotha, the Gravetide is the canonical card: a player may play a land
///   and cast a permanent spell of each permanent type from their graveyard each
///   turn, so each permanent type acts as an independent per-turn slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum CastFrequency {
    /// No per-turn limit — Omniscience, Conduit of Worlds, Crucible of Worlds.
    #[default]
    Unlimited,
    /// At most one cast per turn from this source — Lurrus, Karador, Zaffai.
    OncePerTurn,
    /// CR 110.4 + CR 305.1 + CR 601.2a: Once per turn per permanent type from this
    /// source — Muldrotha, the Gravetide. Lands, creatures, artifacts, enchantments,
    /// planeswalkers, and battles each have an independent per-turn slot tracked
    /// by `(source_id, CoreType)` in `graveyard_cast_permissions_used_per_type`.
    OncePerTurnPerPermanentType,
}

impl CastFrequency {
    pub fn is_unlimited(&self) -> bool {
        matches!(self, CastFrequency::Unlimited)
    }
}

impl fmt::Display for CastFrequency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CastFrequency::Unlimited => write!(f, "unlimited"),
            CastFrequency::OncePerTurn => write!(f, "once_per_turn"),
            CastFrequency::OncePerTurnPerPermanentType => {
                write!(f, "once_per_turn_per_permanent_type")
            }
        }
    }
}

impl FromStr for CastFrequency {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "unlimited" => Ok(CastFrequency::Unlimited),
            "once_per_turn" => Ok(CastFrequency::OncePerTurn),
            "once_per_turn_per_permanent_type" => Ok(CastFrequency::OncePerTurnPerPermanentType),
            // CR 601.2a: Legacy bool-encoded wire format from pre-CastFrequency
            // migration — "true" meant once_per_turn, "false" meant unlimited.
            "true" => Ok(CastFrequency::OncePerTurn),
            "false" => Ok(CastFrequency::Unlimited),
            other => Err(format!("unknown CastFrequency: {other}")),
        }
    }
}

/// CR 601.2a + CR 903.8: Which origin zones a continuous free-cast permission
/// may replace the mana cost from.
///
/// The axis is separate from `CastFrequency`: Omniscience/Zaffai explicitly say
/// "from your hand", while Dracogenesis omits a zone qualifier and therefore
/// also reaches command-zone roles that are already authorized by CR 903.8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum CastFreeOrigin {
    /// Explicit "from your hand" permissions.
    #[default]
    Hand,
    /// No explicit origin qualifier. This does not create a new zone permission;
    /// runtime casting still has to prove the object is in a built-in cast zone
    /// (hand, or an already-authorized command-zone role).
    DefaultCastPermission,
}

impl fmt::Display for CastFreeOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CastFreeOrigin::Hand => write!(f, "hand"),
            CastFreeOrigin::DefaultCastPermission => write!(f, "default_cast_permission"),
        }
    }
}

impl FromStr for CastFreeOrigin {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "hand" => Ok(CastFreeOrigin::Hand),
            "default_cast_permission" | "implicit_cast_zone" | "otherwise_castable" => {
                Ok(CastFreeOrigin::DefaultCastPermission)
            }
            other => Err(format!("unknown CastFreeOrigin: {other}")),
        }
    }
}

/// CR 118.9 + CR 601.2a: The cost axis for `StaticMode::ExileCastPermission`.
///
/// Sibling to `CastFrequency` and `CardPlayMode` — each axis of the exile-cast
/// permission is a typed enum rather than a `bool` so the design space stays
/// open. `bool` fields cannot grow to accommodate future cost shapes (e.g. an
/// alternative life cost analogous to Bolas's Citadel).
///
/// - `PayNormalCost` — cast at the spell's normal mana cost. No shipping
///   printing uses this shape today, but it is the natural default; if a future
///   card prints "Once each turn, you may cast a spell from among cards exiled
///   with ~ this turn." (no "without paying" rider), this is the variant.
/// - `WithoutPayingManaCost` — CR 118.9a: cast without paying the printed mana
///   cost. Maralen, Fae Ascendant ("...without paying its mana cost.") is the
///   type specimen and the only shipping printing today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum ExileCastCost {
    /// Cast at the spell's normal mana cost — no alternative cost applied.
    PayNormalCost,
    /// CR 118.9a: Cast without paying the spell's printed mana cost
    /// (Maralen, Fae Ascendant).
    #[default]
    WithoutPayingManaCost,
}

impl fmt::Display for ExileCastCost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExileCastCost::PayNormalCost => write!(f, "pay_normal_cost"),
            ExileCastCost::WithoutPayingManaCost => write!(f, "without_paying_mana_cost"),
        }
    }
}

impl FromStr for ExileCastCost {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pay_normal_cost" => Ok(ExileCastCost::PayNormalCost),
            "without_paying_mana_cost" => Ok(ExileCastCost::WithoutPayingManaCost),
            other => Err(format!("unknown ExileCastCost: {other}")),
        }
    }
}

/// CR 113.6b + CR 406.6: Which exile-link pool a `StaticMode::ExileCastPermission`
/// draws from. A typed axis (not a `bool`) so the open-ended design space — e.g.
/// a future "this game" or windowed pool — slots in without a refactor.
///
/// - `ThisTurn` — the per-turn rolling list
///   (`GameState::cards_exiled_with_source_this_turn`). The card's reference is
///   scoped by the "this turn" suffix (Maralen, Fae Ascendant: "...exiled with ~
///   *this turn*..."). Cards exiled on a prior turn are no longer eligible.
/// - `Persistent` — the lifetime `GameState::exile_links` pool, queried through
///   `linked_exile_cards_for_source` (the same source-keyed set that backs
///   `TargetFilter::ExiledBySource`). The card's reference has no turn bound
///   ("...from among cards exiled with ~." — The Matrix of Time, the
///   Prosper/Tibalt impulse-commander class). Every card still linked to the
///   source remains eligible across turns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum ExileCardPool {
    /// Per-turn rolling pool (`cards_exiled_with_source_this_turn`).
    #[default]
    ThisTurn,
    /// Lifetime per-source `exile_links` pool (`linked_exile_cards_for_source`).
    Persistent,
}

impl fmt::Display for ExileCardPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExileCardPool::ThisTurn => write!(f, "this_turn"),
            ExileCardPool::Persistent => write!(f, "persistent"),
        }
    }
}

impl FromStr for ExileCardPool {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "this_turn" => Ok(ExileCardPool::ThisTurn),
            "persistent" => Ok(ExileCardPool::Persistent),
            other => Err(format!("unknown ExileCardPool: {other}")),
        }
    }
}

/// CR 117.1c + CR 305.1: When a `StaticMode::ExileCastPermission` is active. A
/// typed axis (not a `bool`) so other timing windows (e.g. "during combat")
/// extend without a refactor.
///
/// - `AnyTime` — the permission functions whenever its other gates pass
///   (Maralen, Fae Ascendant: the per-turn cast slot is not turn-restricted).
/// - `YourTurnOnly` — CR 117.1c: the permission functions only while it is the
///   source controller's turn ("*During your turn*, you may play lands and cast
///   spells from among cards exiled with ~." — The Matrix of Time).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum ExileCastTiming {
    /// No turn restriction.
    #[default]
    AnyTime,
    /// CR 117.1c: Active only during the source controller's turn.
    YourTurnOnly,
}

impl fmt::Display for ExileCastTiming {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExileCastTiming::AnyTime => write!(f, "any_time"),
            ExileCastTiming::YourTurnOnly => write!(f, "your_turn_only"),
        }
    }
}

impl FromStr for ExileCastTiming {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "any_time" => Ok(ExileCastTiming::AnyTime),
            "your_turn_only" => Ok(ExileCastTiming::YourTurnOnly),
            other => Err(format!("unknown ExileCastTiming: {other}")),
        }
    }
}

/// CR 118.9 + CR 601.2f: Whether a non-mana cost rider on a graveyard/exile
/// cast-permission static is an *alternative* cost (paid in lieu of the spell's
/// mana cost, which is zeroed — CR 118.9) or an *additional* cost (paid on top
/// of the normal mana cost, which is still due — CR 601.2f).
///
/// A typed enum rather than a `bool` so the design space stays open and each
/// shape is self-documenting at its match sites:
/// - `Alternative` — Valgavoth, Terror Eater: "If you cast a spell this way, pay
///   life equal to its mana value rather than pay its mana cost." The static's
///   `extra_cost.cost` replaces the mana cost (CR 118.9a — exactly one
///   alternative cost applies).
/// - `Additional` — Festival of Embers ("by paying 1 life in addition to their
///   other costs"), Dawnhand Dissident ("by removing three counters … in
///   addition to paying their other costs"). The mana cost is still paid; the
///   static's `extra_cost.cost` is added on top (CR 601.2f).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CastCostMode {
    /// CR 118.9: Replaces the spell's mana cost (the mana cost is zeroed).
    Alternative,
    /// CR 601.2f: Paid on top of the spell's normal mana cost.
    Additional,
}

impl fmt::Display for CastCostMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CastCostMode::Alternative => write!(f, "alternative"),
            CastCostMode::Additional => write!(f, "additional"),
        }
    }
}

impl FromStr for CastCostMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "alternative" => Ok(CastCostMode::Alternative),
            "additional" => Ok(CastCostMode::Additional),
            other => Err(format!("unknown CastCostMode: {other}")),
        }
    }
}

/// CR 118.9 + CR 601.2f: A non-mana cost rider carried by a graveyard/exile
/// cast-permission static. Pairs the `AbilityCost` to pay with the `mode`
/// (`Alternative` vs `Additional`) that decides whether the spell's mana cost is
/// replaced or still due. The single building block covering the whole
/// "cast-from-zone for a non-mana cost" class (Valgavoth alternative pay-life;
/// Festival of Embers additional pay-life; Dawnhand Dissident additional
/// remove-counters). Mirrors `TopOfLibraryCastPermission.alt_cost`, which is
/// always `Alternative` and so needs no `mode` axis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CastExtraCost {
    /// CR 118.9 / CR 601.2f: The cost to pay. Routed through
    /// `pay_additional_cost` (the single non-mana cost-payment authority) so
    /// dynamic refs (`QuantityRef::SelfManaValue`) resolve against the spell at
    /// cast time, exactly as the per-card `ExileWithAltAbilityCost` flow does.
    pub cost: AbilityCost,
    /// CR 118.9 vs CR 601.2f: Alternative (replaces mana cost) vs Additional
    /// (paid on top of the mana cost).
    pub mode: CastCostMode,
}

/// CR 603.2d: The cause-predicate axis for trigger-doubling static abilities.
///
/// "An effect that states a triggered ability of an object triggers additional
/// times" may be restricted to triggers caused by specific events
/// (Panharmonicon: artifact/creature entering the battlefield; Isshin:
/// creature attacking). A wildcard `Any` cause covers hypothetical unrestricted
/// doublers.
///
/// This is a typed enum rather than a boolean because the design space is
/// open-ended: new cards routinely introduce novel cause predicates, and
/// `bool` fields cannot grow to accommodate them.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TriggerCause {
    /// Unrestricted doubler — matches any trigger cause.
    Any,
    /// CR 603.6a: Trigger was caused by a permanent entering the battlefield
    /// (Panharmonicon-class). The `core_types` list narrows the entering
    /// permanent's type — for Panharmonicon this is
    /// `[Artifact, Creature]`; for a hypothetical creature-only Panharmonicon
    /// it would be `[Creature]`.
    EntersBattlefield {
        #[serde(default)]
        core_types: Vec<super::card_type::CoreType>,
    },
    /// CR 508.1 + CR 308.1: Trigger was caused by a creature attacking
    /// (Isshin-class). Matches `GameEvent::AttackersDeclared` regardless of
    /// attack target (player, planeswalker, or battle).
    CreatureAttacking,
    /// CR 603.6c + CR 700.4: Trigger was caused by a creature dying — a
    /// creature moving from the battlefield to the graveyard
    /// (Drivnod-class). Matches `GameEvent::ZoneChanged` from `Battlefield`
    /// to `Graveyard` for an object whose snapshot included `Creature` in
    /// its core types.
    CreatureDying,
    /// CR 603.2d + CR 120.3: Trigger was caused by a creature you control
    /// being dealt damage (Wayta, Trainer Prodigy-class). Matches
    /// `GameEvent::DamageDealt` whose target is a creature controlled by the
    /// doubler's controller.
    ControlledCreatureDealtDamage,
}

impl fmt::Display for TriggerCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TriggerCause::Any => write!(f, "Any"),
            TriggerCause::EntersBattlefield { core_types } => {
                let names: Vec<String> = core_types.iter().map(|ct| format!("{ct:?}")).collect();
                write!(f, "EntersBattlefield([{}])", names.join(","))
            }
            TriggerCause::CreatureAttacking => write!(f, "CreatureAttacking"),
            TriggerCause::CreatureDying => write!(f, "CreatureDying"),
            TriggerCause::ControlledCreatureDealtDamage => {
                write!(f, "ControlledCreatureDealtDamage")
            }
        }
    }
}

/// CR 509.1b: An attacker's "can't be blocked except by ..." restriction.
/// Two structural shapes share this CR sub-rule:
///  - `Quality`: each blocker must individually match a `TargetFilter`
///    (e.g. "artifact creatures").
///  - `MinBlockers`: the attacker must be blocked by `min` or more creatures
///    total, or not at all — the generalization of Menace (CR 702.111b, min = 2).
///
/// NOTE: this enum is deliberately NOT `#[derive(Hash)]` — `TargetFilter` does
/// not implement `Hash`. `StaticMode::hash` handles it manually (discriminant
/// for `Quality`, `min` for `MinBlockers`), mirroring the `CantBeBlockedBy`
/// precedent.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub enum BlockExceptionKind {
    Quality(TargetFilter),
    MinBlockers { min: u32 },
}

/// CR 601.2f: Direction/semantic axis for mana-cost modification statics.
/// All three modes are applied in the CR 601.2f cost-locking step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CostModifyMode {
    /// Subtractive — reduce generic mana (floor: 0).
    Reduce,
    /// Additive — increase generic mana. Thalia, Guardian of Thraben class.
    Raise,
    /// Floor — cost cannot fall below `amount` after all Reduce/Raise settle.
    /// CR 601.2f last-step floor. Trinisphere class.
    Minimum,
}

/// Serde default for the `mode` field on activation-cost statics: the original
/// subtractive (`Reduce`) form, so card-data serialized before the directional
/// field was added still deserializes as a reduction (CR 118.7).
fn cost_modify_mode_reduce() -> CostModifyMode {
    CostModifyMode::Reduce
}

/// CR 601.2f: Whether a static-imposed additional cost applies to spell casting.
/// Distinct from [`CostModifyMode`], which only adjusts the mana component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AdditionalCostTaxAction {
    /// "... cost an additional N life to cast."
    Cast,
}

/// CR 702.122a / CR 702.171a / CR 702.184c: How a creature's contributed power
/// is modified when it crews a Vehicle, saddles a Mount, or stations a permanent.
/// (Station's modifier is explicit in CR 702.184c; Crew/Saddle modify the
/// "total power N" threshold of CR 702.122a / CR 702.171a.)
/// See [`StaticMode::CrewContribution`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CrewContributionKind {
    /// "as though its power were N greater" — contribute `power + delta`.
    PowerDelta { delta: i32 },
    /// "using its toughness rather than its power" — contribute `toughness`.
    ToughnessInsteadOfPower,
}

/// The keyword action being performed. `StaticMode::CrewContribution` stores the
/// exact named actions it modifies. CR 702.122 / 702.171 / 702.184.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CrewAction {
    Crew,
    Saddle,
    Station,
}

/// Which combat action the `CombatAlone` restriction governs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CombatAloneAction {
    Attack,
    Block,
}

/// The polarity of a `CombatAlone` static.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CombatAloneRequirement {
    /// "can't X alone" — the creature must NOT be the sole attacker/blocker.
    NeedsCompanion,
    /// "can only X alone" — the creature must BE the sole attacker; companions are prohibited.
    MustBeSole,
}

/// CR 508.5 + CR 802.1: Which defending player a `MaxAttackersEachCombat` cap
/// restricts. `MaxAttackersEachCombat { defender: None }` is a global cap;
/// `Some(_)` narrows the cap to attacks declared against a specific defending
/// player, leaving attacks against other players unrestricted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AttackDefenderScope {
    /// CR 109.5: "you" — the controller of the permanent carrying this static
    /// (Judoon Enforcers: "No more than one creature can attack you each
    /// combat"). Resolved against the static source's controller at the
    /// declare-attackers step.
    Controller,
}

/// All static ability modes from Forge's static ability registry.
/// Matched case-sensitively against Forge mode strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StaticMode {
    Continuous,
    /// CR 514.2: Normally all damage marked on permanents is removed as a
    /// turn-based action during the cleanup step. This static suppresses that
    /// removal for the permanents matched by the definition's `affected` filter
    /// ("Damage isn't removed from [filter] during cleanup steps" — Ancient
    /// Adamantoise, Patient Zero, Uthgardt Fury, …), so their marked damage
    /// persists across turns.
    DamageNotRemovedDuringCleanup,
    CantAttack,
    CantBlock,
    CantAttackOrBlock,
    /// CR 701.60a + CR 701.60d: The affected permanent can't become suspected
    /// (Airtight Alibi: "Enchanted creature ... can't become suspected"). A
    /// nullary marker static — the `affected` filter scopes which permanents are
    /// protected, and runtime enforcement is the suspect resolver's gate
    /// (`suspect::resolve`), which refuses to designate a permanent carrying this
    /// static. Distinct from CR 701.60d's intrinsic "a suspected permanent can't
    /// become suspected again": this prohibits the designation even while the
    /// permanent is NOT suspected.
    CantBecomeSuspected,
    /// CR 508.1c: No more than `max` creatures can be declared as attackers
    /// each combat. `defender` scopes *which declarations* the cap restricts:
    /// `None` is a global per-combat cap (no more than `max` creatures can
    /// attack at all); `Some(AttackDefenderScope::Controller)` is a
    /// defending-player cap ("no more than `max` creatures can attack *you*
    /// each combat" — Judoon Enforcers), restricting only attackers whose
    /// defending player (CR 508.5) is this static's controller, so opponents
    /// may still be attacked freely (CR 802.1 multiplayer range of influence).
    MaxAttackersEachCombat {
        max: u32,
        #[serde(default)]
        defender: Option<AttackDefenderScope>,
    },
    /// CR 509.1c: No more than `max` creatures can be declared as blockers
    /// each combat.
    MaxBlockersEachCombat {
        max: u32,
    },
    CantBeTargeted,
    /// CR 101.2: Blanket casting prohibition — prevents the scoped player(s) from casting spells.
    /// E.g., Steel Golem: "You can't cast creature spells." (Controller scope + creature filter)
    CantBeCast {
        who: ProhibitionScope,
    },
    /// CR 602.5: "A player can't begin to activate an ability that's prohibited from being activated."
    /// CR 603.2a: Activation-prohibition effects do **not** affect triggered abilities —
    /// use `SuppressTriggers` for the triggered-ability side of the prohibition family.
    ///
    /// `who` = activator-axis (which player is blocked from activating).
    /// `source_filter` = which permanent's activated abilities are blocked.
    ///
    /// - Chalice of Life ("this permanent's activated abilities can't be activated"):
    ///   `who = AllPlayers, source_filter = SelfRef`.
    /// - Clarion Conqueror ("Activated abilities of artifacts, creatures, and planeswalkers
    ///   your opponents control can't be activated"):
    ///   `who = AllPlayers, source_filter = AnyOf(Artifact,Creature,Planeswalker) + ControllerRef::Opponent`.
    /// - Karn, the Great Creator ("Activated abilities of artifacts your opponents control
    ///   can't be activated"): `who = AllPlayers, source_filter = Artifact + ControllerRef::Opponent`.
    ///
    /// `who = AllPlayers` is correct on Clarion/Karn: CR 602.5 prohibitions block the
    /// ability itself, not a specific activator. Opponent-ness rides on the filter's
    /// `ControllerRef`, which survives control-swap effects like Act of Treason.
    ///
    /// `exemption` carries the optional "unless they're mana abilities" clause
    /// (CR 605.1a). Pithing Needle emits `ActivationExemption::ManaAbilities`;
    /// Phyrexian Revoker, Sorcerous Spyglass, and the standard Chalice/Karn
    /// family use `ActivationExemption::None`.
    CantBeActivated {
        who: ProhibitionScope,
        source_filter: TargetFilter,
        #[serde(default)]
        exemption: ActivationExemption,
    },
    /// CR 701.23 + CR 609.3: "Spells and abilities <scope> can't cause their controller
    /// to search their library." E.g., Ashiok, Dream Render's first static ability.
    /// When a muzzled spell/ability would cause a search, the search is treated as
    /// impossible and produces no-op behavior (CR 609.3).
    ///
    /// `cause` = which player's spells/abilities are muzzled (the *source* of the search,
    /// not the searcher). For Ashiok: `cause = Opponents`.
    CantSearchLibrary {
        cause: ProhibitionScope,
    },
    /// CR 603.2 + CR 609.3: "Triggered abilities <scope> can't cause you to
    /// sacrifice or exile <affected>." E.g., The Master, Multiplied — triggered
    /// abilities you control can't cause you to sacrifice or exile creature
    /// tokens you control. When a muzzled trigger would move an affected object
    /// to exile or its controller's graveyard via sacrifice, that object is
    /// skipped (CR 609.3: do as much as possible). Scope of muzzled abilities
    /// rides on `cause`; scope of protected objects rides on `affected`.
    CantCauseSacrificeOrExile {
        cause: ProhibitionScope,
    },
    CastWithFlash,
    /// CR 701.38d: While voting, the controller of this permanent may vote an
    /// additional time. Each active source grants +1 to the controller's
    /// `Player::extra_votes_per_session` snapshot taken at vote-session start
    /// (Tivit, Seller of Secrets — "While voting, you may vote an additional
    /// time.").
    ///
    /// The vote-effect resolver scans the battlefield for permanents with this
    /// static at session start (CR 701.38d: extra votes happen at the same
    /// time the player would otherwise have voted). It does *not* feed into
    /// layer 7 — there is no continuous P/T or keyword grant; the static is a
    /// pure "session-start +1 votes" signal.
    GrantsExtraVote,
    /// CR 701.55c: A replacement effect causes an opponent who would face a
    /// villainous choice to face that choice some number of additional times
    /// (The Valeyard — "If an opponent would face a villainous choice, they face
    /// that choice an additional time."). Each active source controlled by an
    /// opponent of the facing player adds +1 additional villainous-choice
    /// instance for that player; the whole CR 701.55a process is then performed
    /// that many additional times, one at a time (CR 701.55c). Parallel to
    /// `GrantsExtraVote` (CR 701.38d), but counts a different keyword action read
    /// by a different resolver (`choose_one_of`), so the two are not unifiable
    /// (categorical-boundary rule: CR 701.55 vs CR 701.38 are separate sections).
    /// Like `GrantsExtraVote`, it does not feed into layer 7 — there is no
    /// continuous P/T or keyword grant.
    GrantsExtraVillainousChoice,
    /// CR 702.51a: Grants a keyword to spells during casting.
    /// Generalized version of CastWithFlash — the `spell_filter` on the StaticDefinition
    /// determines which spells are affected (e.g., "Creature spells you cast have convoke").
    CastWithKeyword {
        keyword: Keyword,
    },
    /// CR 118.9 + CR 601.2f: A permanent grants its controller a wholesale
    /// alternative cost for spells matching `StaticDefinition::affected` that
    /// the controller casts — they may pay `cost` rather than the spell's mana
    /// cost. Parallel to `CastWithKeyword`. Distinct from
    /// `ModifyCost { mode: Reduce, .. }` (subtractive, CR 601.2f) — this
    /// REPLACES the mana cost wholesale (CR 118.9) and is mutually exclusive
    /// with other alternative costs (CR 118.9a). Rooftop Storm ({0}, Zombie
    /// creature spells), Fist of Suns ({WUBRG}, any spell), Jodah (MV 5+),
    /// Primal Prayers ({E}, creature MV ≤ 3, with an alternative-cost timing
    /// permission).
    CastWithAlternativeCost {
        cost: AbilityCost,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timing_permission: Option<CastTimingPermission>,
    },
    /// CR 118.9 + CR 702.29a + CR 702.122a: Controller may pay `cost` instead
    /// of the printed cost for `keyword` ability activations. Covers New
    /// Perspectives (cycling, {0}), Heart of Kiran (crew, remove-loyalty),
    /// Gavi Nest Warden (cycling, {0}, first-per-turn).
    ///
    /// `frequency`: None = all activations; Some(OncePerTurn) = first per turn.
    ///
    /// Parser-complete structured gap; runtime hook deferred.
    /// CR 702.29a (docs/MagicCompRules.txt:4202), CR 702.122a (docs/MagicCompRules.txt:4870),
    /// CR 118.9 (docs/MagicCompRules.txt:1014).
    AlternativeKeywordCost {
        keyword: KeywordKind,
        cost: AbilityCost,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        frequency: Option<CastFrequency>,
    },
    /// CR 601.2f: Modifies the mana cost of spells matching `spell_filter`
    /// (or all spells when `None`) by `amount`, in the direction described by `mode`.
    /// `Reduce` subtracts generic mana (floor: 0), `Raise` adds generic mana,
    /// `Minimum` floors the total cost after all Reduce/Raise settle.
    ModifyCost {
        mode: CostModifyMode,
        amount: ManaCost,
        spell_filter: Option<TargetFilter>,
        /// Dynamic multiplier (e.g. "for each [thing] you control").
        /// Only meaningful for `Reduce` and `Raise` — always `None` for `Minimum`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dynamic_count: Option<QuantityRef>,
    },
    /// CR 601.2f + CR 118.8: Imposes an additional non-mana cost on spells or
    /// spells matching `spell_filter`. Distinct from [`StaticMode::ModifyCost`],
    /// which adjusts only the mana component. Terror of the Peaks class:
    /// "Spells your opponents cast that target this creature cost an additional
    /// 3 life to cast."
    ImposeAdditionalCost {
        cost: super::ability::AbilityCost,
        spell_filter: Option<TargetFilter>,
        action: AdditionalCostTaxAction,
    },
    /// CR 601.2f + CR 118.7: Modifies the generic mana cost of activated abilities
    /// matching a keyword type, in the direction given by `mode`.
    /// E.g., "Ninjutsu abilities you activate cost {1} less to activate." (`Reduce`)
    /// or "Activated abilities of sources with the chosen name cost {2} more to
    /// activate." (`Raise`, Skyseer's Chariot).
    /// `keyword` identifies which ability type is modified — `"activated"` matches
    /// every activated ability, or a tagged keyword (e.g. "ninjutsu", "equip",
    /// "cycling", "power-up") matches the activating ability's `AbilityTag`.
    /// `amount` is the fixed generic mana adjustment per activation.
    ///
    /// Directional parameterization (not a `Raise`-only sibling): the
    /// `CostModifyMode` axis is shared with [`StaticMode::ModifyCost`] (the
    /// cast-cost analogue). `Minimum` is not meaningful here — only `Reduce`
    /// (CR 118.7) and `Raise` (CR 118.7 increase) are emitted/applied.
    ReduceAbilityCost {
        /// CR 118.7: Direction of the adjustment. `#[serde(default)]` keeps
        /// already-serialized card-data (which predates this field) reading as
        /// the original subtractive form.
        #[serde(default = "cost_modify_mode_reduce")]
        mode: CostModifyMode,
        keyword: String,
        amount: u32,
        /// "This effect can't reduce the mana in that cost to less than one mana."
        /// Only meaningful for `Reduce` — `Raise` never floors.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        minimum_mana: Option<u32>,
        /// CR 601.2f: Dynamic multiplier for the adjustment (e.g., "for each Dragon you control").
        /// When present, the total adjustment is `amount * resolve_quantity(dynamic_count)`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dynamic_count: Option<QuantityRef>,
    },
    /// CR 702.142b: Modifies the per-turn activation limit for abilities matching
    /// a keyword tag. E.g., "Creatures you control can boast twice during each of
    /// your turns rather than once" → overrides `OnlyOnceEachTurn` to `MaxTimesEachTurn(2)`
    /// for boast-tagged abilities on affected permanents.
    ModifyActivationLimit {
        /// The keyword tag whose activation limit is modified.
        keyword: String,
        /// The new per-turn activation count.
        new_limit: u8,
    },
    /// CR 602.5e + CR 611.3a: Static permission allowing affected permanents'
    /// activated abilities in the specified cost category to be activated at
    /// instant timing. The affected permanent filter lives on `StaticDefinition`.
    /// Canonical class: The Wandering Emperor's same-turn loyalty permission.
    ActivateAsInstant {
        cost_category: CostCategory,
    },
    /// CR 118.3 + CR 601.2h + CR 602.2b: The scoped player can't pay a
    /// matching non-mana cost to cast spells or activate abilities.
    ///
    /// Yasharn's class: "Players can't pay life or sacrifice nonland
    /// permanents to cast spells or activate abilities." This does not stop
    /// life loss or effect-driven sacrifices; it is enforced only at cost
    /// payability/payment boundaries.
    CantPayCost {
        who: ProhibitionScope,
        cost: CostPaymentProhibition,
    },
    CantGainLife,
    CantLoseLife,
    /// CR 702.16: The scoped player(s) have protection from a quality —
    /// e.g. Serra's Emissary's "You ... have protection from the chosen card
    /// type." Player scope rides on `StaticDefinition::affected` (identical to
    /// `CantGainLife`); `ProtectionTarget` is the canonical protection-quality
    /// axis. Data-carrying variant — not registry-registered (see
    /// `coverage::is_data_carrying_static`); consumed by direct pattern-match
    /// in `player_protection_from`. Only the `ChosenCardType` arm is
    /// runtime-implemented; other arms are inert.
    PlayerProtection(super::keywords::ProtectionTarget),
    MustAttack,
    /// CR 508.1d: This creature must attack a *specific* player if able ("target
    /// creature attacks you this combat if able"; Alluring Siren, Dulcet Sirens).
    /// Unlike the generic
    /// [`MustAttack`] (attack any defender), this carries the `PlayerId` that must
    /// be attacked. Data-carrying variant — not registry-registered (see
    /// `coverage::is_data_carrying_static`); enforced by direct pattern-match in
    /// `combat.rs` declare-attackers validation. Mirrors [`MustBlockAttacker`].
    MustAttackPlayer {
        player: PlayerId,
    },
    MustBlock,
    /// CR 702.39a / CR 509.1c: This creature must block a *specific* attacker if
    /// able (Provoke; "target creature blocks ~ this turn if able"). Unlike the
    /// generic [`MustBlock`] (block *any* attacker), this carries the `ObjectId`
    /// of the attacker that must be blocked. Data-carrying variant — not
    /// registry-registered (see `coverage::is_data_carrying_static`); enforced by
    /// direct pattern-match in `combat.rs` declare-blockers validation. The
    /// `ObjectId` is stable for the end-of-turn lifetime of the granting effect.
    MustBlockAttacker {
        attacker: ObjectId,
    },
    CantDraw {
        who: ProhibitionScope,
    },
    /// CR 603.2d: "If [cause], a triggered ability of a permanent you control
    /// triggers an additional time." Panharmonicon, Isshin Two Heavens as One,
    /// and the class of trigger-doublers. The `cause` predicate narrows which
    /// trigger-spawning events qualify.
    DoubleTriggers {
        cause: TriggerCause,
    },
    IgnoreHexproof,
    /// CR 509.1a + CR 509.1b: This creature can block additional creatures.
    /// `None` = any number, `Some(n)` = n additional creatures beyond the default 1.
    ExtraBlockers {
        count: Option<u32>,
    },
    /// CR 400.2: Play with the top card of your library revealed.
    /// Variants: "your library" (controller only) or "their libraries" (all players).
    RevealTopOfLibrary {
        all_players: bool,
    },
    /// CR 400.2 + CR 701.20a: Play with hands revealed.
    /// `who` identifies whose hand is public: controller, opponents, or all players.
    RevealHand {
        who: ProhibitionScope,
    },
    /// CR 604.2 + CR 305.1: Static ability granting permission to play/cast
    /// matching cards from owner's graveyard.
    GraveyardCastPermission {
        /// CR 601.2a: Per-turn cast frequency. `OncePerTurn` = "once during each of
        /// your turns" (Lurrus, Karador). `Unlimited` = no per-turn cap (Conduit).
        frequency: CastFrequency,
        /// Play (lands+spells) vs Cast (spells only)
        play_mode: CardPlayMode,
        /// CR 614.1a: "If a spell cast this way would be put into your
        /// graveyard, exile it instead." This is narrower than flashback: it
        /// replaces only stack-to-graveyard destinations produced by this
        /// permission.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        graveyard_destination_replacement: Option<Zone>,
        /// CR 118.9 + CR 601.2f: Optional non-mana cost rider paid when casting
        /// a spell via this permission. `Additional` is paid on top of the
        /// normal mana cost (Festival of Embers: "by paying 1 life in addition
        /// to their other costs"); `Alternative` would replace the mana cost.
        /// `None` (default) preserves the existing graveyard-cast shapes
        /// (Lurrus, Karador, Conduit). Routed through `pay_additional_cost`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        extra_cost: Option<CastExtraCost>,
    },
    /// CR 401.5 + CR 118.9 + CR 601.2a: Static ability granting permission to
    /// play/cast the top card of the controller's library when it matches
    /// `StaticDefinition.affected`. Class members: Realmwalker (creature spells
    /// of the chosen type), Future Sight + Magus of the Future (any spell or
    /// land), Bolas's Citadel (any, with `alt_cost = pay life equal to its mana
    /// value`), Vivien on the Hunt static, etc.
    ///
    /// Distinct from `GraveyardCastPermission`: the source object is the
    /// continually-changing top of `Player.library`, not a graveyard card.
    /// Filter eligibility is therefore re-evaluated each priority window
    /// because `casting::spell_objects_available_to_cast` is called fresh.
    ///
    /// Casting a card via this permission moves it `Library → Stack` directly
    /// (CR 601.2a: "moves that card from where it is to the stack"); there is
    /// NO exile step. This separates the class cleanly from the impulse-draw
    /// class (`Effect::CastFromZone` → `CastingPermission::ExileWithAltCost`),
    /// which exiles the card before granting a permission.
    TopOfLibraryCastPermission {
        /// CR 305.1: `Play` covers both lands (played as a land drop) and
        /// non-land spells (cast as a spell). `Cast` covers only spells.
        /// Realmwalker = `Cast`; Future Sight + Bolas's Citadel = `Play`.
        play_mode: CardPlayMode,
        /// CR 601.2a: Per-turn cast frequency, the same axis carried by the
        /// sibling `GraveyardCastPermission` / `ExileCastPermission` permissions.
        /// `Unlimited` (default) preserves the Realmwalker / Future Sight /
        /// Bolas's Citadel shape (no per-turn cap). `OncePerTurn` gates the
        /// permission to one cast per turn from this source — "Once each turn,
        /// you may cast … from the top of your library." (Assemble the Players,
        /// Johann, Apprentice Sorcerer). Tracked by the source's `ObjectId` in
        /// `GameState::top_of_library_cast_permissions_used`.
        #[serde(default)]
        frequency: CastFrequency,
        /// CR 118.9 + CR 119.4: Optional alternative cost paid in lieu of the
        /// spell's mana cost when cast via this permission. Bolas's Citadel
        /// uses `Some(AbilityCost::PayLife { amount: SelfManaValue })`.
        /// `None` for permissions that pay the normal mana cost
        /// (Realmwalker, Future Sight). When `Some(_)`, the casting pipeline
        /// zeros the spell's mana cost and routes this cost through
        /// `pay_additional_cost` (mirrors the `ExileWithAltAbilityCost` flow).
        alt_cost: Option<AbilityCost>,
    },
    /// CR 601.2b + CR 118.9a: Static ability granting permission to cast matching
    /// spells without paying their mana costs. `Unlimited` = Omniscience,
    /// Tamiyo emblem, Dracogenesis. `OncePerTurn` = Zaffai and the Tempests.
    CastFromHandFree {
        /// CR 601.2b: Per-turn cast frequency.
        frequency: CastFrequency,
        /// CR 601.2a + CR 903.8: Whether the permission is explicitly hand-only
        /// or applies to built-in cast zones that already authorize the spell.
        #[serde(default)]
        origin: CastFreeOrigin,
    },
    /// CR 601.2a + CR 113.6b + CR 118.9: Static ability granting permission to
    /// cast cards exiled with this source — restricted to cards exiled *this
    /// turn* — typically without paying the mana cost. Maralen, Fae Ascendant
    /// is the type specimen ("Once each turn, you may cast a spell with mana
    /// value less than or equal to the number of Elves and Faeries you control
    /// from among cards exiled with Maralen this turn without paying its mana
    /// cost.").
    ///
    /// The source pool is the per-turn list
    /// `GameState::cards_exiled_with_source_this_turn[source_id]`. The static's
    /// `affected: TargetFilter` constrains the eligible cards (type, mana
    /// value, etc.). Per-turn frequency tracking is keyed on `source_id` in
    /// `GameState::exile_cast_permissions_used` for `OncePerTurn`; `Unlimited`
    /// skips tracking.
    ///
    /// Distinct from `GraveyardCastPermission`: the source pool is exile
    /// (per-turn-scoped), not the controller's graveyard. Distinct from
    /// `TopOfLibraryCastPermission`: the eligible cards are a tracked exile
    /// set carved out by a prior exile-with-source effect, not the live top
    /// of library. Distinct from `Effect::CastFromZone` (Court of Locthwain,
    /// Mizzix's Mastery): that is a one-shot effect that grants per-card
    /// permissions; this is a continuous static that grants the controller a
    /// recurring per-turn cast slot.
    ExileCastPermission {
        /// CR 601.2a: Per-turn cast frequency. `OncePerTurn` consumes a slot
        /// in `exile_cast_permissions_used`; `Unlimited` does not.
        frequency: CastFrequency,
        /// CR 305.1: Play (lands + spells) vs Cast (spells only). All current
        /// printings of this class are `Cast` (Maralen, Fae Ascendant); the
        /// axis is retained for symmetry with the graveyard / top-of-library
        /// permission classes.
        play_mode: CardPlayMode,
        /// CR 118.9a: How the spell's mana cost is paid when cast via this
        /// permission. `WithoutPayingManaCost` zeroes the printed mana cost
        /// (mirrors the Omniscience / `CastFromHandFree` flow — Maralen, Fae
        /// Ascendant). `PayNormalCost` casts at the spell's normal cost (no
        /// published printings today, but the axis keeps the static composable
        /// with future patterns).
        #[serde(default)]
        cost: ExileCastCost,
        /// CR 113.6b + CR 406.6: Which exile-link pool the permission draws
        /// from. `ThisTurn` (default) preserves the Maralen shape (per-turn
        /// rolling list); `Persistent` reads the lifetime `exile_links` set for
        /// the open-ended "cards exiled with ~" class (The Matrix of Time,
        /// Prosper/Tibalt impulse commanders).
        #[serde(default)]
        pool: ExileCardPool,
        /// CR 117.1c: When the permission functions. `AnyTime` (default)
        /// preserves the Maralen shape; `YourTurnOnly` gates the grant to the
        /// source controller's turn ("During your turn, you may play lands and
        /// cast spells from among cards exiled with ~.").
        #[serde(default)]
        timing: ExileCastTiming,
        /// CR 609.4b: Optional payment concession riding alongside the cast
        /// permission — "Mana of any type can be spent to cast those spells."
        /// (Azula, Cunning Usurper). `None` (default) preserves the existing
        /// shapes (Maralen, The Matrix of Time). `Some(AnyTypeOrColor)` scopes
        /// the any-type-mana spend to spells cast via this permission, mirroring
        /// the per-card `CastingPermission::PlayFromExile.mana_spend_permission`
        /// for the persistent-static seam. Consulted in
        /// `casting::player_can_spend_as_any_color_for_spell`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mana_spend_permission: Option<crate::types::ability::ManaSpendPermission>,
        /// CR 601.3b + CR 702.8a: When `true`, spells cast via this permission
        /// may be cast "as though they had flash" — i.e. at instant speed
        /// regardless of their normal timing (Azula, Cunning Usurper: "you may
        /// cast them as though they had flash"). `false` (default) leaves the
        /// normal sorcery-speed timing rules in force. Consulted by the
        /// cast-timing check in `casting::prepare_spell_cast`.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        grants_flash: bool,
        /// CR 118.9 + CR 601.2f: Optional non-mana cost rider paid when casting
        /// a spell via this permission. `Alternative` replaces the spell's mana
        /// cost (Valgavoth, Terror Eater: "pay life equal to its mana value
        /// rather than pay its mana cost"); `Additional` is paid on top of the
        /// normal mana cost (Dawnhand Dissident: "by removing three counters …
        /// in addition to paying their other costs"). `None` (default)
        /// preserves the existing shapes (Maralen, The Matrix of Time, Azula).
        /// Routed through `pay_additional_cost` at cast time (mirrors
        /// `TopOfLibraryCastPermission.alt_cost`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        extra_cost: Option<CastExtraCost>,
    },
    /// CR 113.6 + CR 601.2a: Marker static identifying a source whose linked
    /// "play a card from exile with a collection counter on it" permission is
    /// live (Evelyn, the Covetous). Per CR 113.6, that play permission is a
    /// static ability of the source and functions only while the source is on
    /// the battlefield under the player's control; this marker is what
    /// `casting.rs::source_has_collection_counter_play_permission` consults so
    /// the per-card `CastingPermission::PlayFromExile` (collection-counter +
    /// controller provenance) is honored only while a live source remains.
    /// CR 601.2a: the permission authorizes moving a matching exiled card to
    /// the stack / battlefield (playing it).
    ///
    /// Distinct from `ExileCastPermission`: that static is self-contained and
    /// grants the cast itself, keyed on a source-identity exile pool. This is a
    /// nullary marker; the actual permission lives on each exiled card as a
    /// `CastingPermission::PlayFromExile`, linked by the collection counter
    /// (not source identity), so the authority winks out if every source
    /// leaves but the counter persists.
    ///
    /// RUNTIME: handled by direct match in
    /// `casting.rs::source_has_collection_counter_play_permission`; coverage
    /// support is via `is_data_carrying_static()` (mirrors the cast-permission
    /// cluster, which is also runtime-by-direct-match, not registry-keyed).
    LinkedCollectionCounterPlayPermission,
    /// CR 122.2 + CR 113.6b: Override the default rule that counters cease to
    /// exist when an object changes zones. This source's counters *remain* as
    /// it moves to any zone NOT listed in `excluded_zones`; moves into an
    /// excluded zone follow the normal CR 122.2 clear.
    ///
    /// Class members (verbatim "Counters remain on [self] as it moves to any
    /// zone other than a player's hand or library"): Me, the Immortal and
    /// Skullbriar, the Walking Grave. Both encode
    /// `excluded_zones = [Hand, Library]` — counters persist into the
    /// battlefield, graveyard, exile, and command zone, but are cleared into a
    /// hand or library (CR 122.2 still applies there).
    ///
    /// CR 113.6b: This ability states the zones it functions from implicitly —
    /// per Me's official ruling ("only works if it has that ability in the zone
    /// it's moving from"), the persistence is read from the object's state in
    /// the *from*-zone at the moment of the move, not the destination.
    CountersPersistAcrossZones {
        /// CR 122.2: Destination zones where counters still cease to exist
        /// (the "any zone other than" exclusions). `[Hand, Library]` for both
        /// current class members. Typed `Vec<Zone>` so future "other than
        /// [zones]" variants compose without a new variant.
        excluded_zones: Vec<Zone>,
    },
    /// CR 101.2: This spell/permanent can't be countered.
    CantBeCountered,
    /// CR 101.2 + CR 707.10: This spell can't be copied by spells or abilities.
    /// Enforced in `copy_spell::resolve` when selecting the spell to copy.
    CantBeCopied,
    /// CR 604.3: Cards in specified zones can't enter the battlefield.
    CantEnterBattlefieldFrom,
    /// CR 601.3 + CR 101.2 + CR 109.5: The scoped player(s) can't cast spells from
    /// the zones encoded in `StaticDefinition::affected` (via `FilterProp::InAnyZone`).
    /// CR 601.3: a player can cast a spell only if no effect prohibits it; CR 101.2:
    /// this "can't" overrides any cast-from-zone permission (escape, flashback,
    /// foretell, commander).
    ///
    /// Two phrasings collapse onto this one variant:
    /// - Grafdigger's Cage ("Players can't cast spells from graveyards or
    ///   libraries"): `who = AllPlayers`, `affected = InAnyZone { [Graveyard, Library] }`.
    /// - Drannith Magistrate ("Your opponents can't cast spells from anywhere
    ///   other than their hands"): `who = Opponents`, `affected = InAnyZone {
    ///   [Graveyard, Library, Exile, Command] }` — every cast-capable zone except
    ///   the hand. The parser inverts "anywhere other than [hand]" into the
    ///   explicit prohibited-zone list so the runtime check stays a single
    ///   `InAnyZone` membership test.
    ///
    /// `who` rides the player axis (CR 109.5); the prohibited zones ride the
    /// `affected` filter. Enforcement is in
    /// `casting.rs::is_blocked_from_casting_from_zone`.
    CantCastFrom {
        who: ProhibitionScope,
    },
    /// CR 101.2: Continuous casting prohibition — prevents players from casting
    /// spells under specified conditions (turn/phase-scoped).
    /// E.g., "Your opponents can't cast spells during your turn."
    CantCastDuring {
        who: ProhibitionScope,
        when: CastingProhibitionCondition,
    },
    /// CR 602.5 + CR 117.1b: Continuous activation prohibition — prevents the
    /// scoped player(s) from activating activated abilities during the specified
    /// turn condition.
    ///
    /// E.g., City of Solitude: "Players can cast spells and activate abilities
    /// only during their own turns." (Activation half — the cast half is emitted
    /// as a sibling `CantCastDuring`.)
    ///
    /// Distinct from `CantBeActivated`:
    /// - `CantBeActivated` narrows by **permanent** (which permanent's abilities
    ///   are blocked) and has no time axis.
    /// - `CantActivateDuring` narrows by **time** (which turn condition the
    ///   prohibition is active under) and has no permanent narrowing — every
    ///   activated ability is blocked when the timing predicate matches.
    ///
    /// `exemption: ActivationExemption` carries CR 605.1a's "unless they're mana
    /// abilities" exemption. City of Solitude per its 2009-10-01 ruling
    /// ("This stops players from activating mana abilities") emits
    /// `ActivationExemption::None`. Field is present for structural parallelism
    /// with `CantBeActivated` and `GameRestriction::ProhibitActivity::ActivateAbilities`.
    ///
    /// CR 605.3a permits mana ability activation at priority generally, but
    /// per-card prohibitions override that general permission per CR 101.1.
    CantActivateDuring {
        who: ProhibitionScope,
        when: CastingProhibitionCondition,
        #[serde(default)]
        exemption: ActivationExemption,
    },
    /// CR 101.2 + CR 604.1: Per-turn casting limit — static ability generating a
    /// continuous "can't" effect that restricts how many spells a player may cast.
    /// E.g., Rule of Law: "Each player can't cast more than one spell each turn."
    /// E.g., Deafening Silence: "Each player can't cast more than one noncreature spell each turn."
    PerTurnCastLimit {
        who: ProhibitionScope,
        max: u32,
        spell_filter: Option<TargetFilter>,
    },
    /// CR 101.2: Per-turn draw limit — restricts how many cards a player may draw.
    /// E.g., Spirit of the Labyrinth: "Each player can't draw more than one card each turn."
    /// E.g., Narset, Parter of Veils: "Each opponent can't draw more than one card each turn."
    PerTurnDrawLimit {
        who: ProhibitionScope,
        max: u32,
    },
    /// CR 603.2g: "An event that's prevented or replaced won't trigger anything."
    /// Generalizes this rule into a typed prohibition: for a permanent matching
    /// `source_filter`, declare that the listed trigger events (ETB / Dies) never
    /// register, so no triggered ability fires in response to them.
    ///
    /// This is NOT a replacement effect (CR 614) — the event still happens, it simply
    /// does not cause any triggered abilities. Replacement effects that key on the
    /// same event (e.g., ETB tapped) are unaffected. Per CR 603.6d, static "enters with"
    /// / "enters tapped" / "as X enters" effects are also unaffected — they are
    /// static abilities, not triggered.
    ///
    /// `source_filter` matches the **subject of the trigger event** (the entering /
    /// dying permanent) — NOT the trigger-source permanent. A creature entering
    /// suppresses every ETB trigger caused by that entry, including observer triggers
    /// on other permanents (e.g., Soul Warden's "whenever another creature enters").
    /// Reading confirmed by official Torpor Orb rulings.
    ///
    /// - Torpor Orb: `source_filter = creatures, events = [EntersBattlefield]`.
    /// - Hushbringer: `source_filter = creatures, events = [EntersBattlefield, Dies]`.
    ///
    /// `events` is a unique-invariant Vec treated as a set. Parser constructs in the
    /// canonical order `[EntersBattlefield, Dies]`. Promote to a typed set newtype
    /// only if the variant population grows beyond two.
    SuppressTriggers {
        source_filter: TargetFilter,
        events: Vec<SuppressedTriggerEvent>,
    },

    // -- Tier 1: Keyword/evasion statics with dedicated handlers --
    /// CR 509.1b: This creature can't be blocked.
    CantBeBlocked,
    /// CR 509.1b: This creature can't be blocked except by blockers satisfying `kind`.
    CantBeBlockedExceptBy {
        kind: BlockExceptionKind,
    },
    /// CR 509.1b: This creature can't be blocked by creatures matching filter.
    /// Inverse of CantBeBlockedExceptBy — blockers matching the filter are prohibited.
    CantBeBlockedBy {
        filter: TargetFilter,
    },
    /// CR 509.1b: This creature can't be blocked by more than `max` creatures
    /// (Stalking Tiger, Outland Colossus). A per-creature blocker *maximum* — the
    /// inverse of menace (`CantBeBlockedExceptBy { MinBlockers }`, a minimum) and
    /// distinct from `MaxBlockersEachCombat` (a global per-combat cap). Enforced
    /// in `combat.rs` declare-blockers validation.
    CantBeBlockedByMoreThan {
        max: u32,
    },
    /// CR 301.5 + CR 303.4 + CR 701.3a: Positive attachment restriction — this
    /// Aura/Equipment "can be attached only to" a permanent matching `filter`.
    /// The complement of the negative `Other("CantBeEquipped" | "CantBeEnchanted"
    /// | "CantBeAttached")` host-prohibition family: those live on the *host* and
    /// refuse any attachment, whereas this lives on the *attachment* and whitelists
    /// the legal hosts it may attach to. CR 701.3a folds equip/enchant legality
    /// into one attach gate, so a single typed variant covers both Equipment
    /// (CR 301.5) and Aura (CR 303.4) — the `filter` (a reused `TargetFilter`)
    /// expresses "a creature with power N or greater", "a legendary creature",
    /// "an {type}", etc. Corpus: Strata Scythe, Brass Knuckles ("a creature with
    /// power/toughness N or greater"), Konda's Banner ("a legendary creature").
    ///
    /// Data-carrying variant (holds `TargetFilter`) — not registry-registered
    /// (see `coverage::is_data_carrying_static`); enforced via the attachment's
    /// active static definitions in `game/effects/attach.rs::attachment_illegality`.
    /// A candidate host that does not match `filter` is an illegal attach/equip
    /// target (CR 301.5b / CR 303.4j: the attachment doesn't move).
    AttachmentRestriction {
        filter: TargetFilter,
    },
    /// CR 702.16: Protection prevents targeting, blocking, damage, and attachment.
    Protection,
    /// CR 702.12: Indestructible — prevents destruction by lethal damage and destroy effects.
    Indestructible,
    /// Permanent cannot be destroyed (distinct from Indestructible).
    CantBeDestroyed,
    /// CR 701.19c: "[Permanent] can't be regenerated [this turn]." This does not
    /// stop regeneration abilities from being activated or shields from being
    /// created; rather, it causes regeneration shields to not be applied the
    /// next time the affected permanent would be destroyed. The per-target
    /// `StaticDefinition::affected` filter carries which permanent is marked;
    /// runtime enforcement bypasses the regen shield in
    /// `replacement.rs::destroy_applier` (CR 701.19). Distinct from
    /// `CantBeDestroyed` (which prevents destruction outright) and the inline
    /// `Effect::Destroy { cant_regenerate }` (the "Destroy X. It can't be
    /// regenerated." one-shot) — this is the standalone, until-end-of-turn form
    /// (Hurr Jackal, Furnace Brood, Lim-Dûl's Cohort).
    CantBeRegenerated,
    /// CR 702.34: Flashback — allows casting from graveyard, exiled after resolution.
    FlashBack,
    /// CR 702.18: Shroud — permanent cannot be the target of spells or abilities.
    Shroud,
    /// CR 702.11: Hexproof — affected player/permanent cannot be the target of
    /// spells or abilities an opponent controls. Applied at the player scope
    /// ("You have hexproof.") mirroring `Shroud`; permanent-scope hexproof
    /// grants flow through `ContinuousModification::AddKeyword` instead.
    Hexproof,
    /// CR 702.20: Vigilance — attacking doesn't cause this creature to tap.
    Vigilance,
    /// CR 702.111: Menace — can't be blocked except by two or more creatures.
    Menace,
    /// CR 702.17: Reach — can block creatures with flying.
    Reach,
    /// CR 702.9: Flying — can't be blocked except by creatures with flying or reach.
    Flying,
    /// CR 702.19: Trample — excess combat damage is assigned to the defending player.
    Trample,
    /// CR 702.2: Deathtouch — any amount of damage dealt is lethal.
    Deathtouch,
    /// CR 702.15: Lifelink — damage dealt also causes controller to gain that much life.
    Lifelink,

    // -- Tier 2: Rule-modification statics --
    CantTap,
    CantUntap,
    /// CR 509.1c: This creature must be blocked if able — the defending player
    /// must assign at least one legal blocker to it.
    MustBeBlocked,
    /// CR 509.1c: "All creatures able to block this creature do so" (Lure,
    /// Prized Unicorn, Breaker of Armies, …). Unlike [`MustBeBlocked`], this
    /// places a block requirement on *every* creature able to block it: each
    /// such untapped defender must be declared as a blocker of this attacker,
    /// not merely one.
    MustBeBlockedByAll,
    /// CR 701.15b: This creature is goaded for as long as the static applies.
    /// The source controller is the goading player for the "attack another
    /// player if able" requirement.
    Goaded,
    /// CR 506.5 + CR 508.1c + CR 509.1b: Parameterized "alone" combat
    /// restriction.  `action` selects whether it applies to attacking or
    /// blocking; `requirement` selects the polarity:
    /// - `NeedsCompanion` → "can't attack/block alone" (Bonded Construct,
    ///   Mogg Flunkies) — the creature must NOT be the sole attacker/blocker.
    /// - `MustBeSole` → "can only attack alone" (Master of Cruelties) — the
    ///   creature must BE the sole attacker; no companions allowed.
    CombatAlone {
        action: CombatAloneAction,
        requirement: CombatAloneRequirement,
    },
    /// CR 702.122d: This creature can't crew Vehicles.
    CantCrew,
    /// CR 702.122a / CR 702.171a / CR 702.184c: This creature contributes to a
    /// crew/saddle/station cost as though its power were modified (Reckoner
    /// Bankbuster: "as though its power were 2 greater") or using its toughness
    /// instead of its power (Giant Ox). `actions` records which keyword actions
    /// the modifier applies to, since a card may name only some of them.
    CrewContribution {
        kind: CrewContributionKind,
        actions: Vec<CrewAction>,
    },
    MayLookAtTopOfLibrary,
    /// CR 708.5: Static permission to look at face-down permanents the
    /// controller would otherwise not be allowed to see. The default rule lets
    /// you look only at face-down permanents you control; this static lifts that
    /// for the permanents matched by `StaticDefinition::affected` (resolved from
    /// the static's source controller). Parameterized by scope on the affected
    /// filter — "your opponents control" (Found Footage → `controller: Opponent`)
    /// and "you don't control" (Lumbering Laundry → `Not { controller: You }`)
    /// are the same permission, differing only in the filter. The look-permission
    /// is enforced in `visibility.rs` (face-down identity redaction) and surfaced
    /// to the controller, not via layer 7.
    MayLookAtFaceDown,
    /// CR 116.2b + CR 708.7: Prohibition preventing the permanents matched by
    /// `StaticDefinition::affected` from being turned face up. Turning a
    /// face-down permanent face up is a special action (CR 116.2b) that the
    /// rules allowing it normally permit (CR 708.7); this static blocks it for
    /// the affected permanents. The optional timing window rides on
    /// `StaticDefinition::condition` (Karlov Watchdog: `DuringYourTurn` —
    /// "Permanents your opponents control can't be turned face up during your
    /// turn"). The affected filter is resolved from the static's source
    /// controller, so `controller: Opponent` means the source controller's
    /// opponents' permanents. Enforced in `morph::turn_face_up`.
    CantBeTurnedFaceUp,

    // -- Tier 3: Parser-produced statics --
    /// CR 502.3: You may choose not to untap this permanent during your untap step.
    MayChooseNotToUntap,
    /// CR 305.2: Player may play additional lands on each of their turns.
    /// `count` is the number of extra land drops granted (e.g., 1 for Exploration, 2 for Azusa).
    AdditionalLandDrop {
        count: u8,
    },
    EmblemStatic,
    /// CR 509.1b: Blocker-side restriction — this creature can block only
    /// attackers matching `filter` (Cloud Sprite, Pinnacle Emissary Drone).
    BlockRestriction {
        filter: super::ability::TargetFilter,
    },
    /// CR 402.2: No maximum hand size.
    NoMaximumHandSize,
    /// CR 402.2 + CR 514.1: Maximum hand size modification.
    /// Applied during cleanup to determine the discard threshold.
    MaximumHandSize {
        modification: HandSizeModification,
    },
    MayPlayAdditionalLand,

    /// CR 702: Creatures can't have or gain a specific keyword (Archetype cycle).
    /// Prevents both existing instances and future grants of the keyword.
    CantHaveKeyword {
        keyword: Keyword,
    },

    /// CR 104.3a: This player can't win the game (Platinum Angel effect).
    CantWinTheGame,
    /// CR 104.3b: This player can't lose the game (Platinum Angel effect).
    CantLoseTheGame,
    /// CR 704.5j: The "legend rule" doesn't apply to the affected permanents, so
    /// they are excluded from the same-name legendary grouping in the legend-rule
    /// SBA. Scope rides on `StaticDefinition::affected`: `None` = global (Mirror
    /// Gallery, "the legend rule doesn't apply"); a controller-scoped filter =
    /// "doesn't apply to [permanents/tokens/Slivers/commanders] you control"
    /// (Sakashima of a Thousand Faces, Mirror Box, Cadric, Sliver Gravemother,
    /// Try-My-Deck Elemental, ...). Enforced per-permanent in `sba.rs` via
    /// `check_static_ability` with the candidate as the target object.
    LegendRuleDoesntApply,
    /// Speed may increase beyond 4, and 4+ still counts as max speed for that player.
    SpeedCanIncreaseBeyondFour,
    /// CR 118.12a: Defiler cycle — "As an additional cost to cast [color] permanent
    /// spells, you may pay [N] life. Those spells cost {C} less to cast."
    /// Optional life payment during casting with conditional mana reduction.
    DefilerCostReduction {
        /// The color of permanent spells this applies to
        color: ManaColor,
        /// Life cost to pay (e.g., 2 for the Defiler cycle)
        life_cost: u32,
        /// Mana cost reduction if life is paid
        mana_reduction: ManaCost,
    },
    /// CR 614.1b + CR 614.10: "Skip your [step] step" — replacement effect that replaces
    /// the named step with nothing. Parameterized by Phase to cover draw/untap/upkeep.
    SkipStep {
        step: Phase,
    },
    /// CR 609.4b: "You may spend mana as though it were mana of any color" /
    /// "You may spend mana of any type to cast [filtered] spells." Allows the
    /// controller to pay colored mana costs with mana of any type or color.
    ///
    /// `spell_filter` is the leaf parameterization of the spell-class axis (same
    /// CR 609.4b section, so a field, not a sibling variant):
    /// - `None` — board-wide (Chromatic Orrery / Joiner Adept): the concession
    ///   applies to every cost the controller pays (spell casts, effect
    ///   payments, activations).
    /// - `Some(filter)` — scoped to spells the controller casts that match the
    ///   filter (Vizier of the Menagerie: "creature spells"). The concession is
    ///   re-derived against the spell object at spend time and never applies to
    ///   non-spell payments. Consulted by
    ///   `casting::player_can_spend_as_any_color_for_optional_spell`.
    SpendManaAsAnyColor {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spell_filter: Option<TargetFilter>,
    },
    /// CR 107.4f: "For each {C} in a cost, you may pay 2 life rather than pay
    /// that mana." Player-scope payment substitution; the indicated color may
    /// be paid as 2 life instead of 1 colored mana, with the same 1-color-or-2-life
    /// shape Phyrexian mana symbols define. Canonical card: K'rrik, Son of
    /// Yawgmoth (color = Black). `ManaColor` parameterization admits future
    /// printings for any other single color without enum proliferation.
    ///
    /// **Scope note**: this static currently affects spell-cast mana costs only.
    /// Activated-ability mana costs are covered by the same 2024-06-07 K'rrik
    /// ruling but require a `pending_activation` pause/resume primitive not yet
    /// built. Deferred to GH issue #600.
    PayLifeAsColoredMana {
        color: ManaColor,
    },
    /// CR 106.4 + CR 500.5 + CR 703.4q + CR 614.1a: How the affected player's
    /// unspent mana is handled as steps and phases end. `filter` selects which
    /// mana the rule applies to (`None` = every color including colorless;
    /// `Some(color)` = only matching units); `action` is what happens to a
    /// matching unit at the would-be-empty event.
    ///
    /// Unified across the retention family (Upwelling, Electro, Omnath Locus
    /// of Mana, The Last Agni Kai) and the transformation family (Horizon
    /// Stone, Kruphix, Omnath Locus of All, Ozai) per the parameterization
    /// rule — both differ only on what happens at the CR 703.4q event, not on
    /// which event they react to.
    StepEndUnspentMana {
        filter: Option<ManaColor>,
        action: StepEndManaAction,
    },
    /// CR 702.3b: Allows creatures with defender to attack despite having the keyword.
    /// "can attack as though it didn't have defender" overrides the defender restriction.
    CanAttackWithDefender,
    /// CR 509.1b + CR 609.4 + CR 702.14c: Globally cancel the landwalk blocking
    /// restriction for the named qualifier. The attacker still has the keyword
    /// (CR 609.4: "as though" is scoped to the stated effect); only its
    /// blocking-restriction consequence is suppressed. Per CR 702.14d, qualifiers
    /// cancel independently: a swampwalk canceller leaves a co-present islandwalk
    /// intact.
    ///
    /// INVARIANT: `qualifier` MUST match `Keyword::Landwalk`'s canonical capitalized
    /// form (e.g. "Swamp", "Island", "Plains", "Mountain", "Forest"). `None` reserved
    /// for a hypothetical "all landwalk" global canceller; no printed card currently
    /// requires this, but the option preserves room for that class.
    ///
    /// Class: the Portal/Legends "creatures with Xwalk can be blocked as though
    /// they didn't have Xwalk" cycle (Ur-Drago and four siblings — one per basic
    /// land subtype). Global rule modification (`affected: None`); enforced inside
    /// `is_landwalk_unblockable` rather than as a layer-6 ability removal.
    IgnoreLandwalkForBlocking {
        qualifier: Option<String>,
    },
    /// CR 602.5a: Bypasses the summoning-sickness gate on a creature's `{T}`/`{Q}`
    /// activated abilities — "You may activate abilities of creatures you control as
    /// though those creatures had haste." This is NOT `AddKeyword(Haste)`: only the
    /// CR 602.5a activation restriction is lifted, combat attacker validation
    /// (CR 508.1a) is untouched. Canonical card: Tyvar, Jubilant Brawler.
    CanActivateAbilitiesAsThoughHaste,
    /// CR 509.1b + CR 609.4 + CR 702.28b: Per-source block permission that lifts
    /// the shadow blocker-side restriction for the affected creature only — it may
    /// block creatures with shadow despite not having shadow itself. Shadow is the
    /// unique evasion keyword (CR 702.28b) with a symmetric "without-shadow can't
    /// block with-shadow" pairing, so this is keyword-specific (mirrors
    /// `CanAttackWithDefender`) rather than a generic keyword-parameterized form,
    /// which would cross keyword CR sections with distinct runtime resolvers.
    ///
    /// Captures both printed phrasings of the same CR 509.1b outcome: "can block
    /// creatures with shadow as though they didn't have shadow" (Heartwood Dryad)
    /// and "can block creatures with shadow as though it had shadow" (Wall of
    /// Diffusion). `affected: SelfRef` (per-source, not the global rule
    /// modification `IgnoreLandwalkForBlocking` uses). Enforced inside the shadow
    /// block-legality seam in `combat.rs`, not as a layer-6 keyword grant.
    CanBlockShadow,
    /// CR 510.1a: This creature assigns no combat damage.
    /// Used for creatures like Ornithopter of Paradise and various Walls that can
    /// attack/block but deal 0 combat damage.
    AssignNoCombatDamage,
    /// CR 502.3 + CR 113.6: Continuous static that grants a second untap pass
    /// during each OTHER player's untap step. The source's controller untaps
    /// the permanents matching `StaticDefinition.affected` — NOT the active
    /// player's permanents. Canonical card: Seedborn Muse ("Untap all
    /// permanents you control during each other player's untap step.").
    /// Runtime: `turns::execute_untap` runs a second pass after the active
    /// player's normal untap, scanning the battlefield for this variant on
    /// permanents whose controller != active_player.
    UntapsDuringEachOtherPlayersUntapStep,
    /// CR 502.3: "Players can't untap more than `max` `filter` during their
    /// untap steps." A continuous restriction on the untap turn-based action:
    /// while any source with this static is on the battlefield, the active
    /// player may untap at most `max` permanents matching `filter` during their
    /// untap step. CR 502.3 makes the untap a player-determined choice ("the
    /// active player determines which permanents they control will untap"), so
    /// when more than `max` matching permanents are tapped the active player
    /// chooses which `max` untap; the rest stay tapped.
    ///
    /// Built for the class — `filter` carries the permanent type (creature,
    /// artifact, nonbasic land, …) and `max` the cap, covering Smoke /
    /// Stoic Angel (creature), Damping Field / Imi Statue (artifact), and the
    /// Winter Orb / nonbasic-land family in one variant. The restriction is a
    /// global rule modification keyed on the active player, not on the source's
    /// controller — Smoke restricts every player — so the affected-permanent
    /// scope rides inline on `filter` rather than `StaticDefinition::affected`
    /// (mirroring `BlockRestriction` / `CantBeBlockedBy`). Runtime enforcement
    /// is in `turns::execute_untap_with_choices`, which clamps each matching
    /// group to `max`, and `turns::untap_choice_candidates`, which surfaces the
    /// over-cap members for the CR 502.3 player determination.
    MaxUntapPerType {
        filter: TargetFilter,
        max: u32,
    },
    /// CR 614.1c + CR 122.1: Continuous "enters with an additional counter"
    /// replacement static. A permanent matching `StaticDefinition::affected`
    /// (e.g. "Other creatures you control", "Legendary creatures you control",
    /// "Nontoken creatures you control") that would enter the battlefield does
    /// so with `count` additional counters of `counter_type` on it.
    ///
    /// Per CR 614.1c these "enters with …" effects are replacement effects, not
    /// triggered abilities; the affected-permanent scope rides on
    /// `StaticDefinition::affected` (the controller-scoped "you control" plus any
    /// Other/Legendary/Nontoken qualifier), exactly like the anthem statics.
    /// Runtime integration lives in the battlefield-entry counter hook in
    /// `effects/change_zone.rs`, which scans active statics whose `affected`
    /// filter matches the entering object and folds `count` `counter_type`
    /// counters into the entry's counter list.
    ///
    /// Class members (fixed-count form): Kalain, Reclusive Painter; Bard Class;
    /// Gorma the Gullet; Master Chef. The dynamically *scaled* distributive form
    /// (Gev, Scaled Scorch — "enter with … counter on them for each opponent who
    /// lost life this turn") is NOT modeled here (this mode carries a fixed
    /// `count`); it routes instead to the dynamic-capable
    /// `ReplacementEvent::ChangeZone` + `Effect::PutCounter { count: QuantityExpr }`
    /// path via the enters-with-counter replacement parser.
    EntersWithAdditionalCounters {
        counter_type: super::counter::CounterType,
        count: u32,
    },
    /// Fallback for unrecognized static mode strings.
    Other(String),
}

/// Manual Hash impl because `ModifyCost` contains `TargetFilter` and `QuantityRef`
/// which don't implement `Hash`. For data-carrying variants, we hash only the discriminant +
/// simple fields. This is safe because data-carrying variants are never used as HashMap keys
/// (they're handled by `is_data_carrying_static` in coverage.rs instead).
impl Hash for StaticMode {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            StaticMode::ReduceAbilityCost {
                mode,
                keyword,
                amount,
                minimum_mana,
                ..
            } => {
                mode.hash(state);
                keyword.hash(state);
                amount.hash(state);
                minimum_mana.hash(state);
            }
            StaticMode::ModifyActivationLimit { keyword, new_limit } => {
                keyword.hash(state);
                new_limit.hash(state);
            }
            StaticMode::ActivateAsInstant { cost_category } => {
                cost_category.hash(state);
            }
            StaticMode::CrewContribution { kind, actions } => {
                kind.hash(state);
                actions.hash(state);
            }
            StaticMode::ExtraBlockers { count } => count.hash(state),
            StaticMode::MustBlockAttacker { attacker } => attacker.hash(state),
            StaticMode::MustAttackPlayer { player } => player.hash(state),
            StaticMode::MaxAttackersEachCombat { max, defender } => {
                max.hash(state);
                defender.hash(state);
            }
            StaticMode::MaxBlockersEachCombat { max } => max.hash(state),
            // CR 502.3: filter is a non-Hash TargetFilter; hash the enum
            // discriminant alongside the cap so creature/artifact/land caps
            // with the same max don't collide.
            StaticMode::MaxUntapPerType { filter, max } => {
                std::mem::discriminant(filter).hash(state);
                max.hash(state);
            }
            StaticMode::RevealTopOfLibrary { all_players } => all_players.hash(state),
            StaticMode::RevealHand { who } => who.hash(state),
            StaticMode::CantBeBlockedExceptBy { kind } => match kind {
                // TargetFilter does not implement Hash; discriminant only.
                BlockExceptionKind::Quality(_) => {}
                BlockExceptionKind::MinBlockers { min } => min.hash(state),
            },
            StaticMode::CantBeBlockedBy { .. } => {} // TargetFilter does not implement Hash; discriminant only
            StaticMode::BlockRestriction { .. } => {} // TargetFilter does not implement Hash; discriminant only
            StaticMode::AttachmentRestriction { .. } => {} // TargetFilter does not implement Hash; discriminant only
            StaticMode::CantBeBlockedByMoreThan { max } => max.hash(state),
            StaticMode::AdditionalLandDrop { count } => count.hash(state),
            StaticMode::StepEndUnspentMana { filter, action } => {
                filter.hash(state);
                action.hash(state);
            }
            StaticMode::IgnoreLandwalkForBlocking { qualifier } => qualifier.hash(state),
            StaticMode::Other(s) => s.hash(state),
            StaticMode::GraveyardCastPermission {
                frequency,
                play_mode,
                graveyard_destination_replacement,
                extra_cost,
            } => {
                frequency.hash(state);
                play_mode.hash(state);
                graveyard_destination_replacement.hash(state);
                // `AbilityCost` (inside `CastExtraCost`) lacks `Hash` — hash the
                // mode marker only (mirrors the `alt_cost` treatment) so the
                // alternative/additional shapes don't collide.
                extra_cost.as_ref().map(|e| e.mode).hash(state);
            }
            StaticMode::TopOfLibraryCastPermission {
                play_mode,
                frequency,
                ..
            } => {
                // alt_cost contains AbilityCost which lacks Hash; discriminant +
                // play_mode + frequency only.
                play_mode.hash(state);
                frequency.hash(state);
            }
            StaticMode::CastFromHandFree { frequency, origin } => {
                frequency.hash(state);
                origin.hash(state);
            }
            StaticMode::ExileCastPermission {
                frequency,
                play_mode,
                cost,
                pool,
                timing,
                mana_spend_permission,
                grants_flash,
                extra_cost,
            } => {
                frequency.hash(state);
                play_mode.hash(state);
                pool.hash(state);
                timing.hash(state);
                cost.hash(state);
                // `ManaSpendPermission` does not derive `Hash` (mirrors the
                // `TopOfLibraryCastPermission.alt_cost` treatment above) — hash
                // its presence so the two payment-concession shapes don't collide.
                mana_spend_permission.is_some().hash(state);
                grants_flash.hash(state);
                // `AbilityCost` (inside `CastExtraCost`) lacks `Hash` — hash the
                // mode marker only so the alternative/additional shapes differ.
                extra_cost.as_ref().map(|e| e.mode).hash(state);
            }
            // CR 122.2: Zone derives Hash; hash the excluded-zone list so
            // [Hand, Library] does not collide with other zone sets.
            StaticMode::CountersPersistAcrossZones { excluded_zones } => {
                excluded_zones.hash(state);
            }
            StaticMode::SkipStep { step } => step.hash(state),
            StaticMode::DoubleTriggers { cause } => cause.hash(state),
            // CR 107.4f: Parameterized by ManaColor — hash the color so distinct
            // grants (Black vs Red) don't collide.
            StaticMode::PayLifeAsColoredMana { color } => color.hash(state),
            // CR 118.9: Parameterized by KeywordKind — hash the keyword so
            // distinct grants (Cycling vs Crew) don't collide. The `cost` and
            // `frequency` fields are non-Hash / discriminant-covered.
            StaticMode::AlternativeKeywordCost { keyword, .. } => {
                keyword.hash(state);
            }
            // Data-carrying variants with non-Hash fields: discriminant only.
            // These are never used as HashMap keys (handled by is_data_carrying_static).
            StaticMode::ModifyCost { .. }
            | StaticMode::ImposeAdditionalCost { .. }
            | StaticMode::CantPayCost { .. }
            | StaticMode::DefilerCostReduction { .. }
            | StaticMode::CantDraw { .. }
            | StaticMode::PerTurnCastLimit { .. }
            | StaticMode::PerTurnDrawLimit { .. }
            | StaticMode::MaximumHandSize { .. }
            | StaticMode::CastWithKeyword { .. }
            | StaticMode::CastWithAlternativeCost { .. }
            | StaticMode::CantBeActivated { .. }
            | StaticMode::CantActivateDuring { .. }
            | StaticMode::CantSearchLibrary { .. }
            | StaticMode::CantCauseSacrificeOrExile { .. }
            // CR 614.1c: data-carrying (CounterType + count); consumed by direct
            // match in change_zone.rs, never used as a HashMap key.
            | StaticMode::EntersWithAdditionalCounters { .. }
            | StaticMode::SuppressTriggers { .. } => {}
            // All other variants are unit variants — discriminant suffices.
            _ => {}
        }
    }
}

impl StaticMode {
    /// Map bare keyword static modes onto their corresponding keyword identity.
    pub fn as_keyword(&self) -> Option<Keyword> {
        match self {
            StaticMode::Indestructible => Some(Keyword::Indestructible),
            StaticMode::Shroud => Some(Keyword::Shroud),
            StaticMode::Hexproof => Some(Keyword::Hexproof),
            StaticMode::Flying => Some(Keyword::Flying),
            StaticMode::Vigilance => Some(Keyword::Vigilance),
            StaticMode::Menace => Some(Keyword::Menace),
            StaticMode::Reach => Some(Keyword::Reach),
            StaticMode::Trample => Some(Keyword::Trample),
            StaticMode::Deathtouch => Some(Keyword::Deathtouch),
            StaticMode::Lifelink => Some(Keyword::Lifelink),
            StaticMode::Continuous
            | StaticMode::CantAttack
            | StaticMode::CantBlock
            | StaticMode::CantAttackOrBlock
            | StaticMode::CantBecomeSuspected
            | StaticMode::MaxAttackersEachCombat { .. }
            | StaticMode::MaxBlockersEachCombat { .. }
            | StaticMode::CantBeTargeted
            | StaticMode::CantBeCast { .. }
            | StaticMode::CantBeActivated { .. }
            | StaticMode::CantSearchLibrary { .. }
            | StaticMode::CantCauseSacrificeOrExile { .. }
            | StaticMode::CastWithFlash
            | StaticMode::GrantsExtraVote
            | StaticMode::GrantsExtraVillainousChoice
            | StaticMode::CastWithKeyword { .. }
            | StaticMode::CastWithAlternativeCost { .. }
            | StaticMode::AlternativeKeywordCost { .. }
            | StaticMode::ModifyCost { .. }
            | StaticMode::ImposeAdditionalCost { .. }
            | StaticMode::ReduceAbilityCost { .. }
            | StaticMode::ModifyActivationLimit { .. }
            | StaticMode::ActivateAsInstant { .. }
            | StaticMode::CantPayCost { .. }
            | StaticMode::CantGainLife
            | StaticMode::CantLoseLife
            | StaticMode::PlayerProtection(_)
            | StaticMode::MustAttack
            | StaticMode::MustAttackPlayer { .. }
            | StaticMode::MustBlock
            | StaticMode::MustBlockAttacker { .. }
            | StaticMode::CantDraw { .. }
            | StaticMode::DoubleTriggers { .. }
            | StaticMode::IgnoreHexproof
            | StaticMode::ExtraBlockers { .. }
            | StaticMode::RevealTopOfLibrary { .. }
            | StaticMode::RevealHand { .. }
            | StaticMode::GraveyardCastPermission { .. }
            | StaticMode::TopOfLibraryCastPermission { .. }
            | StaticMode::CastFromHandFree { .. }
            | StaticMode::ExileCastPermission { .. }
            | StaticMode::CountersPersistAcrossZones { .. }
            | StaticMode::CantBeCountered
            | StaticMode::CantBeCopied
            | StaticMode::CantEnterBattlefieldFrom
            | StaticMode::CantCastFrom { .. }
            | StaticMode::CantCastDuring { .. }
            | StaticMode::CantActivateDuring { .. }
            | StaticMode::PerTurnCastLimit { .. }
            | StaticMode::PerTurnDrawLimit { .. }
            | StaticMode::SuppressTriggers { .. }
            | StaticMode::CantBeBlocked
            | StaticMode::CantBeBlockedExceptBy { .. }
            | StaticMode::CantBeBlockedBy { .. }
            | StaticMode::CantBeBlockedByMoreThan { .. }
            | StaticMode::AttachmentRestriction { .. }
            | StaticMode::Protection
            | StaticMode::CantBeDestroyed
            | StaticMode::CantBeRegenerated
            | StaticMode::FlashBack
            | StaticMode::CantTap
            | StaticMode::CantUntap
            | StaticMode::MustBeBlocked
            | StaticMode::MustBeBlockedByAll
            | StaticMode::Goaded
            | StaticMode::CombatAlone { .. }
            | StaticMode::CantCrew
            | StaticMode::CrewContribution { .. }
            | StaticMode::MayLookAtTopOfLibrary
            | StaticMode::MayLookAtFaceDown
            | StaticMode::CantBeTurnedFaceUp
            | StaticMode::MayChooseNotToUntap
            | StaticMode::AdditionalLandDrop { .. }
            | StaticMode::EmblemStatic
            | StaticMode::BlockRestriction { .. }
            | StaticMode::NoMaximumHandSize
            | StaticMode::MaximumHandSize { .. }
            | StaticMode::MayPlayAdditionalLand
            | StaticMode::CantHaveKeyword { .. }
            | StaticMode::CantWinTheGame
            | StaticMode::CantLoseTheGame
            | StaticMode::LegendRuleDoesntApply
            | StaticMode::SpeedCanIncreaseBeyondFour
            | StaticMode::DefilerCostReduction { .. }
            | StaticMode::SkipStep { .. }
            | StaticMode::SpendManaAsAnyColor { .. }
            | StaticMode::PayLifeAsColoredMana { .. }
            | StaticMode::StepEndUnspentMana { .. }
            | StaticMode::CanAttackWithDefender
            | StaticMode::IgnoreLandwalkForBlocking { .. }
            | StaticMode::CanActivateAbilitiesAsThoughHaste
            | StaticMode::CanBlockShadow
            | StaticMode::AssignNoCombatDamage
            | StaticMode::UntapsDuringEachOtherPlayersUntapStep
            | StaticMode::MaxUntapPerType { .. }
            | StaticMode::EntersWithAdditionalCounters { .. }
            | StaticMode::LinkedCollectionCounterPlayPermission
            | StaticMode::DamageNotRemovedDuringCleanup
            | StaticMode::Other(_) => None,
        }
    }
}

impl fmt::Display for StaticMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StaticMode::Continuous => write!(f, "Continuous"),
            StaticMode::DamageNotRemovedDuringCleanup => {
                write!(f, "DamageNotRemovedDuringCleanup")
            }
            StaticMode::CantAttack => write!(f, "CantAttack"),
            StaticMode::CantBlock => write!(f, "CantBlock"),
            StaticMode::CantAttackOrBlock => write!(f, "CantAttackOrBlock"),
            StaticMode::CantBecomeSuspected => write!(f, "CantBecomeSuspected"),
            StaticMode::MaxAttackersEachCombat { max, defender } => match defender {
                None => write!(f, "MaxAttackersEachCombat({max})"),
                Some(AttackDefenderScope::Controller) => {
                    write!(f, "MaxAttackersEachCombat({max},Controller)")
                }
            },
            StaticMode::MaxBlockersEachCombat { max } => {
                write!(f, "MaxBlockersEachCombat({max})")
            }
            StaticMode::CantBeTargeted => write!(f, "CantBeTargeted"),
            StaticMode::CantBeCast { who } => write!(f, "CantBeCast({who})"),
            StaticMode::CantBeActivated { who, .. } => write!(f, "CantBeActivated({who})"),
            StaticMode::CantSearchLibrary { cause } => write!(f, "CantSearchLibrary({cause})"),
            StaticMode::CantCauseSacrificeOrExile { cause } => {
                write!(f, "CantCauseSacrificeOrExile({cause})")
            }
            StaticMode::SuppressTriggers { events, .. } => {
                let parts: Vec<String> = events.iter().map(|e| e.to_string()).collect();
                write!(f, "SuppressTriggers({})", parts.join("+"))
            }
            StaticMode::CastWithFlash => write!(f, "CastWithFlash"),
            StaticMode::GrantsExtraVote => write!(f, "GrantsExtraVote"),
            StaticMode::GrantsExtraVillainousChoice => {
                write!(f, "GrantsExtraVillainousChoice")
            }
            StaticMode::CastWithKeyword { keyword } => {
                write!(f, "CastWithKeyword({keyword:?})")
            }
            StaticMode::CastWithAlternativeCost { cost, .. } => {
                write!(f, "CastWithAlternativeCost({cost:?})")
            }
            StaticMode::AlternativeKeywordCost { keyword, .. } => {
                write!(f, "AlternativeKeywordCost({keyword:?})")
            }
            StaticMode::ModifyCost { mode, .. } => match mode {
                CostModifyMode::Reduce => write!(f, "ReduceCost"),
                CostModifyMode::Raise => write!(f, "RaiseCost"),
                CostModifyMode::Minimum => write!(f, "MinimumCost"),
            },
            StaticMode::ImposeAdditionalCost { action, .. } => match action {
                AdditionalCostTaxAction::Cast => write!(f, "ImposeAdditionalCastCost"),
            },
            StaticMode::ReduceAbilityCost {
                mode,
                keyword,
                amount,
                minimum_mana,
                ..
            } => {
                // CR 118.7: Encode the direction so the registry round-trip is
                // lossless. A leading "+"/"-" marks Raise/Reduce; legacy strings
                // with no marker default to Reduce in `from_str` for back-compat.
                let sign = match mode {
                    CostModifyMode::Raise => "+",
                    _ => "-",
                };
                if let Some(minimum_mana) = minimum_mana {
                    write!(
                        f,
                        "ReduceAbilityCost({sign}{keyword},{amount},{minimum_mana})"
                    )
                } else {
                    write!(f, "ReduceAbilityCost({sign}{keyword},{amount})")
                }
            }
            StaticMode::ModifyActivationLimit { keyword, new_limit } => {
                write!(f, "ModifyActivationLimit({keyword},{new_limit})")
            }
            StaticMode::ActivateAsInstant { cost_category } => {
                write!(f, "ActivateAsInstant({cost_category:?})")
            }
            StaticMode::CantPayCost { who, cost } => write!(f, "CantPayCost({who},{cost})"),
            StaticMode::CantGainLife => write!(f, "CantGainLife"),
            StaticMode::CantLoseLife => write!(f, "CantLoseLife"),
            StaticMode::PlayerProtection(target) => {
                write!(f, "PlayerProtection({target:?})")
            }
            StaticMode::MustAttack => write!(f, "MustAttack"),
            StaticMode::MustAttackPlayer { player } => {
                write!(f, "MustAttackPlayer({player:?})")
            }
            StaticMode::MustBlock => write!(f, "MustBlock"),
            StaticMode::MustBlockAttacker { attacker } => {
                write!(f, "MustBlockAttacker({attacker:?})")
            }
            StaticMode::CantDraw { who } => write!(f, "CantDraw({who})"),
            StaticMode::DoubleTriggers { cause } => write!(f, "DoubleTriggers({cause})"),
            StaticMode::IgnoreHexproof => write!(f, "IgnoreHexproof"),
            StaticMode::GraveyardCastPermission {
                frequency,
                play_mode,
                graveyard_destination_replacement,
                extra_cost,
            } => {
                write!(f, "GraveyardCastPermission({play_mode},{frequency}")?;
                if matches!(graveyard_destination_replacement, Some(Zone::Exile)) {
                    write!(f, ",exile_on_graveyard")?;
                }
                // CR 601.2f: the extra_cost payload is preserved through serde,
                // not the Display round-trip; emit only a tagged marker so the
                // historical 2-/3-segment forms keep parsing unchanged.
                if let Some(extra) = extra_cost {
                    write!(f, ",extra_cost={}", extra.mode)?;
                }
                write!(f, ")")
            }
            StaticMode::TopOfLibraryCastPermission {
                play_mode,
                frequency,
                alt_cost,
            } => {
                // CR 601.2a: `frequency` is appended as a tagged segment only
                // when non-default (`OncePerTurn`) so the historical
                // 1-/2-segment Unlimited forms keep parsing unchanged.
                let freq_seg = if frequency.is_unlimited() {
                    String::new()
                } else {
                    format!(",freq={frequency}")
                };
                if alt_cost.is_some() {
                    write!(
                        f,
                        "TopOfLibraryCastPermission({play_mode}{freq_seg},alt_cost)"
                    )
                } else {
                    write!(f, "TopOfLibraryCastPermission({play_mode}{freq_seg})")
                }
            }
            StaticMode::CastFromHandFree { frequency, origin } => {
                if matches!(origin, CastFreeOrigin::Hand) {
                    write!(f, "CastFromHandFree({frequency})")
                } else {
                    write!(f, "CastFromHandFree({frequency},{origin})")
                }
            }
            StaticMode::ExileCastPermission {
                frequency,
                play_mode,
                cost,
                pool,
                timing,
                mana_spend_permission,
                grants_flash,
                extra_cost,
            } => {
                // Positional, lossless round-trip. Segments 1-2 (play_mode,
                // frequency) are always present; the optional "free" cost
                // marker, the pool scope, the timing scope, the any-type-mana
                // spend marker, the flash-grant marker, and the extra-cost
                // marker are appended as tagged segments only when non-default
                // so the historical 2-/3-segment Maralen forms keep parsing
                // unchanged.
                write!(f, "ExileCastPermission({play_mode},{frequency}")?;
                if matches!(cost, ExileCastCost::WithoutPayingManaCost) {
                    write!(f, ",free")?;
                }
                if matches!(pool, ExileCardPool::Persistent) {
                    write!(f, ",pool={pool}")?;
                }
                if matches!(timing, ExileCastTiming::YourTurnOnly) {
                    write!(f, ",timing={timing}")?;
                }
                if mana_spend_permission.is_some() {
                    write!(f, ",anymana")?;
                }
                if *grants_flash {
                    write!(f, ",flash")?;
                }
                // CR 118.9 + CR 601.2f: extra_cost payload preserved through
                // serde; emit only the mode marker here.
                if let Some(extra) = extra_cost {
                    write!(f, ",extra_cost={}", extra.mode)?;
                }
                write!(f, ")")
            }
            // CR 122.2: Diagnostic Display lists the excluded destination zones.
            StaticMode::CountersPersistAcrossZones { excluded_zones } => {
                let zones: Vec<String> = excluded_zones.iter().map(|z| format!("{z:?}")).collect();
                write!(f, "CountersPersistAcrossZones({})", zones.join("+"))
            }
            StaticMode::CantBeCountered => write!(f, "CantBeCountered"),
            StaticMode::CantBeCopied => write!(f, "CantBeCopied"),
            StaticMode::CantEnterBattlefieldFrom => write!(f, "CantEnterBattlefieldFrom"),
            StaticMode::CantCastFrom { who } => write!(f, "CantCastFrom({who})"),
            StaticMode::CantCastDuring { who, when } => {
                write!(f, "CantCastDuring({who},{when})")
            }
            // CR 602.5 + CR 117.1b: Diagnostic-only Display; `exemption` is data-carrying
            // and omitted (mirrors `CantBeActivated`'s Display which also drops
            // `source_filter` + `exemption`).
            StaticMode::CantActivateDuring { who, when, .. } => {
                write!(f, "CantActivateDuring({who},{when})")
            }
            StaticMode::PerTurnCastLimit { who, max, .. } => {
                write!(f, "PerTurnCastLimit({who},{max})")
            }
            StaticMode::PerTurnDrawLimit { who, max } => {
                write!(f, "PerTurnDrawLimit({who},{max})")
            }
            StaticMode::ExtraBlockers { count } => match count {
                None => write!(f, "ExtraBlockers(any)"),
                Some(n) => write!(f, "ExtraBlockers({n})"),
            },
            StaticMode::RevealTopOfLibrary { all_players } => {
                if *all_players {
                    write!(f, "RevealTopOfLibrary(all)")
                } else {
                    write!(f, "RevealTopOfLibrary(you)")
                }
            }
            StaticMode::RevealHand { who } => write!(f, "RevealHand({who})"),
            // Tier 1
            StaticMode::CantBeBlocked => write!(f, "CantBeBlocked"),
            StaticMode::CantBeBlockedExceptBy { kind } => match kind {
                BlockExceptionKind::MinBlockers { min } => {
                    write!(f, "CantBeBlockedExceptBy:Min:{min}")
                }
                // TargetFilter has no parseable string form — Debug format, one-way
                // (mirrors CantBeBlockedBy above). No from_str reconstruction.
                BlockExceptionKind::Quality(filter) => {
                    write!(f, "CantBeBlockedExceptBy:Quality({filter:?})")
                }
            },
            StaticMode::CantBeBlockedBy { filter } => {
                write!(f, "CantBeBlockedBy({filter:?})")
            }
            // CR 301.5 + CR 303.4: TargetFilter has no parseable string form —
            // Debug format, one-way (mirrors CantBeBlockedBy). No from_str
            // reconstruction; the variant is data-carrying.
            StaticMode::AttachmentRestriction { filter } => {
                write!(f, "AttachmentRestriction({filter:?})")
            }
            StaticMode::CantBeBlockedByMoreThan { max } => {
                write!(f, "CantBeBlockedByMoreThan({max})")
            }
            StaticMode::Protection => write!(f, "Protection"),
            StaticMode::Indestructible => write!(f, "Indestructible"),
            StaticMode::CantBeDestroyed => write!(f, "CantBeDestroyed"),
            StaticMode::CantBeRegenerated => write!(f, "CantBeRegenerated"),
            StaticMode::FlashBack => write!(f, "FlashBack"),
            StaticMode::Shroud => write!(f, "Shroud"),
            StaticMode::Hexproof => write!(f, "Hexproof"),
            StaticMode::Vigilance => write!(f, "Vigilance"),
            StaticMode::Menace => write!(f, "Menace"),
            StaticMode::Reach => write!(f, "Reach"),
            StaticMode::Flying => write!(f, "Flying"),
            StaticMode::Trample => write!(f, "Trample"),
            StaticMode::Deathtouch => write!(f, "Deathtouch"),
            StaticMode::Lifelink => write!(f, "Lifelink"),
            // Tier 2
            StaticMode::CantTap => write!(f, "CantTap"),
            StaticMode::CantUntap => write!(f, "CantUntap"),
            StaticMode::MustBeBlocked => write!(f, "MustBeBlocked"),
            StaticMode::MustBeBlockedByAll => write!(f, "MustBeBlockedByAll"),
            StaticMode::Goaded => write!(f, "Goaded"),
            StaticMode::CombatAlone {
                action,
                requirement,
            } => {
                let a = match action {
                    CombatAloneAction::Attack => "Attack",
                    CombatAloneAction::Block => "Block",
                };
                let r = match requirement {
                    CombatAloneRequirement::NeedsCompanion => "NeedsCompanion",
                    CombatAloneRequirement::MustBeSole => "MustBeSole",
                };
                write!(f, "CombatAlone({a},{r})")
            }
            StaticMode::CantCrew => write!(f, "CantCrew"),
            // Debug format, one-way (mirrors CantBeBlockedBy). No from_str reconstruction.
            StaticMode::CrewContribution { kind, actions } => {
                write!(f, "CrewContribution({kind:?},{actions:?})")
            }
            StaticMode::MayLookAtTopOfLibrary => write!(f, "MayLookAtTopOfLibrary"),
            StaticMode::MayLookAtFaceDown => write!(f, "MayLookAtFaceDown"),
            StaticMode::CantBeTurnedFaceUp => write!(f, "CantBeTurnedFaceUp"),
            // Tier 3
            StaticMode::MayChooseNotToUntap => write!(f, "MayChooseNotToUntap"),
            StaticMode::AdditionalLandDrop { count } => {
                write!(f, "AdditionalLandDrop({count})")
            }
            StaticMode::EmblemStatic => write!(f, "EmblemStatic"),
            StaticMode::BlockRestriction { filter } => {
                if *filter == block_only_creatures_with_flying_filter() {
                    write!(f, "BlockRestriction")
                } else {
                    write!(f, "BlockRestriction:Quality({filter:?})")
                }
            }
            StaticMode::NoMaximumHandSize => write!(f, "NoMaximumHandSize"),
            StaticMode::MaximumHandSize { modification } => {
                write!(f, "MaximumHandSize({modification})")
            }
            StaticMode::MayPlayAdditionalLand => write!(f, "MayPlayAdditionalLand"),
            StaticMode::CantHaveKeyword { keyword } => {
                write!(f, "CantHaveKeyword({keyword:?})")
            }
            StaticMode::CantWinTheGame => write!(f, "CantWinTheGame"),
            StaticMode::CantLoseTheGame => write!(f, "CantLoseTheGame"),
            StaticMode::LegendRuleDoesntApply => write!(f, "LegendRuleDoesntApply"),
            StaticMode::SpeedCanIncreaseBeyondFour => write!(f, "SpeedCanIncreaseBeyondFour"),
            StaticMode::DefilerCostReduction { color, .. } => {
                write!(f, "DefilerCostReduction({color:?})")
            }
            StaticMode::SkipStep { step } => write!(f, "SkipStep({step:?})"),
            StaticMode::SpendManaAsAnyColor { .. } => write!(f, "SpendManaAsAnyColor"),
            // CR 107.4f: K'rrik-class life-for-color payment substitution.
            StaticMode::PayLifeAsColoredMana { color } => {
                write!(f, "PayLifeAsColoredMana({color:?})")
            }
            StaticMode::StepEndUnspentMana { filter, action } => {
                write!(f, "StepEndUnspentMana({filter:?},{action})")
            }
            StaticMode::CanAttackWithDefender => write!(f, "CanAttackWithDefender"),
            // CR 509.1b + CR 609.4 + CR 702.14c: Display follows the existing
            // parenthesized-payload pattern. `None` = all-landwalk canceller.
            StaticMode::IgnoreLandwalkForBlocking { qualifier } => match qualifier {
                None => write!(f, "IgnoreLandwalkForBlocking"),
                Some(q) => write!(f, "IgnoreLandwalkForBlocking({q})"),
            },
            StaticMode::CanActivateAbilitiesAsThoughHaste => {
                write!(f, "CanActivateAbilitiesAsThoughHaste")
            }
            StaticMode::CanBlockShadow => write!(f, "CanBlockShadow"),
            StaticMode::AssignNoCombatDamage => write!(f, "AssignNoCombatDamage"),
            StaticMode::UntapsDuringEachOtherPlayersUntapStep => {
                write!(f, "UntapsDuringEachOtherPlayersUntapStep")
            }
            // CR 502.3: Debug format, one-way (mirrors BlockRestriction). The
            // `filter` carries a TargetFilter, so no from_str reconstruction.
            StaticMode::MaxUntapPerType { filter, max } => {
                write!(f, "MaxUntapPerType({max},{filter:?})")
            }
            // CR 614.1c + CR 122.1: "enters with an additional [counter] counter"
            // — Display carries both the counter type and the fixed count.
            StaticMode::EntersWithAdditionalCounters {
                counter_type,
                count,
            } => {
                write!(f, "EntersWithAdditionalCounters({counter_type:?},{count})")
            }
            StaticMode::LinkedCollectionCounterPlayPermission => {
                write!(f, "LinkedCollectionCounterPlayPermission")
            }
            // Fallback
            StaticMode::Other(s) => write!(f, "{s}"),
        }
    }
}

impl FromStr for StaticMode {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mode = match s {
            "Continuous" => StaticMode::Continuous,
            "CantAttack" => StaticMode::CantAttack,
            "CantBlock" => StaticMode::CantBlock,
            "CantAttackOrBlock" => StaticMode::CantAttackOrBlock,
            "CantBecomeSuspected" => StaticMode::CantBecomeSuspected,
            "LinkedCollectionCounterPlayPermission" => {
                StaticMode::LinkedCollectionCounterPlayPermission
            }
            s if parse_max_attackers_each_combat_args(s).is_some() => {
                let (max, defender) = parse_max_attackers_each_combat_args(s).unwrap();
                StaticMode::MaxAttackersEachCombat { max, defender }
            }
            s if parse_static_mode_u32_arg(s, "MaxBlockersEachCombat").is_some() => {
                StaticMode::MaxBlockersEachCombat {
                    max: parse_static_mode_u32_arg(s, "MaxBlockersEachCombat").unwrap(),
                }
            }
            s if parse_static_mode_u32_arg(s, "CantBeBlockedByMoreThan").is_some() => {
                StaticMode::CantBeBlockedByMoreThan {
                    max: parse_static_mode_u32_arg(s, "CantBeBlockedByMoreThan").unwrap(),
                }
            }
            "CantBeTargeted" => StaticMode::CantBeTargeted,
            "CantBeCast" => StaticMode::CantBeCast {
                who: ProhibitionScope::Controller,
            },
            // CR 602.5: Legacy unit-string defaults to the self-reference case
            // (Chalice-of-Life-class): `who = AllPlayers, source_filter = SelfRef`.
            // This preserves backward compatibility for the Forge DB constructor and
            // any card-data JSON that serialized the pre-widening form.
            "CantBeActivated" => StaticMode::CantBeActivated {
                who: ProhibitionScope::AllPlayers,
                source_filter: TargetFilter::SelfRef,
                // CR 605.1a: Default to no exemption — legacy serialized form predates
                // the mana-ability exemption field.
                exemption: ActivationExemption::None,
            },
            "CastWithFlash" => StaticMode::CastWithFlash,
            "ReduceCost" => StaticMode::ModifyCost {
                mode: CostModifyMode::Reduce,
                amount: ManaCost::zero(),
                spell_filter: None,
                dynamic_count: None,
            },
            s if s.starts_with("ReduceAbilityCost(") => {
                // Parse "ReduceAbilityCost([+|-]keyword,amount[,minimum_mana])".
                // CR 118.7: a leading "+"/"-" on the keyword marks Raise/Reduce;
                // legacy strings without a marker default to Reduce.
                let inner = s
                    .strip_prefix("ReduceAbilityCost(")
                    .and_then(|s| s.strip_suffix(')'));
                if let Some(inner) = inner {
                    let mut parts = inner.split(',');
                    if let (Some(kw), Some(amt), extra) = (parts.next(), parts.next(), parts.next())
                    {
                        let (mode, keyword) = if let Some(rest) = kw.strip_prefix('+') {
                            (CostModifyMode::Raise, rest)
                        } else if let Some(rest) = kw.strip_prefix('-') {
                            (CostModifyMode::Reduce, rest)
                        } else {
                            (CostModifyMode::Reduce, kw)
                        };
                        StaticMode::ReduceAbilityCost {
                            mode,
                            keyword: keyword.to_string(),
                            amount: amt.parse().unwrap_or(1),
                            minimum_mana: extra.and_then(|value| value.parse().ok()),
                            dynamic_count: None,
                        }
                    } else {
                        StaticMode::Other(s.to_string())
                    }
                } else {
                    StaticMode::Other(s.to_string())
                }
            }
            s if s.starts_with("ModifyActivationLimit(") => {
                let inner = s
                    .strip_prefix("ModifyActivationLimit(")
                    .and_then(|s| s.strip_suffix(')'));
                if let Some(inner) = inner {
                    let mut parts = inner.split(',');
                    if let (Some(kw), Some(limit)) = (parts.next(), parts.next()) {
                        StaticMode::ModifyActivationLimit {
                            keyword: kw.to_string(),
                            new_limit: limit.parse().unwrap_or(2),
                        }
                    } else {
                        StaticMode::Other(s.to_string())
                    }
                } else {
                    StaticMode::Other(s.to_string())
                }
            }
            s if s.starts_with("ActivateAsInstant(") => {
                let inner = s
                    .strip_prefix("ActivateAsInstant(")
                    .and_then(|s| s.strip_suffix(')'));
                match inner {
                    Some("PaysLoyalty") => StaticMode::ActivateAsInstant {
                        cost_category: CostCategory::PaysLoyalty,
                    },
                    _ => StaticMode::Other(s.to_string()),
                }
            }
            "RaiseCost" => StaticMode::ModifyCost {
                mode: CostModifyMode::Raise,
                amount: ManaCost::zero(),
                spell_filter: None,
                dynamic_count: None,
            },
            // CR 601.2f: Cost-floor static (Trinisphere class). Legacy unit-string
            // defaults to a zero floor — meaningful instances are constructed via
            // the parser with the printed amount.
            "MinimumCost" => StaticMode::ModifyCost {
                mode: CostModifyMode::Minimum,
                amount: ManaCost::zero(),
                spell_filter: None,
                dynamic_count: None,
            },
            "CantPayCost" => StaticMode::CantPayCost {
                who: ProhibitionScope::AllPlayers,
                cost: CostPaymentProhibition::PayLife,
            },
            "CantGainLife" => StaticMode::CantGainLife,
            "CantLoseLife" => StaticMode::CantLoseLife,
            "MustAttack" => StaticMode::MustAttack,
            "MustBlock" => StaticMode::MustBlock,
            // CR 603.2d: Legacy name for backward-compat with any already-serialized
            // card data. Canonical form is `DoubleTriggers(EntersBattlefield(...))`.
            "Panharmonicon" => StaticMode::DoubleTriggers {
                cause: TriggerCause::EntersBattlefield {
                    core_types: vec![
                        super::card_type::CoreType::Artifact,
                        super::card_type::CoreType::Creature,
                    ],
                },
            },
            "IgnoreHexproof" => StaticMode::IgnoreHexproof,
            "GraveyardCastPermission" => StaticMode::GraveyardCastPermission {
                frequency: CastFrequency::OncePerTurn,
                play_mode: CardPlayMode::Cast,
                graveyard_destination_replacement: None,
                extra_cost: None,
            },
            s if s.starts_with("GraveyardCastPermission(") => {
                let inner = s
                    .strip_prefix("GraveyardCastPermission(")
                    .and_then(|s| s.strip_suffix(')'))
                    .unwrap_or("");
                let parts = inner.split(',').collect::<Vec<_>>();
                if let [pm, freq, rest @ ..] = parts.as_slice() {
                    StaticMode::GraveyardCastPermission {
                        play_mode: pm.parse().unwrap_or(CardPlayMode::Cast),
                        frequency: freq.parse().unwrap_or(CastFrequency::OncePerTurn),
                        graveyard_destination_replacement: rest
                            .contains(&"exile_on_graveyard")
                            .then_some(Zone::Exile),
                        // CR 601.2f: the extra_cost payload is preserved through
                        // serde, not the FromStr round-trip, so FromStr defaults
                        // to None.
                        extra_cost: None,
                    }
                } else {
                    StaticMode::GraveyardCastPermission {
                        frequency: CastFrequency::OncePerTurn,
                        play_mode: CardPlayMode::Cast,
                        graveyard_destination_replacement: None,
                        extra_cost: None,
                    }
                }
            }
            // CR 401.5 + CR 118.9: Top-of-library cast permission. The Display
            // form omits the alt_cost payload (it's preserved through serde,
            // not the FromStr round-trip), so FromStr defaults alt_cost to None.
            "TopOfLibraryCastPermission" => StaticMode::TopOfLibraryCastPermission {
                play_mode: CardPlayMode::Cast,
                frequency: CastFrequency::Unlimited,
                alt_cost: None,
            },
            s if s.starts_with("TopOfLibraryCastPermission(") => {
                // Display form: "TopOfLibraryCastPermission(<play_mode>
                // [,freq=<frequency>][,alt_cost])". The first segment is
                // positional; the tagged freq= segment and the "alt_cost"
                // marker are present only when non-default, so the historical
                // 1-/2-segment forms round-trip unchanged.
                let inner = s
                    .strip_prefix("TopOfLibraryCastPermission(")
                    .and_then(|s| s.strip_suffix(')'))
                    .unwrap_or("");
                let mut parts = inner.split(',');
                let play_mode = parts
                    .next()
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(CardPlayMode::Cast);
                let frequency = parts
                    .clone()
                    .find_map(|p| p.strip_prefix("freq="))
                    .and_then(|f| f.parse().ok())
                    .unwrap_or(CastFrequency::Unlimited);
                StaticMode::TopOfLibraryCastPermission {
                    play_mode,
                    frequency,
                    // CR 118.9: the alt_cost payload is preserved through serde,
                    // not the FromStr round-trip, so FromStr defaults to None.
                    alt_cost: None,
                }
            }
            "CastFromHandFree" => StaticMode::CastFromHandFree {
                frequency: CastFrequency::Unlimited,
                origin: CastFreeOrigin::Hand,
            },
            s if s.starts_with("CastFromHandFree(") => {
                let inner = s
                    .strip_prefix("CastFromHandFree(")
                    .and_then(|s| s.strip_suffix(')'))
                    .unwrap_or("unlimited");
                let mut parts = inner.split(',');
                let freq = parts.next().unwrap_or("unlimited");
                let origin = parts
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(CastFreeOrigin::Hand);
                StaticMode::CastFromHandFree {
                    frequency: freq.parse().unwrap_or(CastFrequency::Unlimited),
                    origin,
                }
            }
            "ExileCastPermission" => StaticMode::ExileCastPermission {
                frequency: CastFrequency::Unlimited,
                play_mode: CardPlayMode::Cast,
                cost: ExileCastCost::PayNormalCost,
                pool: ExileCardPool::ThisTurn,
                timing: ExileCastTiming::AnyTime,
                mana_spend_permission: None,
                grants_flash: false,
                extra_cost: None,
            },
            s if s.starts_with("ExileCastPermission(") => {
                // Display form: "ExileCastPermission(<play_mode>,<frequency>[,free]
                // [,pool=<scope>][,timing=<scope>])". The first two segments are
                // positional; the optional "free" cost marker and the tagged
                // pool=/timing= segments are present only when non-default, so
                // the historical 2-/3-segment Maralen forms round-trip unchanged.
                let inner = s
                    .strip_prefix("ExileCastPermission(")
                    .and_then(|s| s.strip_suffix(')'))
                    .unwrap_or("");
                let mut parts = inner.split(',');
                let play_mode = parts
                    .next()
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(CardPlayMode::Cast);
                let frequency = parts
                    .next()
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(CastFrequency::Unlimited);
                let mut cost = ExileCastCost::PayNormalCost;
                let mut pool = ExileCardPool::ThisTurn;
                let mut timing = ExileCastTiming::AnyTime;
                let mut mana_spend_permission = None;
                let mut grants_flash = false;
                for seg in parts {
                    if seg == "free" {
                        cost = ExileCastCost::WithoutPayingManaCost;
                    } else if seg == "anymana" {
                        // CR 609.4b: any-type-mana spend concession.
                        mana_spend_permission =
                            Some(crate::types::ability::ManaSpendPermission::AnyTypeOrColor);
                    } else if seg == "flash" {
                        // CR 601.3b: cast-as-though-flash concession.
                        grants_flash = true;
                    } else if let Some(scope) = seg.strip_prefix("pool=") {
                        if let Ok(p) = scope.parse() {
                            pool = p;
                        }
                    } else if let Some(scope) = seg.strip_prefix("timing=") {
                        if let Ok(t) = scope.parse() {
                            timing = t;
                        }
                    }
                    // CR 118.9 + CR 601.2f: the extra_cost payload rides on serde,
                    // not the Display round-trip — the "extra_cost=<mode>" marker
                    // is diagnostic-only, so FromStr defaults the field to None.
                }
                StaticMode::ExileCastPermission {
                    frequency,
                    play_mode,
                    cost,
                    pool,
                    timing,
                    mana_spend_permission,
                    grants_flash,
                    extra_cost: None,
                }
            }
            "CantBeCountered" => StaticMode::CantBeCountered,
            "CantBeCopied" => StaticMode::CantBeCopied,
            "CantEnterBattlefieldFrom" => StaticMode::CantEnterBattlefieldFrom,
            // Tier 1
            "CantBeBlocked" => StaticMode::CantBeBlocked,
            "Protection" => StaticMode::Protection,
            "Indestructible" => StaticMode::Indestructible,
            "CantBeDestroyed" => StaticMode::CantBeDestroyed,
            // CR 701.19c: "[Permanent] can't be regenerated."
            "CantBeRegenerated" => StaticMode::CantBeRegenerated,
            "FlashBack" => StaticMode::FlashBack,
            "Shroud" => StaticMode::Shroud,
            "Hexproof" => StaticMode::Hexproof,
            "Vigilance" => StaticMode::Vigilance,
            "Menace" => StaticMode::Menace,
            "Reach" => StaticMode::Reach,
            "Flying" => StaticMode::Flying,
            "Trample" => StaticMode::Trample,
            "Deathtouch" => StaticMode::Deathtouch,
            "Lifelink" => StaticMode::Lifelink,
            // Tier 2
            "CantTap" => StaticMode::CantTap,
            "CantUntap" => StaticMode::CantUntap,
            "MustBeBlocked" => StaticMode::MustBeBlocked,
            "MustBeBlockedByAll" => StaticMode::MustBeBlockedByAll,
            "Goaded" => StaticMode::Goaded,
            "CombatAlone(Attack,NeedsCompanion)" => StaticMode::CombatAlone {
                action: CombatAloneAction::Attack,
                requirement: CombatAloneRequirement::NeedsCompanion,
            },
            "CombatAlone(Block,NeedsCompanion)" => StaticMode::CombatAlone {
                action: CombatAloneAction::Block,
                requirement: CombatAloneRequirement::NeedsCompanion,
            },
            "CombatAlone(Attack,MustBeSole)" => StaticMode::CombatAlone {
                action: CombatAloneAction::Attack,
                requirement: CombatAloneRequirement::MustBeSole,
            },
            "CantCrew" => StaticMode::CantCrew,
            "MayLookAtTopOfLibrary" => StaticMode::MayLookAtTopOfLibrary,
            "MayLookAtFaceDown" => StaticMode::MayLookAtFaceDown,
            "CantBeTurnedFaceUp" => StaticMode::CantBeTurnedFaceUp,
            // Tier 3
            "MayChooseNotToUntap" => StaticMode::MayChooseNotToUntap,
            // AdditionalLandDrop is parameterized — parsed in the `other` branch below
            "EmblemStatic" => StaticMode::EmblemStatic,
            "BlockRestriction" => StaticMode::BlockRestriction {
                filter: block_only_creatures_with_flying_filter(),
            },
            "NoMaximumHandSize" => StaticMode::NoMaximumHandSize,
            s if s.starts_with("MaximumHandSize(") => {
                // MaximumHandSize is data-carrying; FromStr round-trip not required.
                // Display output is for diagnostics only.
                StaticMode::Other(s.to_string())
            }
            "MayPlayAdditionalLand" => StaticMode::MayPlayAdditionalLand,
            "CantWinTheGame" => StaticMode::CantWinTheGame,
            "CantLoseTheGame" => StaticMode::CantLoseTheGame,
            "LegendRuleDoesntApply" => StaticMode::LegendRuleDoesntApply,
            "CanAttackWithDefender" => StaticMode::CanAttackWithDefender,
            // CR 509.1b + CR 609.4 + CR 702.14c: bare form = all-landwalk canceller.
            "IgnoreLandwalkForBlocking" => {
                StaticMode::IgnoreLandwalkForBlocking { qualifier: None }
            }
            "CanActivateAbilitiesAsThoughHaste" => StaticMode::CanActivateAbilitiesAsThoughHaste,
            "CanBlockShadow" => StaticMode::CanBlockShadow,
            s if s.starts_with("StepEndUnspentMana(") => StaticMode::Other(s.to_string()),
            "UntapsDuringEachOtherPlayersUntapStep" => {
                StaticMode::UntapsDuringEachOtherPlayersUntapStep
            }
            // CR 701.38d: "While voting, you may vote an additional time."
            "GrantsExtraVote" => StaticMode::GrantsExtraVote,
            // CR 701.55c: "If an opponent would face a villainous choice, they
            // face that choice an additional time." (The Valeyard)
            "GrantsExtraVillainousChoice" => StaticMode::GrantsExtraVillainousChoice,
            // Parameterized
            other => {
                if let Some(inner) = other
                    .strip_prefix("CantDraw(")
                    .and_then(|s| s.strip_suffix(')'))
                {
                    if let Ok(who) = ProhibitionScope::from_str(inner) {
                        return Ok(StaticMode::CantDraw { who });
                    }
                    return Ok(StaticMode::Other(other.to_string()));
                } else if let Some(inner) = other
                    .strip_prefix("CantBeCast(")
                    .and_then(|s| s.strip_suffix(')'))
                {
                    if let Ok(who) = ProhibitionScope::from_str(inner) {
                        return Ok(StaticMode::CantBeCast { who });
                    }
                    return Ok(StaticMode::Other(other.to_string()));
                } else if let Some(inner) = other
                    .strip_prefix("CantCastFrom(")
                    .and_then(|s| s.strip_suffix(')'))
                {
                    // CR 601.3 + CR 109.5: Round-trip of the scope identifier;
                    // the prohibited-zone list is data-carrying (`affected`).
                    if let Ok(who) = ProhibitionScope::from_str(inner) {
                        return Ok(StaticMode::CantCastFrom { who });
                    }
                    return Ok(StaticMode::Other(other.to_string()));
                } else if let Some(inner) = other
                    .strip_prefix("CantBeActivated(")
                    .and_then(|s| s.strip_suffix(')'))
                {
                    // CR 602.5: Round-trip of the parameterized form is diagnostic-only;
                    // `source_filter` is data-carrying and defaults to `SelfRef`.
                    if let Ok(who) = ProhibitionScope::from_str(inner) {
                        return Ok(StaticMode::CantBeActivated {
                            who,
                            source_filter: TargetFilter::SelfRef,
                            // CR 605.1a: Display round-trip is diagnostic-only; the
                            // exemption field is data-carrying and defaults to `None`.
                            exemption: ActivationExemption::None,
                        });
                    }
                    return Ok(StaticMode::Other(other.to_string()));
                } else if let Some(inner) = other
                    .strip_prefix("CantSearchLibrary(")
                    .and_then(|s| s.strip_suffix(')'))
                {
                    // CR 701.23: Round-trip of the scope identifier.
                    if let Ok(cause) = ProhibitionScope::from_str(inner) {
                        return Ok(StaticMode::CantSearchLibrary { cause });
                    }
                    return Ok(StaticMode::Other(other.to_string()));
                } else if let Some(inner) = other
                    .strip_prefix("CantCauseSacrificeOrExile(")
                    .and_then(|s| s.strip_suffix(')'))
                {
                    // CR 603.2 + CR 609.3: Round-trip of the scope identifier.
                    if let Ok(cause) = ProhibitionScope::from_str(inner) {
                        return Ok(StaticMode::CantCauseSacrificeOrExile { cause });
                    }
                    return Ok(StaticMode::Other(other.to_string()));
                } else if other.starts_with("SuppressTriggers(") {
                    // CR 603.2g: Data-carrying — round-trip preserves discriminant only.
                    // Callers that need the full filter/events read from the typed field.
                    return Ok(StaticMode::Other(other.to_string()));
                } else if other.starts_with("EntersWithAdditionalCounters(") {
                    // CR 614.1c: Data-carrying (CounterType + count). The Display
                    // form uses the Debug rendering of `CounterType`, which has no
                    // FromStr inverse; round-trip is diagnostic-only and callers
                    // read the typed field. Mirrors MaximumHandSize / SuppressTriggers.
                    return Ok(StaticMode::Other(other.to_string()));
                } else if let Some(inner) = other
                    .strip_prefix("CantCastDuring(")
                    .and_then(|s| s.strip_suffix(')'))
                {
                    if let Some((who_str, when_str)) = inner.split_once(',') {
                        if let (Ok(who), Ok(when)) = (
                            ProhibitionScope::from_str(who_str),
                            CastingProhibitionCondition::from_str(when_str),
                        ) {
                            return Ok(StaticMode::CantCastDuring { who, when });
                        }
                    }
                    return Ok(StaticMode::Other(other.to_string()));
                } else if let Some(inner) = other
                    .strip_prefix("CantActivateDuring(")
                    .and_then(|s| s.strip_suffix(')'))
                {
                    // CR 602.5 + CR 117.1b: Round-trip preserves the (who, when) axes;
                    // `exemption` is data-carrying and defaults to `None` (mirrors the
                    // `CantBeActivated` round-trip above — diagnostic-only).
                    if let Some((who_str, when_str)) = inner.split_once(',') {
                        if let (Ok(who), Ok(when)) = (
                            ProhibitionScope::from_str(who_str),
                            CastingProhibitionCondition::from_str(when_str),
                        ) {
                            return Ok(StaticMode::CantActivateDuring {
                                who,
                                when,
                                exemption: ActivationExemption::None,
                            });
                        }
                    }
                    return Ok(StaticMode::Other(other.to_string()));
                } else if let Some(inner) = other
                    .strip_prefix("PerTurnCastLimit(")
                    .and_then(|s| s.strip_suffix(')'))
                {
                    if let Some((who_str, max_str)) = inner.split_once(',') {
                        if let (Ok(who), Ok(max)) =
                            (ProhibitionScope::from_str(who_str), max_str.parse::<u32>())
                        {
                            return Ok(StaticMode::PerTurnCastLimit {
                                who,
                                max,
                                spell_filter: None,
                            });
                        }
                    }
                    return Ok(StaticMode::Other(other.to_string()));
                } else if let Some(inner) = other
                    .strip_prefix("PerTurnDrawLimit(")
                    .and_then(|s| s.strip_suffix(')'))
                {
                    if let Some((who_str, max_str)) = inner.split_once(',') {
                        if let (Ok(who), Ok(max)) =
                            (ProhibitionScope::from_str(who_str), max_str.parse::<u32>())
                        {
                            return Ok(StaticMode::PerTurnDrawLimit { who, max });
                        }
                    }
                    return Ok(StaticMode::Other(other.to_string()));
                } else if let Some(rest) = other.strip_prefix("CantBeBlockedExceptBy:Min:") {
                    match rest.parse::<u32>() {
                        Ok(min) => StaticMode::CantBeBlockedExceptBy {
                            kind: BlockExceptionKind::MinBlockers { min },
                        },
                        Err(_) => StaticMode::Other(other.to_string()),
                    }
                } else if let Some(rest) = other.strip_prefix("ExtraBlockers(") {
                    let rest = rest.strip_suffix(')').unwrap_or(rest);
                    if rest == "any" {
                        StaticMode::ExtraBlockers { count: None }
                    } else {
                        StaticMode::ExtraBlockers {
                            count: rest.parse().ok(),
                        }
                    }
                } else if let Some(rest) = other.strip_prefix("RevealTopOfLibrary(") {
                    let rest = rest.strip_suffix(')').unwrap_or(rest);
                    StaticMode::RevealTopOfLibrary {
                        all_players: rest == "all",
                    }
                } else if let Some(inner) = other
                    .strip_prefix("RevealHand(")
                    .and_then(|s| s.strip_suffix(')'))
                {
                    if let Ok(who) = ProhibitionScope::from_str(inner) {
                        return Ok(StaticMode::RevealHand { who });
                    }
                    return Ok(StaticMode::Other(other.to_string()));
                } else if let Some(rest) = other.strip_prefix("AdditionalLandDrop(") {
                    let rest = rest.strip_suffix(')').unwrap_or(rest);
                    StaticMode::AdditionalLandDrop {
                        count: rest.parse().unwrap_or(1),
                    }
                } else if let Some(inner) = other
                    .strip_prefix("CastWithKeyword(")
                    .and_then(|s| s.strip_suffix(')'))
                {
                    let keyword = Keyword::from_str(inner).unwrap();
                    StaticMode::CastWithKeyword { keyword }
                } else if let Some(inner) = other
                    .strip_prefix("IgnoreLandwalkForBlocking(")
                    .and_then(|s| s.strip_suffix(')'))
                {
                    // CR 509.1b + CR 609.4 + CR 702.14c: parameterized form carries
                    // the canonical capitalized land subtype (e.g. "Swamp").
                    StaticMode::IgnoreLandwalkForBlocking {
                        qualifier: Some(inner.to_string()),
                    }
                } else if let Some(inner) = other
                    .strip_prefix("SkipStep(")
                    .and_then(|s| s.strip_suffix(')'))
                {
                    let step = match inner {
                        "Draw" => Phase::Draw,
                        "Untap" => Phase::Untap,
                        "Upkeep" => Phase::Upkeep,
                        _ => return Ok(StaticMode::Other(other.to_string())),
                    };
                    StaticMode::SkipStep { step }
                } else {
                    StaticMode::Other(other.to_string())
                }
            }
        };
        Ok(mode)
    }
}

fn parse_static_mode_u32_arg(s: &str, prefix: &str) -> Option<u32> {
    s.strip_prefix(prefix)?
        .strip_prefix('(')?
        .strip_suffix(')')?
        .parse()
        .ok()
}

/// Round-trip the `MaxAttackersEachCombat(max[,Controller])` Display form back
/// to its `(max, defender)` arguments. Mirrors the two `fmt::Display` branches.
fn parse_max_attackers_each_combat_args(s: &str) -> Option<(u32, Option<AttackDefenderScope>)> {
    let args = s
        .strip_prefix("MaxAttackersEachCombat")?
        .strip_prefix('(')?
        .strip_suffix(')')?;
    match args.split_once(',') {
        None => Some((args.parse().ok()?, None)),
        Some((max, "Controller")) => {
            Some((max.parse().ok()?, Some(AttackDefenderScope::Controller)))
        }
        Some(_) => None,
    }
}

/// CR 509.1b: Canonical attacker filter for "can block only creatures with flying."
pub fn block_only_creatures_with_flying_filter() -> TargetFilter {
    use super::ability::{FilterProp, TypedFilter};
    TargetFilter::Typed(
        TypedFilter::creature().properties(vec![FilterProp::WithKeyword {
            value: Keyword::Flying,
        }]),
    )
}

/// Forward-compatible deserializer for `StaticMode` fields in persisted JSON
/// (card-data.json). Handles the common case where a new unit-variant is added
/// to the engine but an older WASM binary tries to load card data that contains
/// that variant: instead of a hard error, the variant is silently mapped to
/// `StaticMode::Other(name)` and the card continues to load.
///
/// Usage: `#[serde(deserialize_with = "crate::types::statics::deserialize_static_mode_fwd")]`
///
/// # How it avoids infinite recursion
/// For both string and object values, the function delegates to
/// `serde_json::from_value::<StaticMode>`, which invokes the **derived**
/// `StaticMode::Deserialize` impl — not this field-level helper. For unknown
/// unit variants (string values that the derived impl rejects), the fallback
/// wraps the raw string in `Other(s)`. No cycle is possible.
///
/// # Why not `FromStr`?
/// `FromStr` for `StaticMode` does not enumerate every unit variant by its
/// exact Rust identifier (it's a separate parser for human-facing strings).
/// Using `FromStr` would map known variants like `"Flying"` to
/// `Other("Flying")` whenever they aren't explicitly listed,
/// breaking coverage and registry lookups for those cards.
pub fn deserialize_static_mode_fwd<'de, D>(d: D) -> Result<StaticMode, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: serde_json::Value = serde_json::Value::deserialize(d)?;
    match raw {
        serde_json::Value::String(ref s) => {
            // Unit variant path. Handle legacy cost-modify unit variants first.
            if let Some(mode) = deserialize_legacy_cost_modify_string(s) {
                return Ok(mode);
            }
            if s == "BlockRestriction" {
                return Ok(StaticMode::BlockRestriction {
                    filter: block_only_creatures_with_flying_filter(),
                });
            }
            // Try the derived deserializer so all known unit variants
            // (e.g. "CantTap", "Flying", …) round-trip correctly. Struct/
            // parameterized variants (e.g. the now-struct `SpendManaAsAnyColor`)
            // are serialized as objects; a bare string for them legitimately
            // falls through to `Other(s)` below.
            // If the derived impl rejects the string (unknown variant from a newer
            // engine build), fall back to Other(s) so the card still loads.
            match serde_json::from_value::<StaticMode>(serde_json::Value::String(s.clone())) {
                Ok(mode) => Ok(mode),
                Err(_) => Ok(StaticMode::Other(s.clone())),
            }
        }
        other => {
            if let Some(mode) = deserialize_legacy_modify_cost_object(&other) {
                return mode.map_err(serde::de::Error::custom);
            }
            // Data-carrying variant path. Delegate to the derived Deserialize
            // which handles all struct/newtype variants correctly.
            serde_json::from_value::<StaticMode>(other).map_err(serde::de::Error::custom)
        }
    }
}

fn deserialize_legacy_cost_modify_string(s: &str) -> Option<StaticMode> {
    let mode = match s {
        "ReduceCost" => CostModifyMode::Reduce,
        "RaiseCost" => CostModifyMode::Raise,
        "MinimumCost" => CostModifyMode::Minimum,
        _ => return None,
    };
    Some(StaticMode::ModifyCost {
        mode,
        amount: ManaCost::zero(),
        spell_filter: None,
        dynamic_count: None,
    })
}

#[derive(Deserialize)]
struct LegacyModifyCostPayload {
    #[serde(default)]
    amount: ManaCost,
    #[serde(default)]
    spell_filter: Option<TargetFilter>,
    #[serde(default)]
    dynamic_count: Option<QuantityRef>,
}

fn deserialize_legacy_modify_cost_object(
    raw: &serde_json::Value,
) -> Option<serde_json::Result<StaticMode>> {
    let map = raw.as_object()?;
    if map.len() != 1 {
        return None;
    }

    let (name, payload) = map.iter().next()?;
    let mode = match name.as_str() {
        "ReduceCost" => CostModifyMode::Reduce,
        "RaiseCost" => CostModifyMode::Raise,
        "MinimumCost" => CostModifyMode::Minimum,
        _ => return None,
    };

    Some(
        serde_json::from_value::<LegacyModifyCostPayload>(payload.clone()).map(|payload| {
            StaticMode::ModifyCost {
                mode,
                amount: payload.amount,
                spell_filter: payload.spell_filter,
                dynamic_count: payload.dynamic_count,
            }
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_block_restriction_string_deserializes_with_flying_filter() {
        use super::super::ability::StaticDefinition;

        let def: StaticDefinition = serde_json::from_str(
            r#"{"mode":"BlockRestriction","modifications":[],"affected":{"type":"SelfRef"}}"#,
        )
        .expect("legacy card-data unit variant");
        assert_eq!(
            def.mode,
            StaticMode::BlockRestriction {
                filter: block_only_creatures_with_flying_filter(),
            }
        );
    }

    #[test]
    fn parse_known_static_modes() {
        assert_eq!(
            StaticMode::from_str("Continuous").unwrap(),
            StaticMode::Continuous
        );
        assert_eq!(
            StaticMode::from_str("CantAttack").unwrap(),
            StaticMode::CantAttack
        );
        // CR 603.2d: Legacy "Panharmonicon" string rehydrates to the canonical
        // typed form with the Panharmonicon cause predicate.
        use super::super::card_type::CoreType;
        assert_eq!(
            StaticMode::from_str("Panharmonicon").unwrap(),
            StaticMode::DoubleTriggers {
                cause: TriggerCause::EntersBattlefield {
                    core_types: vec![CoreType::Artifact, CoreType::Creature],
                },
            }
        );
        assert_eq!(
            StaticMode::from_str("IgnoreHexproof").unwrap(),
            StaticMode::IgnoreHexproof
        );
        // CR 701.55c: GrantsExtraVillainousChoice (The Valeyard) must round-trip
        // through Display/FromStr so card data persists the variant across the
        // WASM serde boundary, mirroring its CR 701.38d twin GrantsExtraVote.
        assert_eq!(
            StaticMode::from_str("GrantsExtraVillainousChoice").unwrap(),
            StaticMode::GrantsExtraVillainousChoice
        );
        assert_eq!(
            StaticMode::GrantsExtraVillainousChoice.to_string(),
            "GrantsExtraVillainousChoice"
        );
    }

    #[test]
    fn parse_promoted_static_modes() {
        assert_eq!(
            StaticMode::from_str("CantBeBlocked").unwrap(),
            StaticMode::CantBeBlocked
        );
        assert_eq!(StaticMode::from_str("Flying").unwrap(), StaticMode::Flying);
        assert_eq!(
            StaticMode::from_str("MustBeBlocked").unwrap(),
            StaticMode::MustBeBlocked
        );
        assert_eq!(
            StaticMode::from_str("NoMaximumHandSize").unwrap(),
            StaticMode::NoMaximumHandSize
        );
    }

    #[test]
    fn static_mode_as_keyword_maps_bare_keyword_modes() {
        let cases = [
            (StaticMode::Indestructible, Keyword::Indestructible),
            (StaticMode::Shroud, Keyword::Shroud),
            (StaticMode::Hexproof, Keyword::Hexproof),
            (StaticMode::Flying, Keyword::Flying),
            (StaticMode::Vigilance, Keyword::Vigilance),
            (StaticMode::Menace, Keyword::Menace),
            (StaticMode::Reach, Keyword::Reach),
            (StaticMode::Trample, Keyword::Trample),
            (StaticMode::Deathtouch, Keyword::Deathtouch),
            (StaticMode::Lifelink, Keyword::Lifelink),
        ];
        for (mode, keyword) in cases {
            assert_eq!(mode.as_keyword(), Some(keyword));
        }
        assert_eq!(StaticMode::Protection.as_keyword(), None);
        assert_eq!(StaticMode::CantBeBlocked.as_keyword(), None);
    }

    #[test]
    fn parse_unknown_static_mode() {
        assert_eq!(
            StaticMode::from_str("FakeMode").unwrap(),
            StaticMode::Other("FakeMode".to_string())
        );
    }

    #[test]
    fn display_roundtrips() {
        let modes = vec![
            // Pre-existing variants
            StaticMode::Continuous,
            StaticMode::CantAttack,
            StaticMode::ExtraBlockers { count: None },
            StaticMode::ExtraBlockers { count: Some(1) },
            StaticMode::MaxAttackersEachCombat {
                max: 2,
                defender: None,
            },
            StaticMode::MaxAttackersEachCombat {
                max: 1,
                defender: Some(AttackDefenderScope::Controller),
            },
            StaticMode::MaxBlockersEachCombat { max: 3 },
            StaticMode::CantBeBlockedByMoreThan { max: 2 },
            StaticMode::RevealTopOfLibrary { all_players: false },
            StaticMode::RevealTopOfLibrary { all_players: true },
            StaticMode::RevealHand {
                who: ProhibitionScope::Opponents,
            },
            StaticMode::RevealHand {
                who: ProhibitionScope::AllPlayers,
            },
            // Tier 1: keyword/evasion statics
            StaticMode::CantBeBlocked,
            StaticMode::CantBeBlockedExceptBy {
                kind: BlockExceptionKind::MinBlockers { min: 3 },
            },
            StaticMode::Protection,
            StaticMode::Indestructible,
            StaticMode::CantBeDestroyed,
            StaticMode::FlashBack,
            StaticMode::Shroud,
            StaticMode::Hexproof,
            StaticMode::Vigilance,
            StaticMode::Menace,
            StaticMode::Reach,
            StaticMode::Flying,
            StaticMode::Trample,
            StaticMode::Deathtouch,
            StaticMode::Lifelink,
            // Tier 2: rule-mod statics
            StaticMode::CantTap,
            StaticMode::CantUntap,
            StaticMode::MustBeBlocked,
            StaticMode::CombatAlone {
                action: CombatAloneAction::Attack,
                requirement: CombatAloneRequirement::NeedsCompanion,
            },
            StaticMode::CombatAlone {
                action: CombatAloneAction::Block,
                requirement: CombatAloneRequirement::NeedsCompanion,
            },
            StaticMode::CombatAlone {
                action: CombatAloneAction::Attack,
                requirement: CombatAloneRequirement::MustBeSole,
            },
            StaticMode::CantCrew,
            StaticMode::MayLookAtTopOfLibrary,
            StaticMode::MayLookAtFaceDown,
            StaticMode::CantBeTurnedFaceUp,
            // Tier 3: parser-produced statics
            StaticMode::MayChooseNotToUntap,
            StaticMode::AdditionalLandDrop { count: 1 },
            StaticMode::AdditionalLandDrop { count: 2 },
            StaticMode::EmblemStatic,
            StaticMode::BlockRestriction {
                filter: block_only_creatures_with_flying_filter(),
            },
            StaticMode::NoMaximumHandSize,
            StaticMode::MayPlayAdditionalLand,
            // Graveyard cast/play permissions
            StaticMode::GraveyardCastPermission {
                frequency: CastFrequency::OncePerTurn,
                play_mode: CardPlayMode::Cast,
                graveyard_destination_replacement: None,
                extra_cost: None,
            },
            StaticMode::GraveyardCastPermission {
                frequency: CastFrequency::Unlimited,
                play_mode: CardPlayMode::Play,
                graveyard_destination_replacement: None,
                extra_cost: None,
            },
            // CR 601.2f: Festival of Embers — graveyard cast with an additional
            // pay-life cost. NOTE: `extra_cost`-bearing variants are NOT in this
            // Display round-trip list — the `AbilityCost` payload rides on serde,
            // not Display (FromStr defaults `extra_cost` to None, mirroring
            // `TopOfLibraryCastPermission.alt_cost`). The payload round-trip is
            // covered by `serde_roundtrip` instead.
            // Cast-from-hand-free permissions (Omniscience; Zaffai).
            StaticMode::CastFromHandFree {
                frequency: CastFrequency::Unlimited,
                origin: CastFreeOrigin::Hand,
            },
            StaticMode::CastFromHandFree {
                frequency: CastFrequency::OncePerTurn,
                origin: CastFreeOrigin::Hand,
            },
            StaticMode::CastFromHandFree {
                frequency: CastFrequency::Unlimited,
                origin: CastFreeOrigin::DefaultCastPermission,
            },
            // Exile-cast permission (Maralen, Fae Ascendant).
            StaticMode::ExileCastPermission {
                frequency: CastFrequency::OncePerTurn,
                play_mode: CardPlayMode::Cast,
                cost: ExileCastCost::WithoutPayingManaCost,
                pool: ExileCardPool::ThisTurn,
                timing: ExileCastTiming::AnyTime,
                mana_spend_permission: None,
                grants_flash: false,
                extra_cost: None,
            },
            StaticMode::ExileCastPermission {
                frequency: CastFrequency::Unlimited,
                play_mode: CardPlayMode::Cast,
                cost: ExileCastCost::PayNormalCost,
                pool: ExileCardPool::ThisTurn,
                timing: ExileCastTiming::AnyTime,
                mana_spend_permission: None,
                grants_flash: false,
                extra_cost: None,
            },
            // Persistent, your-turn-only exile-play permission
            // (The Matrix of Time; Prosper/Tibalt impulse-commander class).
            StaticMode::ExileCastPermission {
                frequency: CastFrequency::Unlimited,
                play_mode: CardPlayMode::Play,
                cost: ExileCastCost::PayNormalCost,
                pool: ExileCardPool::Persistent,
                timing: ExileCastTiming::YourTurnOnly,
                mana_spend_permission: None,
                grants_flash: false,
                extra_cost: None,
            },
            // CR 609.4b + CR 702.8a: Azula, Cunning Usurper — Cast mode from a
            // persistent pool, your-turn-only, granting any-type mana and flash.
            StaticMode::ExileCastPermission {
                frequency: CastFrequency::Unlimited,
                play_mode: CardPlayMode::Cast,
                cost: ExileCastCost::PayNormalCost,
                pool: ExileCardPool::Persistent,
                timing: ExileCastTiming::YourTurnOnly,
                mana_spend_permission: Some(
                    crate::types::ability::ManaSpendPermission::AnyTypeOrColor,
                ),
                grants_flash: true,
                extra_cost: None,
            },
            // NOTE: Valgavoth (alternative pay-life) and Dawnhand (additional
            // remove-counters) `extra_cost`-bearing exile permissions are
            // covered by `serde_roundtrip`, not this Display list — see the
            // note on `GraveyardCastPermission` above.
            // Casting prohibitions
            StaticMode::CantBeCast {
                who: ProhibitionScope::Controller,
            },
            StaticMode::CantBeCast {
                who: ProhibitionScope::Opponents,
            },
            StaticMode::CantCastDuring {
                who: ProhibitionScope::Opponents,
                when: CastingProhibitionCondition::DuringYourTurn,
            },
            StaticMode::CantCastDuring {
                who: ProhibitionScope::AllPlayers,
                when: CastingProhibitionCondition::DuringCombat,
            },
            StaticMode::CantCastDuring {
                who: ProhibitionScope::Controller,
                when: CastingProhibitionCondition::NotDuringYourTurn,
            },
            StaticMode::CantDraw {
                who: ProhibitionScope::AllPlayers,
            },
            StaticMode::CantDraw {
                who: ProhibitionScope::Opponents,
            },
            // Per-turn casting limits
            StaticMode::PerTurnCastLimit {
                who: ProhibitionScope::AllPlayers,
                max: 1,
                spell_filter: None,
            },
            StaticMode::PerTurnCastLimit {
                who: ProhibitionScope::Controller,
                max: 2,
                spell_filter: None,
            },
            // Fallback
            StaticMode::Other("Custom".to_string()),
        ];
        for mode in modes {
            let s = mode.to_string();
            assert_eq!(StaticMode::from_str(&s).unwrap(), mode);
        }
    }

    #[test]
    fn serde_roundtrip() {
        let modes = vec![
            StaticMode::Continuous,
            StaticMode::CantBeTargeted,
            StaticMode::CantBeBlocked,
            StaticMode::Flying,
            StaticMode::MustBeBlocked,
            StaticMode::GrantsExtraVote,
            // CR 118.9: data-carrying ManaCost — serde must preserve {0} and {WUBRG}.
            StaticMode::CastWithAlternativeCost {
                cost: AbilityCost::Mana {
                    cost: ManaCost::zero(),
                },
                timing_permission: None,
            },
            StaticMode::CastWithAlternativeCost {
                cost: AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        shards: vec![
                            super::super::mana::ManaCostShard::White,
                            super::super::mana::ManaCostShard::Blue,
                            super::super::mana::ManaCostShard::Black,
                            super::super::mana::ManaCostShard::Red,
                            super::super::mana::ManaCostShard::Green,
                        ],
                        generic: 0,
                    },
                },
                timing_permission: None,
            },
            // CR 118.9 + CR 601.2f: `CastExtraCost` riders ride on serde (not the
            // Display round-trip). Cover all three shapes of the building block:
            // Valgavoth alternative pay-life, Festival additional pay-life,
            // Dawnhand additional remove-counters.
            StaticMode::ExileCastPermission {
                frequency: CastFrequency::Unlimited,
                play_mode: CardPlayMode::Play,
                cost: ExileCastCost::PayNormalCost,
                pool: ExileCardPool::Persistent,
                timing: ExileCastTiming::YourTurnOnly,
                mana_spend_permission: None,
                grants_flash: false,
                extra_cost: Some(CastExtraCost {
                    cost: AbilityCost::PayLife {
                        amount: QuantityExpr::Ref {
                            qty: QuantityRef::SelfManaValue,
                        },
                    },
                    mode: CastCostMode::Alternative,
                }),
            },
            StaticMode::GraveyardCastPermission {
                frequency: CastFrequency::Unlimited,
                play_mode: CardPlayMode::Cast,
                graveyard_destination_replacement: None,
                extra_cost: Some(CastExtraCost {
                    cost: AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                    },
                    mode: CastCostMode::Additional,
                }),
            },
            StaticMode::ExileCastPermission {
                frequency: CastFrequency::Unlimited,
                play_mode: CardPlayMode::Cast,
                cost: ExileCastCost::PayNormalCost,
                pool: ExileCardPool::Persistent,
                timing: ExileCastTiming::YourTurnOnly,
                mana_spend_permission: None,
                grants_flash: false,
                extra_cost: Some(CastExtraCost {
                    cost: AbilityCost::RemoveCounter {
                        count: 3,
                        counter_type: crate::types::counter::CounterMatch::Any,
                        target: Some(TargetFilter::Any),
                        selection: crate::types::ability::CounterCostSelection::AmongObjects,
                    },
                    mode: CastCostMode::Additional,
                }),
            },
            StaticMode::Other("Custom".to_string()),
        ];
        let json = serde_json::to_string(&modes).unwrap();
        let deserialized: Vec<StaticMode> = serde_json::from_str(&json).unwrap();
        assert_eq!(modes, deserialized);
    }

    /// Regression test for forward-compat: card-data.json produced by a newer
    /// engine (containing a unit variant the current binary doesn't know) must
    /// deserialize as `Other(name)` rather than failing hard.
    ///
    /// Simulates an old WASM reading card data that has `"GrantsExtraVote"` (or
    /// any future unit variant not yet in the enum). `deserialize_static_mode_fwd`
    /// routes the string through `FromStr`, which maps unknown names to `Other`.
    #[test]
    fn fwd_compat_unknown_unit_variant_maps_to_other() {
        #[derive(serde::Deserialize, PartialEq, Debug)]
        struct Wrapper {
            #[serde(deserialize_with = "deserialize_static_mode_fwd")]
            mode: StaticMode,
        }
        // A variant name the binary wouldn't know in the pre-GrantsExtraVote world.
        let json = r#"{"mode":"FutureUnknownVariant"}"#;
        let w: Wrapper = serde_json::from_str(json).unwrap();
        assert_eq!(
            w.mode,
            StaticMode::Other("FutureUnknownVariant".to_string())
        );
        // Known variant still deserializes correctly.
        let json2 = r#"{"mode":"GrantsExtraVote"}"#;
        let w2: Wrapper = serde_json::from_str(json2).unwrap();
        assert_eq!(w2.mode, StaticMode::GrantsExtraVote);
    }

    /// CR 609.4b: `SpendManaAsAnyColor` widened from a unit variant to a struct
    /// variant carrying `spell_filter: Option<TargetFilter>` (Vizier of the
    /// Menagerie spell-class scoping). This pins three serde behaviors:
    ///
    /// (a) the board-wide `None` shape serializes to the externally-tagged
    ///     struct form `{"SpendManaAsAnyColor":{}}` (the `spell_filter` field is
    ///     `skip_serializing_if = "Option::is_none"`) and round-trips;
    /// (b) the spell-filtered `Some(Typed(creature))` shape round-trips;
    /// (c) a LEGACY bare string `"SpendManaAsAnyColor"` (serialized by a
    ///     pre-widening binary that wrote a unit variant) can no longer be read
    ///     as a struct variant — the derived deserializer rejects the string and
    ///     `deserialize_static_mode_fwd` DOWNGRADES it to
    ///     `Other("SpendManaAsAnyColor")`. This is asserted explicitly so the
    ///     downgrade is documented and intentional, not a silent surprise.
    #[test]
    fn spend_mana_as_any_color_struct_serde_and_legacy_downgrade() {
        use super::super::ability::{TargetFilter, TypedFilter};

        // (a) board-wide None: exact serialized form + round-trip.
        let board_wide = StaticMode::SpendManaAsAnyColor { spell_filter: None };
        let json = serde_json::to_string(&board_wide).unwrap();
        assert_eq!(
            json, r#"{"SpendManaAsAnyColor":{}}"#,
            "the None shape must serialize as the externally-tagged struct form with the \
             skipped-if-None spell_filter field omitted"
        );
        let back: StaticMode = serde_json::from_str(&json).unwrap();
        assert_eq!(back, board_wide);

        // (b) spell-filtered Some(Typed(creature)): round-trip preserves the filter.
        let filtered = StaticMode::SpendManaAsAnyColor {
            spell_filter: Some(TargetFilter::Typed(TypedFilter::creature())),
        };
        let json = serde_json::to_string(&filtered).unwrap();
        let back: StaticMode = serde_json::from_str(&json).unwrap();
        assert_eq!(back, filtered, "the spell-filtered shape must round-trip");

        // (c) legacy bare string downgrades to Other through the fwd-compat path.
        #[derive(serde::Deserialize, PartialEq, Debug)]
        struct Wrapper {
            #[serde(deserialize_with = "deserialize_static_mode_fwd")]
            mode: StaticMode,
        }
        let legacy = r#"{"mode":"SpendManaAsAnyColor"}"#;
        let w: Wrapper = serde_json::from_str(legacy).unwrap();
        assert_eq!(
            w.mode,
            StaticMode::Other("SpendManaAsAnyColor".to_string()),
            "a legacy bare-string SpendManaAsAnyColor must DOWNGRADE to Other now that the \
             variant is a struct — the derived deserializer cannot read a struct variant from a \
             plain string, so deserialize_static_mode_fwd falls back to Other"
        );
    }

    #[test]
    fn fwd_compat_legacy_cost_modify_objects_map_to_modify_cost() {
        #[derive(serde::Deserialize, PartialEq, Debug)]
        struct Wrapper {
            #[serde(deserialize_with = "deserialize_static_mode_fwd")]
            mode: StaticMode,
        }

        let cases = [
            ("ReduceCost", CostModifyMode::Reduce),
            ("RaiseCost", CostModifyMode::Raise),
            ("MinimumCost", CostModifyMode::Minimum),
        ];

        for (legacy_name, expected_mode) in cases {
            let json = format!(
                r#"{{"mode":{{"{legacy_name}":{{"amount":{{"type":"Cost","shards":[],"generic":2}},"spell_filter":null,"dynamic_count":null}}}}}}"#
            );
            let wrapper: Wrapper = serde_json::from_str(&json).unwrap();
            assert_eq!(
                wrapper.mode,
                StaticMode::ModifyCost {
                    mode: expected_mode,
                    amount: ManaCost::generic(2),
                    spell_filter: None,
                    dynamic_count: None,
                }
            );
        }
    }

    #[test]
    fn fwd_compat_legacy_cost_modify_strings_map_to_modify_cost() {
        #[derive(serde::Deserialize, PartialEq, Debug)]
        struct Wrapper {
            #[serde(deserialize_with = "deserialize_static_mode_fwd")]
            mode: StaticMode,
        }

        let cases = [
            ("ReduceCost", CostModifyMode::Reduce),
            ("RaiseCost", CostModifyMode::Raise),
            ("MinimumCost", CostModifyMode::Minimum),
        ];

        for (legacy_name, expected_mode) in cases {
            let json = format!(r#"{{"mode":"{legacy_name}"}}"#);
            let wrapper: Wrapper = serde_json::from_str(&json).unwrap();
            assert_eq!(
                wrapper.mode,
                StaticMode::ModifyCost {
                    mode: expected_mode,
                    amount: ManaCost::zero(),
                    spell_filter: None,
                    dynamic_count: None,
                }
            );
        }
    }

    #[test]
    fn prohibition_family_display_includes_scope() {
        // CR 602.5: CantBeActivated display carries the scope identifier.
        let mode = StaticMode::CantBeActivated {
            who: ProhibitionScope::AllPlayers,
            source_filter: TargetFilter::SelfRef,
            exemption: ActivationExemption::None,
        };
        assert_eq!(mode.to_string(), "CantBeActivated(all_players)");

        // CR 701.23: CantSearchLibrary display carries the cause scope.
        let mode = StaticMode::CantSearchLibrary {
            cause: ProhibitionScope::Opponents,
        };
        assert_eq!(mode.to_string(), "CantSearchLibrary(opponents)");

        // CR 603.2g: SuppressTriggers display enumerates the event set.
        let mode = StaticMode::SuppressTriggers {
            source_filter: TargetFilter::SelfRef,
            events: vec![SuppressedTriggerEvent::EntersBattlefield],
        };
        assert_eq!(mode.to_string(), "SuppressTriggers(EntersBattlefield)");

        let mode = StaticMode::SuppressTriggers {
            source_filter: TargetFilter::SelfRef,
            events: vec![
                SuppressedTriggerEvent::EntersBattlefield,
                SuppressedTriggerEvent::Dies,
            ],
        };
        assert_eq!(mode.to_string(), "SuppressTriggers(EntersBattlefield+Dies)");
    }

    #[test]
    fn cant_be_activated_legacy_string_deserializes_to_self_ref() {
        // CR 602.5: The legacy unit-string `"CantBeActivated"` (from pre-widening
        // serialized data) must still parse, yielding the self-reference default.
        let parsed = StaticMode::from_str("CantBeActivated").unwrap();
        assert_eq!(
            parsed,
            StaticMode::CantBeActivated {
                who: ProhibitionScope::AllPlayers,
                source_filter: TargetFilter::SelfRef,
                exemption: ActivationExemption::None,
            }
        );
    }

    #[test]
    fn ignore_landwalk_for_blocking_display_fromstr_roundtrip() {
        // CR 509.1b + CR 609.4 + CR 702.14c: round-trip the `None` (all-landwalk)
        // form and each of the five basic-land qualifier forms.
        let cases = [
            StaticMode::IgnoreLandwalkForBlocking { qualifier: None },
            StaticMode::IgnoreLandwalkForBlocking {
                qualifier: Some("Plains".to_string()),
            },
            StaticMode::IgnoreLandwalkForBlocking {
                qualifier: Some("Island".to_string()),
            },
            StaticMode::IgnoreLandwalkForBlocking {
                qualifier: Some("Swamp".to_string()),
            },
            StaticMode::IgnoreLandwalkForBlocking {
                qualifier: Some("Mountain".to_string()),
            },
            StaticMode::IgnoreLandwalkForBlocking {
                qualifier: Some("Forest".to_string()),
            },
        ];
        for case in cases {
            let s = case.to_string();
            let parsed = StaticMode::from_str(&s).unwrap();
            assert_eq!(parsed, case, "round-trip failed for {s}");
        }
    }

    #[test]
    fn static_mode_equality_with_string_comparison() {
        // Verify Display output matches the expected Forge string
        assert_eq!(StaticMode::Continuous.to_string(), "Continuous");
        assert_eq!(StaticMode::CantBlock.to_string(), "CantBlock");
        assert_eq!(StaticMode::CantBeBlocked.to_string(), "CantBeBlocked");
        assert_eq!(StaticMode::Flying.to_string(), "Flying");
        assert_eq!(
            StaticMode::Other("NewMode".to_string()).to_string(),
            "NewMode"
        );
    }
}
