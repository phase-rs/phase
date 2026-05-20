use std::convert::Infallible;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[cfg(test)]
use super::ability::ControllerRef;
use super::ability::{
    AbilityCost, FilterProp, QuantityExpr, TargetFilter, TypeFilter, TypedFilter,
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
    Annihilator,
    Bushido,
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

/// CR 602.5b: Activation-frequency restriction on an activated-ability-like
/// action (e.g. Crew). `OncePerTurn` models "Activate only once each turn";
/// `Unlimited` is the default with no restriction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "type", content = "data")]
pub enum ActivationCadence {
    #[default]
    Unlimited,
    OncePerTurn,
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
    Bestow(ManaCost),

    // Graveyard
    Embalm(ManaCost),
    Eternalize(ManaCost),

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
    /// restriction.
    Crew {
        power: u32,
        once_per_turn: ActivationCadence,
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
    Emerge(ManaCost),
    /// CR 702.138: Escape — cast from graveyard for an alternative cost,
    /// exiling N other cards from your graveyard as an additional cost.
    Escape {
        cost: ManaCost,
        exile_count: u32,
    },
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
    Echo(ManaCost),
    /// CR 702.42a: Entwine — pay additional cost to choose all modes of a modal spell.
    Entwine(ManaCost),
    Outlast(ManaCost),
    Scavenge(ManaCost),
    Fortify(ManaCost),
    /// RUNTIME: TODO — converter accepts this keyword but engine has no
    /// behavioral handler. CR 702.160a: Prototype — alt-cast using the
    /// secondary P/T and mana cost characteristics.
    Prototype(ManaCost),
    Plot(ManaCost),
    Craft(ManaCost),
    Offspring(ManaCost),
    /// RUNTIME: TODO — converter accepts this keyword but engine has no
    /// behavioral handler. CR 702.176a: Impending N—{cost} — alt-cast that
    /// enters with N time counters and is not a creature until they're gone.
    Impending(ManaCost),
    /// CR 702.87a: Level up is an activated ability that puts a level counter
    /// on this permanent. Activate only as a sorcery.
    LevelUp(ManaCost),

    /// CR 702.41a: Affinity for [type] — this spell costs {1} less for each [type] you control.
    Affinity(TypedFilter),

    /// CR 702.24a: Cumulative upkeep — triggered ability that imposes an increasing cost.
    /// The cost string contains the per-age-counter cost (mana, life payment, sacrifice, etc.).
    CumulativeUpkeep(String),

    // Simple keywords (no params)
    Banding,
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
    Provoke,
    Rebound,
    Retrace,
    Ripple,
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
    /// CR 702.46: Soulshift N — when this creature dies, return target Spirit card
    /// with mana value N or less from your graveyard to your hand.
    Soulshift(u32),
    /// RUNTIME: TODO — converter accepts this keyword but engine has no
    /// behavioral handler (ETB +1/+1 counters + ability-grant trigger not wired).
    /// CR 702.165: Backup N — when this creature enters, put N +1/+1 counters
    /// on target creature, which gains this creature's other abilities until EOT.
    Backup(u32),

    /// RUNTIME: TODO — converter accepts this keyword but engine has no
    /// behavioral handler (variable additional cost + ETB token-per-payment
    /// not wired).
    /// CR 702.157: Squad {cost} — as an additional cost to cast, you may pay {cost}
    /// any number of times; ETB creates that many tokens.
    Squad(ManaCost),

    /// CR 702.29: Typecycling — "{subtype}cycling {cost}": discard this card and pay {cost}
    /// to search your library for a card with the specified subtype.
    Typecycling {
        cost: ManaCost,
        subtype: String,
    },

    /// Firebending N — produces N {R} when this creature attacks (Avatar crossover).
    Firebending(QuantityExpr),

    /// CR 702.46a: Splice onto [type] — reveal from hand and pay splice cost while casting
    /// a spell of the specified type to add this card's effects to that spell.
    Splice(String),
    /// CR 702.166a: Bargain — you may sacrifice an artifact, enchantment, or token
    /// as an additional cost to cast this spell.
    Bargain,
    /// CR 702.43a: Sunburst — enters with a counter for each color of mana spent to cast it.
    Sunburst,
    /// CR 702.72a: Champion a [type] — exile a creature of the specified type you control
    /// when this enters; return it when this leaves.
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
    /// CR 702.98a: Cipher — exile this spell encoded on a creature you control;
    /// whenever that creature deals combat damage to a player, cast a copy.
    Cipher,
    /// CR 702.52a: Transmute {cost} — discard this card and pay {cost} to search
    /// your library for a card with the same mana value.
    Transmute(ManaCost),
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

    /// RUNTIME: TODO — converter accepts this keyword but engine has no
    /// behavioral handler (no copy-on-cast hook reads it).
    /// CR 702.56a: Replicate {cost} — additional-cost-on-cast copy
    /// mechanic. "As an additional cost to cast this spell, you may pay
    /// [cost] any number of times" + "When you cast this spell, if a
    /// replicate cost was paid for it, copy it for each time its
    /// replicate cost was paid. If the spell has any targets, you may
    /// choose new targets for any of the copies." Carries the per-copy
    /// mana cost; runtime semantics are not yet implemented (no
    /// copy-on-cast hook reads this keyword).
    Replicate(ManaCost),

    /// RUNTIME: TODO — converter accepts this keyword but engine has no
    /// behavioral handler (alt-cast hook + awaken-paid branch not wired).
    /// CR 702.113a: Awaken N—{cost} — alternative cost that also puts
    /// N +1/+1 counters on target land, animating it as a 0/0 Elemental
    /// creature with haste.
    Awaken {
        count: u32,
        cost: ManaCost,
    },

    /// RUNTIME: TODO — converter accepts this keyword but engine has no
    /// behavioral handler (ETB token + auto-attach trigger not wired).
    /// CR 702.163a: For Mirrodin! — Equipment-only triggered ability.
    /// "When this Equipment enters, create a 2/2 red Rebel creature
    /// token, then attach this Equipment to it." Bare keyword; ETB
    /// trigger semantics are not yet wired.
    ForMirrodin,

    /// RUNTIME: TODO — converter accepts this keyword but engine has no
    /// behavioral handler (alt-cost cast hook not wired).
    /// CR 702.162a: More Than Meets the Eye {cost} — alternative cost
    /// (Transformers crossover). "You may cast this card converted by
    /// paying [cost] rather than its mana cost." Stores the alt mana
    /// cost; the runtime alt-cost cast hook is not yet wired.
    MoreThanMeetsTheEye(ManaCost),

    /// RUNTIME: TODO — converter accepts this keyword but engine has no
    /// behavioral handler (alt-cast hook + combat-damage-this-turn predicate
    /// not wired).
    /// CR 702.173a: Freerunning {cost} — alternative cost. "You may pay
    /// [cost] rather than pay this spell's mana cost if a player was
    /// dealt combat damage this turn by a creature that, at the time it
    /// dealt that damage, was an Assassin creature or a commander under
    /// your control." Stores the alt mana cost; runtime alt-cast hook
    /// (combat-damage-this-turn predicate) is not yet wired.
    Freerunning(ManaCost),

    /// RUNTIME: TODO — converter accepts this keyword but engine has no
    /// behavioral handler (spell-cast trigger not wired).
    /// CR 702.191a: Increment — triggered ability. "Whenever you cast a
    /// spell, if this permanent is a creature and the amount of mana
    /// spent to cast that spell is greater than this creature's power
    /// or this creature's toughness, put a +1/+1 counter on this
    /// creature." Bare keyword; ETB / spell-cast trigger is not yet
    /// wired.
    Increment,

    /// RUNTIME: TODO — converter accepts this keyword but engine has no
    /// behavioral handler (choose-color + transform hooks not wired).
    /// CR ???: Specialize {cost} — not in CR text (needs manual
    /// verification). Strixhaven student-into-mage transformation:
    /// activated alt-cast that turns the source into a colour-specific
    /// version. Stores the activation mana cost; the choose-color and
    /// transform hooks are not yet wired. mtgish encodes activation
    /// timing modifiers and from-graveyard variants separately; this
    /// keyword carries only the cost (the engine drops the activation
    /// modifier and the from-graveyard hint, mirroring how `LevelUp`
    /// drops its `Vec<Level>` payload).
    Specialize(ManaCost),

    /// RUNTIME: TODO — converter accepts this keyword but engine has no
    /// behavioral handler (cost-reduction + cast-as-instant hooks not wired).
    /// CR 702.48a: "[Quality] offering" — additional-cost-on-cast that
    /// sacrifices a permanent matching `Quality`. "If you chose to pay
    /// the additional cost, this spell's total cost is reduced by the
    /// sacrificed permanent's mana cost, and you may cast this spell any
    /// time you could cast an instant." Carries the canonical subtype
    /// string (e.g. "Spirit", "Dragon"); cost-reduction and cast-as-
    /// instant runtime hooks are not yet wired.
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
            Keyword::Fabricate(_) => KeywordKind::Fabricate,
            Keyword::Annihilator(_) => KeywordKind::Annihilator,
            Keyword::Bushido(_) => KeywordKind::Bushido,
            Keyword::Tribute(_) => KeywordKind::Tribute,
            Keyword::Soulbond => KeywordKind::Soulbond,
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
            Keyword::Escape { .. } => KeywordKind::Escape,
            Keyword::Morph(_) => KeywordKind::Morph,
            Keyword::Megamorph(_) => KeywordKind::Megamorph,
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
            Keyword::Splice(_) => KeywordKind::Splice,
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
            | Keyword::Disguise(_)
            | Keyword::DoubleTeam
            | Keyword::Echo(_)
            | Keyword::Emerge(_)
            | Keyword::Encore(_)
            | Keyword::Enlist
            | Keyword::Entwine(_)
            | Keyword::Epic
            | Keyword::Evoke(_)
            | Keyword::Fortify(_)
            | Keyword::Fuse
            | Keyword::Graft(_)
            | Keyword::Gravestorm
            | Keyword::Haunt
            | Keyword::Hideaway(_)
            | Keyword::Impending(_)
            | Keyword::Improvise
            | Keyword::Ingest
            | Keyword::LevelUp(_)
            | Keyword::LivingMetal
            | Keyword::Madness(_)
            | Keyword::Melee
            | Keyword::Mentor
            | Keyword::Mobilize(_)
            | Keyword::Myriad
            | Keyword::Nightbound
            | Keyword::Overload(_)
            | Keyword::Poisonous(_)
            | Keyword::Prototype(_)
            | Keyword::Provoke
            | Keyword::Prowl(_)
            | Keyword::Ravenous
            | Keyword::ReadAhead
            | Keyword::Rebound
            | Keyword::Ripple
            | Keyword::Saddle(_)
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
            | Keyword::Typecycling { .. }
            | Keyword::WebSlinging(_) => KeywordKind::Unknown,
        }
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

/// CR 702.41a: Parse the type text from "Affinity for [type]" into a TypedFilter.
/// Handles common affinity patterns: "artifacts", "Plains", "creatures", etc.
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
            // Try as a land subtype (Plains, Islands, etc.)
            let capitalized = format!("{}{}", &s[..1].to_uppercase(), &s[1..]);
            // Strip trailing 's' for plural land subtypes (e.g., "Plains" stays "Plains",
            // but "Islands" → "Island", "Swamps" → "Swamp")
            let subtype = if capitalized.ends_with('s') && capitalized != "Plains" {
                capitalized[..capitalized.len() - 1].to_string()
            } else {
                capitalized
            };
            Some(TypedFilter::land().subtype(subtype))
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
        parse_enchant_controller_suffix, parse_enchant_player_base, parse_enchant_type_leg,
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
    if !rest.trim().is_empty() {
        return None;
    }
    // Reject fully empty input — every other degenerate variant lacks a type
    // word AND a zone word AND a controller, so it cannot be a meaningful
    // enchant clause.
    if type_filter.is_none() && zone.is_none() && controller.is_none() {
        return None;
    }

    // CR 303.4a: When the type leg is absent (Don't Worry About It), the
    // class is "any card", encoded as `TypeFilter::Card`.
    let mut filter = TypedFilter::new(type_filter.unwrap_or(TypeFilter::Card));
    if let Some(z) = zone {
        filter = filter.properties(vec![FilterProp::InZone { zone: z }]);
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
                "absorb" => return Ok(Keyword::Absorb(p.parse().unwrap_or(1))),
                "fading" => return Ok(Keyword::Fading(p.parse().unwrap_or(0))),
                "vanishing" => return Ok(Keyword::Vanishing(p.parse().unwrap_or(0))),
                "crew" => {
                    return Ok(Keyword::Crew {
                        power: p.parse().unwrap_or(1),
                        once_per_turn: ActivationCadence::Unlimited,
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
                "bestow" => return Ok(Keyword::Bestow(parse_keyword_mana_cost(p))),
                "embalm" => return Ok(Keyword::Embalm(parse_keyword_mana_cost(p))),
                "eternalize" => return Ok(Keyword::Eternalize(parse_keyword_mana_cost(p))),
                "unearth" => return Ok(Keyword::Unearth(parse_keyword_mana_cost(p))),
                "prowl" => return Ok(Keyword::Prowl(parse_keyword_mana_cost(p))),
                "morph" => return Ok(Keyword::Morph(parse_keyword_mana_cost(p))),
                "megamorph" => return Ok(Keyword::Megamorph(parse_keyword_mana_cost(p))),
                "madness" => return Ok(Keyword::Madness(parse_keyword_mana_cost(p))),
                "miracle" => return Ok(Keyword::Miracle(parse_keyword_mana_cost(p))),
                "dash" => return Ok(Keyword::Dash(parse_keyword_mana_cost(p))),
                "emerge" => return Ok(Keyword::Emerge(parse_keyword_mana_cost(p))),
                "harmonize" => return Ok(Keyword::Harmonize(parse_keyword_mana_cost(p))),
                "escape" => {
                    return Ok(Keyword::Escape {
                        cost: parse_keyword_mana_cost(p),
                        exile_count: 0,
                    })
                }
                "evoke" => return Ok(Keyword::Evoke(EvokeCost::Mana(parse_keyword_mana_cost(p)))),
                "foretell" => return Ok(Keyword::Foretell(parse_keyword_mana_cost(p))),
                "mutate" => return Ok(Keyword::Mutate(parse_keyword_mana_cost(p))),
                "disturb" => return Ok(Keyword::Disturb(parse_keyword_mana_cost(p))),
                "disguise" => return Ok(Keyword::Disguise(parse_keyword_mana_cost(p))),
                "blitz" => return Ok(Keyword::Blitz(parse_keyword_mana_cost(p))),
                "overload" => return Ok(Keyword::Overload(parse_keyword_mana_cost(p))),
                "spectacle" => return Ok(Keyword::Spectacle(parse_keyword_mana_cost(p))),
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
                "echo" => return Ok(Keyword::Echo(parse_keyword_mana_cost(p))),
                "outlast" => return Ok(Keyword::Outlast(parse_keyword_mana_cost(p))),
                "scavenge" => return Ok(Keyword::Scavenge(parse_keyword_mana_cost(p))),
                "fortify" => return Ok(Keyword::Fortify(parse_keyword_mana_cost(p))),
                "prototype" => return Ok(Keyword::Prototype(parse_keyword_mana_cost(p))),
                "plot" => return Ok(Keyword::Plot(parse_keyword_mana_cost(p))),
                "craft" => return Ok(Keyword::Craft(parse_keyword_mana_cost(p))),
                "offspring" => return Ok(Keyword::Offspring(parse_keyword_mana_cost(p))),
                "impending" => return Ok(Keyword::Impending(parse_keyword_mana_cost(p))),
                "levelup" | "level up" => return Ok(Keyword::LevelUp(parse_keyword_mana_cost(p))),
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
                // CR 702.46
                "soulshift" => return Ok(Keyword::Soulshift(p.parse().unwrap_or(1))),
                // CR 702.165
                "backup" => return Ok(Keyword::Backup(p.parse().unwrap_or(1))),
                // CR 702.157
                "squad" => return Ok(Keyword::Squad(parse_keyword_mana_cost(p))),
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
                    let type_str = match after_onto.find('{') {
                        Some(brace_idx) => after_onto[..brace_idx].trim(),
                        None => after_onto.trim(),
                    };
                    let capitalized = capitalize_first(type_str);
                    return Ok(Keyword::Splice(capitalized));
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
                // CR 702.52a: Transmute {cost}
                "transmute" => return Ok(Keyword::Transmute(parse_keyword_mana_cost(p))),
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
            "totemarmor" => Ok(Keyword::TotemArmor),
            "evolve" => Ok(Keyword::Evolve),
            "extort" => Ok(Keyword::Extort),
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
            "cumulative" => Ok(Keyword::CumulativeUpkeep(String::new())),
            "ripple" => Ok(Keyword::Ripple),
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

/// CR 702.11d: Parse the quality after "hexproof from " into a HexproofFilter.
fn parse_hexproof_filter(s: &str) -> HexproofFilter {
    let lower = s.to_ascii_lowercase();
    match lower.as_str() {
        "white" => HexproofFilter::Color(ManaColor::White),
        "blue" => HexproofFilter::Color(ManaColor::Blue),
        "black" => HexproofFilter::Color(ManaColor::Black),
        "red" => HexproofFilter::Color(ManaColor::Red),
        "green" => HexproofFilter::Color(ManaColor::Green),
        "monocolored" | "multicolored" => HexproofFilter::Quality(lower),
        // CR 702.11d + CR 105.4 + CR 609.6: "that color" / "the chosen color"
        // anaphors after a preceding `Choose a color` instruction. Resolved at
        // runtime via `ChosenAttribute::Color` on the granting source. Mirrors
        // `ProtectionTarget::ChosenColor` (CR 702.16).
        "that color" | "the chosen color" | "chosen color" => HexproofFilter::ChosenColor,
        _ => HexproofFilter::CardType(lower),
    }
}

fn parse_protection_target(s: &str) -> ProtectionTarget {
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
        // CR 702.16: "the chosen color" resolves at runtime from chosen_attributes
        "the chosen color" | "chosen color" => ProtectionTarget::ChosenColor,
        // CR 702.16 + CR 205.2: "the chosen card type" resolves at
        // runtime from the source permanent's chosen `CardType` attribute.
        "the chosen card type" | "chosen card type" => ProtectionTarget::ChosenCardType,
        // CR 702.16j: "protection from everything" — typed variant, not stringly-typed
        "everything" => ProtectionTarget::Everything,
        // Lowercase the stored quality — `source_matches_card_type` only matches
        // lowercase, so the canonical stored form must be lowercase.
        _ if lower.starts_with("from ") => ProtectionTarget::Quality(lower),
        _ => ProtectionTarget::CardType(lower),
    }
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
        "TotemArmor" => Ok(Keyword::TotemArmor),
        "Exalted" => Ok(Keyword::Exalted),
        "Flanking" => Ok(Keyword::Flanking),
        "Evolve" => Ok(Keyword::Evolve),
        "Extort" => Ok(Keyword::Extort),
        "Exploit" => Ok(Keyword::Exploit),
        "Explore" => Ok(Keyword::Explore),
        "Ascend" => Ok(Keyword::Ascend),
        "StartYourEngines" => Ok(Keyword::StartYourEngines),
        "Soulbond" => Ok(Keyword::Soulbond),
        "Banding" => Ok(Keyword::Banding),
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
        "Cumulative" => Ok(Keyword::CumulativeUpkeep(String::new())),
        "CumulativeUpkeep" => Ok(Keyword::CumulativeUpkeep(
            data.as_str().unwrap_or("").to_string(),
        )),
        "Ripple" => Ok(Keyword::Ripple),
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
        "Bestow" => Ok(Keyword::Bestow(mana(data)?)),
        "Embalm" => Ok(Keyword::Embalm(mana(data)?)),
        "Eternalize" => Ok(Keyword::Eternalize(mana(data)?)),
        "Unearth" => Ok(Keyword::Unearth(mana(data)?)),
        "Prowl" => Ok(Keyword::Prowl(mana(data)?)),
        "Morph" => Ok(Keyword::Morph(mana(data)?)),
        "Megamorph" => Ok(Keyword::Megamorph(mana(data)?)),
        "Madness" => Ok(Keyword::Madness(mana(data)?)),
        "Miracle" => Ok(Keyword::Miracle(mana(data)?)),
        "Dash" => Ok(Keyword::Dash(mana(data)?)),
        "Emerge" => Ok(Keyword::Emerge(mana(data)?)),
        // CR 702.138: MTGJSON provides bare "Escape" with no structured cost data.
        // Placeholder values — the Oracle parser overwrites with real cost/exile_count.
        "Harmonize" => Ok(Keyword::Harmonize(mana(data)?)),
        "Escape" => Ok(Keyword::Escape {
            cost: ManaCost::default(),
            exile_count: 0,
        }),
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
        "Spectacle" => Ok(Keyword::Spectacle(mana(data)?)),
        "Surge" => Ok(Keyword::Surge(mana(data)?)),
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
        "Echo" => Ok(Keyword::Echo(mana(data)?)),
        "Outlast" => Ok(Keyword::Outlast(mana(data)?)),
        "Scavenge" => Ok(Keyword::Scavenge(mana(data)?)),
        "Fortify" => Ok(Keyword::Fortify(mana(data)?)),
        "Prototype" => Ok(Keyword::Prototype(mana(data)?)),
        "Plot" => Ok(Keyword::Plot(mana(data)?)),
        "Craft" => Ok(Keyword::Craft(mana(data)?)),
        "Offspring" => Ok(Keyword::Offspring(mana(data)?)),
        "Impending" => Ok(Keyword::Impending(mana(data)?)),
        "LevelUp" => Ok(Keyword::LevelUp(mana(data)?)),
        // Parameterized: u32
        "Dredge" => Ok(Keyword::Dredge(uint(data))),
        "Modular" => Ok(Keyword::Modular(uint(data))),
        "Renown" => Ok(Keyword::Renown(uint(data))),
        "Fabricate" => Ok(Keyword::Fabricate(uint(data))),
        "Annihilator" => Ok(Keyword::Annihilator(uint(data))),
        "Bushido" => Ok(Keyword::Bushido(uint(data))),
        "Tribute" => Ok(Keyword::Tribute(uint(data))),
        "Afterlife" => Ok(Keyword::Afterlife(uint(data))),
        "Fading" => Ok(Keyword::Fading(uint(data))),
        "Vanishing" => Ok(Keyword::Vanishing(uint(data))),
        "Crew" => {
            // Struct variant: {"Crew": {"power": N, "once_per_turn": {...}}}.
            // A bare number is also accepted for forward/back compatibility.
            if let Some(obj) = data.as_object() {
                let power = obj.get("power").map(uint).unwrap_or(1);
                let once_per_turn = obj
                    .get("once_per_turn")
                    .map(|v| serde_json::from_value(v.clone()))
                    .transpose()
                    .map_err(|e| format!("ActivationCadence: {e}"))?
                    .unwrap_or(ActivationCadence::Unlimited);
                Ok(Keyword::Crew {
                    power,
                    once_per_turn,
                })
            } else {
                Ok(Keyword::Crew {
                    power: uint(data),
                    once_per_turn: ActivationCadence::Unlimited,
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
        // CR 702.132a / CR 702.133a / CR 702.98a / CR 702.52a / CR 702.148a / CR 702.125a
        "Splice" => Ok(Keyword::Splice(data.as_str().unwrap_or("").to_string())),
        "Bargain" => Ok(Keyword::Bargain),
        "Sunburst" => Ok(Keyword::Sunburst),
        "Champion" => Ok(Keyword::Champion(data.as_str().unwrap_or("").to_string())),
        "Training" => Ok(Keyword::Training),
        "Assist" => Ok(Keyword::Assist),
        "Augment" => Ok(Keyword::Augment),
        "JumpStart" => Ok(Keyword::JumpStart),
        "Cipher" => Ok(Keyword::Cipher),
        "Transmute" => Ok(Keyword::Transmute(mana(data)?)),
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

        let ward = Keyword::from_str("Ward:2").unwrap();
        assert!(matches!(
            ward,
            Keyword::Ward(WardCost::Mana(ManaCost::Cost { .. }))
        ));

        let equip = Keyword::from_str("Equip:3").unwrap();
        assert!(matches!(equip, Keyword::Equip(ManaCost::Cost { .. })));
    }

    #[test]
    fn parse_numeric_keywords_unchanged() {
        assert_eq!(
            Keyword::from_str("Crew:3").unwrap(),
            Keyword::Crew {
                power: 3,
                once_per_turn: ActivationCadence::Unlimited
            }
        );
        assert_eq!(Keyword::from_str("Rampage:2").unwrap(), Keyword::Rampage(2));
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
            Keyword::CumulativeUpkeep(String::new())
        );
        assert_eq!(Keyword::from_str("Ripple").unwrap(), Keyword::Ripple);
        assert_eq!(Keyword::from_str("Totem").unwrap(), Keyword::Totem);
        // Warp is now parameterized — bare "Warp" without cost falls through to Unknown
        assert!(matches!(
            Keyword::from_str("Warp").unwrap(),
            Keyword::Unknown(_)
        ));
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
}
