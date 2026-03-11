use std::convert::Infallible;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::ability::TargetFilter;
use super::mana::{ManaColor, ManaCost};

/// What a Protection keyword protects from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProtectionTarget {
    Color(ManaColor),
    CardType(String),
    Quality(String),
}

/// All MTG keywords as typed enum variants.
/// Simple (unit) variants for keywords with no parameters.
/// Parameterized variants carry associated data (ManaCost for costs, amounts, etc.).
/// Unknown captures any unrecognized keyword string for forward compatibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    EtbCounter { counter_type: String, count: u32 },

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
    let type_filter = match lower.as_str() {
        "creature" => Some(TypeFilter::Creature),
        "land" => Some(TypeFilter::Land),
        "artifact" => Some(TypeFilter::Artifact),
        "enchantment" => Some(TypeFilter::Enchantment),
        "planeswalker" => Some(TypeFilter::Planeswalker),
        "permanent" => Some(TypeFilter::Permanent),
        _ => None,
    };

    match type_filter {
        Some(tf) => TargetFilter::Typed {
            card_type: Some(tf),
            subtype: None,
            controller: None,
            properties: vec![],
        },
        // If not a recognized type, use a typed filter with the string as subtype
        None => TargetFilter::Typed {
            card_type: None,
            subtype: Some(s.to_string()),
            controller: None,
            properties: vec![],
        },
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

        // Simple (unit) keywords -- case-insensitive match
        match name_lower.as_str() {
            "flying" => Ok(Keyword::Flying),
            "first strike" => Ok(Keyword::FirstStrike),
            "double strike" => Ok(Keyword::DoubleStrike),
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
            "battlecry" | "battle cry" => Ok(Keyword::Battlecry),
            "decayed" => Ok(Keyword::Decayed),
            "unleash" => Ok(Keyword::Unleash),
            "riot" => Ok(Keyword::Riot),
            "living weapon" => Ok(Keyword::LivingWeapon),
            "totem armor" => Ok(Keyword::TotemArmor),
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
            "split second" => Ok(Keyword::SplitSecond),
            "storm" => Ok(Keyword::Storm),
            "suspend" => Ok(Keyword::Suspend),
            "gift" => Ok(Keyword::Gift),
            "ravenous" => Ok(Keyword::Ravenous),
            "daybound" => Ok(Keyword::Daybound),
            "nightbound" => Ok(Keyword::Nightbound),
            "enlist" => Ok(Keyword::Enlist),
            "read ahead" => Ok(Keyword::ReadAhead),
            "compleated" => Ok(Keyword::Compleated),
            "conspire" => Ok(Keyword::Conspire),
            "demonstrate" => Ok(Keyword::Demonstrate),
            "dethrone" => Ok(Keyword::Dethrone),
            "double team" => Ok(Keyword::DoubleTeam),
            "living metal" => Ok(Keyword::LivingMetal),
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
        _ if lower.starts_with("from ") => ProtectionTarget::Quality(s.to_string()),
        _ => ProtectionTarget::CardType(s.to_string()),
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
            Keyword::Enchant(TargetFilter::Typed { .. })
        ));
        if let Keyword::Enchant(TargetFilter::Typed { card_type, .. }) = &enchant {
            assert!(matches!(
                card_type,
                Some(super::super::ability::TypeFilter::Creature)
            ));
        }
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
