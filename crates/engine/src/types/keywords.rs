use std::convert::Infallible;
use std::fmt;
use std::str::FromStr;

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::combinator::value;
use nom::sequence::preceded;
use nom::Parser;
use serde::{Deserialize, Serialize};

#[cfg(test)]
use super::ability::ControllerRef;
use super::ability::{
    AbilityCost, ActivationRestriction, Comparator, CostObjectCount, FilterProp, QuantityExpr,
    TargetFilter, TypeFilter, TypedFilter,
};
use super::counter::{parse_counter_type, CounterType};
use super::mana::{ManaColor, ManaCost};

/// CR 702.34a: Flashback cost — either a mana cost or a non-mana cost
/// (e.g., "Tap three untapped white creatures you control").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum FlashbackCost {
    Mana(ManaCost),
    NonMana(AbilityCost),
}

/// CR 702.128a + CR 602.1a: Embalm cost — either a mana cost ("Embalm {3}{W}")
/// or a non-mana/composite cost ("Embalm—{2}{W}{W}, Discard a card."). Mirrors
/// `CyclingCost`/`FlashbackCost` so a composite non-mana cost composes through
/// the existing `AbilityCost::Composite` activated-ability pipeline in
/// `database::embalm_eternalize::token_copy_ability`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum EmbalmCost {
    Mana(ManaCost),
    NonMana(AbilityCost),
}

/// CR 702.129a + CR 602.1a: Eternalize cost — either a mana cost or a
/// non-mana/composite cost ("Eternalize—{3}{U}{U}, Discard a card." — the
/// Champion of Wits family). Mirrors `EmbalmCost`/`CyclingCost`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum EternalizeCost {
    Mana(ManaCost),
    NonMana(AbilityCost),
}

/// CR 702.29a: Cycling cost — either a mana cost or a non-mana cost
/// (e.g., Street Wraith's "Pay 2 life"). Mirrors `FlashbackCost` so the
/// synthesis in `database::synthesis::synthesize_cycling` can route
/// composite non-mana costs through the existing `AbilityCost::Composite`
/// activated-ability pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum CyclingCost {
    Mana(ManaCost),
    NonMana(AbilityCost),
}

/// CR 702.27a: Buyback cost — the optional additional cost a player may pay
/// as they cast a spell with Buyback. Most Buyback cards use a pure mana cost
/// (e.g., Capsize "Buyback {3}") but some use a non-mana cost (Constant Mists
/// "Buyback—Sacrifice a land"). Mirrors `FlashbackCost` so non-mana costs
/// compose through the existing `AbilityCost` pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum BuybackCost {
    Mana(ManaCost),
    NonMana(AbilityCost),
}

/// CR 702.74a + CR 118.9: Evoke cost — the alternative cost a player may pay
/// in place of the spell's mana cost. Original Lorwyn Evoke used pure mana
/// costs (e.g., Mulldrifter "Evoke {3}{U}"); the Modern Horizons 2 elemental
/// cycle (Solitude, Endurance, Grief, Subtlety, Fury) introduced non-mana
/// evoke ("Evoke—Exile a [color] card from your hand."). Mirrors
/// `FlashbackCost`/`BuybackCost`/`CyclingCost` so non-mana costs compose
/// through the existing `AbilityCost` pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum EvokeCost {
    Mana(ManaCost),
    NonMana(AbilityCost),
}

/// CR 702.30a: Echo cost — either a mana cost (Urza-block / errata cards, e.g.
/// Orcish Hellraiser "Echo {R}") or a non-mana cost ("Echo—Discard a card" on
/// Rakdos Headliner, Deepcavern Imp). Mirrors `EvokeCost`/`BuybackCost`/
/// `CyclingCost`/`FlashbackCost` so non-mana costs compose through the existing
/// `AbilityCost` unless-pay pipeline in `build_echo_trigger`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum EchoCost {
    Mana(ManaCost),
    NonMana(AbilityCost),
}

/// CR 702.103a + CR 118.9: Bestow cost — the alternative cost paid to cast the
/// card as an Aura. Classic Theros bestow uses a pure mana cost (e.g. Boon Satyr
/// "Bestow {3}{G}{G}"). Murders at Karlov Manor reprints introduced compound
/// bestow costs with a non-mana rider ("Bestow—{R}, Collect evidence 6." on
/// Detective's Phoenix), where the residual non-mana sub-cost is paid alongside
/// the mana sub-cost. Mirrors `EvokeCost`/`FlashbackCost`/`CyclingCost` so the
/// non-mana portion composes through the existing `AbilityCost` /
/// `pay_additional_cost` pipeline. `split_bestow_cost_components` (casting.rs)
/// separates the mana sub-cost (paid via the normal mana flow, CR 601.2g) from
/// the residual non-mana sub-cost (paid via `pay_additional_cost`, CR 601.2h).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum BestowCost {
    Mana(ManaCost),
    NonMana(AbilityCost),
}

/// CR 702.138a + CR 118.9 + CR 601.2f-h: Escape cost — an alternative cost paid
/// to cast a card from the graveyard (CR 702.138a). Almost always a compound
/// cost: a mana sub-cost plus "Exile N other cards from your graveyard". A few
/// cards add further sub-costs (e.g. Lunar Hatchling: "Exile a land you control,
/// Exile five other cards from your graveyard"). Mirrors
/// `EvokeCost`/`FlashbackCost`/`BestowCost` so the compound cost composes through
/// `AbilityCost::Composite` and is split at runtime by `split_escape_cost_components`
/// (casting.rs): the mana sub-cost is paid via the normal mana flow (CR 601.2g)
/// and the residual exile sub-cost(s) via `pay_additional_cost` (CR 601.2h).
/// Exiling permanents/cards as a cost is CR 701.13 (exile), NOT CR 701.21
/// (sacrifice) — no sacrifice/death triggers fire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum EscapeCost {
    Mana(ManaCost),
    NonMana(AbilityCost),
}

/// Discriminant-level keyword identity used when the Oracle text refers to a keyword class
/// without caring about its parameter payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KeywordKind {
    Flying,
    FirstStrike,
    DoubleStrike,
    Trample,
    TrampleOverPlaneswalkers,
    Deathtouch,
    Lifelink,
    Vigilance,
    Haste,
    Reach,
    Defender,
    Menace,
    Indestructible,
    Hexproof,
    Shroud,
    Flash,
    Fear,
    Intimidate,
    Skulk,
    Shadow,
    Horsemanship,
    Wither,
    Infect,
    Afflict,
    Prowess,
    Undying,
    Persist,
    Cascade,
    Exalted,
    Flanking,
    Evolve,
    Extort,
    Exploit,
    Explore,
    Ascend,
    StartYourEngines,
    Dredge,
    Modular,
    Renown,
    Fabricate,
    /// CR 702.58a: Graft N — see `Keyword::Graft`.
    Graft,
    Annihilator,
    Bushido,
    Frenzy,
    Tribute,
    Soulbond,
    Unearth,
    Convoke,
    Waterbend,
    Delve,
    Devoid,
    Changeling,
    Phasing,
    Battlecry,
    Decayed,
    Unleash,
    Riot,
    Afterlife,
    Enchant,
    EtbCounter,
    Reconfigure,
    LivingWeapon,
    JobSelect,
    TotemArmor,
    Bestow,
    Embalm,
    Eternalize,
    Fading,
    Vanishing,
    Protection,
    Kicker,
    Cycling,
    Typecycling,
    Flashback,
    Retrace,
    Ward,
    Equip,
    Landwalk,
    Rampage,
    Absorb,
    Crew,
    Partner,
    Companion,
    Doctor,
    Background,
    CommanderNinjutsu,
    Ninjutsu,
    Sneak,
    Mutate,
    Escape,
    Morph,
    Megamorph,
    /// CR 702.35a: Madness — see `Keyword::Madness`.
    Madness,
    /// CR 702.168: Disguise — see `Keyword::Disguise`. A discriminant-level kind
    /// (like Morph/Mutate) so `FilterProp::HasKeywordKind { Disguise }` can name
    /// the class regardless of the `Disguise(ManaCost)` parameter payload.
    Disguise,
    /// CR 702.187: Mayhem — see `Keyword::Mayhem`.
    Mayhem,
    Suspend,
    Blitz,
    Disturb,
    UnearthAlt,
    Foretell,
    Plot,
    Gift,
    Outlast,
    Dash,
    Craft,
    Harmonize,
    Warp,
    Devour,
    Offspring,
    Splice,
    Bargain,
    Sunburst,
    Champion,
    Training,
    Assist,
    /// Acorn/Un-set keyword: Augment. Not present in the main Comprehensive
    /// Rules file, but it is still a keyword characteristic for card filters.
    Augment,
    /// CR 702.127: Aftermath — see `Keyword::Aftermath`.
    Aftermath,
    JumpStart,
    Cipher,
    Transmute,
    /// CR 702.71: Transfigure — see `Keyword::Transfigure`.
    Transfigure,
    Cleave,
    Undaunted,
    Station,
    /// CR 702.xxx: Paradigm (Strixhaven) — see `Keyword::Paradigm`.
    Paradigm,
    /// CR 702.94: Miracle — see `Keyword::Miracle`.
    Miracle,
    /// CR 702.56: Replicate — see `Keyword::Replicate`.
    Replicate,
    /// CR 702.113: Awaken — see `Keyword::Awaken`.
    Awaken,
    /// CR 702.163: For Mirrodin! — see `Keyword::ForMirrodin`.
    ForMirrodin,
    /// CR 702.162: More Than Meets the Eye — see `Keyword::MoreThanMeetsTheEye`.
    MoreThanMeetsTheEye,
    /// CR 702.173: Freerunning — see `Keyword::Freerunning`.
    Freerunning,
    /// CR 702.191: Increment — see `Keyword::Increment`.
    Increment,
    /// CR 702.189: Firebending — see `Keyword::Firebending`.
    Firebending,
    /// CR ???: Specialize — not in CR text (needs manual verification).
    /// See `Keyword::Specialize`.
    Specialize,
    /// CR 702.48: Offering — see `Keyword::Offering`.
    Offering,
    /// CR 702.120: Escalate — see `Keyword::Escalate`.
    Escalate,
    /// CR 702.59: Recover — see `Keyword::Recover`.
    Recover,
    /// CR 702.102: Fuse — see `Keyword::Fuse`.
    Fuse,
    /// CR 702.22: Bands with other [quality] — see `Keyword::BandsWithOther`.
    BandsWithOther,
    Unknown,
}

/// Keywords that accept a dynamic numeric parameter via "where X is [quantity]".
/// Used by `ContinuousModification::AddDynamicKeyword` to construct the runtime keyword.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DynamicKeywordKind {
    Annihilator,
    Modular,
}

impl DynamicKeywordKind {
    /// Construct the concrete `Keyword` from a resolved parameter value.
    pub fn with_value(&self, value: u32) -> Keyword {
        match self {
            Self::Annihilator => Keyword::Annihilator(value),
            Self::Modular => Keyword::Modular(value),
        }
    }

    /// Parse a keyword name into a `DynamicKeywordKind`, if it supports dynamic parameters.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "annihilator" => Some(Self::Annihilator),
            "modular" => Some(Self::Modular),
            _ => None,
        }
    }
}

/// CR 702.124: Partner variant keywords for co-commander deckbuilding.
/// Each variant specifies which other partner types it can legally pair with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum PartnerType {
    /// CR 702.124a: Generic "Partner" — pairs with any other generic Partner.
    Generic,
    /// CR 702.124c: "Partner with [Name]" — pairs only with the named card.
    With(String),
    /// CR 702.124f: "Friends forever" — pairs with any other Friends Forever card.
    FriendsForever,
    /// CR 702.124: "Partner—Character select" — pairs with any other Character Select card.
    CharacterSelect,
    /// CR 702.124: "Doctor's companion" — pairs with any creature with the Doctor subtype.
    DoctorsCompanion,
    /// CR 702.124: "Choose a Background" — pairs with any enchantment with Background subtype.
    ChooseABackground,
}

/// CR 702.139: Companion deckbuilding conditions.
/// Each of the 10 companion cards has a unique condition the starting deck must satisfy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum CompanionCondition {
    /// Gyruda: Each nonland card in your starting deck has an even mana value.
    EvenManaValues,
    /// Jegantha: No card in your starting deck has more than one of the same mana symbol in its cost.
    NoRepeatedManaSymbols,
    /// Kaheera: Each creature card in your starting deck is of one of the listed types.
    CreatureTypeRestriction(Vec<String>),
    /// Keruga: Each nonland card in your starting deck has mana value 3 or greater.
    MinManaValue(u32),
    /// Lurrus: Each permanent card in your starting deck has mana value 2 or less.
    MaxPermanentManaValue(u32),
    /// Lutri: No nonland card in your starting deck has the same name as another.
    Singleton,
    /// Obosh: Each nonland card in your starting deck has an odd mana value.
    OddManaValues,
    /// Umori: Each nonland card in your starting deck shares a card type.
    SharedCardType,
    /// Yorion: Your starting deck has at least 80 cards (20 over the minimum).
    MinDeckSizeOver(u32),
    /// Zirda: Each permanent card in your starting deck has an activated ability.
    PermanentsHaveActivatedAbilities,
}

/// The type of gift promised by the Gift keyword.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum GiftKind {
    /// Opponent draws a card.
    Card,
    /// Opponent creates a Treasure token.
    Treasure,
    /// Opponent creates a Food token.
    Food,
    /// Opponent creates a tapped 1/1 blue Fish creature token.
    TappedFish,
}

/// CR 702.11d: What a hexproof-from keyword protects against.
/// Mirrors ProtectionTarget but only applies to targeting (not DEBT).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum HexproofFilter {
    Color(ManaColor),
    /// "hexproof from artifacts", "hexproof from instants", "hexproof from planeswalkers"
    CardType(String),
    /// "hexproof from monocolored", "hexproof from multicolored"
    Quality(String),
    /// CR 702.11d + CR 105.4 + CR 609.6: "Hexproof from that color" / "from the
    /// chosen color" — resolved at runtime from the source permanent's
    /// `chosen_attributes`. Parallels `ProtectionTarget::ChosenColor`.
    ChosenColor,
}

/// What a Protection keyword protects from (CR 702.16).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProtectionTarget {
    Color(ManaColor),
    CardType(String),
    Quality(String),
    Multicolored,
    /// CR 702.16: Protection from the chosen color — resolved at runtime from the
    /// source permanent's `chosen_attributes`.
    ChosenColor,
    /// CR 702.16 + CR 205.2: "Protection from the chosen card type" —
    /// resolved at runtime from the source permanent's `chosen_attributes`
    /// (the `CardType` chosen as the permanent entered). Parallels `ChosenColor`.
    ChosenCardType,
    /// CR 702.16j: "Protection from everything" — protection from each object
    /// regardless of that object's characteristic values. Matches every source
    /// in `source_matches_protection_target`.
    Everything,
    /// CR 702.16a: "Protection from [quality]" where the quality is a
    /// characteristic-value predicate expressible as a `TargetFilter`.
    /// Covers "protection from mana value N or less/greater" and any future
    /// filter-based protection (e.g., "protection from power 2 or less").
    /// Evaluated by testing the source object's characteristics against the
    /// filter's properties — only `FilterProp` predicates that can be resolved
    /// from the object alone (without game state) are valid here.
    Filter(super::ability::TargetFilter),
    /// CR 702.16k: "Protection from [a player]" — protection from each object
    /// controlled by the scoped player(s), relative to the protected object's
    /// controller, regardless of the source's characteristic values. Covers
    /// "protection from each of your opponents" (Figure of Fable's Avatar form)
    /// via `ControllerRef::Opponent`. CR 702.16i makes "each of your opponents"
    /// behave as protection from every opponent, which the Opponent scope
    /// captures in one variant.
    FromPlayer(super::ability::ControllerRef),
}

/// CR 702.21a: Ward cost — what the targeting player must pay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum WardCost {
    Mana(ManaCost),
    PayLife(i32),
    DiscardCard,
    /// CR 702.21a: Sacrifice N permanents matching a filter as ward cost.
    Sacrifice {
        count: u32,
        filter: crate::types::ability::TargetFilter,
    },
    /// CR 702.21a: Ward cost paid via waterbend — tap artifacts/creatures to help pay.
    /// Distinct from Mana because waterbend has unique payment semantics (CR 701.67).
    Waterbend(ManaCost),
    /// CR 702.21a: Compound ward cost — multiple costs that must all be paid.
    /// Used for "Ward—{2}, Pay 2 life" where comma-separated sub-costs are conjoined.
    Compound(Vec<WardCost>),
}

/// CR 702.54a + CR 702.54b: Bloodthirst has fixed-N and X-count forms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum BloodthirstValue {
    Fixed(u32),
    X,
}

/// All MTG keywords as typed enum variants.
/// Simple (unit) variants for keywords with no parameters.
/// Parameterized variants carry associated data (ManaCost for costs, amounts, etc.).
/// Unknown captures any unrecognized keyword string for forward compatibility.
///
/// Custom Deserialize: accepts both the typed externally-tagged format (new)
/// and plain "Name:Param" strings (legacy card-data.json).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Keyword {
    // Evasion / Combat
    Flying,
    FirstStrike,
    DoubleStrike,
    Trample,
    /// CR 702.19c: Trample over planeswalkers — excess damage can spill to PW controller.
    TrampleOverPlaneswalkers,
    Deathtouch,
    Lifelink,
    Vigilance,
    Haste,
    Reach,
    Defender,
    Menace,
    Indestructible,
    Hexproof,
    /// CR 702.11d: "Hexproof from [quality]" — prevents targeting by sources with the quality.
    HexproofFrom(HexproofFilter),
    Shroud,
    Flash,
    Fear,
    Intimidate,
    Skulk,
    Shadow,
    Horsemanship,

    // Damage modification
    Wither,
    Infect,
    /// CR 702.130a: "Afflict N" — when blocked, defending player loses N life.
    Afflict(u32),
    /// Digital-only Alchemy (no CR entry): "Starting intensity N" — the card's
    /// initial intensity value, stamped onto the object at creation.
    StartingIntensity(u32),

    // Triggered abilities
    Prowess,
    Undying,
    Persist,
    Cascade,
    Exalted,
    Flanking,
    Evolve,
    Extort,
    Exploit,
    Explore,
    Ascend,
    /// CR 702.179: Grants the player a speed value via SBA and enables the inherent speed trigger.
    StartYourEngines,
    Dredge(u32),
    Modular(u32),
    Renown(u32),
    Fabricate(u32),
    Annihilator(u32),
    Bushido(u32),
    /// CR 702.68a: Frenzy N — "Whenever this creature attacks and isn't blocked, it gets +N/+0 until end of turn." CR 702.68b: each instance triggers separately.
    Frenzy(u32),
    Tribute(u32),
    Soulbond,
    Unearth(ManaCost),

    // Cost reduction / alternative costs
    Convoke,
    /// Waterbend: tap-to-pay keyword for Avatar waterbending abilities.
    Waterbend,
    Delve,
    Devoid,

    // Creature type / characteristics
    Changeling,

    // Phase / zone
    Phasing,

    // Combat triggers
    Battlecry,
    Decayed,
    Unleash,
    Riot,
    Afterlife(u32),

    // Enchantment
    Enchant(TargetFilter),

    // ETB counter (e.g., P1P1:1)
    EtbCounter {
        counter_type: CounterType,
        count: u32,
    },

    // Equipment / attachment
    Reconfigure(ManaCost),
    LivingWeapon,
    /// CR 702.182a: Job select — "When this Equipment enters, create a 1/1
    /// colorless Hero creature token, then attach this Equipment to it."
    JobSelect,
    TotemArmor,
    Bestow(BestowCost),

    // Graveyard
    Embalm(EmbalmCost),
    Eternalize(EternalizeCost),

    // Token / counter
    Fading(u32),
    Vanishing(u32),

    // Parameterized keywords with ManaCost
    Protection(ProtectionTarget),
    Kicker(ManaCost),
    Cycling(CyclingCost),
    Flashback(FlashbackCost),
    Ward(WardCost),
    Equip(ManaCost),
    Landwalk(String),
    Rampage(u32),
    Absorb(u32),
    /// CR 702.122 (Crew) + CR 602.5b: `power` is the total power required to
    /// crew; `once_per_turn` carries an optional "Activate only once each turn"
    /// restriction (`Some(ActivationRestriction::OnlyOnceEachTurn)`), or `None`
    /// for the unrestricted default. Boxed to break the
    /// `Keyword → ActivationRestriction → ParsedCondition → Keyword` size cycle
    /// (`ParsedCondition::SourceLacksKeyword` holds a `Keyword` by value).
    Crew {
        power: u32,
        once_per_turn: Option<Box<ActivationRestriction>>,
    },
    /// CR 702.124: Partner and its variant keywords for co-commander pairing.
    Partner(PartnerType),
    /// CR 702.139: Companion — deckbuilding restriction that allows this card
    /// to be declared as a companion from outside the game.
    Companion(CompanionCondition),
    Ninjutsu(ManaCost),
    /// CR 702.49d: Commander ninjutsu — activate from hand or command zone.
    CommanderNinjutsu(ManaCost),

    // Additional common keywords with ManaCost
    Prowl(ManaCost),
    Morph(ManaCost),
    Megamorph(ManaCost),
    /// CR 702.187b: Mayhem {cost} — cast from graveyard if you discarded this
    /// card this turn, paying the mayhem cost rather than its mana cost.
    Mayhem(ManaCost),
    Madness(ManaCost),
    /// CR 702.94a: Miracle {cost} — static ability linked (CR 603.11) to a
    /// triggered ability. Static: "You may reveal this card from your hand as
    /// you draw it if it's the first card you've drawn this turn." Linked
    /// trigger: "When you reveal this card this way, you may cast it by paying
    /// [cost] rather than its mana cost." Runtime support: draw event populates
    /// `first_card_drawn_this_turn`; on that event a `WaitingFor::MiracleReveal`
    /// prompt is offered for miracle-keyworded cards. Casting uses
    /// `CastingVariant::Miracle` with the miracle mana cost.
    Miracle(ManaCost),
    Dash(ManaCost),
    /// CR 702.119a-c: Emerge is an alternative cost paid by sacrificing a
    /// creature and reducing the emerge cost by that creature's mana value.
    Emerge(ManaCost),
    /// CR 702.138a: Escape — cast from graveyard for an alternative cost. The
    /// compound escape cost (mana sub-cost plus one or more exile sub-costs) is
    /// modeled by `EscapeCost` and split at runtime by
    /// `split_escape_cost_components` (casting.rs): the mana portion is paid via
    /// the normal mana flow and the residual exile sub-cost(s) via
    /// `pay_additional_cost`.
    Escape(EscapeCost),
    /// CR 702.180: Harmonize {cost} — cast from graveyard for harmonize cost,
    /// tap up to one creature to reduce cost by its power, exile on resolution.
    Harmonize(ManaCost),
    /// CR 702.74a + CR 118.9: see `EvokeCost` for the mana / non-mana split.
    /// Pure-mana evoke (Lorwyn cycle) is `EvokeCost::Mana`; MH2 Incarnations
    /// (Solitude et al.) carry `EvokeCost::NonMana(AbilityCost::Exile { .. })`.
    Evoke(EvokeCost),
    Foretell(ManaCost),
    Mutate(ManaCost),
    Disturb(ManaCost),
    Disguise(ManaCost),
    Blitz(ManaCost),
    Overload(ManaCost),
    Spectacle(ManaCost),
    Surge(ManaCost),
    Encore(ManaCost),
    /// CR 702.27a: Buyback — optional additional cost; if paid, the spell returns
    /// to its owner's hand instead of the graveyard as it resolves.
    Buyback(BuybackCost),
    /// CR 702.153a: Casualty N — as an additional cost, you may sacrifice a creature
    /// with power N or greater. When you do, copy this spell.
    Casualty(u32),
    /// CR 702.30a: see `EchoCost` for the mana / non-mana split. Urza-block /
    /// errata cards (e.g. Orcish Hellraiser "Echo {R}") use `EchoCost::Mana`;
    /// "Echo—Discard a card" cards (Rakdos Headliner, Deepcavern Imp) carry
    /// `EchoCost::NonMana(AbilityCost::Discard { .. })`.
    Echo(EchoCost),
    /// CR 702.42a: Entwine — pay additional cost to choose all modes of a modal spell.
    Entwine(ManaCost),
    Outlast(ManaCost),
    Scavenge(ManaCost),
    /// CR 702.77a: Reinforce N—[cost] means "[Cost], Discard this card:
    /// Put N +1/+1 counters on target creature."
    Reinforce {
        count: u32,
        cost: ManaCost,
    },
    Fortify(ManaCost),
    /// CR 702.160a: Prototype — a player may cast this spell prototyped; if
    /// they do, the alternative power, toughness, and mana cost characteristics
    /// are used.
    Prototype {
        cost: ManaCost,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        power: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        toughness: Option<i32>,
    },
    Plot(ManaCost),
    /// CR 702.167a/b: Craft with [materials] [cost] — an activated ability
    /// "[Cost], Exile this permanent, Exile [materials] from among permanents
    /// you control and/or cards in your graveyard: Return this card to the
    /// battlefield transformed under its owner's control. Activate only as a
    /// sorcery." `materials` is the typed object class to exile (CR 702.167b:
    /// a bare type/subtype matches permanents on the battlefield OR cards in a
    /// graveyard); `count` is the exact/minimum material-count requirement.
    Craft {
        cost: ManaCost,
        materials: TargetFilter,
        #[serde(default)]
        count: CostObjectCount,
    },
    Offspring(ManaCost),
    /// CR 702.176a: Impending N—{cost} — alternative cast that enters with
    /// `counters` time counters and is not a creature until the last is removed.
    /// At the beginning of your end step the permanent loses one time counter.
    Impending {
        cost: ManaCost,
        counters: u32,
    },
    /// CR 702.87a: Level up is an activated ability that puts a level counter
    /// on this permanent. Activate only as a sorcery.
    LevelUp(ManaCost),

    /// CR 702.41a: Affinity for [type] — this spell costs {1} less for each [type] you control.
    Affinity(TypedFilter),

    /// CR 702.24a: cost paid per age counter on this permanent at the
    /// start of the controller's upkeep, or sacrifice. The typed
    /// `AbilityCost` lets the synthesis pipeline wire the
    /// cumulative-upkeep trigger uniformly across mana / life /
    /// sacrifice / disjunctive cost shapes.
    CumulativeUpkeep(AbilityCost),

    // Simple keywords (no params)
    Banding,
    /// CR 702.22: "Bands with other [quality]". The payload is the normalized
    /// quality text, currently used for subtype qualities such as "Wolf" and
    /// the historical "Legend" / "Legends" shape.
    BandsWithOther(String),
    Epic,
    Fuse,
    Gravestorm,
    Haunt,
    /// CR 702.74a: Hideaway N — look at top N cards, exile one face down, rest on bottom.
    Hideaway(u32),
    Improvise,
    Ingest,
    Melee,
    Mentor,
    Myriad,
    /// CR 702.39a: Provoke — "Whenever this creature attacks, you may have
    /// target creature defending player controls untap and block it this turn
    /// if able." Synthesized into an optional Attacks trigger (untap + the
    /// source-referential `Effect::ForceBlock`) in `database::synthesis`.
    Provoke,
    Rebound,
    Retrace,
    /// CR 702.60a: Ripple N — when you cast this spell, reveal the top N cards and
    /// cast same-named cards for free. `u32` is N.
    Ripple(u32),
    SplitSecond,
    Storm,
    /// CR 702.62a: Suspend N—{cost} — exile from hand with N time counters,
    /// remove one each upkeep, cast without paying when last removed.
    Suspend {
        count: u32,
        cost: ManaCost,
    },
    Totem,
    /// Warp {cost}: alternative casting cost. Cast from hand for warp cost,
    /// exile at next end step, then may cast from exile later.
    Warp(ManaCost),
    /// CR 702.190a: Sneak {cost} — alternative cost to cast this card from
    /// graveyard during the declare-blockers step by returning an unblocked
    /// attacker. CR 702.190b: the resulting permanent enters tapped and
    /// attacking the same defender.
    Sneak(ManaCost),
    /// CR 702.49 variant: Web-slinging — return a tapped creature to cast.
    WebSlinging(ManaCost),
    /// Mobilize N — when this creature attacks, create N 1/1 red Warrior tokens
    /// tapped and attacking, sacrifice them at end of combat.
    Mobilize(QuantityExpr),
    /// Gift — optional casting-time promise. If promised, opponent receives the gift
    /// at resolution before the spell's other effects.
    Gift(GiftKind),
    /// CR 701.57a: Discover N — exile from top until nonland card with MV ≤ N.
    Discover(u32),
    Spree,
    Ravenous,
    Daybound,
    Nightbound,
    Enlist,
    ReadAhead,
    Compleated,
    Conspire,
    Demonstrate,
    Dethrone,
    DoubleTeam,
    LivingMetal,
    Poisonous(u32),
    Bloodthirst(BloodthirstValue),
    Amplify(u32),
    Graft(u32),
    /// RUNTIME: synthesized as-enters replacement by
    /// `database/synthesis.rs::synthesize_devour` — a `ReplacementEvent::Moved`
    /// replacement on `SelfRef` whose execute chain is a ranged `Effect::Sacrifice`
    /// over the controller's creatures + a per-sacrifice `PutCounter(+1/+1)`
    /// sub-ability. CR 702.82a: Devour N — as it enters, you may sacrifice any
    /// number of creatures; it enters with N +1/+1 counters per sacrifice.
    Devour(u32),

    /// CR 702.164: Toxic N — when this creature deals combat damage to a player,
    /// that player gets N poison counters.
    Toxic(u32),
    /// CR 702.171a: Saddle N — tap creatures with total power N+ to saddle this Mount.
    Saddle(u32),
    /// CR 702.194a: Teamwork N — "As an additional cost to cast this spell, you
    /// may tap any number of creatures you control with total power N or more."
    /// The spell's body then references whether this optional cost was paid —
    /// "if this spell was cast using teamwork, ..." (CR 702.194b).
    ///
    /// Added to the printed Comprehensive Rules in the June 19, 2026 update
    /// (Marvel Super Heroes set keyword). Its tap-any-number-with-total-power-N
    /// cost is structurally identical to Crew (CR 702.122a) and Saddle
    /// (CR 702.171a); paying it follows the additional-cost rules CR 601.2b and
    /// CR 601.2f–h (CR 702.194a).
    ///
    /// Runtime: `database::synthesis::synthesize_teamwork` builds an
    /// `AdditionalCost::Optional { cost: TapCreatures { requirement:
    /// Aggregate { TotalPower, GE, N }, .. } }`. Paying it sets the spell's
    /// `additional_cost_paid` flag, which the body's `AdditionalCostPaid`
    /// condition reads (mirrors Conspire).
    Teamwork(u32),
    /// CR 702.46: Soulshift N — when this creature dies, return target Spirit card
    /// with mana value N or less from your graveyard to your hand.
    Soulshift(u32),
    /// CR 702.165: Backup N — when this creature enters, put N +1/+1 counters
    /// on target creature, which gains this creature's other abilities until EOT.
    Backup(u32),

    /// CR 702.157a: Squad {cost} — "As an additional cost to cast this spell,
    /// you may pay {cost} any number of times." "When this creature enters, if
    /// its squad cost was paid, create a token that's a copy of it for each time
    /// its squad cost was paid." (CR 702.157b: each instance triggers separately.)
    ///
    /// Runtime: `database::synthesis::synthesize_squad` builds a
    /// `AdditionalCost::Optional { repeatability: Repeatable }` additional-cost
    /// instance (origin: `AdditionalCostOrigin::Squad`) and an ETB copy trigger
    /// keyed on `QuantityRef::AdditionalCostPaymentCountFor { origin: Squad }`.
    /// `casting_costs::effective_squad_additional_cost_instances` surfaces the
    /// per-instance additional costs during casting. Fully wired.
    Squad(ManaCost),

    /// CR 702.29: Typecycling — "{subtype}cycling {cost}": discard this card and pay {cost}
    /// to search your library for a card with the specified subtype.
    Typecycling {
        cost: ManaCost,
        subtype: String,
    },

    /// Firebending N — produces N {R} when this creature attacks (Avatar crossover).
    Firebending(QuantityExpr),

    /// CR 702.47a: Splice onto [type] [cost] — reveal this card from hand and pay
    /// its splice cost as you cast a spell of the specified type to copy this
    /// card's text box onto that spell.
    Splice {
        subtype: String,
        cost: ManaCost,
    },
    /// CR 702.166a: Bargain — you may sacrifice an artifact, enchantment, or token
    /// as an additional cost to cast this spell.
    Bargain,
    /// CR 702.44a: Sunburst — as an object enters the battlefield as a resolving
    /// spell, it enters with a +1/+1 counter (if entering as a creature) or a
    /// charge counter (otherwise) for each color of mana spent to cast it. Wired
    /// at runtime by `synthesize_sunburst` as an ETB-counter replacement whose
    /// count is the distinct-colors-spent metric. Per CR 702.44d each instance
    /// works separately.
    Sunburst,
    /// CR 702.72a: Champion a [type] — exile another object of the specified
    /// type you control or sacrifice this permanent when it enters; return the
    /// exiled card when this leaves. Wired at build time by
    /// `synthesize_champion`; CR 702.72b makes the two abilities linked.
    Champion(String),
    /// CR 702.149a: Training — whenever this creature attacks with another creature
    /// with greater power, put a +1/+1 counter on this creature.
    Training,
    /// CR 702.132a: Assist — another player can help pay the generic mana cost of this spell.
    Assist,
    /// Acorn/Un-set keyword: Augment. Runtime host-combine semantics are not
    /// implemented; the keyword identity is used for characteristic filters
    /// such as "a card with augment".
    Augment,
    /// CR 702.127a: Aftermath allows casting this half of a split card only
    /// from a graveyard, and exiles the spell any time it leaves the stack if
    /// it was cast from a graveyard.
    Aftermath,
    /// CR 702.133a: Jump-start — cast from graveyard by discarding a card, then exile.
    JumpStart,
    /// CR 702.99a: Cipher — exile this spell encoded on a creature you control;
    /// whenever that creature deals combat damage to a player, cast a copy.
    Cipher,
    /// CR 702.53a: Transmute {cost} — "[Cost], Discard this card: Search your
    /// library for a card with the same mana value as the discarded card, reveal
    /// it, put it into your hand, then shuffle. Activate only as a sorcery."
    /// Runtime: `synthesize_transmute` (database/synthesis.rs).
    Transmute(ManaCost),
    /// CR 702.71a: Transfigure {cost} — "[Cost], Sacrifice this permanent: Search
    /// your library for a creature card with the same mana value as this permanent
    /// and put it onto the battlefield. Then shuffle your library. Activate only as
    /// a sorcery." Runtime: `synthesize_transfigure` (database/synthesis.rs).
    Transfigure(ManaCost),
    /// CR 702.120a: Escalate [cost] — additional cost for each mode chosen beyond the first
    /// on a modal spell.
    Escalate(AbilityCost),
    /// CR 702.59a: Recover {cost} — triggered ability: when a creature is put into your
    /// graveyard from the battlefield, you may pay {cost} to return this card from your
    /// graveyard to your hand; otherwise exile it.
    Recover(ManaCost),
    /// CR 702.148a: Cleave — alternative cost that removes bracketed text from Oracle text.
    Cleave(ManaCost),
    /// CR 702.125a: Undaunted — costs {1} less for each opponent.
    Undaunted,
    /// CR 702.xxx: Paradigm (Strixhaven) — a keyword on instants/sorceries.
    /// Reminder: "Then exile this spell. After you first resolve a spell with
    /// this name, you may cast a copy of it from exile without paying its
    /// mana cost at the beginning of each of your first main phases."
    /// Runtime hooks in `stack.rs` (first-resolution arming) and `turns.rs`
    /// (first-main-phase offer) carry the semantics. Assign when WotC
    /// publishes SOS CR update.
    Paradigm,

    /// CR 702.184a: Station — "Tap another untapped creature you control:
    /// Put a number of charge counters on this permanent equal to the tapped
    /// creature's power. Activate only as a sorcery." The keyword is fixed
    /// (no parameter) — the full semantics come from the rule text. Runtime
    /// activation is handled via `GameAction::ActivateStation`, not through
    /// the generic activated-ability dispatch.
    Station,

    /// RUNTIME: `database::synthesis::synthesize_replicate` — repeatable
    /// optional additional cost (`AdditionalCost::Optional { repeatability: Repeatable }`)
    /// plus a `SpellCast` trigger whose execute is
    /// `replicate_copy_ability_definition()` (a `CopySpell` with
    /// `repeat_for = AdditionalCostPaymentCount`).
    /// CR 702.56a: Replicate {cost} — additional-cost-on-cast copy
    /// mechanic. "As an additional cost to cast this spell, you may pay
    /// [cost] any number of times" + "When you cast this spell, if a
    /// replicate cost was paid for it, copy it for each time its
    /// replicate cost was paid. If the spell has any targets, you may
    /// choose new targets for any of the copies." Carries the per-copy
    /// mana cost.
    Replicate(ManaCost),

    /// CR 702.113a: Awaken N—{cost} — alternative cost that also puts N +1/+1
    /// counters on target land you control, animating it as a 0/0 Elemental
    /// creature with haste (it's still a land). Casting with awaken follows
    /// CR 601.2b and CR 601.2f–h. CR 702.113b: the land target exists only
    /// when the awaken cost was paid.
    ///
    /// Runtime: `CastingVariant::Awaken` + `casting::handle_awaken_cost_choice`
    /// substitutes the awaken mana cost for the printed cost and calls
    /// `effects::awaken::append_awaken_rider` to append the resolution rider
    /// (`PutCounter{N, land you control}` → `Animate{0/0 Elemental, Haste,
    /// Permanent}`) at the tail of the spell's ability tree. Fully wired.
    Awaken {
        count: u32,
        cost: ManaCost,
    },

    /// CR 702.163a: For Mirrodin! — Equipment-only triggered ability.
    /// "When this Equipment enters, create a 2/2 red Rebel creature
    /// token, then attach this Equipment to it." Bare keyword; ETB
    /// trigger semantics are synthesized in `database::synthesis`.
    ForMirrodin,

    /// CR 702.162a: More Than Meets the Eye {cost} — alternative cost
    /// (Transformers crossover). "You may cast this card converted by paying
    /// [cost] rather than its mana cost." Follows CR 701.28 (Convert) —
    /// the permanent enters the battlefield transformed (back face up).
    /// Alternative cost rules: CR 601.2b, CR 601.2f–h, CR 118.9.
    ///
    /// Runtime: `CastingVariant::MoreThanMeetsTheEye` + `casting::handle_mtmte_cost_choice`
    /// substitutes the MTMTE mana cost for the printed cost and routes through
    /// `continue_cast_with_alternative_spell_face`, which sets the stack spell
    /// to use back-face characteristics. On resolution, `enter_transformed`
    /// ZoneChange seed ensures the permanent enters back face up.
    /// `CastingVariant::restores_front_face_after_stack_exit` handles cleanup
    /// if the spell leaves the stack without resolving. Fully wired.
    MoreThanMeetsTheEye(ManaCost),

    /// CR 702.173a: Freerunning {cost} — alternative cost. "You may pay [cost]
    /// rather than pay this spell's mana cost if a player was dealt combat damage
    /// this turn by a creature that, at the time it dealt that damage, was an
    /// Assassin creature or a commander under your control." Follows CR 601.2b
    /// and CR 601.2f–h. A pure cost substitution — no resolution rider.
    ///
    /// Runtime: The eligibility predicate is tracked in
    /// `GameState::assassin_or_commander_dealt_combat_damage_this_turn`
    /// (a `HashSet<PlayerId>` seeded by `triggers::collect_pending_triggers`
    /// when an Assassin or commander deals combat damage, per CR 702.173a).
    /// The ledger is cleared at cleanup (CR 514) by `turns::run_cleanup`.
    /// `casting::casting_variant_candidates` checks the ledger to surface
    /// `CastingVariant::Freerunning`; `casting_costs` substitutes the
    /// Freerunning cost for the printed cost. Fully wired.
    Freerunning(ManaCost),

    /// CR 702.191a: Increment — spell-cast trigger synthesized in
    /// `database::synthesis::synthesize_increment`.
    Increment,

    /// Digital-only Specialize (Alchemy Horizons: Baldur's Gate) — not in the
    /// Comprehensive Rules; behavior follows MTG Arena. "{cost}, Discard a
    /// colored card or a card with a basic land subtype: This permanent becomes
    /// the matching color-specialized face permanently." Activated ability,
    /// sorcery speed. The keyword carries only the activation mana cost; the
    /// discard filter and color selection are built at synthesis time.
    ///
    /// Runtime: `database::synthesis::synthesize_specialize` builds a sorcery-
    /// speed `AbilityKind::Activated` with `Effect::Specialize` and
    /// `AbilityCost::Composite { Mana + Discard { filter: specialize_discard_filter } }`.
    /// `effects::specialize::resolve` reads the discarded card's LKI snapshot to
    /// determine eligible colors via `game::specialize::eligible_specialize_colors`,
    /// then either calls `specialize_permanent` directly (one option) or sets
    /// `WaitingFor::SpecializeColor` for the player to choose.
    /// `engine_resolution_choices` dispatches `GameAction::ChooseSpecializeColor`
    /// to complete the transformation. Fully wired.
    Specialize(ManaCost),

    /// CR 702.48a: "[Quality] offering" — as an additional cost to cast this
    /// spell, you may sacrifice a [Quality] permanent. If you do, this spell's
    /// total cost is reduced by the sacrificed permanent's mana cost (CR 702.48c),
    /// and you may cast this spell any time you could cast an instant. Carries
    /// the canonical subtype string (e.g. "Spirit", "Dragon"). Runtime behavior
    /// is fully wired: timing unlock, sacrifice selection, and cost reduction in
    /// `casting_costs.rs`.
    Offering(String),

    /// Fallback for unrecognized keywords.
    Unknown(String),
}

impl Keyword {
    /// CR 122.1b: Promote a bare `KeywordKind` (as stored on `CounterType::Keyword`)
    /// to the full `Keyword` enum for insertion into an object's keyword list.
    /// Every enumerated keyword-counter kind maps to a parameterless Keyword
    /// variant (keyword counters never carry parameters like Ward N or Afflict N),
    /// so this is lossless for the CR-enumerated set. Returns `None` for any
    /// `KeywordKind` whose full `Keyword` variant requires parameters we cannot
    /// synthesize from a bare counter.
    pub fn promote_keyword_kind(kind: KeywordKind) -> Option<Self> {
        Some(match kind {
            KeywordKind::Flying => Keyword::Flying,
            KeywordKind::FirstStrike => Keyword::FirstStrike,
            KeywordKind::DoubleStrike => Keyword::DoubleStrike,
            KeywordKind::Deathtouch => Keyword::Deathtouch,
            KeywordKind::Decayed => Keyword::Decayed,
            KeywordKind::Exalted => Keyword::Exalted,
            KeywordKind::Haste => Keyword::Haste,
            KeywordKind::Hexproof => Keyword::Hexproof,
            KeywordKind::Indestructible => Keyword::Indestructible,
            KeywordKind::Lifelink => Keyword::Lifelink,
            KeywordKind::Menace => Keyword::Menace,
            KeywordKind::Reach => Keyword::Reach,
            KeywordKind::Shadow => Keyword::Shadow,
            KeywordKind::Trample => Keyword::Trample,
            KeywordKind::Vigilance => Keyword::Vigilance,
            _ => return None,
        })
    }

    pub fn kind(&self) -> KeywordKind {
        match self {
            Keyword::Flying => KeywordKind::Flying,
            Keyword::FirstStrike => KeywordKind::FirstStrike,
            Keyword::DoubleStrike => KeywordKind::DoubleStrike,
            Keyword::Trample => KeywordKind::Trample,
            Keyword::TrampleOverPlaneswalkers => KeywordKind::TrampleOverPlaneswalkers,
            Keyword::Deathtouch => KeywordKind::Deathtouch,
            Keyword::Lifelink => KeywordKind::Lifelink,
            Keyword::Vigilance => KeywordKind::Vigilance,
            Keyword::Haste => KeywordKind::Haste,
            Keyword::Reach => KeywordKind::Reach,
            Keyword::Defender => KeywordKind::Defender,
            Keyword::Menace => KeywordKind::Menace,
            Keyword::Indestructible => KeywordKind::Indestructible,
            Keyword::Hexproof | Keyword::HexproofFrom(_) => KeywordKind::Hexproof,
            Keyword::Shroud => KeywordKind::Shroud,
            Keyword::Flash => KeywordKind::Flash,
            Keyword::Fear => KeywordKind::Fear,
            Keyword::Intimidate => KeywordKind::Intimidate,
            Keyword::Skulk => KeywordKind::Skulk,
            Keyword::Shadow => KeywordKind::Shadow,
            Keyword::Horsemanship => KeywordKind::Horsemanship,
            Keyword::Wither => KeywordKind::Wither,
            Keyword::Infect => KeywordKind::Infect,
            Keyword::Afflict(_) => KeywordKind::Afflict,
            Keyword::StartingIntensity(_) => KeywordKind::Unknown,
            Keyword::Prowess => KeywordKind::Prowess,
            Keyword::Undying => KeywordKind::Undying,
            Keyword::Persist => KeywordKind::Persist,
            Keyword::Cascade => KeywordKind::Cascade,
            Keyword::Exalted => KeywordKind::Exalted,
            Keyword::Flanking => KeywordKind::Flanking,
            Keyword::Evolve => KeywordKind::Evolve,
            Keyword::Extort => KeywordKind::Extort,
            Keyword::Exploit => KeywordKind::Exploit,
            Keyword::Explore => KeywordKind::Explore,
            Keyword::Ascend => KeywordKind::Ascend,
            Keyword::StartYourEngines => KeywordKind::StartYourEngines,
            Keyword::Dredge(_) => KeywordKind::Dredge,
            Keyword::Modular(_) => KeywordKind::Modular,
            Keyword::Renown(_) => KeywordKind::Renown,
            Keyword::Graft(_) => KeywordKind::Graft,
            Keyword::Fabricate(_) => KeywordKind::Fabricate,
            Keyword::Annihilator(_) => KeywordKind::Annihilator,
            Keyword::Bushido(_) => KeywordKind::Bushido,
            Keyword::Frenzy(_) => KeywordKind::Frenzy,
            Keyword::Tribute(_) => KeywordKind::Tribute,
            Keyword::Soulbond => KeywordKind::Soulbond,
            Keyword::BandsWithOther(_) => KeywordKind::BandsWithOther,
            Keyword::Unearth(_) => KeywordKind::Unearth,
            Keyword::Convoke => KeywordKind::Convoke,
            Keyword::Waterbend => KeywordKind::Waterbend,
            Keyword::Delve => KeywordKind::Delve,
            Keyword::Devoid => KeywordKind::Devoid,
            Keyword::Changeling => KeywordKind::Changeling,
            Keyword::Phasing => KeywordKind::Phasing,
            Keyword::Battlecry => KeywordKind::Battlecry,
            Keyword::Decayed => KeywordKind::Decayed,
            Keyword::Unleash => KeywordKind::Unleash,
            Keyword::Riot => KeywordKind::Riot,
            Keyword::Afterlife(_) => KeywordKind::Afterlife,
            Keyword::Enchant(_) => KeywordKind::Enchant,
            Keyword::EtbCounter { .. } => KeywordKind::EtbCounter,
            Keyword::Reconfigure(_) => KeywordKind::Reconfigure,
            Keyword::LivingWeapon => KeywordKind::LivingWeapon,
            Keyword::JobSelect => KeywordKind::JobSelect,
            Keyword::TotemArmor => KeywordKind::TotemArmor,
            Keyword::Bestow(_) => KeywordKind::Bestow,
            Keyword::Embalm(_) => KeywordKind::Embalm,
            Keyword::Eternalize(_) => KeywordKind::Eternalize,
            Keyword::Fading(_) => KeywordKind::Fading,
            Keyword::Vanishing(_) => KeywordKind::Vanishing,
            Keyword::Protection(_) => KeywordKind::Protection,
            Keyword::Kicker(_) => KeywordKind::Kicker,
            Keyword::Cycling(_) => KeywordKind::Cycling,
            Keyword::Typecycling { .. } => KeywordKind::Typecycling,
            Keyword::Flashback(_) => KeywordKind::Flashback,
            Keyword::Retrace => KeywordKind::Retrace,
            Keyword::Ward(_) => KeywordKind::Ward,
            Keyword::Equip(_) => KeywordKind::Equip,
            Keyword::Landwalk(_) => KeywordKind::Landwalk,
            Keyword::Rampage(_) => KeywordKind::Rampage,
            Keyword::Absorb(_) => KeywordKind::Absorb,
            Keyword::Crew { .. } => KeywordKind::Crew,
            Keyword::Partner(PartnerType::DoctorsCompanion) => KeywordKind::Doctor,
            Keyword::Partner(PartnerType::ChooseABackground) => KeywordKind::Background,
            Keyword::Partner(_) => KeywordKind::Partner,
            Keyword::Companion(_) => KeywordKind::Companion,
            Keyword::CommanderNinjutsu(_) => KeywordKind::CommanderNinjutsu,
            Keyword::Ninjutsu(_) => KeywordKind::Ninjutsu,
            Keyword::Sneak(_) => KeywordKind::Sneak,
            Keyword::Mutate(_) => KeywordKind::Mutate,
            Keyword::Escape(_) => KeywordKind::Escape,
            Keyword::Morph(_) => KeywordKind::Morph,
            Keyword::Megamorph(_) => KeywordKind::Megamorph,
            Keyword::Madness(_) => KeywordKind::Madness,
            // CR 702.168: Disguise — its own discriminant kind (mirrors Morph)
            // so `HasKeywordKind { Disguise }` selects face-up disguise creatures
            // rather than being folded into the shared `Unknown` catch-all.
            Keyword::Disguise(_) => KeywordKind::Disguise,
            Keyword::Mayhem(_) => KeywordKind::Mayhem,
            Keyword::Suspend { .. } => KeywordKind::Suspend,
            Keyword::Blitz(_) => KeywordKind::Blitz,
            Keyword::Disturb(_) => KeywordKind::Disturb,
            Keyword::Foretell(_) => KeywordKind::Foretell,
            Keyword::Miracle(_) => KeywordKind::Miracle,
            Keyword::Plot(_) => KeywordKind::Plot,
            Keyword::Gift(_) => KeywordKind::Gift,
            Keyword::Outlast(_) => KeywordKind::Outlast,
            Keyword::Dash(_) => KeywordKind::Dash,
            Keyword::Craft { .. } => KeywordKind::Craft,
            Keyword::Harmonize(_) => KeywordKind::Harmonize,
            Keyword::Warp(_) => KeywordKind::Warp,
            Keyword::Devour(_) => KeywordKind::Devour,
            Keyword::Offspring(_) => KeywordKind::Offspring,
            Keyword::Splice { .. } => KeywordKind::Splice,
            Keyword::Bargain => KeywordKind::Bargain,
            Keyword::Sunburst => KeywordKind::Sunburst,
            Keyword::Champion(_) => KeywordKind::Champion,
            Keyword::Training => KeywordKind::Training,
            Keyword::Assist => KeywordKind::Assist,
            Keyword::Augment => KeywordKind::Augment,
            Keyword::Aftermath => KeywordKind::Aftermath,
            Keyword::JumpStart => KeywordKind::JumpStart,
            Keyword::Cipher => KeywordKind::Cipher,
            Keyword::Transmute(_) => KeywordKind::Transmute,
            Keyword::Transfigure(_) => KeywordKind::Transfigure,
            Keyword::Cleave(_) => KeywordKind::Cleave,
            Keyword::Undaunted => KeywordKind::Undaunted,
            Keyword::Station => KeywordKind::Station,
            Keyword::Paradigm => KeywordKind::Paradigm,
            Keyword::Replicate(_) => KeywordKind::Replicate,
            Keyword::Awaken { .. } => KeywordKind::Awaken,
            Keyword::ForMirrodin => KeywordKind::ForMirrodin,
            Keyword::MoreThanMeetsTheEye(_) => KeywordKind::MoreThanMeetsTheEye,
            Keyword::Freerunning(_) => KeywordKind::Freerunning,
            Keyword::Increment => KeywordKind::Increment,
            Keyword::Firebending(_) => KeywordKind::Firebending,
            Keyword::Specialize(_) => KeywordKind::Specialize,
            Keyword::Offering(_) => KeywordKind::Offering,
            Keyword::Escalate(_) => KeywordKind::Escalate,
            Keyword::Recover(_) => KeywordKind::Recover,
            // CR 702.102: Fuse — the runtime cast layer reads this kind to offer
            // the fuse casting variant for split cards in hand.
            Keyword::Fuse => KeywordKind::Fuse,
            Keyword::Unknown(_) => KeywordKind::Unknown,
            // Variants whose KeywordKind axis is currently the catch-all `Unknown`
            // because the AI/coverage layer that consumes `KeywordKind` does not
            // need to distinguish them yet. Listed exhaustively so that adding a
            // new `Keyword::*` variant is a compile error here — at which point
            // the author either adds a matching `KeywordKind::*` variant or maps
            // the new arm to `Unknown` explicitly. Do NOT replace this list with
            // a `_ => Unknown` wildcard; that defeats the whole point.
            Keyword::Affinity(_)
            | Keyword::Amplify(_)
            | Keyword::Backup(_)
            | Keyword::Banding
            | Keyword::Bloodthirst(_)
            | Keyword::Buyback(_)
            | Keyword::Casualty(_)
            | Keyword::Compleated
            | Keyword::Conspire
            | Keyword::CumulativeUpkeep(_)
            | Keyword::Daybound
            | Keyword::Demonstrate
            | Keyword::Dethrone
            | Keyword::Discover(_)
            | Keyword::DoubleTeam
            | Keyword::Echo(_)
            | Keyword::Emerge(_)
            | Keyword::Encore(_)
            | Keyword::Enlist
            | Keyword::Entwine(_)
            | Keyword::Epic
            | Keyword::Evoke(_)
            | Keyword::Fortify(_)
            | Keyword::Gravestorm
            | Keyword::Haunt
            | Keyword::Hideaway(_)
            | Keyword::Impending { .. }
            | Keyword::Improvise
            | Keyword::Ingest
            | Keyword::LevelUp(_)
            | Keyword::LivingMetal
            | Keyword::Melee
            | Keyword::Mentor
            | Keyword::Mobilize(_)
            | Keyword::Myriad
            | Keyword::Nightbound
            | Keyword::Overload(_)
            | Keyword::Poisonous(_)
            | Keyword::Prototype { .. }
            | Keyword::Provoke
            | Keyword::Prowl(_)
            | Keyword::Ravenous
            | Keyword::ReadAhead
            | Keyword::Rebound
            | Keyword::Reinforce { .. }
            | Keyword::Ripple(_)
            | Keyword::Saddle(_)
            | Keyword::Teamwork(_)
            | Keyword::Scavenge(_)
            | Keyword::Soulshift(_)
            | Keyword::Spectacle(_)
            | Keyword::SplitSecond
            | Keyword::Spree
            | Keyword::Squad(_)
            | Keyword::Storm
            | Keyword::Surge(_)
            | Keyword::Totem
            | Keyword::Toxic(_)
            | Keyword::WebSlinging(_) => KeywordKind::Unknown,
        }
    }

    /// CR 601.2f + CR 707.2: Keywords that only function while a player is
    /// casting a spell. A token created by `CopyTokenOf` was not cast, so these
    /// keywords are inert on the copy and are stripped at creation time so the
    /// token does not display cast-only reminders (Offspring, Kicker, etc.).
    ///
    /// Maintenance note: every new alternative-cost or additional-cost casting
    /// keyword added to `Keyword` must also be added here, or token copies of
    /// permanents carrying it re-introduce the inert-reminder display bug.
    ///
    /// Deliberately excluded: `Prototype` — CR 718.2a makes the alternative
    /// characteristics part of the object's copiable values and CR 718.3d
    /// treats a copy of a prototyped permanent as itself prototyped, so the
    /// keyword must survive copying.
    pub fn is_spell_casting_only(&self) -> bool {
        matches!(
            self,
            Keyword::Offspring(_)
                | Keyword::Kicker(_)
                | Keyword::Buyback(_)
                | Keyword::Flashback(_)
                | Keyword::Retrace
                | Keyword::Blitz(_)
                | Keyword::Dash(_)
                | Keyword::Sneak(_)
                | Keyword::Ninjutsu(_)
                | Keyword::Mutate(_)
                | Keyword::Escape(_)
                | Keyword::Foretell(_)
                | Keyword::Plot(_)
                | Keyword::Miracle(_)
                | Keyword::Gift(_)
                | Keyword::Bargain
                | Keyword::Replicate(_)
                | Keyword::Squad(_)
                | Keyword::Conspire
                | Keyword::Harmonize(_)
                | Keyword::Casualty(_)
                | Keyword::Aftermath
                | Keyword::Disturb(_)
                | Keyword::JumpStart
                | Keyword::Cipher
                | Keyword::Evoke(_)
                | Keyword::Emerge(_)
                | Keyword::Bestow(_)
                | Keyword::Madness(_)
                | Keyword::Suspend { .. }
                | Keyword::Morph(_)
                | Keyword::Megamorph(_)
                | Keyword::Disguise(_)
                | Keyword::Spectacle(_)
                | Keyword::Surge(_)
                | Keyword::Overload(_)
                | Keyword::Splice { .. }
                | Keyword::Escalate(_)
                | Keyword::Prowl(_)
                | Keyword::Impending { .. }
                | Keyword::MoreThanMeetsTheEye(_)
                | Keyword::Freerunning(_)
                // CR 601.2b/f: Teamwork is an optional additional cast cost; it
                // is inert on a non-cast token copy.
                | Keyword::Teamwork(_)
        )
    }

    /// CR 113.2c: keywords whose multiple instances each function separately AND
    /// are printed in Oracle text as repeated bare words, so every printed
    /// occurrence must survive as a distinct `Keyword` on the card face (MTGJSON
    /// dedupes them to one). Two distinct runtime consumption shapes both rely on
    /// the surviving instance count:
    ///
    /// - Cascade (CR 702.85c: each instance triggers separately) / Storm
    ///   (CR 702.40b: each instance triggers separately) — stack-functioning
    ///   triggered abilities whose instance count is consumed by `for _ in 0..count`
    ///   loops in `game/triggers.rs`.
    /// - Myriad (CR 702.116a: a triggered ability; CR 702.116b: each instance
    ///   triggers separately) / Increment (CR 702.191a: a triggered ability;
    ///   CR 702.191b: each instance triggers separately) / Provoke (CR 702.39b:
    ///   each instance triggers separately) / Exalted (CR 702.83a: a triggered ability;
    ///   per-instance multiplicity grounded in the general CR 113.2c rule, since
    ///   CR 702.83 has no card-specific multiplicity clause) — one trigger is
    ///   installed per face `Keyword` instance by
    ///   `KeywordTriggerInstaller::install_matching`, invoked from `synthesize_all`.
    ///
    /// Returns `false` for everything else, including:
    /// - CR 702.44d Sunburst — also "works separately" per instance, but it is
    ///   an as-enters STATIC ability, so its per-instance multiplicity is
    ///   realized by `synthesize_sunburst` (one ETB-counter replacement per
    ///   `Keyword::Sunburst`), not by the runtime trigger installer this
    ///   predicate gates. Out of this class for that reason.
    /// - Prowess — runtime presence is a boolean `has_prowess` check, so counting
    ///   instances would be inert (separate deeper bug, not addressed here).
    pub fn instances_function_separately(&self) -> bool {
        matches!(
            self,
            Keyword::Cascade
                | Keyword::Storm
                | Keyword::Myriad
                | Keyword::Increment
                | Keyword::Provoke
                | Keyword::Exalted
                | Keyword::DoubleTeam
        )
    }

    /// CR 702.164b: Keywords whose multiple instances SUM their parameter values
    /// into a single aggregate (e.g. a creature's total toxic value), rather than
    /// collapsing identical instances. When such a keyword is granted on top of an
    /// identical printed instance, BOTH must remain on the keyword list so the
    /// aggregate reader counts every copy. Distinct from `instances_function_separately`
    /// (which gates per-instance trigger installation — a different semantic axis).
    /// Conservative/CR-driven: only Toxic sums today (CR 702.164b). Protection
    /// (CR 702.16g), Ward, Annihilator, Afflict, Frenzy do NOT sum — they keep
    /// deduping identical instances. Add any future "sum of all N" keyword here.
    ///
    /// Out of scope (intentionally not gated by this predicate): cast-time spell
    /// keyword merge (`casting.rs` `upsert_keyword_by_kind`/`merge_spell_keyword` —
    /// Toxic is inert at cast time) and the layers `AddDynamicKeyword` arm
    /// (`DynamicKeywordKind` is only Annihilator/Modular, never Toxic).
    pub fn sums_across_instances(&self) -> bool {
        matches!(self, Keyword::Toxic(_))
    }

    /// CR 613.7: When multiple effects grant the same single-authoritative-value
    /// keyword (one whose payload is the *current* effective value, not an
    /// accumulating count), the most recently applied grant must replace any
    /// earlier instance of the same kind rather than coexist with it — otherwise
    /// readers that pick "the first match" (e.g. `find_map`) can read a stale
    /// value while a different one is intended to be authoritative. Crew/Saddle
    /// (CR 702.122/702.171, vehicle/mount crew-power) and Enchant (CR 702.5a,
    /// an Aura's current legal-attachment filter, reachable via
    /// `AddKeyword{Enchant(_)}` from `install_aura_continuous_effect`) are the
    /// currently known members. Contrast `sums_across_instances` (Toxic, which
    /// accumulates) and the default (Protection/Ward/Annihilator, which coexist
    /// as separate instances per CR 702.16g).
    pub fn overrides_same_kind_on_grant(&self) -> bool {
        matches!(
            self,
            Keyword::Crew { .. } | Keyword::Enchant(_) | Keyword::Saddle(_)
        )
    }
}

/// Capitalize the first character of a string (for type name normalization).
fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// CR 702.139: Parse a companion condition string from Oracle text into a typed enum.
/// Handles the 10 known companion cards by matching on distinctive phrases.
fn parse_companion_condition(text: &str) -> CompanionCondition {
    let lower = text.to_lowercase();

    if lower.contains("even mana value") || lower.contains("even converted mana cost") {
        CompanionCondition::EvenManaValues
    } else if lower.contains("odd mana value") || lower.contains("odd converted mana cost") {
        CompanionCondition::OddManaValues
    } else if lower.contains("no two nonland") || lower.contains("singleton") {
        CompanionCondition::Singleton
    } else if lower.contains("share a card type") {
        CompanionCondition::SharedCardType
    } else if lower.contains("has an activated ability") {
        CompanionCondition::PermanentsHaveActivatedAbilities
    } else if lower.contains("more than one of the same mana symbol") {
        CompanionCondition::NoRepeatedManaSymbols
    } else if lower.contains("twenty or more cards") || lower.contains("80") {
        CompanionCondition::MinDeckSizeOver(20)
    } else if lower.contains("mana value 3 or greater") {
        CompanionCondition::MinManaValue(3)
    } else if lower.contains("mana value 2 or less") {
        CompanionCondition::MaxPermanentManaValue(2)
    } else if lower.contains("cat") && lower.contains("elemental") {
        // Kaheera: extract subtypes from the condition text
        let subtypes = extract_companion_subtypes(&lower);
        CompanionCondition::CreatureTypeRestriction(subtypes)
    } else {
        // Fallback: attempt to identify by partial matching
        CompanionCondition::SharedCardType
    }
}

/// Extract creature subtypes from Kaheera-style companion condition text.
fn extract_companion_subtypes(text: &str) -> Vec<String> {
    let known_types = ["cat", "elemental", "nightmare", "dinosaur", "beast"];
    known_types
        .iter()
        .filter(|t| text.contains(**t))
        .map(|t| {
            let mut c = t.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        })
        .collect()
}

/// CR 702.167b: Public re-export of the default craft materials filter (the
/// creature class) so external crates (the dormant `mtgish-import` converter)
/// and the keyword deserializers can request it without reaching into the
/// `pub(crate)` parser module. The single authority remains
/// `parser::oracle_keyword::craft_materials_filter`.
pub fn craft_materials_default() -> TargetFilter {
    crate::parser::oracle_keyword::craft_materials_default()
}

/// Parse a mana cost string into ManaCost. Supports both MTGJSON format ({1}{W})
/// and simple format (1W, 2, W, etc.) for keyword parameters.
fn parse_keyword_mana_cost(s: &str) -> ManaCost {
    // If it contains braces, delegate to the MTGJSON parser
    if s.contains('{') {
        return crate::database::mtgjson::parse_mtgjson_mana_cost(s);
    }

    // Simple format: try to parse as pure generic (e.g. "3"), or as mana symbols
    let s = s.trim();
    if s.is_empty() {
        return ManaCost::zero();
    }

    let mut generic: u32 = 0;
    let mut shards = Vec::new();
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            'W' => shards.push(crate::types::mana::ManaCostShard::White),
            'U' => shards.push(crate::types::mana::ManaCostShard::Blue),
            'B' => shards.push(crate::types::mana::ManaCostShard::Black),
            'R' => shards.push(crate::types::mana::ManaCostShard::Red),
            'G' => shards.push(crate::types::mana::ManaCostShard::Green),
            'C' => shards.push(crate::types::mana::ManaCostShard::Colorless),
            'X' => shards.push(crate::types::mana::ManaCostShard::X),
            '0'..='9' => {
                // Collect consecutive digits
                let mut num_str = String::new();
                num_str.push(c);
                while let Some(&next) = chars.peek() {
                    if next.is_ascii_digit() {
                        num_str.push(chars.next().unwrap());
                    } else {
                        break;
                    }
                }
                generic += num_str.parse::<u32>().unwrap_or(0);
            }
            _ => {} // Ignore unrecognized characters
        }
    }

    ManaCost::Cost { shards, generic }
}

/// CR 702.41a: Parse the text from "Affinity for [text]" into the permanents
/// counted for the cost reduction.
/// CR 205.2: Map a single (possibly plural) card-type word to its `TypeFilter`.
/// Used by `parse_affinity_type` to recognize "affinity for planeswalkers" /
/// "affinity for artifact creatures" as card types rather than subtypes. Returns
/// `None` for any word that is not a card type (e.g. a creature subtype), so a
/// multi-word phrase only becomes a type conjunction when every word is a type.
fn affinity_card_type_word(word: &str) -> Option<super::ability::TypeFilter> {
    use super::ability::TypeFilter;
    // Singularize: "sorceries" → "sorcery"; otherwise strip a trailing plural 's'.
    let singular = word
        .strip_suffix("ies")
        .map(|stem| format!("{stem}y"))
        .unwrap_or_else(|| word.strip_suffix('s').unwrap_or(word).to_string());
    Some(match singular.as_str() {
        "artifact" => TypeFilter::Artifact,
        "creature" => TypeFilter::Creature,
        "land" => TypeFilter::Land,
        "enchantment" => TypeFilter::Enchantment,
        "planeswalker" => TypeFilter::Planeswalker,
        "instant" => TypeFilter::Instant,
        "sorcery" => TypeFilter::Sorcery,
        "battle" => TypeFilter::Battle,
        _ => return None,
    })
}

fn parse_affinity_type(s: &str) -> Option<TypedFilter> {
    use super::ability::TypeFilter;
    // MTGJSON provides "Affinity for artifacts" — FromStr splits on first ':' giving
    // param "for artifacts". Strip the "for " prefix if present.
    let s = s.strip_prefix("for ").unwrap_or(s);
    let lower = s.to_ascii_lowercase();
    match lower.as_str() {
        "artifacts" => Some(TypedFilter::new(TypeFilter::Artifact)),
        "creatures" => Some(TypedFilter::creature()),
        "lands" => Some(TypedFilter::land()),
        "enchantments" => Some(TypedFilter::new(TypeFilter::Enchantment)),
        "tokens" => Some(TypedFilter::creature().properties(vec![FilterProp::Token])),
        "equipment" => {
            Some(TypedFilter::new(TypeFilter::Artifact).subtype("Equipment".to_string()))
        }
        _ => {
            // CR 205.2 + CR 702.41a: "Affinity for <card type(s)>" — a card type
            // ("planeswalkers", Tomik, Wielder of Law) or a type combination
            // ("artifact creatures", Urza, Chief Artificer). Tokenize and map each
            // singularized word to a card type; if EVERY word is a card type, build
            // a conjunctive type filter (CR 205: all type constraints must match),
            // not a bogus multi-word subtype. Otherwise fall through to subtype.
            if let Some(types) = lower
                .split_whitespace()
                .map(affinity_card_type_word)
                .collect::<Option<Vec<TypeFilter>>>()
            {
                if let Some((first, rest)) = types.split_first() {
                    let mut filter = TypedFilter::new(first.clone());
                    for ty in rest {
                        filter = filter.with_type(ty.clone());
                    }
                    return Some(filter);
                }
            }
            // CR 702.41a + CR 205.3: otherwise the text is a subtype. Unknown names
            // are subtypes, but not always land subtypes ("Daleks", "Cats",
            // "Birds"). Keep this as a bare subtype constraint so it covers land,
            // artifact, enchantment, and creature subtype affinity without adding a
            // false type conjunct.
            let capitalized = format!("{}{}", &s[..1].to_uppercase(), &s[1..]);
            // Strip trailing 's' for plural subtype words (e.g., "Daleks" →
            // "Dalek", "Islands" → "Island"; "Plains" stays "Plains").
            let subtype = if capitalized.ends_with('s') && capitalized != "Plains" {
                capitalized[..capitalized.len() - 1].to_string()
            } else {
                capitalized
            };
            Some(TypedFilter::default().subtype(subtype))
        }
    }
}

/// CR 303.4a + CR 702.5a: Parse an `Enchant <text>` MTGJSON parameter into the
/// typed `TargetFilter` that scopes the Aura's legal target set. Zone phrases
/// like "in a graveyard" / "in your hand" resolve to `FilterProp::InZone` so
/// cast-time targeting (`game/casting.rs` Aura branch → `find_legal_targets`)
/// and the Aura SBA (`game/sba.rs::is_valid_attachment_target`) enumerate and
/// validate against the same predicate.
///
/// Class shape: `Enchant <type>? card? <zone>? <controller>?` plus the
/// `Enchant <player-base> <controller>?` (CR 702.5d) branch. All four object-
/// branch legs are independently optional — admits "creature card in a
/// graveyard" (Animate Dead), "instant card in a graveyard" (Spellweaver
/// Volute), and "card in your hand" (Don't Worry About It, no type leg).
///
/// Returns `None` for degenerate input (empty / unrecognized) so the
/// `FromStr` call site at the `Enchant:` arm can route through the existing
/// `Keyword::Unknown` fallthrough rather than silently dumping the raw text
/// into a `TypeFilter::Subtype` (the bug fixed by issue #537).
fn parse_enchant_target(s: &str) -> Option<TargetFilter> {
    use crate::parser::oracle_nom::enchant::{
        parse_enchant_attachment_qualifier, parse_enchant_controller_suffix,
        parse_enchant_player_base, parse_enchant_type_leg,
    };
    use crate::parser::oracle_nom::error::OracleResult;
    use crate::parser::oracle_nom::filter::parse_zone_filter;
    use nom::bytes::complete::tag;
    use nom::combinator::opt;
    use nom::sequence::preceded;
    use nom::Parser;

    // Typed sub-combinator: optional " card" word with optional leading space
    // (the space is only required when a preceding type leg was consumed).
    fn parse_card_word(input: &str) -> OracleResult<'_, ()> {
        let (rest, _) = opt(tag::<
            &str,
            &str,
            crate::parser::oracle_nom::error::OracleError<'_>,
        >(" "))
        .parse(input)?;
        let (rest, _) =
            tag::<&str, &str, crate::parser::oracle_nom::error::OracleError<'_>>("card")
                .parse(rest)?;
        Ok((rest, ()))
    }

    // Typed sub-combinator: " " + zone phrase.
    fn parse_leading_zone(input: &str) -> OracleResult<'_, crate::types::zones::Zone> {
        preceded(
            tag::<&str, &str, crate::parser::oracle_nom::error::OracleError<'_>>(" "),
            parse_zone_filter,
        )
        .parse(input)
    }

    let lower = s.trim().to_ascii_lowercase();
    let input = lower.as_str();

    // CR 702.5d: Player-axis Aura. The player-base combinator yields the
    // typed `TargetFilter` directly; an optional `you control` clause folds
    // onto the typed-filter branch (`Enchant player you control`).
    if let Ok((rest, base)) = parse_enchant_player_base(input) {
        let (rest, controller) = opt(parse_enchant_controller_suffix).parse(rest).ok()?;
        if !rest.trim().is_empty() {
            return None;
        }
        return Some(match (base, controller) {
            // "Enchant player you control" / "Enchant player an opponent controls"
            // narrows the player axis via the controller-on-typed branch.
            (TargetFilter::Player, Some(c)) => {
                TargetFilter::Typed(TypedFilter::default().controller(c))
            }
            // Bare "Enchant player" → any player at the table.
            (TargetFilter::Player, None) => TargetFilter::Player,
            // "Enchant opponent" already encodes the opponent-controller
            // restriction in its typed filter; an explicit controller suffix
            // here is grammatically odd, but defer to the more specific
            // clause if present.
            (_, Some(c)) => TargetFilter::Typed(TypedFilter::default().controller(c)),
            (base, None) => base,
        });
    }

    // CR 303.4a + CR 702.5a: Object-axis Aura.
    //   <type>? <" card">? <" <zone>">? <controller>?
    // Each leg is independently optional so the class covers:
    //   "creature"                      (Pacifism)
    //   "creature you control"          (Lifelink etc.)
    //   "creature card in a graveyard"  (Animate Dead, Dance of the Dead)
    //   "instant card in a graveyard"   (Spellweaver Volute)
    //   "card in your hand"             (Don't Worry About It — no type leg)
    let (rest, type_filter) = opt(parse_enchant_type_leg).parse(input).ok()?;
    let (rest, _card_word) = opt(parse_card_word).parse(rest).ok()?;
    let (rest, zone) = opt(parse_leading_zone).parse(rest).ok()?;
    let (rest, controller) = opt(parse_enchant_controller_suffix).parse(rest).ok()?;
    // CR 303.4 + CR 702.5a: Optional trailing attachment qualifier — "with
    // another Aura attached to it" (Daybreak Coronet) narrows the legal target
    // set to objects that already carry an attachment of the named kind.
    let (rest, attachment) = opt(parse_enchant_attachment_qualifier).parse(rest).ok()?;
    let (rest, without_keyword) =
        match crate::parser::oracle_target::parse_without_keyword_suffix(rest) {
            Some((props, consumed)) => (&rest[consumed..], props),
            None => (rest, Vec::new()),
        };
    if !rest.trim().is_empty() {
        return None;
    }
    // Reject fully empty input — every other degenerate variant lacks a type
    // word AND a zone word AND a controller, so it cannot be a meaningful
    // enchant clause. (An attachment qualifier cannot stand alone: its leading
    // space requires a preceding type leg, so it never reaches this guard.)
    if type_filter.is_none() && zone.is_none() && controller.is_none() {
        return None;
    }

    // CR 303.4a: When the type leg is absent (Don't Worry About It), the
    // class is "any card", encoded as `TypeFilter::Card`.
    let mut props = Vec::new();
    if let Some(z) = zone {
        props.push(FilterProp::InZone { zone: z });
    }
    if let Some(prop) = attachment {
        props.push(prop);
    }
    props.extend(without_keyword);
    let mut filter = TypedFilter::new(type_filter.unwrap_or(TypeFilter::Card));
    if !props.is_empty() {
        filter = filter.properties(props);
    }
    if let Some(c) = controller {
        filter = filter.controller(c);
    }
    Some(TargetFilter::Typed(filter))
}

/// Parse an EtbCounter parameter string (e.g., "P1P1:1") into counter_type and count.
fn parse_etb_counter(s: &str) -> (CounterType, u32) {
    if let Some(idx) = s.rfind(':') {
        let counter_type = parse_counter_type(&s[..idx]);
        let count = s[idx + 1..].parse::<u32>().unwrap_or(1);
        (counter_type, count)
    } else {
        (parse_counter_type(s), 1)
    }
}

fn parse_bloodthirst_value(s: &str) -> BloodthirstValue {
    if s.trim().eq_ignore_ascii_case("x") {
        BloodthirstValue::X
    } else {
        BloodthirstValue::Fixed(s.trim().parse().unwrap_or(1))
    }
}

/// CR 608.2d: User-facing label for a `Keyword` option in a
/// `ChoiceType::Keyword` prompt, and the canonical Display rendering for any
/// engine-side string format that surfaces a keyword to the player.
///
/// Typed match per the workspace's "no string-matching dispatch" rule; the
/// engine owns this rendering because the frontend must not derive game
/// text from raw enum names. Only the keyword shapes that today's
/// `Action::ChooseACheckableAbility` emission can produce are enumerated
/// explicitly; every other variant falls through to the catch-all
/// `Debug`-derived form below. That fallback is unambiguous (no two
/// `Keyword` variants share Debug output) but not necessarily pretty —
/// new variants surfaced through a choice prompt or any other Display
/// site should get an explicit arm here so the rendered label matches
/// the canonical Oracle spelling.
impl fmt::Display for Keyword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Keyword::FirstStrike => write!(f, "First Strike"),
            Keyword::DoubleStrike => write!(f, "Double Strike"),
            // CR 702.14a: Landwalk surfaces in Oracle text as "[type]walk" when
            // [type] is a land subtype (Plainswalk, Desertwalk, Gatewalk, ...).
            // When [type] is a supertype or non-land card type, the canonical
            // phrasing is "[type] landwalk" (Legendary landwalk, Nonbasic landwalk,
            // Snow landwalk, artifact landwalk). Capitalize "Landwalk" in the
            // composed form so the rendered label matches the title-case style
            // used by the rest of this Display impl (e.g. "First Strike").
            Keyword::Landwalk(subtype) => match subtype.as_str() {
                "Plains" => write!(f, "Plainswalk"),
                "Island" => write!(f, "Islandwalk"),
                "Swamp" => write!(f, "Swampwalk"),
                "Mountain" => write!(f, "Mountainwalk"),
                "Forest" => write!(f, "Forestwalk"),
                "Legendary" | "Nonbasic" | "Snow" | "Artifact" => {
                    write!(f, "{subtype} Landwalk")
                }
                other => write!(f, "{other}walk"),
            },
            Keyword::Flying => write!(f, "Flying"),
            Keyword::Trample => write!(f, "Trample"),
            Keyword::Vigilance => write!(f, "Vigilance"),
            Keyword::Haste => write!(f, "Haste"),
            Keyword::Lifelink => write!(f, "Lifelink"),
            Keyword::Deathtouch => write!(f, "Deathtouch"),
            Keyword::Reach => write!(f, "Reach"),
            Keyword::Menace => write!(f, "Menace"),
            Keyword::Defender => write!(f, "Defender"),
            Keyword::Flash => write!(f, "Flash"),
            Keyword::Hexproof => write!(f, "Hexproof"),
            Keyword::Indestructible => write!(f, "Indestructible"),
            Keyword::Shroud => write!(f, "Shroud"),
            Keyword::Skulk => write!(f, "Skulk"),
            Keyword::Shadow => write!(f, "Shadow"),
            Keyword::Fear => write!(f, "Fear"),
            Keyword::Horsemanship => write!(f, "Horsemanship"),
            Keyword::Infect => write!(f, "Infect"),
            Keyword::BandsWithOther(quality) => write!(f, "Bands with other {quality}"),
            // Debug-fallback for variants that don't have an explicit
            // user-facing label yet. Unambiguous (no two `Keyword`
            // variants share Debug output) but not necessarily pretty —
            // add an explicit arm above when a new variant surfaces
            // through a Display site (choice prompt, log line, etc.).
            _ => write!(f, "{self:?}"),
        }
    }
}

impl FromStr for Keyword {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Split on first colon for parameterized keywords (MTGJSON canonical form,
        // e.g., "Affinity:for artifacts"). For grant-keyword Oracle text that omits
        // the colon (e.g., "affinity for creatures"), we normalize the space-separated
        // form by splitting on the first colon when present, otherwise leaving the whole
        // string as the name (space-containing multi-word keywords like "first strike" are
        // handled by the name_nospace match below).
        let (name, param) = match s.find(':') {
            Some(idx) => (&s[..idx], Some(s[idx + 1..].to_string())),
            None => (s, None),
        };
        let name_lower = name.to_ascii_lowercase();
        // CR 702.41a: "affinity for [type]" — grant-keyword form without a colon.
        // Normalize to the colon form so it routes through the same `parse_affinity_type`
        // path. Without this, granted Affinity becomes Keyword::Unknown — a silent no-op.
        if param.is_none() {
            if let Some((kw, rest)) = name_lower.split_once(' ') {
                if kw == "affinity" {
                    if let Some(tf) = parse_affinity_type(rest) {
                        return Ok(Keyword::Affinity(tf));
                    }
                }
                // CR 702.176a: "Impending N—{cost}" — space-separated form from Oracle
                // text and MTGJSON keyword arrays (no colon). Extract N before the em-dash.
                if kw == "impending" {
                    let (counters, cost_str) = rest
                        .split_once('\u{2014}')
                        .map(|(n, c)| (n.trim().parse().unwrap_or(0), c.trim()))
                        .unwrap_or((0, rest));
                    return Ok(Keyword::Impending {
                        cost: parse_keyword_mana_cost(cost_str),
                        counters,
                    });
                }
            }
        }

        // If there's a param, try parameterized keywords first
        if let Some(ref p) = param {
            match name_lower.as_str() {
                "protection" => return Ok(Keyword::Protection(parse_protection_target(p))),
                "kicker" => return Ok(Keyword::Kicker(parse_keyword_mana_cost(p))),
                "cycling" => {
                    return Ok(Keyword::Cycling(CyclingCost::Mana(
                        parse_keyword_mana_cost(p),
                    )))
                }
                "flashback" => {
                    return Ok(Keyword::Flashback(FlashbackCost::Mana(
                        parse_keyword_mana_cost(p),
                    )))
                }
                "ward" => return Ok(Keyword::Ward(WardCost::Mana(parse_keyword_mana_cost(p)))),
                "equip" => return Ok(Keyword::Equip(parse_keyword_mana_cost(p))),
                "landwalk" => return Ok(Keyword::Landwalk(p.clone())),
                "rampage" => return Ok(Keyword::Rampage(p.parse().unwrap_or(1))),
                "bushido" => return Ok(Keyword::Bushido(p.parse().unwrap_or(1))),
                "frenzy" => return Ok(Keyword::Frenzy(p.parse().unwrap_or(1))),
                "absorb" => return Ok(Keyword::Absorb(p.parse().unwrap_or(1))),
                "fading" => return Ok(Keyword::Fading(p.parse().unwrap_or(0))),
                "vanishing" => return Ok(Keyword::Vanishing(p.parse().unwrap_or(0))),
                "crew" => {
                    return Ok(Keyword::Crew {
                        power: p.parse().unwrap_or(1),
                        once_per_turn: None,
                    });
                }
                "partner" => return Ok(Keyword::Partner(PartnerType::With(p.clone()))),
                "companion" => return Ok(Keyword::Companion(parse_companion_condition(p))),
                "commanderninjutsu" | "commander ninjutsu" => {
                    return Ok(Keyword::CommanderNinjutsu(parse_keyword_mana_cost(p)))
                }
                "ninjutsu" => return Ok(Keyword::Ninjutsu(parse_keyword_mana_cost(p))),
                "dredge" => return Ok(Keyword::Dredge(p.parse().unwrap_or(1))),
                "modular" => return Ok(Keyword::Modular(p.parse().unwrap_or(1))),
                "renown" => return Ok(Keyword::Renown(p.parse().unwrap_or(1))),
                "fabricate" => return Ok(Keyword::Fabricate(p.parse().unwrap_or(1))),
                "annihilator" => return Ok(Keyword::Annihilator(p.parse().unwrap_or(1))),
                "tribute" => return Ok(Keyword::Tribute(p.parse().unwrap_or(1))),
                "afterlife" => return Ok(Keyword::Afterlife(p.parse().unwrap_or(1))),
                "reconfigure" => return Ok(Keyword::Reconfigure(parse_keyword_mana_cost(p))),
                "bestow" => {
                    return Ok(Keyword::Bestow(BestowCost::Mana(parse_keyword_mana_cost(
                        p,
                    ))))
                }
                "embalm" => {
                    return Ok(Keyword::Embalm(EmbalmCost::Mana(parse_keyword_mana_cost(
                        p,
                    ))))
                }
                "eternalize" => {
                    return Ok(Keyword::Eternalize(EternalizeCost::Mana(
                        parse_keyword_mana_cost(p),
                    )))
                }
                "unearth" => return Ok(Keyword::Unearth(parse_keyword_mana_cost(p))),
                "prowl" => return Ok(Keyword::Prowl(parse_keyword_mana_cost(p))),
                "morph" => return Ok(Keyword::Morph(parse_keyword_mana_cost(p))),
                "megamorph" => return Ok(Keyword::Megamorph(parse_keyword_mana_cost(p))),
                "mayhem" => return Ok(Keyword::Mayhem(parse_keyword_mana_cost(p))),
                "madness" => return Ok(Keyword::Madness(parse_keyword_mana_cost(p))),
                "miracle" => return Ok(Keyword::Miracle(parse_keyword_mana_cost(p))),
                "dash" => return Ok(Keyword::Dash(parse_keyword_mana_cost(p))),
                "emerge" => return Ok(Keyword::Emerge(parse_keyword_mana_cost(p))),
                "harmonize" => return Ok(Keyword::Harmonize(parse_keyword_mana_cost(p))),
                "escape" => {
                    // CR 702.138a: MTGJSON's keywords array carries only the bare
                    // escape mana cost. This placeholder (mana sub-cost, no exile
                    // residual) is overwritten by the Oracle parser with the real
                    // compound `EscapeCost::NonMana`. With no residual it is
                    // rejected by `effective_escape_data` until overwritten.
                    return Ok(Keyword::Escape(EscapeCost::Mana(parse_keyword_mana_cost(
                        p,
                    ))));
                }
                "evoke" => return Ok(Keyword::Evoke(EvokeCost::Mana(parse_keyword_mana_cost(p)))),
                "foretell" => return Ok(Keyword::Foretell(parse_keyword_mana_cost(p))),
                "mutate" => return Ok(Keyword::Mutate(parse_keyword_mana_cost(p))),
                "disturb" => return Ok(Keyword::Disturb(parse_keyword_mana_cost(p))),
                "disguise" => return Ok(Keyword::Disguise(parse_keyword_mana_cost(p))),
                "blitz" => return Ok(Keyword::Blitz(parse_keyword_mana_cost(p))),
                "overload" => return Ok(Keyword::Overload(parse_keyword_mana_cost(p))),
                // CR 702.162a: More Than Meets the Eye {cost} — alternative cost to cast converted.
                "more than meets the eye" => {
                    return Ok(Keyword::MoreThanMeetsTheEye(parse_keyword_mana_cost(p)))
                }
                "spectacle" => return Ok(Keyword::Spectacle(parse_keyword_mana_cost(p))),
                // CR 702.173a: Freerunning alternative cost.
                "freerunning" => return Ok(Keyword::Freerunning(parse_keyword_mana_cost(p))),
                "surge" => return Ok(Keyword::Surge(parse_keyword_mana_cost(p))),
                "encore" => return Ok(Keyword::Encore(parse_keyword_mana_cost(p))),
                "buyback" => {
                    return Ok(Keyword::Buyback(BuybackCost::Mana(
                        parse_keyword_mana_cost(p),
                    )))
                }
                "casualty" => return Ok(Keyword::Casualty(p.parse().unwrap_or(1))),
                "entwine" => return Ok(Keyword::Entwine(parse_keyword_mana_cost(p))),
                "affinity" => {
                    if let Some(tf) = parse_affinity_type(p) {
                        return Ok(Keyword::Affinity(tf));
                    }
                    // Fall through to Unknown for unrecognized affinity types
                }
                "echo" => return Ok(Keyword::Echo(EchoCost::Mana(parse_keyword_mana_cost(p)))),
                "outlast" => return Ok(Keyword::Outlast(parse_keyword_mana_cost(p))),
                "scavenge" => return Ok(Keyword::Scavenge(parse_keyword_mana_cost(p))),
                "reinforce" => {
                    // CR 702.77a: "Reinforce N\u{2014}[cost]" \u{2014} N is the first token, rest is mana cost.
                    // "x" or "X" maps to count=0 (sentinel for Variable X).
                    let p = p.trim();
                    if let Some((n_str, cost_str)) = p.split_once(' ') {
                        let n_trimmed = n_str.trim();
                        let count = if n_trimmed.eq_ignore_ascii_case("x") {
                            0
                        } else {
                            n_trimmed.parse::<u32>().unwrap_or(1)
                        };
                        let cost = parse_keyword_mana_cost(cost_str.trim());
                        return Ok(Keyword::Reinforce { count, cost });
                    } else if p.eq_ignore_ascii_case("x") {
                        return Ok(Keyword::Reinforce {
                            count: 0,
                            cost: ManaCost::zero(),
                        });
                    } else if let Ok(count) = p.parse::<u32>() {
                        return Ok(Keyword::Reinforce {
                            count,
                            cost: ManaCost::zero(),
                        });
                    }
                    // Fall through to Unknown
                }
                // CR 702.113a: Awaken N—{cost} — same count+cost shape as Reinforce.
                "awaken" => {
                    let p = p.trim();
                    // Handle "4—{5}{w}{w}{w}" (em-dash) or "4 {5}{w}{w}{w}" (space)
                    let split = p.split_once('\u{2014}').or_else(|| p.split_once(' '));
                    if let Some((n_str, cost_str)) = split {
                        let count = n_str.trim().parse::<u32>().unwrap_or(0);
                        let cost = parse_keyword_mana_cost(cost_str.trim());
                        return Ok(Keyword::Awaken { count, cost });
                    } else if let Ok(count) = p.parse::<u32>() {
                        return Ok(Keyword::Awaken {
                            count,
                            cost: ManaCost::zero(),
                        });
                    }
                    // Fall through to Unknown
                }
                "fortify" => return Ok(Keyword::Fortify(parse_keyword_mana_cost(p))),
                "prototype" => {
                    return Ok(Keyword::Prototype {
                        cost: parse_keyword_mana_cost(p),
                        power: None,
                        toughness: None,
                    });
                }
                "plot" => return Ok(Keyword::Plot(parse_keyword_mana_cost(p))),
                // CR 702.167a/b: The MTGJSON keyword list carries only "Craft"
                // and the activation cost; the materials class is supplied by
                // the Oracle-line parser (`parse_craft_keyword_line`). This
                // bare-keyword path defaults to the most common materials class
                // (creature) so a card whose Oracle line is unavailable still
                // synthesizes a usable craft ability.
                "craft" => {
                    return Ok(Keyword::Craft {
                        cost: parse_keyword_mana_cost(p),
                        materials: craft_materials_default(),
                        count: CostObjectCount::exactly(1),
                    })
                }
                "offspring" => return Ok(Keyword::Offspring(parse_keyword_mana_cost(p))),
                "impending" => {
                    // CR 702.176a: "Impending N—{cost}" — extract N before the em-dash,
                    // then parse the mana cost from the remainder.
                    let (counters, cost_str) = p
                        .split_once('\u{2014}')
                        .map(|(n, c)| (n.trim().parse().unwrap_or(0), c.trim()))
                        .unwrap_or((0, p));
                    return Ok(Keyword::Impending {
                        cost: parse_keyword_mana_cost(cost_str),
                        counters,
                    });
                }
                "levelup" | "level up" => return Ok(Keyword::LevelUp(parse_keyword_mana_cost(p))),
                "specialize" => return Ok(Keyword::Specialize(parse_keyword_mana_cost(p))),
                "warp" => return Ok(Keyword::Warp(parse_keyword_mana_cost(p))),
                "sneak" => return Ok(Keyword::Sneak(parse_keyword_mana_cost(p))),
                "web-slinging" | "webslinging" => {
                    return Ok(Keyword::WebSlinging(parse_keyword_mana_cost(p)))
                }
                "mobilize" => {
                    let n: i32 = p.parse().unwrap_or(1);
                    return Ok(Keyword::Mobilize(QuantityExpr::Fixed { value: n }));
                }
                "poisonous" => return Ok(Keyword::Poisonous(p.parse().unwrap_or(1))),
                "bloodthirst" => return Ok(Keyword::Bloodthirst(parse_bloodthirst_value(p))),
                "amplify" => return Ok(Keyword::Amplify(p.parse().unwrap_or(1))),
                "graft" => return Ok(Keyword::Graft(p.parse().unwrap_or(1))),
                "devour" => return Ok(Keyword::Devour(p.parse().unwrap_or(1))),
                // CR 702.164
                "toxic" => return Ok(Keyword::Toxic(p.parse().unwrap_or(1))),
                // CR 702.171a
                "saddle" => return Ok(Keyword::Saddle(p.parse().unwrap_or(1))),
                // Teamwork N — optional additional cast cost (CR 601.2b/f);
                // tap-any-number-with-total-power-N mirrors Crew/Saddle.
                "teamwork" => return Ok(Keyword::Teamwork(p.parse().unwrap_or(1))),
                // CR 702.46
                "soulshift" => return Ok(Keyword::Soulshift(p.parse().unwrap_or(1))),
                // CR 702.165
                "backup" => return Ok(Keyword::Backup(p.parse().unwrap_or(1))),
                // CR 702.157
                "squad" => return Ok(Keyword::Squad(parse_keyword_mana_cost(p))),
                // CR 702.56a: Replicate {cost} — repeatable optional additional
                // cost paid at cast; copy the spell once per payment.
                "replicate" => return Ok(Keyword::Replicate(parse_keyword_mana_cost(p))),
                // CR 702.29: Typecycling — "typecycling:{subtype}:{cost}"
                "typecycling" => {
                    if let Some(colon_pos) = p.find(':') {
                        let subtype = {
                            let s = &p[..colon_pos];
                            let mut c = s.chars();
                            c.next()
                                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                                .unwrap_or_default()
                        };
                        let cost_str = &p[colon_pos + 1..];
                        return Ok(Keyword::Typecycling {
                            cost: parse_keyword_mana_cost(cost_str),
                            subtype,
                        });
                    }
                    return Ok(Keyword::Unknown(s.to_string()));
                }
                "firebending" => {
                    let n: i32 = p.parse().unwrap_or(1);
                    return Ok(Keyword::Firebending(QuantityExpr::Fixed { value: n }));
                }
                // CR 702.47a: Splice onto [type] [cost]
                "splice" => {
                    // Strip "onto " prefix if present (e.g., "onto arcane {w}" → "arcane {w}")
                    let after_onto = p.strip_prefix("onto ").unwrap_or(p);
                    // Separate type name from cost — cost starts with '{'
                    let (type_str, cost_str) = match after_onto.find('{') {
                        Some(brace_idx) => {
                            (after_onto[..brace_idx].trim(), &after_onto[brace_idx..])
                        }
                        None => (after_onto.trim(), ""),
                    };
                    let capitalized = capitalize_first(type_str);
                    let cost = parse_keyword_mana_cost(cost_str);
                    return Ok(Keyword::Splice {
                        subtype: capitalized,
                        cost,
                    });
                }
                // CR 702.72a: Champion a [type]
                "champion" => {
                    // Strip "a " or "an " prefix (e.g., "a kithkin" → "Kithkin")
                    let type_str = p
                        .strip_prefix("a ")
                        .or_else(|| p.strip_prefix("an "))
                        .unwrap_or(p);
                    let capitalized = capitalize_first(type_str);
                    return Ok(Keyword::Champion(capitalized));
                }
                // CR 702.53a: Transmute {cost}
                "transmute" => return Ok(Keyword::Transmute(parse_keyword_mana_cost(p))),
                // CR 702.71a: Transfigure {cost}
                "transfigure" => return Ok(Keyword::Transfigure(parse_keyword_mana_cost(p))),
                // CR 702.120a: Escalate [cost]
                "escalate" => {
                    return Ok(Keyword::Escalate(AbilityCost::Mana {
                        cost: parse_keyword_mana_cost(p),
                    }))
                }
                // CR 702.59a: Recover {cost}
                "recover" => return Ok(Keyword::Recover(parse_keyword_mana_cost(p))),
                // CR 702.148a: Cleave {cost}
                "cleave" => return Ok(Keyword::Cleave(parse_keyword_mana_cost(p))),
                // CR 702.74a
                "hideaway" => return Ok(Keyword::Hideaway(p.parse().unwrap_or(4))),
                "afflict" => return Ok(Keyword::Afflict(p.parse().unwrap_or(1))),
                // CR 303.4a + CR 702.5a: When the enchant clause is unrecognized
                // (degenerate / unmodeled grammar), route through `Keyword::Unknown`
                // rather than dumping the raw text as a free-text Subtype, which
                // matches no real game object and silently breaks Aura targeting.
                "enchant" => {
                    return Ok(match parse_enchant_target(p) {
                        Some(filter) => Keyword::Enchant(filter),
                        None => Keyword::Unknown(s.to_string()),
                    })
                }
                "etbcounter" => {
                    let (counter_type, count) = parse_etb_counter(&s[name.len() + 1..]);
                    return Ok(Keyword::EtbCounter {
                        counter_type,
                        count,
                    });
                }
                _ => return Ok(Keyword::Unknown(s.to_string())),
            }
        }

        // CR 702.11d: "hexproof from [quality]" — must be checked before unit matching
        // since "hexproof from red" has no colon and would otherwise fall to Unknown.
        if let Some(quality) = name_lower.strip_prefix("hexproof from ") {
            return Ok(Keyword::HexproofFrom(parse_hexproof_filter(quality)));
        }

        // Simple (unit) keywords -- case-insensitive, space-normalized match
        // Stripping spaces lets PascalCase ("FirstStrike") and Oracle text ("first strike") both match.
        let name_nospace = name_lower.replace(' ', "");
        match name_nospace.as_str() {
            "flying" => Ok(Keyword::Flying),
            "firststrike" => Ok(Keyword::FirstStrike),
            "doublestrike" => Ok(Keyword::DoubleStrike),
            "trampleoverplaneswalkers" => Ok(Keyword::TrampleOverPlaneswalkers),
            "trample" => Ok(Keyword::Trample),
            "deathtouch" => Ok(Keyword::Deathtouch),
            "lifelink" => Ok(Keyword::Lifelink),
            "vigilance" => Ok(Keyword::Vigilance),
            "haste" => Ok(Keyword::Haste),
            "reach" => Ok(Keyword::Reach),
            "defender" => Ok(Keyword::Defender),
            "menace" => Ok(Keyword::Menace),
            "indestructible" => Ok(Keyword::Indestructible),
            "hexproof" => Ok(Keyword::Hexproof),
            "shroud" => Ok(Keyword::Shroud),
            "flash" => Ok(Keyword::Flash),
            "fear" => Ok(Keyword::Fear),
            "intimidate" => Ok(Keyword::Intimidate),
            "skulk" => Ok(Keyword::Skulk),
            "shadow" => Ok(Keyword::Shadow),
            "horsemanship" => Ok(Keyword::Horsemanship),
            "wither" => Ok(Keyword::Wither),
            "infect" => Ok(Keyword::Infect),
            "afflict" => Ok(Keyword::Afflict(1)),
            "frenzy" => Ok(Keyword::Frenzy(1)),
            // CR 702.164: Toxic N — bare "toxic" (grant text / "if that creature has toxic"
            // condition). Parameter defaults to 1; has_keyword matches by discriminant so N
            // is irrelevant for presence. The colon form ("Toxic:N") is handled above.
            "toxic" => Ok(Keyword::Toxic(1)),
            "prowess" => Ok(Keyword::Prowess),
            "undying" => Ok(Keyword::Undying),
            "persist" => Ok(Keyword::Persist),
            "cascade" => Ok(Keyword::Cascade),
            "convoke" => Ok(Keyword::Convoke),
            "waterbend" => Ok(Keyword::Waterbend),
            "delve" => Ok(Keyword::Delve),
            "devoid" => Ok(Keyword::Devoid),
            "exalted" => Ok(Keyword::Exalted),
            "flanking" => Ok(Keyword::Flanking),
            "changeling" => Ok(Keyword::Changeling),
            "phasing" => Ok(Keyword::Phasing),
            "battlecry" => Ok(Keyword::Battlecry),
            "decayed" => Ok(Keyword::Decayed),
            "unleash" => Ok(Keyword::Unleash),
            "riot" => Ok(Keyword::Riot),
            "livingweapon" => Ok(Keyword::LivingWeapon),
            "jobselect" => Ok(Keyword::JobSelect),
            // Accept both the Oracle spelling ("For Mirrodin!") and the
            // serialized variant name ("ForMirrodin"). `Serialize` emits the
            // bare variant name (no "!"), so card-data.json round-trips through
            // this path as "formirrodin"; without the second spelling it would
            // fall to `Keyword::Unknown` and drop the keyword on reload.
            "formirrodin!" | "formirrodin" => Ok(Keyword::ForMirrodin),
            // CR 702.89a/b: "umbra armor" is the current name; "totem armor" is the
            // obsolete printing both Oracle text and MTGJSON may still carry.
            "totemarmor" | "totem armor" | "umbra armor" | "umbraarmor" => Ok(Keyword::TotemArmor),
            "evolve" => Ok(Keyword::Evolve),
            "extort" => Ok(Keyword::Extort),
            "increment" => Ok(Keyword::Increment),
            "exploit" => Ok(Keyword::Exploit),
            "explore" => Ok(Keyword::Explore),
            "ascend" => Ok(Keyword::Ascend),
            "startyourengines" => Ok(Keyword::StartYourEngines),
            "startyourengines!" => Ok(Keyword::StartYourEngines),
            "soulbond" => Ok(Keyword::Soulbond),
            "partner" => Ok(Keyword::Partner(PartnerType::Generic)),
            "chooseabackground" => Ok(Keyword::Partner(PartnerType::ChooseABackground)),
            "doctor'scompanion" => Ok(Keyword::Partner(PartnerType::DoctorsCompanion)),
            "friendsforever" => Ok(Keyword::Partner(PartnerType::FriendsForever)),
            "characterselect" => Ok(Keyword::Partner(PartnerType::CharacterSelect)),
            "banding" => Ok(Keyword::Banding),
            s if s.starts_with("bandswithother:") => {
                let quality = &s["bandswithother:".len()..];
                Ok(Keyword::BandsWithOther(normalize_bands_with_other_quality(
                    quality,
                )))
            }
            s if s.starts_with("bandswithother") && s.len() > "bandswithother".len() => {
                let quality = &s["bandswithother".len()..];
                Ok(Keyword::BandsWithOther(normalize_bands_with_other_quality(
                    quality,
                )))
            }
            "epic" => Ok(Keyword::Epic),
            "fuse" => Ok(Keyword::Fuse),
            "gravestorm" => Ok(Keyword::Gravestorm),
            "haunt" => Ok(Keyword::Haunt),
            "improvise" => Ok(Keyword::Improvise),
            "ingest" => Ok(Keyword::Ingest),
            "melee" => Ok(Keyword::Melee),
            "mentor" => Ok(Keyword::Mentor),
            "myriad" => Ok(Keyword::Myriad),
            "provoke" => Ok(Keyword::Provoke),
            "rebound" => Ok(Keyword::Rebound),
            "retrace" => Ok(Keyword::Retrace),
            "splitsecond" => Ok(Keyword::SplitSecond),
            "storm" => Ok(Keyword::Storm),
            "suspend" => Ok(Keyword::Suspend {
                count: 0,
                cost: ManaCost::default(),
            }),
            "gift" => Ok(Keyword::Gift(GiftKind::Card)),
            s if s.starts_with("gift:") => {
                let kind = match &s["gift:".len()..] {
                    "card" => GiftKind::Card,
                    "treasure" => GiftKind::Treasure,
                    "food" => GiftKind::Food,
                    "tappedfish" => GiftKind::TappedFish,
                    _ => GiftKind::Card,
                };
                Ok(Keyword::Gift(kind))
            }
            s if s.starts_with("discover:") => {
                let n = s["discover:".len()..].parse::<u32>().unwrap_or(1);
                Ok(Keyword::Discover(n))
            }
            "spree" => Ok(Keyword::Spree),
            "ravenous" => Ok(Keyword::Ravenous),
            "daybound" => Ok(Keyword::Daybound),
            "nightbound" => Ok(Keyword::Nightbound),
            "enlist" => Ok(Keyword::Enlist),
            "readahead" => Ok(Keyword::ReadAhead),
            "compleated" => Ok(Keyword::Compleated),
            "conspire" => Ok(Keyword::Conspire),
            "demonstrate" => Ok(Keyword::Demonstrate),
            "dethrone" => Ok(Keyword::Dethrone),
            "doubleteam" => Ok(Keyword::DoubleTeam),
            "livingmetal" => Ok(Keyword::LivingMetal),
            "firebending" => Ok(Keyword::Firebending(QuantityExpr::Fixed { value: 1 })),
            "bloodthirst" => Ok(Keyword::Bloodthirst(BloodthirstValue::Fixed(1))),
            "hideaway" => Ok(Keyword::Hideaway(4)),
            "cumulative" => Ok(Keyword::CumulativeUpkeep(AbilityCost::Mana {
                cost: ManaCost::zero(),
            })),
            "ripple" => Ok(Keyword::Ripple(1)),
            "totem" => Ok(Keyword::Totem),
            // Unit keywords added for MTGJSON keyword name recognition
            "bargain" => Ok(Keyword::Bargain),
            "sunburst" => Ok(Keyword::Sunburst),
            "training" => Ok(Keyword::Training),
            "assist" => Ok(Keyword::Assist),
            "augment" => Ok(Keyword::Augment),
            "aftermath" => Ok(Keyword::Aftermath),
            "jump-start" | "jumpstart" => Ok(Keyword::JumpStart),
            "cipher" => Ok(Keyword::Cipher),
            "undaunted" => Ok(Keyword::Undaunted),
            // CR 702.184a: Station is a fixed activated ability — no parameter.
            "station" => Ok(Keyword::Station),
            // CR 702.xxx: Paradigm (Strixhaven) — bare keyword, no parameter.
            "paradigm" => Ok(Keyword::Paradigm),
            // CR 702.14: Landwalk variants — MTGJSON sends "Islandwalk" etc. as keyword names.
            "islandwalk" => Ok(Keyword::Landwalk("Island".to_string())),
            "swampwalk" => Ok(Keyword::Landwalk("Swamp".to_string())),
            "forestwalk" => Ok(Keyword::Landwalk("Forest".to_string())),
            "mountainwalk" => Ok(Keyword::Landwalk("Mountain".to_string())),
            "plainswalk" => Ok(Keyword::Landwalk("Plains".to_string())),
            // CR 702.14: Non-basic landwalk variants using supertypes/properties.
            "legendarylandwalk" => Ok(Keyword::Landwalk("Legendary".to_string())),
            "nonbasiclandwalk" => Ok(Keyword::Landwalk("Nonbasic".to_string())),
            "snowlandwalk" => Ok(Keyword::Landwalk("Snow".to_string())),
            _ => Ok(Keyword::Unknown(s.to_string())),
        }
    }
}

pub fn normalize_bands_with_other_quality(raw: &str) -> String {
    let trimmed = raw
        .trim()
        .trim_matches('.')
        .trim_start_matches("other ")
        .trim();
    let words: Vec<String> = trimmed
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric())
                .to_string()
        })
        .filter(|word| !word.is_empty())
        .collect();
    let joined = words.join(" ");
    let singular = match joined.to_ascii_lowercase().as_str() {
        "legends" | "legendary creatures" | "legendary creature" => "Legend".to_string(),
        "wolves" => "Wolf".to_string(),
        "walls" => "Wall".to_string(),
        other if other.ends_with("ies") && other.len() > 3 => {
            format!("{}y", &joined[..joined.len() - 3])
        }
        other if other.ends_with('s') && other.len() > 1 => joined[..joined.len() - 1].to_string(),
        _ => joined,
    };
    singular
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// CR 702.11d: Parse the quality after "hexproof from " into a HexproofFilter.
fn parse_hexproof_filter(s: &str) -> HexproofFilter {
    let lower = s.to_ascii_lowercase();
    match lower.as_str() {
        "white" => HexproofFilter::Color(ManaColor::White),
        "blue" => HexproofFilter::Color(ManaColor::Blue),
        "black" => HexproofFilter::Color(ManaColor::Black),
        "red" => HexproofFilter::Color(ManaColor::Red),
        "green" => HexproofFilter::Color(ManaColor::Green),
        "colorless" | "monocolored" | "multicolored" => HexproofFilter::Quality(lower),
        // CR 702.11d + CR 105.4 + CR 609.6: "that color" / "the chosen color"
        // anaphors after a preceding `Choose a color` instruction. Resolved at
        // runtime via `ChosenAttribute::Color` on the granting source. Mirrors
        // `ProtectionTarget::ChosenColor` (CR 702.16).
        "that color"
        | "the chosen color"
        | "chosen color"
        | "the color of your choice"
        | "color of your choice" => HexproofFilter::ChosenColor,
        _ => HexproofFilter::CardType(lower),
    }
}

pub(crate) fn parse_protection_target(s: &str) -> ProtectionTarget {
    // Lookup table on an atomic quality string (not Oracle-text dispatch) — the
    // caller has already isolated the quality token from "protection from X".
    let lower = s.to_ascii_lowercase();
    match lower.as_str() {
        "white" => ProtectionTarget::Color(ManaColor::White),
        "blue" => ProtectionTarget::Color(ManaColor::Blue),
        "black" => ProtectionTarget::Color(ManaColor::Black),
        "red" => ProtectionTarget::Color(ManaColor::Red),
        "green" => ProtectionTarget::Color(ManaColor::Green),
        "multicolored" => ProtectionTarget::Multicolored,
        // CR 702.16a + CR 105.2a: "protection from monocolored" is a color-count
        // quality (exactly one color), NOT a card type. Route to the runtime
        // `source_matches_quality` arm (game/keywords.rs) which already evaluates
        // `source.color.len() == 1`. Multicolored keeps its dedicated variant;
        // monocolored reuses the existing Quality variant (no new variant / no game
        // change). Mirrors parse_hexproof_filter's monocolored→Quality handling.
        "colorless" | "monocolored" => ProtectionTarget::Quality(lower),
        // CR 702.16 + CR 105.4: "the chosen color" / "the color of your choice"
        // resolve at runtime from the granting source's `ChosenAttribute::Color`.
        // "color of your choice" is the as-resolved phrasing (Mother of Runes,
        // Apostle's Blessing, …); "the chosen color" is the anaphor after a
        // preceding "choose a color" instruction. Both land on the same variant.
        "the chosen color"
        | "chosen color"
        | "the color of your choice"
        | "color of your choice" => ProtectionTarget::ChosenColor,
        // CR 702.16 + CR 205.2: "the chosen card type" resolves at
        // runtime from the source permanent's chosen `CardType` attribute.
        "the chosen card type" | "chosen card type" => ProtectionTarget::ChosenCardType,
        // CR 702.16j: "protection from everything" — typed variant, not stringly-typed
        "everything" => ProtectionTarget::Everything,
        // CR 702.16k: "protection from each of your opponents" (Figure of
        // Fable's Avatar form) and its phrasings — protection from every
        // opponent of the protected permanent's controller.
        // CR 702.16i: "protection from each ... players" is shorthand for
        // separate protection from each; the Opponent scope captures all of them.
        "each of your opponents" | "your opponents" | "an opponent" | "opponents" => {
            ProtectionTarget::FromPlayer(super::ability::ControllerRef::Opponent)
        }
        // Lowercase the stored quality — `source_matches_card_type` only matches
        // lowercase, so the canonical stored form must be lowercase.
        _ if lower.starts_with("from ") => ProtectionTarget::Quality(lower),
        _ => {
            // CR 702.16a + CR 202.3: "mana value N or less/greater" — protection
            // from objects whose mana value satisfies a comparator threshold.
            if let Some(filter) = parse_protection_mana_value_filter(&lower) {
                return ProtectionTarget::Filter(TargetFilter::Typed(filter));
            }
            ProtectionTarget::CardType(lower)
        }
    }
}

/// CR 702.16a + CR 202.3: Parse "mana value N or less/greater" into a
/// `TypedFilter` with a `Cmc` property. Uses nom combinators for structured
/// extraction. Returns `None` if the input doesn't match.
fn parse_protection_mana_value_filter(s: &str) -> Option<TypedFilter> {
    type E<'a> = nom::error::Error<&'a str>;

    // "mana value N or less" / "mana value N or greater"
    let (rest, _) = tag::<_, _, E<'_>>("mana value ").parse(s).ok()?;
    let (rest, n) = crate::parser::oracle_nom::primitives::parse_number(rest).ok()?;
    let (rest, comparator) = preceded(
        tag::<_, _, E<'_>>(" or "),
        alt((
            value(Comparator::LE, tag("less")),
            value(Comparator::GE, tag("greater")),
        )),
    )
    .parse(rest)
    .ok()?;
    if !rest.is_empty() {
        return None;
    }

    Some(TypedFilter::default().properties(vec![FilterProp::Cmc {
        comparator,
        value: QuantityExpr::Fixed { value: n as i32 },
    }]))
}

/// Custom Deserialize: accepts both the typed externally-tagged format (new)
/// and plain "Name:Param" strings (legacy card-data.json).
///
/// Plain strings are parsed via FromStr (handles "Flying", "Equip:3", etc).
/// Tagged objects are deserialized via the default externally-tagged format.
impl<'de> Deserialize<'de> for Keyword {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;

        match &value {
            serde_json::Value::String(s) => {
                // Plain string: parse via FromStr (handles both "Flying" and "Equip:3")
                Ok(s.parse::<Keyword>().unwrap())
            }
            serde_json::Value::Object(map) => {
                // Externally-tagged enum: the key is the variant name
                // For unit variants serialized as strings this path won't be hit.
                // For parameterized variants: {"Kicker": {"Cost": ...}}
                if let Some((variant, data)) = map.iter().next() {
                    keyword_from_tagged(variant, data).map_err(serde::de::Error::custom)
                } else {
                    Err(serde::de::Error::custom("empty object for Keyword"))
                }
            }
            _ => Err(serde::de::Error::custom(
                "expected string or object for Keyword",
            )),
        }
    }
}

/// Reconstruct a Keyword from an externally-tagged JSON object.
fn keyword_from_tagged(variant: &str, data: &serde_json::Value) -> Result<Keyword, String> {
    // Helper to deserialize ManaCost from Value
    fn mana(v: &serde_json::Value) -> Result<ManaCost, String> {
        serde_json::from_value(v.clone()).map_err(|e| format!("ManaCost: {e}"))
    }
    fn uint(v: &serde_json::Value) -> u32 {
        v.as_u64().unwrap_or(0) as u32
    }
    // CR 602.5b: Crew's `once_per_turn` cadence. Accepts the current
    // `Option<ActivationRestriction>` shape (`null` / `{"type":"OnlyOnceEachTurn"}`)
    // and the legacy `ActivationCadence` tagged shape
    // (`{"type":"Unlimited"}` / `{"type":"OncePerTurn"}`), mapping both to
    // `Some(ActivationRestriction::OnlyOnceEachTurn)` for the once-each-turn case.
    fn crew_cadence_from_value(
        v: &serde_json::Value,
    ) -> Result<Option<Box<ActivationRestriction>>, String> {
        if v.is_null() {
            return Ok(None);
        }
        match v.get("type").and_then(|t| t.as_str()) {
            Some("Unlimited") => Ok(None),
            Some("OncePerTurn") => Ok(Some(Box::new(ActivationRestriction::OnlyOnceEachTurn))),
            _ => serde_json::from_value::<ActivationRestriction>(v.clone())
                .map(|r| Some(Box::new(r)))
                .map_err(|e| format!("Crew once_per_turn: {e}")),
        }
    }
    fn bloodthirst(v: &serde_json::Value) -> Result<BloodthirstValue, String> {
        if let Some(s) = v.as_str() {
            Ok(parse_bloodthirst_value(s))
        } else if v.is_number() {
            Ok(BloodthirstValue::Fixed(uint(v)))
        } else {
            serde_json::from_value(v.clone()).map_err(|e| format!("Bloodthirst: {e}"))
        }
    }

    match variant {
        "Flying" => Ok(Keyword::Flying),
        "FirstStrike" => Ok(Keyword::FirstStrike),
        "DoubleStrike" => Ok(Keyword::DoubleStrike),
        "Trample" => Ok(Keyword::Trample),
        "TrampleOverPlaneswalkers" => Ok(Keyword::TrampleOverPlaneswalkers),
        "Deathtouch" => Ok(Keyword::Deathtouch),
        "Lifelink" => Ok(Keyword::Lifelink),
        "Vigilance" => Ok(Keyword::Vigilance),
        "Haste" => Ok(Keyword::Haste),
        "Reach" => Ok(Keyword::Reach),
        "Defender" => Ok(Keyword::Defender),
        "Menace" => Ok(Keyword::Menace),
        "Indestructible" => Ok(Keyword::Indestructible),
        "Hexproof" => Ok(Keyword::Hexproof),
        "Shroud" => Ok(Keyword::Shroud),
        "Flash" => Ok(Keyword::Flash),
        "Fear" => Ok(Keyword::Fear),
        "Intimidate" => Ok(Keyword::Intimidate),
        "Skulk" => Ok(Keyword::Skulk),
        "Shadow" => Ok(Keyword::Shadow),
        "Horsemanship" => Ok(Keyword::Horsemanship),
        "Wither" => Ok(Keyword::Wither),
        "Infect" => Ok(Keyword::Infect),
        "Afflict" => Ok(Keyword::Afflict(uint(data).max(1))),
        "StartingIntensity" => Ok(Keyword::StartingIntensity(uint(data))),
        "Prowess" => Ok(Keyword::Prowess),
        "Undying" => Ok(Keyword::Undying),
        "Persist" => Ok(Keyword::Persist),
        "Cascade" => Ok(Keyword::Cascade),
        "Convoke" => Ok(Keyword::Convoke),
        "Waterbend" => Ok(Keyword::Waterbend),
        "Delve" => Ok(Keyword::Delve),
        "Devoid" => Ok(Keyword::Devoid),
        "Changeling" => Ok(Keyword::Changeling),
        "Phasing" => Ok(Keyword::Phasing),
        "Battlecry" => Ok(Keyword::Battlecry),
        "Decayed" => Ok(Keyword::Decayed),
        "Unleash" => Ok(Keyword::Unleash),
        "Riot" => Ok(Keyword::Riot),
        "LivingWeapon" => Ok(Keyword::LivingWeapon),
        "JobSelect" => Ok(Keyword::JobSelect),
        "ForMirrodin" => Ok(Keyword::ForMirrodin),
        "TotemArmor" => Ok(Keyword::TotemArmor),
        "Exalted" => Ok(Keyword::Exalted),
        "Flanking" => Ok(Keyword::Flanking),
        "Evolve" => Ok(Keyword::Evolve),
        "Extort" => Ok(Keyword::Extort),
        "Increment" => Ok(Keyword::Increment),
        "Exploit" => Ok(Keyword::Exploit),
        "Explore" => Ok(Keyword::Explore),
        "Ascend" => Ok(Keyword::Ascend),
        "StartYourEngines" => Ok(Keyword::StartYourEngines),
        "Soulbond" => Ok(Keyword::Soulbond),
        "Banding" => Ok(Keyword::Banding),
        "BandsWithOther" => Ok(Keyword::BandsWithOther(
            data.as_str()
                .map(normalize_bands_with_other_quality)
                .unwrap_or_default(),
        )),
        "Epic" => Ok(Keyword::Epic),
        "Fuse" => Ok(Keyword::Fuse),
        "Gravestorm" => Ok(Keyword::Gravestorm),
        "Haunt" => Ok(Keyword::Haunt),
        "Hideaway" => {
            // Accept both unit (legacy null/string) and parameterized u32
            Ok(Keyword::Hideaway(data.as_u64().unwrap_or(4) as u32))
        }
        "Improvise" => Ok(Keyword::Improvise),
        "Ingest" => Ok(Keyword::Ingest),
        "Melee" => Ok(Keyword::Melee),
        "Mentor" => Ok(Keyword::Mentor),
        "Myriad" => Ok(Keyword::Myriad),
        "Provoke" => Ok(Keyword::Provoke),
        "Rebound" => Ok(Keyword::Rebound),
        "Retrace" => Ok(Keyword::Retrace),
        "SplitSecond" => Ok(Keyword::SplitSecond),
        "Storm" => Ok(Keyword::Storm),
        "Suspend" => Ok(Keyword::Suspend {
            count: 0,
            cost: ManaCost::default(),
        }),
        "Gift" => Ok(Keyword::Gift(GiftKind::Card)),
        "Discover" => Ok(Keyword::Discover(0)),
        "Spree" => Ok(Keyword::Spree),
        "Ravenous" => Ok(Keyword::Ravenous),
        "Daybound" => Ok(Keyword::Daybound),
        "Nightbound" => Ok(Keyword::Nightbound),
        "Enlist" => Ok(Keyword::Enlist),
        "ReadAhead" => Ok(Keyword::ReadAhead),
        "Compleated" => Ok(Keyword::Compleated),
        "Conspire" => Ok(Keyword::Conspire),
        "Demonstrate" => Ok(Keyword::Demonstrate),
        "Aftermath" => Ok(Keyword::Aftermath),
        "Dethrone" => Ok(Keyword::Dethrone),
        "DoubleTeam" => Ok(Keyword::DoubleTeam),
        "LivingMetal" => Ok(Keyword::LivingMetal),
        // CR 702.24a: Legacy serialized data had `Keyword::CumulativeUpkeep`
        // carry a raw `String` cost (e.g. "{1}"). Task 3 changed the field
        // to a typed `AbilityCost`, but parsing the legacy string requires
        // the Oracle parser, which doesn't live in this deserialization
        // path. Card-data.json is regenerated from MTGJSON+Oracle text by
        // the pipeline (`./scripts/gen-card-data.sh`), so the practical
        // fix is to re-run that pipeline rather than recover legacy data
        // here. The zero-cost sentinel is a well-formed placeholder until
        // the pipeline rebuilds the typed cost.
        "Cumulative" => Ok(Keyword::CumulativeUpkeep(AbilityCost::Mana {
            cost: ManaCost::zero(),
        })),
        // CR 702.24a: Legacy serialized data had `Keyword::CumulativeUpkeep`
        // carry a raw `String` cost (e.g. "{1}"). Task 3 changed the field
        // to a typed `AbilityCost`, but parsing the legacy string requires
        // the Oracle parser, which doesn't live in this deserialization
        // path. Card-data.json is regenerated from MTGJSON+Oracle text by
        // the pipeline (`./scripts/gen-card-data.sh`), so the practical
        // fix is to re-run that pipeline rather than recover legacy data
        // here. The zero-cost sentinel is a well-formed placeholder until
        // the pipeline rebuilds the typed cost.
        "CumulativeUpkeep" => Ok(Keyword::CumulativeUpkeep(AbilityCost::Mana {
            cost: ManaCost::zero(),
        })),
        "Ripple" => Ok(Keyword::Ripple(1)),
        "Totem" => Ok(Keyword::Totem),
        // Parameterized: ManaCost (new keywords)
        "Warp" => Ok(Keyword::Warp(mana(data)?)),
        "Sneak" => Ok(Keyword::Sneak(mana(data)?)),
        "WebSlinging" => Ok(Keyword::WebSlinging(mana(data)?)),
        // Parameterized: u32 (new keywords)
        "Mobilize" => {
            // Accept both integer (legacy) and QuantityExpr object
            if let Some(n) = data.as_u64() {
                Ok(Keyword::Mobilize(QuantityExpr::Fixed { value: n as i32 }))
            } else {
                let expr: QuantityExpr =
                    serde_json::from_value(data.clone()).map_err(|e| format!("Mobilize: {e}"))?;
                Ok(Keyword::Mobilize(expr))
            }
        }
        // Parameterized: ManaCost
        "Kicker" => Ok(Keyword::Kicker(mana(data)?)),
        "Cycling" => {
            // Accept both legacy ManaCost format and new CyclingCost tagged format.
            if let Ok(cycling_cost) = serde_json::from_value::<CyclingCost>(data.clone()) {
                Ok(Keyword::Cycling(cycling_cost))
            } else {
                Ok(Keyword::Cycling(CyclingCost::Mana(mana(data)?)))
            }
        }
        "Flashback" => {
            // Accept both legacy ManaCost format and new FlashbackCost tagged format
            if let Ok(fb_cost) = serde_json::from_value::<FlashbackCost>(data.clone()) {
                Ok(Keyword::Flashback(fb_cost))
            } else {
                Ok(Keyword::Flashback(FlashbackCost::Mana(mana(data)?)))
            }
        }
        "Ward" => {
            // Accept both legacy ManaCost format and new WardCost tagged format
            if let Ok(ward_cost) = serde_json::from_value::<WardCost>(data.clone()) {
                Ok(Keyword::Ward(ward_cost))
            } else {
                Ok(Keyword::Ward(WardCost::Mana(mana(data)?)))
            }
        }
        "Equip" => Ok(Keyword::Equip(mana(data)?)),
        "Ninjutsu" => Ok(Keyword::Ninjutsu(mana(data)?)),
        "CommanderNinjutsu" => Ok(Keyword::CommanderNinjutsu(mana(data)?)),
        "Reconfigure" => Ok(Keyword::Reconfigure(mana(data)?)),
        "Bestow" => {
            // Accept both the legacy bare ManaCost format and the new tagged
            // BestowCost format (Mana / NonMana) — mirrors Flashback/Embalm.
            if let Ok(bestow_cost) = serde_json::from_value::<BestowCost>(data.clone()) {
                Ok(Keyword::Bestow(bestow_cost))
            } else {
                Ok(Keyword::Bestow(BestowCost::Mana(mana(data)?)))
            }
        }
        "Embalm" => {
            // Accept both legacy ManaCost format and new EmbalmCost tagged format.
            if let Ok(embalm_cost) = serde_json::from_value::<EmbalmCost>(data.clone()) {
                Ok(Keyword::Embalm(embalm_cost))
            } else {
                Ok(Keyword::Embalm(EmbalmCost::Mana(mana(data)?)))
            }
        }
        "Eternalize" => {
            // Accept both legacy ManaCost format and new EternalizeCost tagged format.
            if let Ok(eternalize_cost) = serde_json::from_value::<EternalizeCost>(data.clone()) {
                Ok(Keyword::Eternalize(eternalize_cost))
            } else {
                Ok(Keyword::Eternalize(EternalizeCost::Mana(mana(data)?)))
            }
        }
        "Unearth" => Ok(Keyword::Unearth(mana(data)?)),
        "Prowl" => Ok(Keyword::Prowl(mana(data)?)),
        "Morph" => Ok(Keyword::Morph(mana(data)?)),
        "Megamorph" => Ok(Keyword::Megamorph(mana(data)?)),
        // CR 702.187b: MTGJSON may provide bare "Mayhem"; the Oracle parser
        // overwrites with the real mana cost extracted from reminder text.
        "Mayhem" => Ok(Keyword::Mayhem(mana(data)?)),
        "Madness" => Ok(Keyword::Madness(mana(data)?)),
        "Miracle" => Ok(Keyword::Miracle(mana(data)?)),
        "Dash" => Ok(Keyword::Dash(mana(data)?)),
        "Emerge" => Ok(Keyword::Emerge(mana(data)?)),
        "Harmonize" => Ok(Keyword::Harmonize(mana(data)?)),
        // CR 702.138a: MTGJSON provides bare "Escape" with no structured cost data.
        // Accept both legacy ManaCost format and new EscapeCost tagged format
        // (Mana / NonMana compound) — mirrors Flashback/Evoke/Bestow.
        "Escape" => {
            if let Ok(escape_cost) = serde_json::from_value::<EscapeCost>(data.clone()) {
                Ok(Keyword::Escape(escape_cost))
            } else {
                Ok(Keyword::Escape(EscapeCost::Mana(mana(data)?)))
            }
        }
        "Evoke" => {
            // Accept both legacy ManaCost format and new EvokeCost tagged format.
            if let Ok(ev_cost) = serde_json::from_value::<EvokeCost>(data.clone()) {
                Ok(Keyword::Evoke(ev_cost))
            } else {
                Ok(Keyword::Evoke(EvokeCost::Mana(mana(data)?)))
            }
        }
        "Foretell" => Ok(Keyword::Foretell(mana(data)?)),
        "Mutate" => Ok(Keyword::Mutate(mana(data)?)),
        "Disturb" => Ok(Keyword::Disturb(mana(data)?)),
        "Disguise" => Ok(Keyword::Disguise(mana(data)?)),
        "Blitz" => Ok(Keyword::Blitz(mana(data)?)),
        "Overload" => Ok(Keyword::Overload(mana(data)?)),
        // CR 702.162a: More Than Meets the Eye {cost} — alternative cost to cast converted.
        "MoreThanMeetsTheEye" => Ok(Keyword::MoreThanMeetsTheEye(mana(data)?)),
        "Spectacle" => Ok(Keyword::Spectacle(mana(data)?)),
        // CR 702.173a: Freerunning alternative cost.
        "Freerunning" => Ok(Keyword::Freerunning(mana(data)?)),
        "Surge" => Ok(Keyword::Surge(mana(data)?)),
        // CR 702.59a: Recover {cost}
        "Recover" => Ok(Keyword::Recover(mana(data)?)),
        "Encore" => Ok(Keyword::Encore(mana(data)?)),
        "Buyback" => {
            // Accept both legacy ManaCost format and new BuybackCost tagged format.
            if let Ok(bb_cost) = serde_json::from_value::<BuybackCost>(data.clone()) {
                Ok(Keyword::Buyback(bb_cost))
            } else {
                Ok(Keyword::Buyback(BuybackCost::Mana(mana(data)?)))
            }
        }
        // CR 702.153a
        "Casualty" => Ok(Keyword::Casualty(uint(data))),
        // CR 702.42a
        "Entwine" => Ok(Keyword::Entwine(mana(data)?)),
        // CR 702.120a: accept both legacy ManaCost format and new AbilityCost tagged format.
        "Escalate" => {
            if let Ok(cost) = serde_json::from_value::<AbilityCost>(data.clone()) {
                Ok(Keyword::Escalate(cost))
            } else {
                Ok(Keyword::Escalate(AbilityCost::Mana { cost: mana(data)? }))
            }
        }
        // CR 702.41a
        "Affinity" => {
            let tf: TypedFilter =
                serde_json::from_value(data.clone()).map_err(|e| format!("Affinity: {e}"))?;
            Ok(Keyword::Affinity(tf))
        }
        // CR 702.30a: accept both legacy ManaCost format and new EchoCost tagged format.
        "Echo" => {
            if let Ok(echo_cost) = serde_json::from_value::<EchoCost>(data.clone()) {
                Ok(Keyword::Echo(echo_cost))
            } else {
                Ok(Keyword::Echo(EchoCost::Mana(mana(data)?)))
            }
        }
        "Outlast" => Ok(Keyword::Outlast(mana(data)?)),
        "Scavenge" => Ok(Keyword::Scavenge(mana(data)?)),
        // CR 702.77a: Reinforce N—[cost]. Data is { "count": N, "cost": "..." }.
        // count may be a number (fixed N) or the string "x"/"X" (Variable X, stored as 0).
        "Reinforce" => {
            let obj = data.as_object().ok_or("Reinforce: expected object")?;
            let count = obj
                .get("count")
                .map(|v| {
                    if let Some(n) = v.as_u64() {
                        n as u32
                    } else if v.as_str().is_some_and(|s| s.eq_ignore_ascii_case("x")) {
                        0
                    } else {
                        1
                    }
                })
                .unwrap_or(1);
            let cost_val = obj.get("cost").unwrap_or(data);
            let cost = mana(cost_val)?;
            Ok(Keyword::Reinforce { count, cost })
        }
        // CR 702.113a: Awaken N—{cost} — same count+cost shape as Reinforce.
        "Awaken" => {
            let obj = data.as_object().ok_or("Awaken: expected object")?;
            let count = obj.get("count").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let cost_val = obj.get("cost").unwrap_or(data);
            let cost = mana(cost_val)?;
            Ok(Keyword::Awaken { count, cost })
        }
        "Fortify" => Ok(Keyword::Fortify(mana(data)?)),
        "Prototype" => {
            if let Some(cost_val) = data.get("cost") {
                let cost = mana(cost_val)?;
                let power = data.get("power").and_then(|v| v.as_i64()).map(|v| v as i32);
                let toughness = data
                    .get("toughness")
                    .and_then(|v| v.as_i64())
                    .map(|v| v as i32);
                Ok(Keyword::Prototype {
                    cost,
                    power,
                    toughness,
                })
            } else {
                Ok(Keyword::Prototype {
                    cost: mana(data)?,
                    power: None,
                    toughness: None,
                })
            }
        }
        "Plot" => Ok(Keyword::Plot(mana(data)?)),
        // CR 702.167a/b: New struct format
        // `{"Craft": {"cost": {...}, "materials": {...}, "count": N}}`.
        // Legacy format `{"Craft": {mana_cost}}` (and the bare-mana fallback)
        // defaults materials to the creature class and count to 1.
        "Craft" => {
            if let Some(cost_val) = data.get("cost") {
                let materials = data
                    .get("materials")
                    .map(|m| {
                        serde_json::from_value::<TargetFilter>(m.clone())
                            .map_err(|e| format!("Craft materials: {e}"))
                    })
                    .transpose()?
                    .unwrap_or_else(craft_materials_default);
                let count = data
                    .get("count")
                    .and_then(|value| {
                        serde_json::from_value::<CostObjectCount>(value.clone())
                            .ok()
                            .or_else(|| {
                                value
                                    .as_u64()
                                    .map(|count| CostObjectCount::exactly(count as u32))
                            })
                    })
                    .unwrap_or_default();
                Ok(Keyword::Craft {
                    cost: mana(cost_val)?,
                    materials,
                    count,
                })
            } else {
                Ok(Keyword::Craft {
                    cost: mana(data)?,
                    materials: craft_materials_default(),
                    count: CostObjectCount::exactly(1),
                })
            }
        }
        "Offspring" => Ok(Keyword::Offspring(mana(data)?)),
        "Impending" => {
            // New format: {"Impending": {"cost": {...}, "counters": N}}
            // Legacy format: {"Impending": {mana_cost}} — treat as counters=0 fallback.
            if let Some(cost_val) = data.get("cost") {
                let counters = data.get("counters").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                Ok(Keyword::Impending {
                    cost: mana(cost_val)?,
                    counters,
                })
            } else {
                Ok(Keyword::Impending {
                    cost: mana(data)?,
                    counters: 0,
                })
            }
        }
        "LevelUp" => Ok(Keyword::LevelUp(mana(data)?)),
        // Parameterized: u32
        "Dredge" => Ok(Keyword::Dredge(uint(data))),
        "Modular" => Ok(Keyword::Modular(uint(data))),
        "Renown" => Ok(Keyword::Renown(uint(data))),
        "Fabricate" => Ok(Keyword::Fabricate(uint(data))),
        "Annihilator" => Ok(Keyword::Annihilator(uint(data))),
        "Bushido" => Ok(Keyword::Bushido(uint(data))),
        "Frenzy" => Ok(Keyword::Frenzy(uint(data))),
        "Tribute" => Ok(Keyword::Tribute(uint(data))),
        "Afterlife" => Ok(Keyword::Afterlife(uint(data))),
        "Fading" => Ok(Keyword::Fading(uint(data))),
        "Vanishing" => Ok(Keyword::Vanishing(uint(data))),
        // CR 702.48: Offering — `Offering(String)` serializes as
        // {"Offering": "<quality>"}; round-trip it back rather than dropping
        // the keyword to Unknown on reload of card-data.json.
        "Offering" => Ok(Keyword::Offering(
            data.as_str().unwrap_or_default().to_string(),
        )),
        // Specialize (Alchemy Horizons: Baldur's Gate) — `Specialize(ManaCost)`
        // serializes as {"Specialize": <ManaCost>}; round-trip it back to the
        // typed variant so the synthesized specialize ability is not lost.
        "Specialize" => mana(data).map(Keyword::Specialize),
        "Crew" => {
            // Struct variant: {"Crew": {"power": N, "once_per_turn": {...}}}.
            // A bare number is also accepted for forward/back compatibility.
            if let Some(obj) = data.as_object() {
                let power = obj.get("power").map(uint).unwrap_or(1);
                let once_per_turn = obj
                    .get("once_per_turn")
                    .map(crew_cadence_from_value)
                    .transpose()?
                    .flatten();
                Ok(Keyword::Crew {
                    power,
                    once_per_turn,
                })
            } else {
                Ok(Keyword::Crew {
                    power: uint(data),
                    once_per_turn: None,
                })
            }
        }
        "Rampage" => Ok(Keyword::Rampage(uint(data))),
        "Absorb" => Ok(Keyword::Absorb(uint(data))),
        "Poisonous" => Ok(Keyword::Poisonous(uint(data))),
        "Bloodthirst" => Ok(Keyword::Bloodthirst(bloodthirst(data)?)),
        "Amplify" => Ok(Keyword::Amplify(uint(data))),
        "Graft" => Ok(Keyword::Graft(uint(data))),
        "Devour" => Ok(Keyword::Devour(uint(data))),
        // CR 702.164 / CR 702.171a / CR 702.46 / CR 702.165
        "Toxic" => Ok(Keyword::Toxic(uint(data))),
        "Saddle" => Ok(Keyword::Saddle(uint(data))),
        // Teamwork N — optional additional cast cost (CR 601.2b/f).
        "Teamwork" => Ok(Keyword::Teamwork(uint(data))),
        "Soulshift" => Ok(Keyword::Soulshift(uint(data))),
        "Backup" => Ok(Keyword::Backup(uint(data))),
        // Avatar crossover: Firebending
        "Firebending" => {
            if let Some(n) = data.as_u64() {
                Ok(Keyword::Firebending(QuantityExpr::Fixed {
                    value: n as i32,
                }))
            } else {
                let expr: QuantityExpr = serde_json::from_value(data.clone())
                    .map_err(|e| format!("Firebending: {e}"))?;
                Ok(Keyword::Firebending(expr))
            }
        }
        // CR 702.157
        "Squad" => Ok(Keyword::Squad(mana(data)?)),
        // CR 702.56a: Replicate {cost}
        "Replicate" => Ok(Keyword::Replicate(mana(data)?)),
        // CR 702.29
        "Typecycling" => {
            let obj = data.as_object().ok_or("Typecycling: expected object")?;
            let cost: ManaCost =
                serde_json::from_value(obj.get("cost").cloned().unwrap_or_default())
                    .map_err(|e| format!("Typecycling cost: {e}"))?;
            let subtype = obj
                .get("subtype")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(Keyword::Typecycling { cost, subtype })
        }
        // Parameterized: special
        "HexproofFrom" => {
            let hf: HexproofFilter =
                serde_json::from_value(data.clone()).map_err(|e| format!("HexproofFrom: {e}"))?;
            Ok(Keyword::HexproofFrom(hf))
        }
        "Protection" => {
            let pt: ProtectionTarget =
                serde_json::from_value(data.clone()).map_err(|e| format!("Protection: {e}"))?;
            Ok(Keyword::Protection(pt))
        }
        "Landwalk" => Ok(Keyword::Landwalk(data.as_str().unwrap_or("").to_string())),
        "Partner" => {
            let pt: PartnerType =
                serde_json::from_value(data.clone()).map_err(|e| format!("Partner: {e}"))?;
            Ok(Keyword::Partner(pt))
        }
        "Companion" => {
            let condition: CompanionCondition =
                serde_json::from_value(data.clone()).map_err(|e| format!("Companion: {e}"))?;
            Ok(Keyword::Companion(condition))
        }
        "Enchant" => {
            let tf: TargetFilter =
                serde_json::from_value(data.clone()).map_err(|e| format!("Enchant: {e}"))?;
            Ok(Keyword::Enchant(tf))
        }
        "EtbCounter" => {
            let obj = data.as_object().ok_or("EtbCounter: expected object")?;
            let counter_type = obj
                .get("counter_type")
                .and_then(|v| v.as_str())
                .map(parse_counter_type)
                .unwrap_or(CounterType::Plus1Plus1);
            let count = obj.get("count").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
            Ok(Keyword::EtbCounter {
                counter_type,
                count,
            })
        }
        // CR 702.47a / CR 702.166a / CR 702.43a / CR 702.72a / CR 702.149a
        // CR 702.132a / CR 702.133a / CR 702.99a / CR 702.53a / CR 702.148a / CR 702.125a
        "Splice" => {
            // Struct form `{ "subtype": "Arcane", "cost": {..} }` (mirrors Typecycling).
            // A bare string is treated as a costless legacy subtype.
            if let Some(subtype) = data.as_str() {
                return Ok(Keyword::Splice {
                    subtype: subtype.to_string(),
                    cost: ManaCost::zero(),
                });
            }
            let obj = data
                .as_object()
                .ok_or("Splice: expected object or string")?;
            let cost: ManaCost =
                serde_json::from_value(obj.get("cost").cloned().unwrap_or_default())
                    .map_err(|e| format!("Splice cost: {e}"))?;
            let subtype = obj
                .get("subtype")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(Keyword::Splice { subtype, cost })
        }
        "Bargain" => Ok(Keyword::Bargain),
        "Sunburst" => Ok(Keyword::Sunburst),
        "Champion" => Ok(Keyword::Champion(data.as_str().unwrap_or("").to_string())),
        "Training" => Ok(Keyword::Training),
        "Assist" => Ok(Keyword::Assist),
        "Augment" => Ok(Keyword::Augment),
        "JumpStart" => Ok(Keyword::JumpStart),
        "Cipher" => Ok(Keyword::Cipher),
        "Transmute" => Ok(Keyword::Transmute(mana(data)?)),
        // CR 702.71a: Transfigure {cost}
        "Transfigure" => Ok(Keyword::Transfigure(mana(data)?)),
        "Cleave" => Ok(Keyword::Cleave(mana(data)?)),
        "Undaunted" => Ok(Keyword::Undaunted),
        // CR 702.184a: Station — fixed activated ability keyword.
        "Station" => Ok(Keyword::Station),
        // CR 702.xxx: Paradigm (Strixhaven) — bare keyword.
        "Paradigm" => Ok(Keyword::Paradigm),
        "Unknown" => Ok(Keyword::Unknown(data.as_str().unwrap_or("").to_string())),
        _ => Ok(Keyword::Unknown(format!("{variant}:{data}"))),
    }
}

/// Check if a game object has a specific keyword, using discriminant-based matching.
/// For parameterized keywords, checks the base keyword only (ignoring the parameter value).
pub fn has_keyword(obj: &crate::game::game_object::GameObject, keyword: &Keyword) -> bool {
    use std::mem::discriminant;
    obj.keywords
        .iter()
        .any(|k| discriminant(k) == discriminant(keyword))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_keywords() {
        assert_eq!(Keyword::from_str("Flying").unwrap(), Keyword::Flying);
        assert_eq!(Keyword::from_str("flying").unwrap(), Keyword::Flying);
        assert_eq!(Keyword::from_str("FLYING").unwrap(), Keyword::Flying);
        assert_eq!(Keyword::from_str("Haste").unwrap(), Keyword::Haste);
        assert_eq!(
            Keyword::from_str("Deathtouch").unwrap(),
            Keyword::Deathtouch
        );
        assert_eq!(
            Keyword::from_str("Indestructible").unwrap(),
            Keyword::Indestructible
        );
        assert_eq!(Keyword::from_str("Hexproof").unwrap(), Keyword::Hexproof);
        assert_eq!(Keyword::from_str("Shroud").unwrap(), Keyword::Shroud);
        assert_eq!(Keyword::from_str("Flash").unwrap(), Keyword::Flash);
    }

    #[test]
    fn parse_multi_word_keywords() {
        assert_eq!(
            Keyword::from_str("First Strike").unwrap(),
            Keyword::FirstStrike
        );
        assert_eq!(
            Keyword::from_str("first strike").unwrap(),
            Keyword::FirstStrike
        );
        assert_eq!(
            Keyword::from_str("Double Strike").unwrap(),
            Keyword::DoubleStrike
        );
        assert_eq!(
            Keyword::from_str("Living Weapon").unwrap(),
            Keyword::LivingWeapon
        );
        assert_eq!(Keyword::from_str("Job Select").unwrap(), Keyword::JobSelect);
        assert_eq!(
            Keyword::from_str("For Mirrodin!").unwrap(),
            Keyword::ForMirrodin
        );
        assert_eq!(
            Keyword::from_str("Totem Armor").unwrap(),
            Keyword::TotemArmor
        );
        assert_eq!(
            Keyword::from_str("Split Second").unwrap(),
            Keyword::SplitSecond
        );
        assert_eq!(Keyword::from_str("Battle Cry").unwrap(), Keyword::Battlecry);
        assert_eq!(Keyword::from_str("Aftermath").unwrap(), Keyword::Aftermath);
    }

    #[test]
    fn unit_keywords_survive_serde_round_trip() {
        // `Serialize` emits the bare variant name; the custom `Deserialize`
        // routes plain strings through `FromStr`. Every unit keyword must
        // round-trip back to itself rather than degrading to `Unknown`.
        // ForMirrodin regressed here: its variant name "ForMirrodin" lacks the
        // "!" that the Oracle-spelling `FromStr` arm required.
        for kw in [
            Keyword::Flying,
            Keyword::LivingWeapon,
            Keyword::JobSelect,
            Keyword::TotemArmor,
            Keyword::ForMirrodin,
        ] {
            let value = serde_json::to_value(&kw).unwrap();
            let back: Keyword = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(back, kw, "round-trip failed for {value:?}");
        }
    }

    #[test]
    fn display_landwalk_uses_oracle_spelling_for_subtypes_and_card_types() {
        assert_eq!(
            Keyword::Landwalk("Island".to_string()).to_string(),
            "Islandwalk"
        );
        assert_eq!(
            Keyword::Landwalk("Snow".to_string()).to_string(),
            "Snow Landwalk"
        );
        assert_eq!(
            Keyword::Landwalk("Artifact".to_string()).to_string(),
            "Artifact Landwalk"
        );
    }

    #[test]
    fn display_bands_with_other_uses_oracle_spelling() {
        assert_eq!(
            Keyword::BandsWithOther("Wolf".to_string()).to_string(),
            "Bands with other Wolf"
        );
    }

    #[test]
    fn parse_parameterized_keywords_as_mana_cost() {
        // Cost-bearing keywords now parse to ManaCost
        let kicker = Keyword::from_str("Kicker:1G").unwrap();
        assert!(matches!(kicker, Keyword::Kicker(ManaCost::Cost { .. })));
        if let Keyword::Kicker(ManaCost::Cost { generic, shards }) = &kicker {
            assert_eq!(*generic, 1);
            assert_eq!(shards.len(), 1); // G
        }

        let cycling = Keyword::from_str("Cycling:2").unwrap();
        assert!(matches!(
            cycling,
            Keyword::Cycling(CyclingCost::Mana(ManaCost::Cost { .. }))
        ));
        if let Keyword::Cycling(CyclingCost::Mana(ManaCost::Cost { generic, .. })) = &cycling {
            assert_eq!(*generic, 2);
        }

        let flashback = Keyword::from_str("Flashback:3BB").unwrap();
        assert!(matches!(
            flashback,
            Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost { .. }))
        ));
        if let Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost { generic, shards })) =
            &flashback
        {
            assert_eq!(*generic, 3);
            assert_eq!(shards.len(), 2); // BB
        }

        // CR 702.187b: Mayhem carries a plain mana cost.
        let mayhem = Keyword::from_str("Mayhem:1R").unwrap();
        assert!(matches!(mayhem, Keyword::Mayhem(ManaCost::Cost { .. })));
        if let Keyword::Mayhem(ManaCost::Cost { generic, shards }) = &mayhem {
            assert_eq!(*generic, 1);
            assert_eq!(shards.len(), 1); // R
        }

        let ward = Keyword::from_str("Ward:2").unwrap();
        assert!(matches!(
            ward,
            Keyword::Ward(WardCost::Mana(ManaCost::Cost { .. }))
        ));

        let equip = Keyword::from_str("Equip:3").unwrap();
        assert!(matches!(equip, Keyword::Equip(ManaCost::Cost { .. })));
    }

    #[test]
    fn parse_affinity_for_arbitrary_subtype_without_land_constraint() {
        let daleks = Keyword::from_str("Affinity for Daleks").unwrap();
        let Keyword::Affinity(dalek_filter) = daleks else {
            panic!("expected Affinity keyword");
        };
        assert_eq!(
            dalek_filter.type_filters,
            vec![TypeFilter::Subtype("Dalek".to_string())],
            "CR 702.41a: arbitrary subtype affinity must not add a false Land constraint"
        );

        let islands = Keyword::from_str("Affinity for Islands").unwrap();
        let Keyword::Affinity(island_filter) = islands else {
            panic!("expected Affinity keyword");
        };
        assert_eq!(
            island_filter.type_filters,
            vec![TypeFilter::Subtype("Island".to_string())],
            "land subtype affinity still matches by subtype without requiring an explicit Land conjunct"
        );
    }

    #[test]
    fn parse_affinity_for_card_type_and_type_combination() {
        // CR 205.2: Tomik, Wielder of Law — "affinity for planeswalkers" is a card
        // TYPE, not a subtype. (Regression: previously parsed as Subtype("Planeswalker").)
        let Keyword::Affinity(pw) = Keyword::from_str("Affinity for planeswalkers").unwrap() else {
            panic!("expected Affinity keyword");
        };
        assert_eq!(
            pw.type_filters,
            vec![TypeFilter::Planeswalker],
            "affinity for planeswalkers is the Planeswalker card type"
        );

        // CR 205: Urza, Chief Artificer — "affinity for artifact creatures" is a
        // type COMBINATION (conjunction), not the bogus Subtype("Artifact creature").
        let Keyword::Affinity(ac) = Keyword::from_str("Affinity for artifact creatures").unwrap()
        else {
            panic!("expected Affinity keyword");
        };
        assert_eq!(
            ac.type_filters,
            vec![TypeFilter::Artifact, TypeFilter::Creature],
            "affinity for artifact creatures is Artifact AND Creature"
        );

        // Regression guard: a genuine subtype still falls through to Subtype, not a
        // type conjunction (only words that are ALL card types build a conjunction).
        let Keyword::Affinity(citizens) = Keyword::from_str("Affinity for Citizens").unwrap()
        else {
            panic!("expected Affinity keyword");
        };
        assert_eq!(
            citizens.type_filters,
            vec![TypeFilter::Subtype("Citizen".to_string())],
            "a non-type word remains a subtype"
        );
    }

    #[test]
    fn parse_numeric_keywords_unchanged() {
        assert_eq!(
            Keyword::from_str("Crew:3").unwrap(),
            Keyword::Crew {
                power: 3,
                once_per_turn: None
            }
        );
        assert_eq!(Keyword::from_str("Rampage:2").unwrap(), Keyword::Rampage(2));
    }

    #[test]
    fn parse_frenzy_colon_and_bare() {
        // CR 702.68a: colon-form carries N.
        assert_eq!(Keyword::from_str("Frenzy:2").unwrap(), Keyword::Frenzy(2));
        // CR 702.68a: bare MTGJSON keyword-list form defaults to Frenzy(1),
        // mirroring the bare `afflict` arm — must NOT fall to Unknown.
        assert_eq!(Keyword::from_str("frenzy").unwrap(), Keyword::Frenzy(1));
        assert_eq!(Keyword::from_str("Frenzy").unwrap(), Keyword::Frenzy(1));
    }

    #[test]
    fn parse_bloodthirst_fixed_and_x() {
        assert_eq!(
            Keyword::from_str("Bloodthirst:3").unwrap(),
            Keyword::Bloodthirst(BloodthirstValue::Fixed(3))
        );
        assert_eq!(
            Keyword::from_str("Bloodthirst:X").unwrap(),
            Keyword::Bloodthirst(BloodthirstValue::X)
        );
        assert_eq!(
            Keyword::from_str("Bloodthirst").unwrap(),
            Keyword::Bloodthirst(BloodthirstValue::Fixed(1))
        );
    }

    #[test]
    fn bloodthirst_serialization_accepts_legacy_fixed_and_x() {
        let legacy_fixed: Keyword = serde_json::from_value(serde_json::json!({
            "Bloodthirst": 2
        }))
        .unwrap();
        assert_eq!(
            legacy_fixed,
            Keyword::Bloodthirst(BloodthirstValue::Fixed(2))
        );

        let legacy_x: Keyword = serde_json::from_value(serde_json::json!({
            "Bloodthirst": "X"
        }))
        .unwrap();
        assert_eq!(legacy_x, Keyword::Bloodthirst(BloodthirstValue::X));

        let json = serde_json::to_value(Keyword::Bloodthirst(BloodthirstValue::X)).unwrap();
        let round_trip: Keyword = serde_json::from_value(json).unwrap();
        assert_eq!(round_trip, Keyword::Bloodthirst(BloodthirstValue::X));
    }

    #[test]
    fn parse_keyword_afflict_n() {
        // CR 702.130a: "Afflict N" — parameterized keyword
        assert_eq!(Keyword::from_str("Afflict:3").unwrap(), Keyword::Afflict(3));
        assert_eq!(Keyword::from_str("Afflict:1").unwrap(), Keyword::Afflict(1));
        // Bare "afflict" without param defaults to 1
        assert_eq!(Keyword::from_str("afflict").unwrap(), Keyword::Afflict(1));
    }

    #[test]
    fn parse_protection_variants() {
        assert_eq!(
            Keyword::from_str("Protection:Red").unwrap(),
            Keyword::Protection(ProtectionTarget::Color(ManaColor::Red))
        );
        assert_eq!(
            Keyword::from_str("Protection:colorless").unwrap(),
            Keyword::Protection(ProtectionTarget::Quality("colorless".to_string()))
        );
        assert_eq!(
            Keyword::from_str("Protection:from everything").unwrap(),
            Keyword::Protection(ProtectionTarget::Quality("from everything".to_string()))
        );
        // CR 702.16j: atomic "everything" quality → typed Everything variant
        assert_eq!(
            Keyword::from_str("Protection:everything").unwrap(),
            Keyword::Protection(ProtectionTarget::Everything)
        );
        assert_eq!(
            Keyword::from_str("Protection:Artifacts").unwrap(),
            Keyword::Protection(ProtectionTarget::CardType("artifacts".to_string()))
        );
        assert_eq!(
            Keyword::from_str("Protection:multicolored").unwrap(),
            Keyword::Protection(ProtectionTarget::Multicolored)
        );
        // CR 702.16: "the chosen color" resolves at runtime
        assert_eq!(
            Keyword::from_str("Protection:the chosen color").unwrap(),
            Keyword::Protection(ProtectionTarget::ChosenColor)
        );
        assert_eq!(
            Keyword::from_str("Protection:chosen color").unwrap(),
            Keyword::Protection(ProtectionTarget::ChosenColor)
        );
    }

    /// CR 702.16a + CR 105.2a: "protection from monocolored" (Guardian of the
    /// Guildpact, Providence of Night) is a color-count quality routed to the
    /// runtime `source_matches_quality` arm (`source.color.len() == 1`), NOT a
    /// card type. Misrouting to `CardType("monocolored")` is inert because
    /// `source_matches_card_type` only matches core types + subtypes, so the
    /// grant would do nothing. This test fails if the fix is reverted.
    #[test]
    fn parse_protection_target_monocolored_is_quality_not_card_type() {
        // Fix-discriminating: monocolored must be a Quality, never a CardType.
        assert_eq!(
            parse_protection_target("monocolored"),
            ProtectionTarget::Quality("monocolored".to_string())
        );
        assert_ne!(
            parse_protection_target("monocolored"),
            ProtectionTarget::CardType("monocolored".to_string())
        );
        // End-to-end through Keyword::from_str (the parser dispatch path).
        assert_eq!(
            Keyword::from_str("Protection:monocolored").unwrap(),
            Keyword::Protection(ProtectionTarget::Quality("monocolored".to_string()))
        );
        // No-regression: multicolored keeps its dedicated typed variant.
        assert_eq!(
            parse_protection_target("multicolored"),
            ProtectionTarget::Multicolored
        );
        // No-regression: colors and chosen-color/card-type arms are unchanged.
        assert_eq!(
            parse_protection_target("red"),
            ProtectionTarget::Color(ManaColor::Red)
        );
        assert_eq!(
            parse_protection_target("the chosen color"),
            ProtectionTarget::ChosenColor
        );
        assert_eq!(
            parse_protection_target("artifacts"),
            ProtectionTarget::CardType("artifacts".to_string())
        );
    }

    /// CR 702.16a + CR 105.2c: "protection from colorless" is a color-quality
    /// predicate (zero colors), NOT a card type. The runtime reads it through
    /// `source_matches_quality` the same way as monocolored/multicolored.
    #[test]
    fn parse_protection_target_colorless_is_quality_not_card_type() {
        assert_eq!(
            parse_protection_target("colorless"),
            ProtectionTarget::Quality("colorless".to_string())
        );
        assert_ne!(
            parse_protection_target("colorless"),
            ProtectionTarget::CardType("colorless".to_string())
        );
        assert_eq!(
            Keyword::from_str("Protection:colorless").unwrap(),
            Keyword::Protection(ProtectionTarget::Quality("colorless".to_string()))
        );
        assert_eq!(
            parse_hexproof_filter("colorless"),
            HexproofFilter::Quality("colorless".to_string())
        );
        assert_eq!(
            Keyword::from_str("hexproof from colorless").unwrap(),
            Keyword::HexproofFrom(HexproofFilter::Quality("colorless".to_string()))
        );
    }

    /// CR 702.16 + CR 105.4: "the color of your choice" / "color of your choice"
    /// (the as-resolved phrasing on Mother of Runes, Aven Liberator, Blessed
    /// Breath, …) parse to the runtime-resolved `ChosenColor` variant — NOT a
    /// literal `CardType("the color of your choice")` that matches no source.
    /// Issue #4371. The same alias is mirrored on `parse_hexproof_filter` for
    /// "gains hexproof from the color of your choice".
    #[test]
    fn parse_protection_target_color_of_your_choice_is_chosen_color() {
        assert_eq!(
            parse_protection_target("the color of your choice"),
            ProtectionTarget::ChosenColor
        );
        assert_eq!(
            parse_protection_target("color of your choice"),
            ProtectionTarget::ChosenColor
        );
        // Mirror on the hexproof classifier (CR 702.11d).
        assert_eq!(
            parse_hexproof_filter("the color of your choice"),
            HexproofFilter::ChosenColor
        );
        assert_eq!(
            parse_hexproof_filter("color of your choice"),
            HexproofFilter::ChosenColor
        );
    }

    /// CR 702.16 + CR 205.2: "the chosen card type" / "chosen card
    /// type" parse to the runtime-resolved `ChosenCardType` variant. Plus the
    /// Blocker-C regression: the `Quality`/`CardType` fallthrough arms must
    /// lowercase their stored string — `source_matches_card_type` only matches
    /// lowercase, so a capitalized stored quality would silently fail to match.
    #[test]
    fn parse_protection_target_chosen_card_type_and_lowercasing() {
        assert_eq!(
            parse_protection_target("the chosen card type"),
            ProtectionTarget::ChosenCardType
        );
        assert_eq!(
            parse_protection_target("chosen card type"),
            ProtectionTarget::ChosenCardType
        );
        // Blocker-C: capitalized input must store lowercase.
        assert_eq!(
            Keyword::from_str("Protection:Artifacts").unwrap(),
            Keyword::Protection(ProtectionTarget::CardType("artifacts".to_string()))
        );
        assert_eq!(
            parse_protection_target("from artifacts"),
            ProtectionTarget::Quality("from artifacts".to_string())
        );
    }

    /// CR 702.16a + CR 202.3: "mana value N or less/greater" parses to
    /// `ProtectionTarget::Filter` with a `Cmc` property.
    #[test]
    fn parse_protection_target_mana_value_filter() {
        // "mana value 3 or less" → Filter(Cmc { LE, Fixed(3) })
        let pt = parse_protection_target("mana value 3 or less");
        assert_eq!(
            pt,
            ProtectionTarget::Filter(TargetFilter::Typed(TypedFilter::default().properties(
                vec![FilterProp::Cmc {
                    comparator: Comparator::LE,
                    value: QuantityExpr::Fixed { value: 3 },
                }]
            )))
        );

        // "mana value 3 or greater" → Filter(Cmc { GE, Fixed(3) })
        let pt = parse_protection_target("mana value 3 or greater");
        assert_eq!(
            pt,
            ProtectionTarget::Filter(TargetFilter::Typed(TypedFilter::default().properties(
                vec![FilterProp::Cmc {
                    comparator: Comparator::GE,
                    value: QuantityExpr::Fixed { value: 3 },
                }]
            )))
        );

        // Roundtrip via Keyword::from_str (MTGJSON colon form)
        let kw = Keyword::from_str("Protection:mana value 3 or less").unwrap();
        match kw {
            Keyword::Protection(ProtectionTarget::Filter(TargetFilter::Typed(tf))) => {
                assert_eq!(tf.properties.len(), 1);
                assert!(
                    matches!(
                        &tf.properties[0],
                        FilterProp::Cmc {
                            comparator: Comparator::LE,
                            value: QuantityExpr::Fixed { value: 3 },
                        }
                    ),
                    "Expected Cmc {{ LE, Fixed(3) }}, got {:?}",
                    tf.properties[0]
                );
            }
            other => panic!("Expected Protection(Filter(Typed(...))), got {other:?}"),
        }
    }

    #[test]
    fn parse_protection_target_mana_value_filter_rejects_trailing_text() {
        assert_eq!(
            parse_protection_target("mana value 3 or less from sources"),
            ProtectionTarget::CardType("mana value 3 or less from sources".to_string())
        );
    }

    #[test]
    fn parse_partner_variants() {
        assert_eq!(
            Keyword::from_str("Partner").unwrap(),
            Keyword::Partner(PartnerType::Generic)
        );
        assert_eq!(
            Keyword::from_str("Partner:Brallin, Skyshark Rider").unwrap(),
            Keyword::Partner(PartnerType::With("Brallin, Skyshark Rider".to_string()))
        );
        // CR 702.124: Partner variant keywords via FromStr
        assert_eq!(
            Keyword::from_str("Choose a background").unwrap(),
            Keyword::Partner(PartnerType::ChooseABackground)
        );
        assert_eq!(
            Keyword::from_str("Doctor's companion").unwrap(),
            Keyword::Partner(PartnerType::DoctorsCompanion)
        );
        assert_eq!(
            Keyword::from_str("Friends forever").unwrap(),
            Keyword::Partner(PartnerType::FriendsForever)
        );
        assert_eq!(
            Keyword::from_str("Character select").unwrap(),
            Keyword::Partner(PartnerType::CharacterSelect)
        );
    }

    #[test]
    fn partner_type_round_trip_serialization() {
        // Verify round-trip through keyword_from_tagged for each PartnerType variant
        let variants = vec![
            Keyword::Partner(PartnerType::Generic),
            Keyword::Partner(PartnerType::With("Shabraz, the Skyshark".to_string())),
            Keyword::Partner(PartnerType::FriendsForever),
            Keyword::Partner(PartnerType::CharacterSelect),
            Keyword::Partner(PartnerType::DoctorsCompanion),
            Keyword::Partner(PartnerType::ChooseABackground),
        ];
        for kw in variants {
            let json = serde_json::to_value(&kw).unwrap();
            let deserialized: Keyword = serde_json::from_value(json.clone()).unwrap();
            assert_eq!(kw, deserialized, "round-trip failed for {json}");
        }
    }

    #[test]
    fn parse_enchant_as_target_filter() {
        let enchant = Keyword::from_str("Enchant:creature").unwrap();
        assert!(matches!(
            enchant,
            Keyword::Enchant(TargetFilter::Typed(TypedFilter { .. }))
        ));
        if let Keyword::Enchant(TargetFilter::Typed(ref tf)) = enchant {
            assert!(matches!(
                tf.get_primary_type(),
                Some(super::super::ability::TypeFilter::Creature)
            ));
        }
    }

    /// CR 303.4 + CR 702.5a: Daybreak Coronet — "Enchant creature with another
    /// Aura attached to it" narrows the legal host set to creatures that already
    /// carry another Aura. The qualifier folds onto the typed filter as
    /// `FilterProp::HasAttachment { Aura, exclude_source: Exclude }` so SBA
    /// legality cannot let Daybreak Coronet count itself after it resolves.
    #[test]
    fn parse_enchant_creature_with_another_aura_attached() {
        use super::super::ability::{AttachmentKind, TypeFilter};
        let enchant =
            Keyword::from_str("Enchant:creature with another aura attached to it").unwrap();
        let Keyword::Enchant(TargetFilter::Typed(tf)) = enchant else {
            panic!("expected Typed; got {enchant:?}")
        };
        assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
        assert!(
            tf.properties.contains(&FilterProp::HasAttachment {
                kind: AttachmentKind::Aura,
                controller: None,
                exclude_source: crate::types::ability::SourceExclusion::Exclude,
            }),
            "expected FilterProp::HasAttachment {{ Aura, exclude_source }}; got {:?}",
            tf.properties
        );
    }

    /// Regression guard: a plain "Enchant creature" must NOT acquire an
    /// attachment predicate — only the explicit qualifier adds `HasAttachment`.
    /// CR 702.5a + CR 702.9: Trapped in the Tower / Roots — "Enchant creature
    /// without flying" must lower to `WithoutKeyword(Flying)` on the Aura target.
    #[test]
    fn parse_enchant_creature_without_flying() {
        use super::super::ability::TypeFilter;
        let enchant = Keyword::from_str("Enchant:creature without flying").unwrap();
        let Keyword::Enchant(TargetFilter::Typed(tf)) = enchant else {
            panic!("expected Typed; got {enchant:?}")
        };
        assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
        assert!(
            tf.properties.iter().any(
                |p| matches!(p, FilterProp::WithoutKeyword { value } if *value == Keyword::Flying)
            ),
            "expected WithoutKeyword(Flying); got {:?}",
            tf.properties
        );
    }

    #[test]
    fn parse_enchant_plain_creature_has_no_attachment_predicate() {
        use super::super::ability::AttachmentKind;
        let enchant = Keyword::from_str("Enchant:creature").unwrap();
        let Keyword::Enchant(TargetFilter::Typed(tf)) = enchant else {
            panic!("expected Typed; got {enchant:?}")
        };
        assert!(
            !tf.properties.iter().any(|p| matches!(
                p,
                FilterProp::HasAttachment {
                    kind: AttachmentKind::Aura,
                    ..
                }
            )),
            "plain Enchant creature must carry no HasAttachment prop; got {:?}",
            tf.properties
        );
    }

    #[test]
    fn parse_enchant_with_controller_restriction() {
        let enchant = Keyword::from_str("Enchant:creature you control").unwrap();
        assert_eq!(
            enchant,
            Keyword::Enchant(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You)
            ))
        );
    }

    /// CR 702.5d + CR 303.4: "Enchant player" maps to `TargetFilter::Player`,
    /// which `find_legal_targets` resolves to every player at the table. The
    /// Aura's resolution path then routes via `attach_to_player` (CR 303.4f).
    #[test]
    fn parse_enchant_player_emits_player_filter() {
        let enchant = Keyword::from_str("Enchant:player").unwrap();
        assert_eq!(enchant, Keyword::Enchant(TargetFilter::Player));
    }

    /// CR 702.5d + CR 303.4: "Enchant opponent" (Curse cycle) maps to a typed
    /// filter with `controller = Opponent` and empty type filters — the
    /// player-only branch of `find_legal_targets` (lines 46-75) restricts the
    /// candidates to opposing players, exactly mirroring "target opponent".
    #[test]
    fn parse_enchant_opponent_targets_only_opponents() {
        let enchant = Keyword::from_str("Enchant:opponent").unwrap();
        assert_eq!(
            enchant,
            Keyword::Enchant(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent)
            ))
        );
    }

    // ---- Issue #537 parser shape tests (5a) -------------------------------
    // CR 702.5a + CR 303.4a: zone-qualified Enchant clauses must carry the
    // zone as `FilterProp::InZone`, never a free-text `TypeFilter::Subtype`.

    #[test]
    fn parse_enchant_creature_card_in_graveyard_carries_graveyard_zone() {
        use super::super::ability::TypeFilter;
        use super::super::zones::Zone;
        // CR 702.5a + CR 303.4c (Animate Dead, Dance of the Dead).
        let kw = Keyword::from_str("Enchant:creature card in a graveyard").unwrap();
        let Keyword::Enchant(TargetFilter::Typed(tf)) = kw else {
            panic!("expected Typed; got {kw:?}")
        };
        assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
        assert!(
            tf.properties.contains(&FilterProp::InZone {
                zone: Zone::Graveyard
            }),
            "expected FilterProp::InZone {{ Graveyard }}; got {:?}",
            tf.properties
        );
        assert!(
            !tf.type_filters
                .iter()
                .any(|t| matches!(t, TypeFilter::Subtype(s) if s.contains("graveyard"))),
            "bug regression: free-text Subtype dump returned"
        );
    }

    #[test]
    fn parse_enchant_instant_card_in_graveyard_for_spellweaver_volute() {
        use super::super::ability::TypeFilter;
        use super::super::zones::Zone;
        let kw = Keyword::from_str("Enchant:instant card in a graveyard").unwrap();
        let Keyword::Enchant(TargetFilter::Typed(tf)) = kw else {
            panic!("expected Typed; got {kw:?}")
        };
        assert_eq!(tf.type_filters, vec![TypeFilter::Instant]);
        assert!(tf.properties.contains(&FilterProp::InZone {
            zone: Zone::Graveyard
        }));
    }

    #[test]
    fn parse_enchant_card_in_your_hand_no_type_leg() {
        use super::super::ability::TypeFilter;
        use super::super::zones::Zone;
        // Don't Worry About It: no type word — pure "card in <zone>" form.
        // CR 303.4a: default TypeFilter::Card (matches any card) when the
        // type leg is absent.
        let kw = Keyword::from_str("Enchant:card in your hand").unwrap();
        let Keyword::Enchant(TargetFilter::Typed(tf)) = kw else {
            panic!("expected Typed; got {kw:?}")
        };
        assert_eq!(tf.type_filters, vec![TypeFilter::Card]);
        assert!(tf
            .properties
            .contains(&FilterProp::InZone { zone: Zone::Hand }));
    }

    #[test]
    fn parse_enchant_dance_of_the_dead_matches_animate_dead_shape() {
        // Dance of the Dead prints the same Enchant clause as Animate Dead.
        // Asserting both parse to identical filters guards against drift.
        let animate = Keyword::from_str("Enchant:creature card in a graveyard").unwrap();
        let dance = Keyword::from_str("Enchant:creature card in a graveyard").unwrap();
        assert_eq!(animate, dance);
    }

    #[test]
    fn parse_etb_counter_typed() {
        let kw = Keyword::from_str("EtbCounter:P1P1:1").unwrap();
        assert!(matches!(kw, Keyword::EtbCounter { .. }));
        if let Keyword::EtbCounter {
            counter_type,
            count,
        } = &kw
        {
            assert_eq!(counter_type, &CounterType::Plus1Plus1);
            assert_eq!(*count, 1);
        }

        let kw2 = Keyword::from_str("EtbCounter:P1P1:3").unwrap();
        if let Keyword::EtbCounter {
            counter_type,
            count,
        } = &kw2
        {
            assert_eq!(counter_type, &CounterType::Plus1Plus1);
            assert_eq!(*count, 3);
        }
    }

    #[test]
    fn parse_toxic_colon_and_bare() {
        // CR 702.164: colon-form carries N (no-regression for the existing path).
        assert_eq!(Keyword::from_str("Toxic:3").unwrap(), Keyword::Toxic(3));
        // CR 702.164: bare "toxic" (grant text / "if that creature has toxic"
        // condition) defaults to Toxic(1) and must NOT fall to Unknown — otherwise
        // has_keyword (discriminant match) never sees a real Toxic(N) and the rider
        // is silently dead. Case-insensitive via the from_str normalizer.
        assert_eq!(Keyword::from_str("toxic").unwrap(), Keyword::Toxic(1));
        assert_eq!(Keyword::from_str("Toxic").unwrap(), Keyword::Toxic(1));
        assert_ne!(
            Keyword::from_str("toxic").unwrap(),
            Keyword::Unknown("toxic".to_string())
        );
    }

    #[test]
    fn parse_new_parameterized_keywords() {
        // CR 702.164: Toxic
        assert_eq!(Keyword::from_str("Toxic:2").unwrap(), Keyword::Toxic(2));
        assert_eq!(Keyword::from_str("Toxic:1").unwrap(), Keyword::Toxic(1));

        // CR 702.171a: Saddle
        assert_eq!(Keyword::from_str("Saddle:3").unwrap(), Keyword::Saddle(3));

        // CR 702.46: Soulshift
        assert_eq!(
            Keyword::from_str("Soulshift:7").unwrap(),
            Keyword::Soulshift(7)
        );

        // CR 702.165: Backup
        assert_eq!(Keyword::from_str("Backup:1").unwrap(), Keyword::Backup(1));

        // CR 702.157: Squad
        let squad = Keyword::from_str("Squad:{2}").unwrap();
        assert!(matches!(squad, Keyword::Squad(ManaCost::Cost { .. })));
    }

    #[test]
    /// CR 702.176a: Impending N—{cost} parses N (counter count) and mana cost.
    fn parse_impending_from_str() {
        // Oracle keyword line: "Impending 5—{1}{B}"
        let kw = Keyword::from_str("Impending 5\u{2014}{1}{B}").unwrap();
        match kw {
            Keyword::Impending { counters, cost } => {
                assert_eq!(counters, 5);
                assert!(matches!(cost, ManaCost::Cost { .. }));
            }
            other => panic!("expected Impending, got {other:?}"),
        }

        // "Impending 3—{2}{U}{U}"
        let kw2 = Keyword::from_str("Impending 3\u{2014}{2}{U}{U}").unwrap();
        match kw2 {
            Keyword::Impending { counters, .. } => assert_eq!(counters, 3),
            other => panic!("expected Impending, got {other:?}"),
        }
    }

    #[test]
    fn parse_typecycling() {
        // CR 702.29: Typecycling colon-form
        let kw = Keyword::from_str("Typecycling:plains:{2}").unwrap();
        assert!(matches!(kw, Keyword::Typecycling { .. }));
        if let Keyword::Typecycling { subtype, .. } = &kw {
            assert_eq!(subtype, "Plains"); // capitalized
        }

        let kw2 = Keyword::from_str("Typecycling:forest:{1}{G}").unwrap();
        if let Keyword::Typecycling { subtype, cost } = &kw2 {
            assert_eq!(subtype, "Forest");
            assert!(matches!(cost, ManaCost::Cost { .. }));
        }

        // Malformed (missing cost) falls through to Unknown
        let kw3 = Keyword::from_str("Typecycling:plains").unwrap();
        assert!(matches!(kw3, Keyword::Unknown(_)));
    }

    #[test]
    fn parse_previously_missing_fromstr_arms() {
        // Step 0: These existed in enum + keyword_from_tagged but were missing from FromStr
        assert_eq!(Keyword::from_str("Hideaway").unwrap(), Keyword::Hideaway(4));
        assert_eq!(
            Keyword::from_str("Cumulative").unwrap(),
            Keyword::CumulativeUpkeep(AbilityCost::Mana {
                cost: ManaCost::zero()
            })
        );
        assert_eq!(Keyword::from_str("Ripple").unwrap(), Keyword::Ripple(1));
        assert_eq!(Keyword::from_str("Totem").unwrap(), Keyword::Totem);
        // Warp is now parameterized — bare "Warp" without cost falls through to Unknown
        assert!(matches!(
            Keyword::from_str("Warp").unwrap(),
            Keyword::Unknown(_)
        ));
    }

    /// CR 702.191: MTGJSON keyword ingestion must parse Increment, not Unknown.
    #[test]
    fn increment_from_str_and_keyword_from_tagged() {
        assert_eq!(Keyword::from_str("Increment").unwrap(), Keyword::Increment);
        assert_eq!(Keyword::from_str("increment").unwrap(), Keyword::Increment);
        let kw = keyword_from_tagged("Increment", &serde_json::Value::Null).unwrap();
        assert_eq!(kw, Keyword::Increment);
    }

    #[test]
    fn parse_hexproof_from_keywords() {
        // CR 702.11d: "hexproof from [quality]" variants
        assert_eq!(
            Keyword::from_str("hexproof from red").unwrap(),
            Keyword::HexproofFrom(HexproofFilter::Color(ManaColor::Red))
        );
        assert_eq!(
            Keyword::from_str("hexproof from black").unwrap(),
            Keyword::HexproofFrom(HexproofFilter::Color(ManaColor::Black))
        );
        assert_eq!(
            Keyword::from_str("hexproof from white").unwrap(),
            Keyword::HexproofFrom(HexproofFilter::Color(ManaColor::White))
        );
        assert_eq!(
            Keyword::from_str("hexproof from blue").unwrap(),
            Keyword::HexproofFrom(HexproofFilter::Color(ManaColor::Blue))
        );
        assert_eq!(
            Keyword::from_str("hexproof from green").unwrap(),
            Keyword::HexproofFrom(HexproofFilter::Color(ManaColor::Green))
        );
        assert_eq!(
            Keyword::from_str("hexproof from monocolored").unwrap(),
            Keyword::HexproofFrom(HexproofFilter::Quality("monocolored".to_string()))
        );
        assert_eq!(
            Keyword::from_str("hexproof from artifacts").unwrap(),
            Keyword::HexproofFrom(HexproofFilter::CardType("artifacts".to_string()))
        );
        // Plain "hexproof" still parses as Hexproof, not HexproofFrom
        assert_eq!(Keyword::from_str("Hexproof").unwrap(), Keyword::Hexproof);
    }

    #[test]
    fn hexproof_from_round_trip_serialization() {
        let variants = vec![
            Keyword::HexproofFrom(HexproofFilter::Color(ManaColor::Red)),
            Keyword::HexproofFrom(HexproofFilter::CardType("artifacts".to_string())),
            Keyword::HexproofFrom(HexproofFilter::Quality("monocolored".to_string())),
        ];
        for kw in variants {
            let json = serde_json::to_value(&kw).unwrap();
            let deserialized: Keyword = serde_json::from_value(json.clone()).unwrap();
            assert_eq!(kw, deserialized, "round-trip failed for {json}");
        }
    }

    #[test]
    fn parse_unknown_keyword() {
        assert_eq!(
            Keyword::from_str("NotARealKeyword").unwrap(),
            Keyword::Unknown("NotARealKeyword".to_string())
        );
    }

    #[test]
    fn keyword_never_fails() {
        // FromStr returns Result<Self, Infallible> -- always Ok
        assert!(Keyword::from_str("").unwrap() == Keyword::Unknown("".to_string()));
        assert!(Keyword::from_str("xyz:abc").unwrap() == Keyword::Unknown("xyz:abc".to_string()));
    }

    #[test]
    fn keyword_serialization_roundtrip() {
        let keywords = vec![
            Keyword::Flying,
            Keyword::Kicker(ManaCost::Cost {
                shards: vec![crate::types::mana::ManaCostShard::Green],
                generic: 1,
            }),
            Keyword::Protection(ProtectionTarget::Color(ManaColor::Blue)),
            Keyword::Protection(ProtectionTarget::ChosenColor),
            Keyword::Unknown("CustomKeyword".to_string()),
            Keyword::EtbCounter {
                counter_type: CounterType::Plus1Plus1,
                count: 2,
            },
            Keyword::Toxic(2),
            Keyword::Saddle(3),
            Keyword::Soulshift(5),
            Keyword::Backup(1),
            Keyword::Squad(ManaCost::Cost {
                shards: vec![],
                generic: 2,
            }),
            Keyword::Typecycling {
                cost: ManaCost::Cost {
                    shards: vec![],
                    generic: 2,
                },
                subtype: "Plains".to_string(),
            },
        ];
        let json = serde_json::to_string(&keywords).unwrap();
        let deserialized: Vec<Keyword> = serde_json::from_str(&json).unwrap();
        assert_eq!(keywords, deserialized);
    }

    #[test]
    fn keyword_count_over_fifty() {
        // Ensure we have 50+ keyword variants (excluding Unknown)
        let test_keywords = vec![
            "Flying",
            "First Strike",
            "Double Strike",
            "Trample",
            "Deathtouch",
            "Lifelink",
            "Vigilance",
            "Haste",
            "Reach",
            "Defender",
            "Menace",
            "Indestructible",
            "Hexproof",
            "Shroud",
            "Flash",
            "Fear",
            "Intimidate",
            "Skulk",
            "Shadow",
            "Horsemanship",
            "Wither",
            "Infect",
            "Prowess",
            "Undying",
            "Persist",
            "Cascade",
            "Convoke",
            "Waterbend",
            "Delve",
            "Devoid",
            "Exalted",
            "Flanking",
            "Changeling",
            "Phasing",
            "Battle Cry",
            "Decayed",
            "Unleash",
            "Riot",
            "Living Weapon",
            "Job Select",
            "Totem Armor",
            "Evolve",
            "Extort",
            "Increment",
            "Exploit",
            "Explore",
            "Ascend",
            "Soulbond",
            "Partner",
            "Banding",
            "Epic",
            "Fuse",
            "Improvise",
            "Ingest",
            "Melee",
            "Mentor",
            "Myriad",
        ];
        let mut non_unknown = 0;
        for kw in &test_keywords {
            let parsed = Keyword::from_str(kw).unwrap();
            if !matches!(parsed, Keyword::Unknown(_)) {
                non_unknown += 1;
            }
        }
        assert!(
            non_unknown >= 50,
            "Expected 50+ known keywords, got {non_unknown}"
        );
    }

    /// CR 702.94: Miracle — FromStr accepts "miracle {cost}" and produces
    /// `Keyword::Miracle(ManaCost)` with the parsed cost.
    #[test]
    fn miracle_from_str_parses_cost() {
        let parsed = Keyword::from_str("Miracle:{1}{W}").unwrap();
        let expected_cost = parse_keyword_mana_cost("{1}{W}");
        match parsed {
            Keyword::Miracle(cost) => {
                assert_eq!(cost, expected_cost, "Miracle cost mismatch");
            }
            other => panic!("expected Keyword::Miracle, got {other:?}"),
        }
    }

    /// CR 702.94: Miracle keyword discriminant and serde round-trip.
    #[test]
    fn miracle_kind_and_round_trip() {
        let kw = Keyword::Miracle(parse_keyword_mana_cost("{1}{W}"));
        assert_eq!(kw.kind(), KeywordKind::Miracle);
        let json = serde_json::to_value(&kw).unwrap();
        let deserialized: Keyword = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(kw, deserialized, "round-trip failed for {json}");
    }

    /// CR 702.138a (#3281): card-data export encodes compound escape costs as
    /// `EscapeCost::NonMana`; deserializing must not collapse them to the bare
    /// MTGJSON placeholder.
    #[test]
    fn escape_compound_cost_deserializes_from_card_data_export() {
        use crate::types::ability::{
            AbilityCost, ControllerRef, FilterProp, TargetFilter, TypedFilter,
        };
        use crate::types::mana::ManaCostShard;
        use crate::types::zones::Zone;

        let expected = Keyword::Escape(EscapeCost::NonMana(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        generic: 0,
                        shards: vec![
                            ManaCostShard::Green,
                            ManaCostShard::Green,
                            ManaCostShard::Blue,
                            ManaCostShard::Blue,
                        ],
                    },
                },
                AbilityCost::Exile {
                    count: 5,
                    zone: Some(Zone::Graveyard),
                    filter: Some(TargetFilter::Typed(
                        TypedFilter::card()
                            .controller(ControllerRef::You)
                            .properties(vec![
                                FilterProp::Another,
                                FilterProp::InZone {
                                    zone: Zone::Graveyard,
                                },
                            ]),
                    )),
                },
            ],
        }));

        let json = serde_json::to_value(&expected).unwrap();
        let deserialized: Keyword = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(expected, deserialized, "round-trip failed for {json}");
    }

    #[test]
    fn firebending_from_str_parses_fixed_amount() {
        assert_eq!(
            Keyword::from_str("Firebending:2").unwrap(),
            Keyword::Firebending(QuantityExpr::Fixed { value: 2 })
        );
        assert_eq!(
            Keyword::from_str("Firebending").unwrap(),
            Keyword::Firebending(QuantityExpr::Fixed { value: 1 })
        );
    }

    #[test]
    fn firebending_deserializes_legacy_number_and_quantity_expr() {
        let legacy: Keyword = serde_json::from_value(serde_json::json!({
            "Firebending": 3
        }))
        .unwrap();
        assert_eq!(
            legacy,
            Keyword::Firebending(QuantityExpr::Fixed { value: 3 })
        );

        let quantity: Keyword = serde_json::from_value(serde_json::json!({
            "Firebending": {
                "type": "Ref",
                "qty": {
                    "type": "Power",
                    "scope": {
                        "type": "Source"
                    }
                }
            }
        }))
        .unwrap();
        assert_eq!(
            quantity,
            Keyword::Firebending(QuantityExpr::Ref {
                qty: crate::types::ability::QuantityRef::Power {
                    scope: crate::types::ability::ObjectScope::Source
                }
            })
        );
    }

    /// CR 702.59a: Recover keyword FromStr parsing.
    #[test]
    fn recover_from_str_parses_cost() {
        let parsed = Keyword::from_str("recover:{2}{B}").unwrap();
        let expected_cost = parse_keyword_mana_cost("{2}{B}");
        match parsed {
            Keyword::Recover(cost) => {
                assert_eq!(cost, expected_cost, "Recover cost mismatch");
            }
            other => panic!("expected Keyword::Recover, got {other:?}"),
        }
    }

    /// CR 702.59a: Recover keyword discriminant and serde round-trip.
    #[test]
    fn recover_kind_and_round_trip() {
        let kw = Keyword::Recover(parse_keyword_mana_cost("{2}{B}"));
        assert_eq!(kw.kind(), KeywordKind::Recover);
        let json = serde_json::to_value(&kw).unwrap();
        let deserialized: Keyword = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(kw, deserialized, "round-trip failed for {json}");
    }

    /// CR 702.59a: Recover keyword_from_tagged deserialization.
    #[test]
    fn recover_keyword_from_tagged() {
        let data = serde_json::json!({
            "type": "Cost",
            "shards": ["Black"],
            "generic": 2
        });
        let kw = keyword_from_tagged("Recover", &data).unwrap();
        assert_eq!(kw.kind(), KeywordKind::Recover);
        match kw {
            Keyword::Recover(_) => {} // cost shape validated by ManaCost deser
            other => panic!("expected Keyword::Recover, got {other:?}"),
        }
    }

    /// CR 702.173a: Freerunning keyword FromStr parsing.
    #[test]
    fn freerunning_from_str_parses_cost() {
        let parsed = Keyword::from_str("freerunning:{3}{B}{B}").unwrap();
        let expected_cost = parse_keyword_mana_cost("{3}{B}{B}");
        match parsed {
            Keyword::Freerunning(cost) => {
                assert_eq!(cost, expected_cost, "Freerunning cost mismatch");
            }
            other => panic!("expected Keyword::Freerunning, got {other:?}"),
        }
    }

    /// CR 702.173a: Freerunning keyword discriminant and serde round-trip.
    #[test]
    fn freerunning_kind_and_round_trip() {
        let kw = Keyword::Freerunning(parse_keyword_mana_cost("{3}{B}{B}"));
        assert_eq!(kw.kind(), KeywordKind::Freerunning);
        let json = serde_json::to_value(&kw).unwrap();
        let deserialized: Keyword = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(kw, deserialized, "round-trip failed for {json}");
    }

    /// CR 702.173a: Freerunning keyword_from_tagged deserialization.
    #[test]
    fn freerunning_keyword_from_tagged() {
        // ManaCost is serde-tagged with "type": "Cost", shards as enum variants, generic as u32.
        let data = serde_json::json!({
            "type": "Cost",
            "shards": ["Black", "Black"],
            "generic": 3
        });
        let kw = keyword_from_tagged("Freerunning", &data).unwrap();
        assert_eq!(kw.kind(), KeywordKind::Freerunning);
        match kw {
            Keyword::Freerunning(_) => {} // cost shape validated by ManaCost deser
            other => panic!("expected Keyword::Freerunning, got {other:?}"),
        }
    }

    #[test]
    fn parameterized_keywords_survive_serde_round_trip() {
        // Serialize emits these as externally-tagged objects
        // ({"Specialize": <ManaCost>}, {"Offering": "<quality>"}); the custom
        // Deserialize must route them back through keyword_from_tagged rather
        // than dropping them to Unknown on reload of card-data.json.
        for kw in [
            Keyword::Specialize(parse_keyword_mana_cost("{2}")),
            Keyword::Offering("Fox".to_string()),
        ] {
            let json = serde_json::to_value(&kw).unwrap();
            let deserialized: Keyword = serde_json::from_value(json.clone()).unwrap();
            assert_eq!(kw, deserialized, "round-trip failed for {json}");
        }
    }

    // ─── Awaken ───────────────────────────────────────────────────────

    #[test]
    fn awaken_from_str_parses_em_dash_format() {
        // Simulates colon_form path: "awaken:4\u{2014}{5}{w}{w}{w}"
        let kw: Keyword = "awaken:4\u{2014}{5}{w}{w}{w}".parse().unwrap();
        match kw {
            Keyword::Awaken { count, cost } => {
                assert_eq!(count, 4);
                assert_eq!(cost, parse_keyword_mana_cost("{5}{W}{W}{W}"));
            }
            other => panic!("expected Keyword::Awaken, got {other:?}"),
        }
    }

    #[test]
    fn awaken_from_str_parses_space_format() {
        // Simulates "awaken:4 {5}{w}{w}{w}" format
        let kw: Keyword = "awaken:4 {5}{w}{w}{w}".parse().unwrap();
        match kw {
            Keyword::Awaken { count, cost } => {
                assert_eq!(count, 4);
                assert_eq!(cost, parse_keyword_mana_cost("{5}{W}{W}{W}"));
            }
            other => panic!("expected Keyword::Awaken, got {other:?}"),
        }
    }

    #[test]
    fn awaken_from_str_count_only() {
        let kw: Keyword = "awaken:3".parse().unwrap();
        match kw {
            Keyword::Awaken { count, cost } => {
                assert_eq!(count, 3);
                assert_eq!(cost, ManaCost::zero());
            }
            other => panic!("expected Keyword::Awaken, got {other:?}"),
        }
    }

    #[test]
    fn awaken_keyword_from_tagged() {
        let data = serde_json::json!({
            "count": 4,
            "cost": {
                "type": "Cost",
                "shards": ["White", "White", "White"],
                "generic": 5
            }
        });
        let kw = keyword_from_tagged("Awaken", &data).unwrap();
        assert_eq!(kw.kind(), KeywordKind::Awaken);
        match kw {
            Keyword::Awaken { count, cost } => {
                assert_eq!(count, 4);
                assert_eq!(cost, parse_keyword_mana_cost("{5}{W}{W}{W}"));
            }
            other => panic!("expected Keyword::Awaken, got {other:?}"),
        }
    }

    #[test]
    fn awaken_kind_round_trip() {
        let kw: Keyword = "awaken:2\u{2014}{3}{u}".parse().unwrap();
        assert_eq!(kw.kind(), KeywordKind::Awaken);
    }
}
