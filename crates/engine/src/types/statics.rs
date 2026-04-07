use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::ability::{CardPlayMode, QuantityRef, TargetFilter};
use super::keywords::Keyword;
use super::mana::{ManaColor, ManaCost};
use super::phase::Phase;

/// CR 101.2: Who is prohibited from casting spells.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum CastingProhibitionScope {
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

impl fmt::Display for CastingProhibitionScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CastingProhibitionScope::Opponents => write!(f, "opponents"),
            CastingProhibitionScope::AllPlayers => write!(f, "all_players"),
            CastingProhibitionScope::Controller => write!(f, "controller"),
            CastingProhibitionScope::EnchantedCreatureController => {
                write!(f, "enchanted_creature_controller")
            }
        }
    }
}

impl FromStr for CastingProhibitionScope {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "opponents" => Ok(CastingProhibitionScope::Opponents),
            "all_players" => Ok(CastingProhibitionScope::AllPlayers),
            "controller" => Ok(CastingProhibitionScope::Controller),
            "enchanted_creature_controller" => {
                Ok(CastingProhibitionScope::EnchantedCreatureController)
            }
            other => Err(format!("unknown CastingProhibitionScope: {other}")),
        }
    }
}

/// CR 101.2: When the casting prohibition applies.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum CastingProhibitionCondition {
    /// "during your turn" — prohibition active on controller's turn.
    DuringYourTurn,
    /// "during combat" — prohibition active during any combat phase.
    DuringCombat,
    /// CR 117.1a + CR 604.1: "only during your turn" ≡ "can't cast when it's not your turn"
    /// — prohibition active when it is NOT the controller's turn.
    /// E.g., Fires of Invention: "You can cast spells only during your turn."
    NotDuringYourTurn,
}

impl fmt::Display for CastingProhibitionCondition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CastingProhibitionCondition::DuringYourTurn => write!(f, "your_turn"),
            CastingProhibitionCondition::DuringCombat => write!(f, "combat"),
            CastingProhibitionCondition::NotDuringYourTurn => write!(f, "not_your_turn"),
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
            other => Err(format!("unknown CastingProhibitionCondition: {other}")),
        }
    }
}

/// All static ability modes from Forge's static ability registry.
/// Matched case-sensitively against Forge mode strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum StaticMode {
    Continuous,
    CantAttack,
    CantBlock,
    CantAttackOrBlock,
    CantBeTargeted,
    /// CR 101.2: Blanket casting prohibition — prevents the scoped player(s) from casting spells.
    /// E.g., Steel Golem: "You can't cast creature spells." (Controller scope + creature filter)
    CantBeCast {
        who: CastingProhibitionScope,
    },
    CantBeActivated,
    CastWithFlash,
    /// CR 601.2f: Reduces the cost of spells matching the filter.
    /// Permanent-based cost reduction applied during casting (not self-cost reduction).
    ReduceCost {
        amount: ManaCost,
        spell_filter: Option<TargetFilter>,
        dynamic_count: Option<QuantityRef>,
    },
    /// CR 601.2f: Reduces the generic mana cost of activated abilities matching a keyword type.
    /// E.g., "Ninjutsu abilities you activate cost {1} less to activate."
    /// `keyword` identifies which ability type is reduced (e.g., "ninjutsu", "equip", "cycling").
    /// `amount` is the fixed generic mana reduction per activation.
    ReduceAbilityCost {
        keyword: String,
        amount: u32,
    },
    /// CR 601.2f: Increases the cost of spells matching the filter.
    /// Permanent-based cost increase applied during casting (Thalia, etc.).
    RaiseCost {
        amount: ManaCost,
        spell_filter: Option<TargetFilter>,
        dynamic_count: Option<QuantityRef>,
    },
    CantGainLife,
    CantLoseLife,
    MustAttack,
    MustBlock,
    CantDraw,
    Panharmonicon,
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
        /// true = "once during each of your turns" (Lurrus, Karador)
        once_per_turn: bool,
        /// Play (lands+spells) vs Cast (spells only)
        play_mode: CardPlayMode,
    },
    /// CR 601.2b + CR 118.9a: Static ability granting permission to cast
    /// matching spells from hand without paying their mana costs (Omniscience).
    CastFromHandFree,
    /// CR 101.2: This spell/permanent can't be countered.
    CantBeCountered,
    /// CR 604.3: Cards in specified zones can't enter the battlefield.
    CantEnterBattlefieldFrom,
    /// CR 604.3: Players can't cast spells from specified zones.
    CantCastFrom,
    /// CR 101.2: Continuous casting prohibition — prevents players from casting
    /// spells under specified conditions (turn/phase-scoped).
    /// E.g., "Your opponents can't cast spells during your turn."
    CantCastDuring {
        who: CastingProhibitionScope,
        when: CastingProhibitionCondition,
    },
    /// CR 101.2 + CR 604.1: Per-turn casting limit — static ability generating a
    /// continuous "can't" effect that restricts how many spells a player may cast.
    /// E.g., Rule of Law: "Each player can't cast more than one spell each turn."
    /// E.g., Deafening Silence: "Each player can't cast more than one noncreature spell each turn."
    PerTurnCastLimit {
        who: CastingProhibitionScope,
        max: u32,
        spell_filter: Option<TargetFilter>,
    },
    /// CR 101.2: Per-turn draw limit — restricts how many cards a player may draw.
    /// E.g., Spirit of the Labyrinth: "Each player can't draw more than one card each turn."
    /// E.g., Narset, Parter of Veils: "Each opponent can't draw more than one card each turn."
    PerTurnDrawLimit {
        who: CastingProhibitionScope,
        max: u32,
    },

    // -- Tier 1: Keyword/evasion statics with dedicated handlers --
    /// CR 509.1b: This creature can't be blocked.
    CantBeBlocked,
    /// CR 509.1b: This creature can't be blocked except by creatures matching filter.
    // TODO: parse filter to TargetFilter for type-safe matching
    CantBeBlockedExceptBy {
        filter: String,
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
    /// CR 702.20: Vigilance — attacking doesn't cause this creature to tap.
    Vigilance,
    /// CR 702.110: Menace — can't be blocked except by two or more creatures.
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
    /// CR 509.1c: This creature must be blocked if able.
    MustBeBlocked,
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
    /// CR 510.1a: This creature assigns no combat damage.
    /// Used for creatures like Ornithopter of Paradise and various Walls that can
    /// attack/block but deal 0 combat damage.
    AssignNoCombatDamage,
    /// Fallback for unrecognized static mode strings.
    Other(String),
}

/// Manual Hash impl because `ReduceCost`/`RaiseCost` contain `TargetFilter` and `QuantityRef`
/// which don't implement `Hash`. For data-carrying variants, we hash only the discriminant +
/// simple fields. This is safe because data-carrying variants are never used as HashMap keys
/// (they're handled by `is_data_carrying_static` in coverage.rs instead).
impl Hash for StaticMode {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            StaticMode::ReduceAbilityCost { keyword, amount } => {
                keyword.hash(state);
                amount.hash(state);
            }
            StaticMode::ExtraBlockers { count } => count.hash(state),
            StaticMode::RevealTopOfLibrary { all_players } => all_players.hash(state),
            StaticMode::CantBeBlockedExceptBy { filter } => filter.hash(state),
            StaticMode::AdditionalLandDrop { count } => count.hash(state),
            StaticMode::Other(s) => s.hash(state),
            StaticMode::GraveyardCastPermission {
                once_per_turn,
                play_mode,
            } => {
                once_per_turn.hash(state);
                play_mode.hash(state);
            }
            StaticMode::SkipStep { step } => step.hash(state),
            // Data-carrying variants with non-Hash fields: discriminant only.
            // These are never used as HashMap keys (handled by is_data_carrying_static).
            StaticMode::ReduceCost { .. }
            | StaticMode::RaiseCost { .. }
            | StaticMode::DefilerCostReduction { .. }
            | StaticMode::PerTurnCastLimit { .. }
            | StaticMode::PerTurnDrawLimit { .. } => {}
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
            StaticMode::CantBeTargeted => write!(f, "CantBeTargeted"),
            StaticMode::CantBeCast { who } => write!(f, "CantBeCast({who})"),
            StaticMode::CantBeActivated => write!(f, "CantBeActivated"),
            StaticMode::CastWithFlash => write!(f, "CastWithFlash"),
            StaticMode::ReduceCost { .. } => write!(f, "ReduceCost"),
            StaticMode::ReduceAbilityCost { keyword, amount } => {
                write!(f, "ReduceAbilityCost({keyword},{amount})")
            }
            StaticMode::RaiseCost { .. } => write!(f, "RaiseCost"),
            StaticMode::CantGainLife => write!(f, "CantGainLife"),
            StaticMode::CantLoseLife => write!(f, "CantLoseLife"),
            StaticMode::MustAttack => write!(f, "MustAttack"),
            StaticMode::MustBlock => write!(f, "MustBlock"),
            StaticMode::CantDraw => write!(f, "CantDraw"),
            StaticMode::Panharmonicon => write!(f, "Panharmonicon"),
            StaticMode::IgnoreHexproof => write!(f, "IgnoreHexproof"),
            StaticMode::GraveyardCastPermission {
                once_per_turn,
                play_mode,
            } => write!(f, "GraveyardCastPermission({play_mode},{once_per_turn})"),
            StaticMode::CastFromHandFree => write!(f, "CastFromHandFree"),
            StaticMode::CantBeCountered => write!(f, "CantBeCountered"),
            StaticMode::CantEnterBattlefieldFrom => write!(f, "CantEnterBattlefieldFrom"),
            StaticMode::CantCastFrom => write!(f, "CantCastFrom"),
            StaticMode::CantCastDuring { who, when } => {
                write!(f, "CantCastDuring({who},{when})")
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
            StaticMode::CantBeBlockedExceptBy { filter } => {
                write!(f, "CantBeBlockedExceptBy:{filter}")
            }
            StaticMode::Protection => write!(f, "Protection"),
            StaticMode::Indestructible => write!(f, "Indestructible"),
            StaticMode::CantBeDestroyed => write!(f, "CantBeDestroyed"),
            StaticMode::FlashBack => write!(f, "FlashBack"),
            StaticMode::Shroud => write!(f, "Shroud"),
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
            StaticMode::MayPlayAdditionalLand => write!(f, "MayPlayAdditionalLand"),
            StaticMode::CantHaveKeyword { keyword } => {
                write!(f, "CantHaveKeyword({keyword:?})")
            }
            StaticMode::CantWinTheGame => write!(f, "CantWinTheGame"),
            StaticMode::CantLoseTheGame => write!(f, "CantLoseTheGame"),
            StaticMode::SpeedCanIncreaseBeyondFour => write!(f, "SpeedCanIncreaseBeyondFour"),
            StaticMode::DefilerCostReduction { color, .. } => {
                write!(f, "DefilerCostReduction({color:?})")
            }
            StaticMode::SkipStep { step } => write!(f, "SkipStep({step:?})"),
            StaticMode::SpendManaAsAnyColor => write!(f, "SpendManaAsAnyColor"),
            StaticMode::AssignNoCombatDamage => write!(f, "AssignNoCombatDamage"),
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
            "CantBeTargeted" => StaticMode::CantBeTargeted,
            "CantBeCast" => StaticMode::CantBeCast {
                who: CastingProhibitionScope::Controller,
            },
            "CantBeActivated" => StaticMode::CantBeActivated,
            "CastWithFlash" => StaticMode::CastWithFlash,
            "ReduceCost" => StaticMode::ReduceCost {
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
                    if let Some((kw, amt)) = inner.split_once(',') {
                        StaticMode::ReduceAbilityCost {
                            keyword: kw.to_string(),
                            amount: amt.parse().unwrap_or(1),
                        }
                    } else {
                        StaticMode::Other(s.to_string())
                    }
                } else {
                    StaticMode::Other(s.to_string())
                }
            }
            "RaiseCost" => StaticMode::RaiseCost {
                amount: ManaCost::zero(),
                spell_filter: None,
                dynamic_count: None,
            },
            "CantGainLife" => StaticMode::CantGainLife,
            "CantLoseLife" => StaticMode::CantLoseLife,
            "MustAttack" => StaticMode::MustAttack,
            "MustBlock" => StaticMode::MustBlock,
            "CantDraw" => StaticMode::CantDraw,
            "Panharmonicon" => StaticMode::Panharmonicon,
            "IgnoreHexproof" => StaticMode::IgnoreHexproof,
            "GraveyardCastPermission" => StaticMode::GraveyardCastPermission {
                once_per_turn: true,
                play_mode: CardPlayMode::Cast,
            },
            s if s.starts_with("GraveyardCastPermission(") => {
                let inner = s
                    .strip_prefix("GraveyardCastPermission(")
                    .and_then(|s| s.strip_suffix(')'))
                    .unwrap_or("");
                if let Some((pm, otp)) = inner.split_once(',') {
                    StaticMode::GraveyardCastPermission {
                        play_mode: pm.parse().unwrap_or(CardPlayMode::Cast),
                        once_per_turn: otp == "true",
                    }
                } else {
                    StaticMode::GraveyardCastPermission {
                        once_per_turn: true,
                        play_mode: CardPlayMode::Cast,
                    }
                }
            }
            "CastFromHandFree" => StaticMode::CastFromHandFree,
            "CantBeCountered" => StaticMode::CantBeCountered,
            "CantEnterBattlefieldFrom" => StaticMode::CantEnterBattlefieldFrom,
            "CantCastFrom" => StaticMode::CantCastFrom,
            // Tier 1
            "CantBeBlocked" => StaticMode::CantBeBlocked,
            "Protection" => StaticMode::Protection,
            "Indestructible" => StaticMode::Indestructible,
            "CantBeDestroyed" => StaticMode::CantBeDestroyed,
            "FlashBack" => StaticMode::FlashBack,
            "Shroud" => StaticMode::Shroud,
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
            "CantAttackAlone" => StaticMode::CantAttackAlone,
            "CantBlockAlone" => StaticMode::CantBlockAlone,
            "MayLookAtTopOfLibrary" => StaticMode::MayLookAtTopOfLibrary,
            // Tier 3
            "MayChooseNotToUntap" => StaticMode::MayChooseNotToUntap,
            // AdditionalLandDrop is parameterized — parsed in the `other` branch below
            "EmblemStatic" => StaticMode::EmblemStatic,
            "BlockRestriction" => StaticMode::BlockRestriction,
            "NoMaximumHandSize" => StaticMode::NoMaximumHandSize,
            "MayPlayAdditionalLand" => StaticMode::MayPlayAdditionalLand,
            "CantWinTheGame" => StaticMode::CantWinTheGame,
            "CantLoseTheGame" => StaticMode::CantLoseTheGame,
            // Parameterized
            other => {
                if let Some(inner) = other
                    .strip_prefix("CantBeCast(")
                    .and_then(|s| s.strip_suffix(')'))
                {
                    if let Ok(who) = CastingProhibitionScope::from_str(inner) {
                        return Ok(StaticMode::CantBeCast { who });
                    }
                    return Ok(StaticMode::Other(other.to_string()));
                } else if let Some(inner) = other
                    .strip_prefix("CantCastDuring(")
                    .and_then(|s| s.strip_suffix(')'))
                {
                    if let Some((who_str, when_str)) = inner.split_once(',') {
                        if let (Ok(who), Ok(when)) = (
                            CastingProhibitionScope::from_str(who_str),
                            CastingProhibitionCondition::from_str(when_str),
                        ) {
                            return Ok(StaticMode::CantCastDuring { who, when });
                        }
                    }
                    return Ok(StaticMode::Other(other.to_string()));
                } else if let Some(inner) = other
                    .strip_prefix("PerTurnCastLimit(")
                    .and_then(|s| s.strip_suffix(')'))
                {
                    if let Some((who_str, max_str)) = inner.split_once(',') {
                        if let (Ok(who), Ok(max)) = (
                            CastingProhibitionScope::from_str(who_str),
                            max_str.parse::<u32>(),
                        ) {
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
                        if let (Ok(who), Ok(max)) = (
                            CastingProhibitionScope::from_str(who_str),
                            max_str.parse::<u32>(),
                        ) {
                            return Ok(StaticMode::PerTurnDrawLimit { who, max });
                        }
                    }
                    return Ok(StaticMode::Other(other.to_string()));
                } else if let Some(filter) = other.strip_prefix("CantBeBlockedExceptBy:") {
                    StaticMode::CantBeBlockedExceptBy {
                        filter: filter.to_string(),
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
        assert_eq!(
            StaticMode::from_str("Panharmonicon").unwrap(),
            StaticMode::Panharmonicon
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
            StaticMode::RevealTopOfLibrary { all_players: false },
            StaticMode::RevealTopOfLibrary { all_players: true },
            // Tier 1: keyword/evasion statics
            StaticMode::CantBeBlocked,
            StaticMode::CantBeBlockedExceptBy {
                filter: "creatures with flying".to_string(),
            },
            StaticMode::Protection,
            StaticMode::Indestructible,
            StaticMode::CantBeDestroyed,
            StaticMode::FlashBack,
            StaticMode::Shroud,
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
                once_per_turn: true,
                play_mode: CardPlayMode::Cast,
            },
            StaticMode::GraveyardCastPermission {
                once_per_turn: false,
                play_mode: CardPlayMode::Play,
            },
            // Casting prohibitions
            StaticMode::CantBeCast {
                who: CastingProhibitionScope::Controller,
            },
            StaticMode::CantBeCast {
                who: CastingProhibitionScope::Opponents,
            },
            StaticMode::CantCastDuring {
                who: CastingProhibitionScope::Opponents,
                when: CastingProhibitionCondition::DuringYourTurn,
            },
            StaticMode::CantCastDuring {
                who: CastingProhibitionScope::AllPlayers,
                when: CastingProhibitionCondition::DuringCombat,
            },
            StaticMode::CantCastDuring {
                who: CastingProhibitionScope::Controller,
                when: CastingProhibitionCondition::NotDuringYourTurn,
            },
            // Per-turn casting limits
            StaticMode::PerTurnCastLimit {
                who: CastingProhibitionScope::AllPlayers,
                max: 1,
                spell_filter: None,
            },
            StaticMode::PerTurnCastLimit {
                who: CastingProhibitionScope::Controller,
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
            StaticMode::Other("Custom".to_string()),
        ];
        let json = serde_json::to_string(&modes).unwrap();
        let deserialized: Vec<StaticMode> = serde_json::from_str(&json).unwrap();
        assert_eq!(modes, deserialized);
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
