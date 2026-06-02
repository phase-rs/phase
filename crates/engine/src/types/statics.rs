use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::ability::{
    AbilityCost, CardPlayMode, CostCategory, QuantityExpr, QuantityRef, TargetFilter,
};
use super::keywords::Keyword;
use super::mana::{ManaColor, ManaCost, StepEndManaAction};
use super::phase::Phase;
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
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CostModifyMode {
    /// Subtractive — reduce generic mana (floor: 0).
    Reduce,
    /// Additive — increase generic mana. Thalia, Guardian of Thraben class.
    Raise,
    /// Floor — cost cannot fall below `amount` after all Reduce/Raise settle.
    /// CR 601.2f last-step floor. Trinisphere class.
    Minimum,
}

/// All static ability modes from Forge's static ability registry.
/// Matched case-sensitively against Forge mode strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StaticMode {
    Continuous,
    CantAttack,
    CantBlock,
    CantAttackOrBlock,
    /// CR 508.1d: No more than `max` creatures can be declared as attackers
    /// each combat.
    MaxAttackersEachCombat {
        max: u32,
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
    /// CR 702.51a: Grants a keyword to spells during casting.
    /// Generalized version of CastWithFlash — the `spell_filter` on the StaticDefinition
    /// determines which spells are affected (e.g., "Creature spells you cast have convoke").
    CastWithKeyword {
        keyword: Keyword,
    },
    /// CR 118.9 + CR 601.2f: A permanent grants its controller a wholesale
    /// alternative MANA cost for spells matching `StaticDefinition::affected`
    /// that the controller casts — they may pay `cost` rather than the spell's
    /// mana cost. Parallel to `CastWithKeyword`. Distinct from
    /// `ModifyCost { mode: Reduce, .. }` (subtractive, CR 601.2f) — this REPLACES
    /// the mana cost wholesale
    /// (CR 118.9) and is mutually exclusive with other alternative costs
    /// (CR 118.9a). Rooftop Storm ({0}, Zombie creature spells), Fist of Suns
    /// ({WUBRG}, any spell), Jodah (MV 5+).
    CastWithAlternativeCost {
        cost: ManaCost,
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
    /// CR 601.2f: Reduces the generic mana cost of activated abilities matching a keyword type.
    /// E.g., "Ninjutsu abilities you activate cost {1} less to activate."
    /// `keyword` identifies which ability type is reduced (e.g., "ninjutsu", "equip", "cycling").
    /// `amount` is the fixed generic mana reduction per activation.
    ReduceAbilityCost {
        keyword: String,
        amount: u32,
        /// "This effect can't reduce the mana in that cost to less than one mana."
        #[serde(default, skip_serializing_if = "Option::is_none")]
        minimum_mana: Option<u32>,
        /// CR 601.2f: Dynamic multiplier for cost reduction (e.g., "for each Dragon you control").
        /// When present, the total reduction is `amount * resolve_quantity(dynamic_count)`.
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
    MustBlock,
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
    /// spells from hand without paying their mana costs. `Unlimited` = Omniscience,
    /// Tamiyo emblem. `OncePerTurn` = Zaffai and the Tempests.
    CastFromHandFree {
        /// CR 601.2b: Per-turn cast frequency.
        frequency: CastFrequency,
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
    },
    /// CR 101.2: This spell/permanent can't be countered.
    CantBeCountered,
    /// CR 101.2 + CR 707.10: This spell can't be copied by spells or abilities.
    /// Enforced in `copy_spell::resolve` when selecting the spell to copy.
    CantBeCopied,
    /// CR 604.3: Cards in specified zones can't enter the battlefield.
    CantEnterBattlefieldFrom,
    /// CR 604.3: Players can't cast spells from specified zones.
    CantCastFrom,
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
    /// CR 702.16: Protection prevents targeting, blocking, damage, and attachment.
    Protection,
    /// CR 702.12: Indestructible — prevents destruction by lethal damage and destroy effects.
    Indestructible,
    /// Permanent cannot be destroyed (distinct from Indestructible).
    CantBeDestroyed,
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
    CantAttackAlone,
    CantBlockAlone,
    MayLookAtTopOfLibrary,

    // -- Tier 3: Parser-produced statics --
    /// CR 502.3: You may choose not to untap this permanent during your untap step.
    MayChooseNotToUntap,
    /// CR 305.2: Player may play additional lands on each of their turns.
    /// `count` is the number of extra land drops granted (e.g., 1 for Exploration, 2 for Azusa).
    AdditionalLandDrop {
        count: u8,
    },
    EmblemStatic,
    BlockRestriction,
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
    /// CR 609.4b: "You may spend mana as though it were mana of any color."
    /// Allows the controller to pay colored mana costs with mana of any color.
    SpendManaAsAnyColor,
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
                keyword,
                amount,
                minimum_mana,
                ..
            } => {
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
            StaticMode::ExtraBlockers { count } => count.hash(state),
            StaticMode::MaxAttackersEachCombat { max }
            | StaticMode::MaxBlockersEachCombat { max } => max.hash(state),
            StaticMode::RevealTopOfLibrary { all_players } => all_players.hash(state),
            StaticMode::CantBeBlockedExceptBy { kind } => match kind {
                // TargetFilter does not implement Hash; discriminant only.
                BlockExceptionKind::Quality(_) => {}
                BlockExceptionKind::MinBlockers { min } => min.hash(state),
            },
            StaticMode::CantBeBlockedBy { .. } => {} // TargetFilter does not implement Hash; discriminant only
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
            } => {
                frequency.hash(state);
                play_mode.hash(state);
                graveyard_destination_replacement.hash(state);
            }
            StaticMode::TopOfLibraryCastPermission { play_mode, .. } => {
                // alt_cost contains AbilityCost which lacks Hash; discriminant + play_mode only.
                play_mode.hash(state);
            }
            StaticMode::CastFromHandFree { frequency } => {
                frequency.hash(state);
            }
            StaticMode::ExileCastPermission {
                frequency,
                play_mode,
                cost,
            } => {
                frequency.hash(state);
                play_mode.hash(state);
                cost.hash(state);
            }
            StaticMode::SkipStep { step } => step.hash(state),
            StaticMode::DoubleTriggers { cause } => cause.hash(state),
            // CR 107.4f: Parameterized by ManaColor — hash the color so distinct
            // grants (Black vs Red) don't collide.
            StaticMode::PayLifeAsColoredMana { color } => color.hash(state),
            // Data-carrying variants with non-Hash fields: discriminant only.
            // These are never used as HashMap keys (handled by is_data_carrying_static).
            StaticMode::ModifyCost { .. }
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
            | StaticMode::SuppressTriggers { .. } => {}
            // All other variants are unit variants — discriminant suffices.
            _ => {}
        }
    }
}

impl fmt::Display for StaticMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StaticMode::Continuous => write!(f, "Continuous"),
            StaticMode::CantAttack => write!(f, "CantAttack"),
            StaticMode::CantBlock => write!(f, "CantBlock"),
            StaticMode::CantAttackOrBlock => write!(f, "CantAttackOrBlock"),
            StaticMode::MaxAttackersEachCombat { max } => {
                write!(f, "MaxAttackersEachCombat({max})")
            }
            StaticMode::MaxBlockersEachCombat { max } => {
                write!(f, "MaxBlockersEachCombat({max})")
            }
            StaticMode::CantBeTargeted => write!(f, "CantBeTargeted"),
            StaticMode::CantBeCast { who } => write!(f, "CantBeCast({who})"),
            StaticMode::CantBeActivated { who, .. } => write!(f, "CantBeActivated({who})"),
            StaticMode::CantSearchLibrary { cause } => write!(f, "CantSearchLibrary({cause})"),
            StaticMode::SuppressTriggers { events, .. } => {
                let parts: Vec<String> = events.iter().map(|e| e.to_string()).collect();
                write!(f, "SuppressTriggers({})", parts.join("+"))
            }
            StaticMode::CastWithFlash => write!(f, "CastWithFlash"),
            StaticMode::GrantsExtraVote => write!(f, "GrantsExtraVote"),
            StaticMode::CastWithKeyword { keyword } => {
                write!(f, "CastWithKeyword({keyword:?})")
            }
            StaticMode::CastWithAlternativeCost { cost } => {
                write!(f, "CastWithAlternativeCost({cost:?})")
            }
            StaticMode::ModifyCost { mode, .. } => match mode {
                CostModifyMode::Reduce => write!(f, "ReduceCost"),
                CostModifyMode::Raise => write!(f, "RaiseCost"),
                CostModifyMode::Minimum => write!(f, "MinimumCost"),
            },
            StaticMode::ReduceAbilityCost {
                keyword,
                amount,
                minimum_mana,
                ..
            } => {
                if let Some(minimum_mana) = minimum_mana {
                    write!(f, "ReduceAbilityCost({keyword},{amount},{minimum_mana})")
                } else {
                    write!(f, "ReduceAbilityCost({keyword},{amount})")
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
            StaticMode::MustBlock => write!(f, "MustBlock"),
            StaticMode::CantDraw { who } => write!(f, "CantDraw({who})"),
            StaticMode::DoubleTriggers { cause } => write!(f, "DoubleTriggers({cause})"),
            StaticMode::IgnoreHexproof => write!(f, "IgnoreHexproof"),
            StaticMode::GraveyardCastPermission {
                frequency,
                play_mode,
                graveyard_destination_replacement,
            } => {
                if matches!(graveyard_destination_replacement, Some(Zone::Exile)) {
                    write!(
                        f,
                        "GraveyardCastPermission({play_mode},{frequency},exile_on_graveyard)"
                    )
                } else {
                    write!(f, "GraveyardCastPermission({play_mode},{frequency})")
                }
            }
            StaticMode::TopOfLibraryCastPermission {
                play_mode,
                alt_cost,
            } => {
                if alt_cost.is_some() {
                    write!(f, "TopOfLibraryCastPermission({play_mode},alt_cost)")
                } else {
                    write!(f, "TopOfLibraryCastPermission({play_mode})")
                }
            }
            StaticMode::CastFromHandFree { frequency } => {
                write!(f, "CastFromHandFree({frequency})")
            }
            StaticMode::ExileCastPermission {
                frequency,
                play_mode,
                cost,
            } => match cost {
                ExileCastCost::WithoutPayingManaCost => {
                    write!(f, "ExileCastPermission({play_mode},{frequency},free)")
                }
                ExileCastCost::PayNormalCost => {
                    write!(f, "ExileCastPermission({play_mode},{frequency})")
                }
            },
            StaticMode::CantBeCountered => write!(f, "CantBeCountered"),
            StaticMode::CantBeCopied => write!(f, "CantBeCopied"),
            StaticMode::CantEnterBattlefieldFrom => write!(f, "CantEnterBattlefieldFrom"),
            StaticMode::CantCastFrom => write!(f, "CantCastFrom"),
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
            StaticMode::Protection => write!(f, "Protection"),
            StaticMode::Indestructible => write!(f, "Indestructible"),
            StaticMode::CantBeDestroyed => write!(f, "CantBeDestroyed"),
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
            StaticMode::CantAttackAlone => write!(f, "CantAttackAlone"),
            StaticMode::CantBlockAlone => write!(f, "CantBlockAlone"),
            StaticMode::MayLookAtTopOfLibrary => write!(f, "MayLookAtTopOfLibrary"),
            // Tier 3
            StaticMode::MayChooseNotToUntap => write!(f, "MayChooseNotToUntap"),
            StaticMode::AdditionalLandDrop { count } => {
                write!(f, "AdditionalLandDrop({count})")
            }
            StaticMode::EmblemStatic => write!(f, "EmblemStatic"),
            StaticMode::BlockRestriction => write!(f, "BlockRestriction"),
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
            StaticMode::SpendManaAsAnyColor => write!(f, "SpendManaAsAnyColor"),
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
            StaticMode::AssignNoCombatDamage => write!(f, "AssignNoCombatDamage"),
            StaticMode::UntapsDuringEachOtherPlayersUntapStep => {
                write!(f, "UntapsDuringEachOtherPlayersUntapStep")
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
            s if parse_static_mode_u32_arg(s, "MaxAttackersEachCombat").is_some() => {
                StaticMode::MaxAttackersEachCombat {
                    max: parse_static_mode_u32_arg(s, "MaxAttackersEachCombat").unwrap(),
                }
            }
            s if parse_static_mode_u32_arg(s, "MaxBlockersEachCombat").is_some() => {
                StaticMode::MaxBlockersEachCombat {
                    max: parse_static_mode_u32_arg(s, "MaxBlockersEachCombat").unwrap(),
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
                // Parse "ReduceAbilityCost(keyword,amount)"
                let inner = s
                    .strip_prefix("ReduceAbilityCost(")
                    .and_then(|s| s.strip_suffix(')'));
                if let Some(inner) = inner {
                    let mut parts = inner.split(',');
                    if let (Some(kw), Some(amt), extra) = (parts.next(), parts.next(), parts.next())
                    {
                        StaticMode::ReduceAbilityCost {
                            keyword: kw.to_string(),
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
                    }
                } else {
                    StaticMode::GraveyardCastPermission {
                        frequency: CastFrequency::OncePerTurn,
                        play_mode: CardPlayMode::Cast,
                        graveyard_destination_replacement: None,
                    }
                }
            }
            // CR 401.5 + CR 118.9: Top-of-library cast permission. The Display
            // form omits the alt_cost payload (it's preserved through serde,
            // not the FromStr round-trip), so FromStr defaults alt_cost to None.
            "TopOfLibraryCastPermission" => StaticMode::TopOfLibraryCastPermission {
                play_mode: CardPlayMode::Cast,
                alt_cost: None,
            },
            s if s.starts_with("TopOfLibraryCastPermission(") => {
                let inner = s
                    .strip_prefix("TopOfLibraryCastPermission(")
                    .and_then(|s| s.strip_suffix(')'))
                    .unwrap_or("");
                let pm_token = inner.split(',').next().unwrap_or("Cast");
                StaticMode::TopOfLibraryCastPermission {
                    play_mode: pm_token.parse().unwrap_or(CardPlayMode::Cast),
                    alt_cost: None,
                }
            }
            "CastFromHandFree" => StaticMode::CastFromHandFree {
                frequency: CastFrequency::Unlimited,
            },
            s if s.starts_with("CastFromHandFree(") => {
                let freq = s
                    .strip_prefix("CastFromHandFree(")
                    .and_then(|s| s.strip_suffix(')'))
                    .unwrap_or("unlimited");
                StaticMode::CastFromHandFree {
                    frequency: freq.parse().unwrap_or(CastFrequency::Unlimited),
                }
            }
            "ExileCastPermission" => StaticMode::ExileCastPermission {
                frequency: CastFrequency::Unlimited,
                play_mode: CardPlayMode::Cast,
                cost: ExileCastCost::PayNormalCost,
            },
            s if s.starts_with("ExileCastPermission(") => {
                // Display form: "ExileCastPermission(<play_mode>,<frequency>)" or
                // "ExileCastPermission(<play_mode>,<frequency>,free)" for the
                // `WithoutPayingManaCost` shape. Parse positionally so a later
                // 3-segment form survives lossless round-trip.
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
                let cost = if matches!(parts.next(), Some("free")) {
                    ExileCastCost::WithoutPayingManaCost
                } else {
                    ExileCastCost::PayNormalCost
                };
                StaticMode::ExileCastPermission {
                    frequency,
                    play_mode,
                    cost,
                }
            }
            "CantBeCountered" => StaticMode::CantBeCountered,
            "CantBeCopied" => StaticMode::CantBeCopied,
            "CantEnterBattlefieldFrom" => StaticMode::CantEnterBattlefieldFrom,
            "CantCastFrom" => StaticMode::CantCastFrom,
            // Tier 1
            "CantBeBlocked" => StaticMode::CantBeBlocked,
            "Protection" => StaticMode::Protection,
            "Indestructible" => StaticMode::Indestructible,
            "CantBeDestroyed" => StaticMode::CantBeDestroyed,
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
            "CantAttackAlone" => StaticMode::CantAttackAlone,
            "CantBlockAlone" => StaticMode::CantBlockAlone,
            "MayLookAtTopOfLibrary" => StaticMode::MayLookAtTopOfLibrary,
            // Tier 3
            "MayChooseNotToUntap" => StaticMode::MayChooseNotToUntap,
            // AdditionalLandDrop is parameterized — parsed in the `other` branch below
            "EmblemStatic" => StaticMode::EmblemStatic,
            "BlockRestriction" => StaticMode::BlockRestriction,
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
            s if s.starts_with("StepEndUnspentMana(") => StaticMode::Other(s.to_string()),
            "UntapsDuringEachOtherPlayersUntapStep" => {
                StaticMode::UntapsDuringEachOtherPlayersUntapStep
            }
            // CR 701.38d: "While voting, you may vote an additional time."
            "GrantsExtraVote" => StaticMode::GrantsExtraVote,
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
                } else if other.starts_with("SuppressTriggers(") {
                    // CR 603.2g: Data-carrying — round-trip preserves discriminant only.
                    // Callers that need the full filter/events read from the typed field.
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
/// Using `FromStr` would map known variants like `"SpendManaAsAnyColor"` to
/// `Other("SpendManaAsAnyColor")` whenever they aren't explicitly listed,
/// breaking coverage and registry lookups for those cards.
pub fn deserialize_static_mode_fwd<'de, D>(d: D) -> Result<StaticMode, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    let raw: serde_json::Value = serde_json::Value::deserialize(d)?;
    match raw {
        serde_json::Value::String(ref s) => {
            // Unit variant path. Try the derived deserializer first so all
            // known unit variants (e.g. "SpendManaAsAnyColor", "Flying", …)
            // round-trip correctly. If the derived impl rejects the string
            // (unknown variant from a newer engine build), fall back to
            // Other(s) so the card still loads without a hard error.
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
            StaticMode::MaxAttackersEachCombat { max: 2 },
            StaticMode::MaxBlockersEachCombat { max: 3 },
            StaticMode::RevealTopOfLibrary { all_players: false },
            StaticMode::RevealTopOfLibrary { all_players: true },
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
            StaticMode::CantAttackAlone,
            StaticMode::CantBlockAlone,
            StaticMode::MayLookAtTopOfLibrary,
            // Tier 3: parser-produced statics
            StaticMode::MayChooseNotToUntap,
            StaticMode::AdditionalLandDrop { count: 1 },
            StaticMode::AdditionalLandDrop { count: 2 },
            StaticMode::EmblemStatic,
            StaticMode::BlockRestriction,
            StaticMode::NoMaximumHandSize,
            StaticMode::MayPlayAdditionalLand,
            // Graveyard cast/play permissions
            StaticMode::GraveyardCastPermission {
                frequency: CastFrequency::OncePerTurn,
                play_mode: CardPlayMode::Cast,
                graveyard_destination_replacement: None,
            },
            StaticMode::GraveyardCastPermission {
                frequency: CastFrequency::Unlimited,
                play_mode: CardPlayMode::Play,
                graveyard_destination_replacement: None,
            },
            // Cast-from-hand-free permissions (Omniscience; Zaffai).
            StaticMode::CastFromHandFree {
                frequency: CastFrequency::Unlimited,
            },
            StaticMode::CastFromHandFree {
                frequency: CastFrequency::OncePerTurn,
            },
            // Exile-cast permission (Maralen, Fae Ascendant).
            StaticMode::ExileCastPermission {
                frequency: CastFrequency::OncePerTurn,
                play_mode: CardPlayMode::Cast,
                cost: ExileCastCost::WithoutPayingManaCost,
            },
            StaticMode::ExileCastPermission {
                frequency: CastFrequency::Unlimited,
                play_mode: CardPlayMode::Cast,
                cost: ExileCastCost::PayNormalCost,
            },
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
                cost: ManaCost::zero(),
            },
            StaticMode::CastWithAlternativeCost {
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
