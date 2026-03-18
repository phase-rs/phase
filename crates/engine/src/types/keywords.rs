use std::convert::Infallible;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::ability::{ControllerRef, TargetFilter, TypedFilter};
use super::mana::{ManaColor, ManaCost};

/// What a Protection keyword protects from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ProtectionTarget {
    Color(ManaColor),
    CardType(String),
    Quality(String),
    Multicolored,
}

/// All MTG keywords as typed enum variants.
/// Simple (unit) variants for keywords with no parameters.
/// Parameterized variants carry associated data (ManaCost for costs, amounts, etc.).
/// Unknown captures any unrecognized keyword string for forward compatibility.
///
/// Custom Deserialize: accepts both the typed externally-tagged format (new)
/// and plain "Name:Param" strings (legacy card-data.json).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub enum Keyword {
    // Evasion / Combat
    Flying,
    FirstStrike,
    DoubleStrike,
    Trample,
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

    // Damage modification
    Wither,
    Infect,
    Afflict,

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
        counter_type: String,
        count: u32,
    },

    // Equipment / attachment
    Reconfigure(ManaCost),
    LivingWeapon,
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
    Cycling(ManaCost),
    Flashback(ManaCost),
    Ward(ManaCost),
    Equip(ManaCost),
    Landwalk(String),
    Rampage(u32),
    Absorb(u32),
    Crew(u32),
    Partner(Option<String>),
    Companion(String),
    Ninjutsu(ManaCost),

    // Additional common keywords with ManaCost
    Prowl(ManaCost),
    Morph(ManaCost),
    Megamorph(ManaCost),
    Madness(ManaCost),
    Dash(ManaCost),
    Emerge(ManaCost),
    Escape(ManaCost),
    Evoke(ManaCost),
    Foretell(ManaCost),
    Mutate(ManaCost),
    Disturb(ManaCost),
    Disguise(ManaCost),
    Blitz(ManaCost),
    Overload(ManaCost),
    Spectacle(ManaCost),
    Surge(ManaCost),
    Encore(ManaCost),
    Buyback(ManaCost),
    Echo(ManaCost),
    Outlast(ManaCost),
    Scavenge(ManaCost),
    Fortify(ManaCost),
    Prototype(ManaCost),
    Plot(ManaCost),
    Craft(ManaCost),
    Offspring(ManaCost),
    Impending(ManaCost),

    // Simple keywords (no params)
    Banding,
    Cumulative,
    Epic,
    Fuse,
    Gravestorm,
    Haunt,
    Hideaway,
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
    Suspend,
    Totem,
    Warp,
    Gift,
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
    Bloodthirst(u32),
    Amplify(u32),
    Graft(u32),
    Devour(u32),

    /// CR 702.164: Toxic N — when this creature deals combat damage to a player,
    /// that player gets N poison counters.
    Toxic(u32),
    /// CR 702.173: Saddle N — tap creatures with total power N+ to saddle this Mount.
    Saddle(u32),
    /// CR 702.46: Soulshift N — when this creature dies, return target Spirit card
    /// with mana value N or less from your graveyard to your hand.
    Soulshift(u32),
    /// CR 702.165: Backup N — when this creature enters, put N +1/+1 counters
    /// on target creature, which gains this creature's other abilities until EOT.
    Backup(u32),

    /// CR 702.157: Squad {cost} — as an additional cost to cast, you may pay {cost}
    /// any number of times; ETB creates that many tokens.
    Squad(ManaCost),

    /// CR 702.29: Typecycling — "{subtype}cycling {cost}": discard this card and pay {cost}
    /// to search your library for a card with the specified subtype.
    Typecycling {
        cost: ManaCost,
        subtype: String,
    },

    /// Fallback for unrecognized keywords.
    Unknown(String),
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

/// Parse an enchant target string into a simple TargetFilter.
fn parse_enchant_target(s: &str) -> TargetFilter {
    use super::ability::TypeFilter;

    let lower = s.to_ascii_lowercase();
    let (controller, base) = if let Some(rest) = lower.strip_suffix(" you control") {
        (Some(ControllerRef::You), rest.trim())
    } else if let Some(rest) = lower.strip_suffix(" an opponent controls") {
        (Some(ControllerRef::Opponent), rest.trim())
    } else if let Some(rest) = lower.strip_suffix(" opponent controls") {
        (Some(ControllerRef::Opponent), rest.trim())
    } else {
        (None, lower.trim())
    };

    let type_filter = match base {
        "creature" => Some(TypeFilter::Creature),
        "land" => Some(TypeFilter::Land),
        "artifact" => Some(TypeFilter::Artifact),
        "enchantment" => Some(TypeFilter::Enchantment),
        "planeswalker" => Some(TypeFilter::Planeswalker),
        "permanent" => Some(TypeFilter::Permanent),
        _ => None,
    };

    match type_filter {
        Some(tf) => {
            let mut filter = TypedFilter::new(tf);
            if let Some(controller) = controller {
                filter = filter.controller(controller);
            }
            TargetFilter::Typed(filter)
        }
        // If not a recognized type, use a typed filter with the string as subtype
        None => {
            let mut filter = TypedFilter::default().subtype(base.to_string());
            if let Some(controller) = controller {
                filter = filter.controller(controller);
            }
            TargetFilter::Typed(filter)
        }
    }
}

/// Parse an EtbCounter parameter string (e.g., "P1P1:1") into counter_type and count.
fn parse_etb_counter(s: &str) -> (String, u32) {
    if let Some(idx) = s.rfind(':') {
        let counter_type = s[..idx].to_string();
        let count = s[idx + 1..].parse::<u32>().unwrap_or(1);
        (counter_type, count)
    } else {
        (s.to_string(), 1)
    }
}

impl FromStr for Keyword {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Split on first colon for parameterized keywords
        let (name, param) = match s.find(':') {
            Some(idx) => (&s[..idx], Some(s[idx + 1..].to_string())),
            None => (s, None),
        };

        let name_lower = name.to_ascii_lowercase();

        // If there's a param, try parameterized keywords first
        if let Some(ref p) = param {
            match name_lower.as_str() {
                "protection" => return Ok(Keyword::Protection(parse_protection_target(p))),
                "kicker" => return Ok(Keyword::Kicker(parse_keyword_mana_cost(p))),
                "cycling" => return Ok(Keyword::Cycling(parse_keyword_mana_cost(p))),
                "flashback" => return Ok(Keyword::Flashback(parse_keyword_mana_cost(p))),
                "ward" => return Ok(Keyword::Ward(parse_keyword_mana_cost(p))),
                "equip" => return Ok(Keyword::Equip(parse_keyword_mana_cost(p))),
                "landwalk" => return Ok(Keyword::Landwalk(p.clone())),
                "rampage" => return Ok(Keyword::Rampage(p.parse().unwrap_or(1))),
                "bushido" => return Ok(Keyword::Bushido(p.parse().unwrap_or(1))),
                "absorb" => return Ok(Keyword::Absorb(p.parse().unwrap_or(1))),
                "fading" => return Ok(Keyword::Fading(p.parse().unwrap_or(0))),
                "vanishing" => return Ok(Keyword::Vanishing(p.parse().unwrap_or(0))),
                "crew" => return Ok(Keyword::Crew(p.parse().unwrap_or(1))),
                "partner" => return Ok(Keyword::Partner(Some(p.clone()))),
                "companion" => return Ok(Keyword::Companion(p.clone())),
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
                "dash" => return Ok(Keyword::Dash(parse_keyword_mana_cost(p))),
                "emerge" => return Ok(Keyword::Emerge(parse_keyword_mana_cost(p))),
                "escape" => return Ok(Keyword::Escape(parse_keyword_mana_cost(p))),
                "evoke" => return Ok(Keyword::Evoke(parse_keyword_mana_cost(p))),
                "foretell" => return Ok(Keyword::Foretell(parse_keyword_mana_cost(p))),
                "mutate" => return Ok(Keyword::Mutate(parse_keyword_mana_cost(p))),
                "disturb" => return Ok(Keyword::Disturb(parse_keyword_mana_cost(p))),
                "disguise" => return Ok(Keyword::Disguise(parse_keyword_mana_cost(p))),
                "blitz" => return Ok(Keyword::Blitz(parse_keyword_mana_cost(p))),
                "overload" => return Ok(Keyword::Overload(parse_keyword_mana_cost(p))),
                "spectacle" => return Ok(Keyword::Spectacle(parse_keyword_mana_cost(p))),
                "surge" => return Ok(Keyword::Surge(parse_keyword_mana_cost(p))),
                "encore" => return Ok(Keyword::Encore(parse_keyword_mana_cost(p))),
                "buyback" => return Ok(Keyword::Buyback(parse_keyword_mana_cost(p))),
                "echo" => return Ok(Keyword::Echo(parse_keyword_mana_cost(p))),
                "outlast" => return Ok(Keyword::Outlast(parse_keyword_mana_cost(p))),
                "scavenge" => return Ok(Keyword::Scavenge(parse_keyword_mana_cost(p))),
                "fortify" => return Ok(Keyword::Fortify(parse_keyword_mana_cost(p))),
                "prototype" => return Ok(Keyword::Prototype(parse_keyword_mana_cost(p))),
                "plot" => return Ok(Keyword::Plot(parse_keyword_mana_cost(p))),
                "craft" => return Ok(Keyword::Craft(parse_keyword_mana_cost(p))),
                "offspring" => return Ok(Keyword::Offspring(parse_keyword_mana_cost(p))),
                "impending" => return Ok(Keyword::Impending(parse_keyword_mana_cost(p))),
                "poisonous" => return Ok(Keyword::Poisonous(p.parse().unwrap_or(1))),
                "bloodthirst" => return Ok(Keyword::Bloodthirst(p.parse().unwrap_or(1))),
                "amplify" => return Ok(Keyword::Amplify(p.parse().unwrap_or(1))),
                "graft" => return Ok(Keyword::Graft(p.parse().unwrap_or(1))),
                "devour" => return Ok(Keyword::Devour(p.parse().unwrap_or(1))),
                // CR 702.164
                "toxic" => return Ok(Keyword::Toxic(p.parse().unwrap_or(1))),
                // CR 702.173
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
                "afflict" => return Ok(Keyword::Afflict),
                "enchant" => return Ok(Keyword::Enchant(parse_enchant_target(p))),
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

        // Simple (unit) keywords -- case-insensitive, space-normalized match
        // Stripping spaces lets PascalCase ("FirstStrike") and Oracle text ("first strike") both match.
        let name_nospace = name_lower.replace(' ', "");
        match name_nospace.as_str() {
            "flying" => Ok(Keyword::Flying),
            "firststrike" => Ok(Keyword::FirstStrike),
            "doublestrike" => Ok(Keyword::DoubleStrike),
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
            "afflict" => Ok(Keyword::Afflict),
            "prowess" => Ok(Keyword::Prowess),
            "undying" => Ok(Keyword::Undying),
            "persist" => Ok(Keyword::Persist),
            "cascade" => Ok(Keyword::Cascade),
            "convoke" => Ok(Keyword::Convoke),
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
            "totemarmor" => Ok(Keyword::TotemArmor),
            "evolve" => Ok(Keyword::Evolve),
            "extort" => Ok(Keyword::Extort),
            "exploit" => Ok(Keyword::Exploit),
            "explore" => Ok(Keyword::Explore),
            "ascend" => Ok(Keyword::Ascend),
            "soulbond" => Ok(Keyword::Soulbond),
            "partner" => Ok(Keyword::Partner(None)),
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
            "suspend" => Ok(Keyword::Suspend),
            "gift" => Ok(Keyword::Gift),
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
            "hideaway" => Ok(Keyword::Hideaway),
            "cumulative" => Ok(Keyword::Cumulative),
            "ripple" => Ok(Keyword::Ripple),
            "totem" => Ok(Keyword::Totem),
            "warp" => Ok(Keyword::Warp),
            _ => Ok(Keyword::Unknown(s.to_string())),
        }
    }
}

fn parse_protection_target(s: &str) -> ProtectionTarget {
    let lower = s.to_ascii_lowercase();
    match lower.as_str() {
        "white" => ProtectionTarget::Color(ManaColor::White),
        "blue" => ProtectionTarget::Color(ManaColor::Blue),
        "black" => ProtectionTarget::Color(ManaColor::Black),
        "red" => ProtectionTarget::Color(ManaColor::Red),
        "green" => ProtectionTarget::Color(ManaColor::Green),
        "multicolored" => ProtectionTarget::Multicolored,
        _ if lower.starts_with("from ") => ProtectionTarget::Quality(s.to_string()),
        _ => ProtectionTarget::CardType(s.to_string()),
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

    match variant {
        "Flying" => Ok(Keyword::Flying),
        "FirstStrike" => Ok(Keyword::FirstStrike),
        "DoubleStrike" => Ok(Keyword::DoubleStrike),
        "Trample" => Ok(Keyword::Trample),
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
        "Afflict" => Ok(Keyword::Afflict),
        "Prowess" => Ok(Keyword::Prowess),
        "Undying" => Ok(Keyword::Undying),
        "Persist" => Ok(Keyword::Persist),
        "Cascade" => Ok(Keyword::Cascade),
        "Convoke" => Ok(Keyword::Convoke),
        "Delve" => Ok(Keyword::Delve),
        "Devoid" => Ok(Keyword::Devoid),
        "Changeling" => Ok(Keyword::Changeling),
        "Phasing" => Ok(Keyword::Phasing),
        "Battlecry" => Ok(Keyword::Battlecry),
        "Decayed" => Ok(Keyword::Decayed),
        "Unleash" => Ok(Keyword::Unleash),
        "Riot" => Ok(Keyword::Riot),
        "LivingWeapon" => Ok(Keyword::LivingWeapon),
        "TotemArmor" => Ok(Keyword::TotemArmor),
        "Exalted" => Ok(Keyword::Exalted),
        "Flanking" => Ok(Keyword::Flanking),
        "Evolve" => Ok(Keyword::Evolve),
        "Extort" => Ok(Keyword::Extort),
        "Exploit" => Ok(Keyword::Exploit),
        "Explore" => Ok(Keyword::Explore),
        "Ascend" => Ok(Keyword::Ascend),
        "Soulbond" => Ok(Keyword::Soulbond),
        "Banding" => Ok(Keyword::Banding),
        "Epic" => Ok(Keyword::Epic),
        "Fuse" => Ok(Keyword::Fuse),
        "Gravestorm" => Ok(Keyword::Gravestorm),
        "Haunt" => Ok(Keyword::Haunt),
        "Hideaway" => Ok(Keyword::Hideaway),
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
        "Suspend" => Ok(Keyword::Suspend),
        "Gift" => Ok(Keyword::Gift),
        "Spree" => Ok(Keyword::Spree),
        "Ravenous" => Ok(Keyword::Ravenous),
        "Daybound" => Ok(Keyword::Daybound),
        "Nightbound" => Ok(Keyword::Nightbound),
        "Enlist" => Ok(Keyword::Enlist),
        "ReadAhead" => Ok(Keyword::ReadAhead),
        "Compleated" => Ok(Keyword::Compleated),
        "Conspire" => Ok(Keyword::Conspire),
        "Demonstrate" => Ok(Keyword::Demonstrate),
        "Dethrone" => Ok(Keyword::Dethrone),
        "DoubleTeam" => Ok(Keyword::DoubleTeam),
        "LivingMetal" => Ok(Keyword::LivingMetal),
        "Cumulative" => Ok(Keyword::Cumulative),
        "Ripple" => Ok(Keyword::Ripple),
        "Totem" => Ok(Keyword::Totem),
        "Warp" => Ok(Keyword::Warp),
        // Parameterized: ManaCost
        "Kicker" => Ok(Keyword::Kicker(mana(data)?)),
        "Cycling" => Ok(Keyword::Cycling(mana(data)?)),
        "Flashback" => Ok(Keyword::Flashback(mana(data)?)),
        "Ward" => Ok(Keyword::Ward(mana(data)?)),
        "Equip" => Ok(Keyword::Equip(mana(data)?)),
        "Ninjutsu" => Ok(Keyword::Ninjutsu(mana(data)?)),
        "Reconfigure" => Ok(Keyword::Reconfigure(mana(data)?)),
        "Bestow" => Ok(Keyword::Bestow(mana(data)?)),
        "Embalm" => Ok(Keyword::Embalm(mana(data)?)),
        "Eternalize" => Ok(Keyword::Eternalize(mana(data)?)),
        "Unearth" => Ok(Keyword::Unearth(mana(data)?)),
        "Prowl" => Ok(Keyword::Prowl(mana(data)?)),
        "Morph" => Ok(Keyword::Morph(mana(data)?)),
        "Megamorph" => Ok(Keyword::Megamorph(mana(data)?)),
        "Madness" => Ok(Keyword::Madness(mana(data)?)),
        "Dash" => Ok(Keyword::Dash(mana(data)?)),
        "Emerge" => Ok(Keyword::Emerge(mana(data)?)),
        "Escape" => Ok(Keyword::Escape(mana(data)?)),
        "Evoke" => Ok(Keyword::Evoke(mana(data)?)),
        "Foretell" => Ok(Keyword::Foretell(mana(data)?)),
        "Mutate" => Ok(Keyword::Mutate(mana(data)?)),
        "Disturb" => Ok(Keyword::Disturb(mana(data)?)),
        "Disguise" => Ok(Keyword::Disguise(mana(data)?)),
        "Blitz" => Ok(Keyword::Blitz(mana(data)?)),
        "Overload" => Ok(Keyword::Overload(mana(data)?)),
        "Spectacle" => Ok(Keyword::Spectacle(mana(data)?)),
        "Surge" => Ok(Keyword::Surge(mana(data)?)),
        "Encore" => Ok(Keyword::Encore(mana(data)?)),
        "Buyback" => Ok(Keyword::Buyback(mana(data)?)),
        "Echo" => Ok(Keyword::Echo(mana(data)?)),
        "Outlast" => Ok(Keyword::Outlast(mana(data)?)),
        "Scavenge" => Ok(Keyword::Scavenge(mana(data)?)),
        "Fortify" => Ok(Keyword::Fortify(mana(data)?)),
        "Prototype" => Ok(Keyword::Prototype(mana(data)?)),
        "Plot" => Ok(Keyword::Plot(mana(data)?)),
        "Craft" => Ok(Keyword::Craft(mana(data)?)),
        "Offspring" => Ok(Keyword::Offspring(mana(data)?)),
        "Impending" => Ok(Keyword::Impending(mana(data)?)),
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
        "Crew" => Ok(Keyword::Crew(uint(data))),
        "Rampage" => Ok(Keyword::Rampage(uint(data))),
        "Absorb" => Ok(Keyword::Absorb(uint(data))),
        "Poisonous" => Ok(Keyword::Poisonous(uint(data))),
        "Bloodthirst" => Ok(Keyword::Bloodthirst(uint(data))),
        "Amplify" => Ok(Keyword::Amplify(uint(data))),
        "Graft" => Ok(Keyword::Graft(uint(data))),
        "Devour" => Ok(Keyword::Devour(uint(data))),
        // CR 702.164 / CR 702.173 / CR 702.46 / CR 702.165
        "Toxic" => Ok(Keyword::Toxic(uint(data))),
        "Saddle" => Ok(Keyword::Saddle(uint(data))),
        "Soulshift" => Ok(Keyword::Soulshift(uint(data))),
        "Backup" => Ok(Keyword::Backup(uint(data))),
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
        "Protection" => {
            let pt: ProtectionTarget =
                serde_json::from_value(data.clone()).map_err(|e| format!("Protection: {e}"))?;
            Ok(Keyword::Protection(pt))
        }
        "Landwalk" => Ok(Keyword::Landwalk(data.as_str().unwrap_or("").to_string())),
        "Partner" => Ok(Keyword::Partner(
            data.as_str().map(|s| s.to_string()).or_else(|| {
                if data.is_null() {
                    None
                } else {
                    Some(data.to_string())
                }
            }),
        )),
        "Companion" => Ok(Keyword::Companion(data.as_str().unwrap_or("").to_string())),
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
                .unwrap_or("P1P1")
                .to_string();
            let count = obj.get("count").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
            Ok(Keyword::EtbCounter {
                counter_type,
                count,
            })
        }
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
        assert_eq!(
            Keyword::from_str("Totem Armor").unwrap(),
            Keyword::TotemArmor
        );
        assert_eq!(
            Keyword::from_str("Split Second").unwrap(),
            Keyword::SplitSecond
        );
        assert_eq!(Keyword::from_str("Battle Cry").unwrap(), Keyword::Battlecry);
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
        assert!(matches!(cycling, Keyword::Cycling(ManaCost::Cost { .. })));
        if let Keyword::Cycling(ManaCost::Cost { generic, .. }) = &cycling {
            assert_eq!(*generic, 2);
        }

        let flashback = Keyword::from_str("Flashback:3BB").unwrap();
        assert!(matches!(
            flashback,
            Keyword::Flashback(ManaCost::Cost { .. })
        ));
        if let Keyword::Flashback(ManaCost::Cost { generic, shards }) = &flashback {
            assert_eq!(*generic, 3);
            assert_eq!(shards.len(), 2); // BB
        }

        let ward = Keyword::from_str("Ward:2").unwrap();
        assert!(matches!(ward, Keyword::Ward(ManaCost::Cost { .. })));

        let equip = Keyword::from_str("Equip:3").unwrap();
        assert!(matches!(equip, Keyword::Equip(ManaCost::Cost { .. })));
    }

    #[test]
    fn parse_numeric_keywords_unchanged() {
        assert_eq!(Keyword::from_str("Crew:3").unwrap(), Keyword::Crew(3));
        assert_eq!(Keyword::from_str("Rampage:2").unwrap(), Keyword::Rampage(2));
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
        assert_eq!(
            Keyword::from_str("Protection:Artifacts").unwrap(),
            Keyword::Protection(ProtectionTarget::CardType("Artifacts".to_string()))
        );
        assert_eq!(
            Keyword::from_str("Protection:multicolored").unwrap(),
            Keyword::Protection(ProtectionTarget::Multicolored)
        );
    }

    #[test]
    fn parse_partner_variants() {
        assert_eq!(
            Keyword::from_str("Partner").unwrap(),
            Keyword::Partner(None)
        );
        assert_eq!(
            Keyword::from_str("Partner:Brallin, Skyshark Rider").unwrap(),
            Keyword::Partner(Some("Brallin, Skyshark Rider".to_string()))
        );
    }

    #[test]
    fn parse_enchant_as_target_filter() {
        let enchant = Keyword::from_str("Enchant:creature").unwrap();
        assert!(matches!(
            enchant,
            Keyword::Enchant(TargetFilter::Typed(TypedFilter { .. }))
        ));
        if let Keyword::Enchant(TargetFilter::Typed(TypedFilter { card_type, .. })) = &enchant {
            assert!(matches!(
                card_type,
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

    #[test]
    fn parse_etb_counter_typed() {
        let kw = Keyword::from_str("EtbCounter:P1P1:1").unwrap();
        assert!(matches!(kw, Keyword::EtbCounter { .. }));
        if let Keyword::EtbCounter {
            counter_type,
            count,
        } = &kw
        {
            assert_eq!(counter_type, "P1P1");
            assert_eq!(*count, 1);
        }

        let kw2 = Keyword::from_str("EtbCounter:P1P1:3").unwrap();
        if let Keyword::EtbCounter {
            counter_type,
            count,
        } = &kw2
        {
            assert_eq!(counter_type, "P1P1");
            assert_eq!(*count, 3);
        }
    }

    #[test]
    fn parse_new_parameterized_keywords() {
        // CR 702.164: Toxic
        assert_eq!(Keyword::from_str("Toxic:2").unwrap(), Keyword::Toxic(2));
        assert_eq!(Keyword::from_str("Toxic:1").unwrap(), Keyword::Toxic(1));

        // CR 702.173: Saddle
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
        assert_eq!(Keyword::from_str("Hideaway").unwrap(), Keyword::Hideaway);
        assert_eq!(
            Keyword::from_str("Cumulative").unwrap(),
            Keyword::Cumulative
        );
        assert_eq!(Keyword::from_str("Ripple").unwrap(), Keyword::Ripple);
        assert_eq!(Keyword::from_str("Totem").unwrap(), Keyword::Totem);
        assert_eq!(Keyword::from_str("Warp").unwrap(), Keyword::Warp);
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
            Keyword::Unknown("CustomKeyword".to_string()),
            Keyword::EtbCounter {
                counter_type: "P1P1".to_string(),
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
            "Afflict",
            "Prowess",
            "Undying",
            "Persist",
            "Cascade",
            "Convoke",
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
}
