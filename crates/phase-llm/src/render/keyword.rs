//! A keyword as cards print it, for the board's battlefield lines.
//!
//! The name is the engine's: `keyword_display_name` spells every keyword the
//! way Oracle text does ("double strike", "web-slinging", "islandcycling"),
//! and this module capitalizes its first letter, as a keyword line starts.
//! Case is otherwise the engine's, which spells some words in a case other
//! than the card prints: a proper noun, a quality or an X can read lower case
//! ("For mirrodin!", "Protection from demons", "Reinforce x"), and landwalk's
//! own word upper case ("Legendary Landwalk").
//!
//! After the name comes the keyword's parameter, printed only when all of it
//! can be: a mana cost, a number, a count and a cost, a color, a card type,
//! or an Aura's enchant restriction made only of type-line words.
//!
//! A parameter that cannot be printed whole leaves the keyword as its name
//! alone, because a partial rendering would state it wrongly. That covers a
//! non-mana cost (cycling by paying life), a filter with a controller or a
//! property ("Enchant creature you control", "Enchant tapped creature"), a
//! deck condition (companion), and a count the engine holds only as a
//! placeholder (Suspend X). Where the board includes rules text, the
//! permanent's own text on the same line states a printed keyword in full.
//!
//! The engine's own `Display` for `Keyword` is used only for landwalk, which
//! its explicit arm spells per CR 702.14a. It spells choice labels in title
//! case ("First Strike") and falls back to `Debug` for most parameterized
//! keywords, which is the text this module replaces.

use engine::parser::oracle::keyword_display_name;
use engine::types::ability::{AbilityCost, QuantityExpr, TargetFilter, TypeFilter, TypedFilter};
use engine::types::keywords::{
    BestowCost, BlitzCost, BloodthirstValue, BuybackCost, CyclingCost, DisguiseCost, EchoCost,
    EmbalmCost, EmergeCost, EntwineCost, EscapeCost, EternalizeCost, EvokeCost, FlashbackCost,
    GiftKind, HexproofFilter, Keyword, MorphCost, ProtectionTarget, WardCost,
};
use engine::types::mana::{ManaColor, ManaCost};

use super::text::{mana_cost_text, one_line};

/// `keyword` as cards print it: `Flying`, `Double strike`, `Cycling {3}`,
/// `Ward {2}`, `Enchant creature or land`.
pub fn keyword_text(keyword: &Keyword) -> String {
    let oracle_name = keyword_display_name(keyword);
    let mut letters = oracle_name.chars();
    let name: String = letters
        .next()
        .map(|first| first.to_uppercase().chain(letters).collect())
        .unwrap_or_default();

    // The name, then `detail` after a space, or the name alone when the
    // parameter prints as nothing (a placeholder cost such as "its mana cost").
    let with_detail = |detail: String| {
        if detail.is_empty() {
            name.clone()
        } else {
            format!("{name} {detail}")
        }
    };
    // A cost after the long dash, as Suspend, Awaken, Reinforce and Impending
    // print it after their count.
    let dashed_cost = |cost: &ManaCost| {
        let cost = mana_cost_text(cost);
        if cost.is_empty() {
            cost
        } else {
            format!("—{cost}")
        }
    };

    let text = match keyword {
        Keyword::Unearth(cost)
        | Keyword::Reconfigure(cost)
        | Keyword::Kicker(cost)
        | Keyword::Equip(cost)
        | Keyword::Ninjutsu(cost)
        | Keyword::CommanderNinjutsu(cost)
        | Keyword::Prowl(cost)
        | Keyword::Megamorph(cost)
        | Keyword::Mayhem(cost)
        | Keyword::Madness(cost)
        | Keyword::Miracle(cost)
        | Keyword::Dash(cost)
        | Keyword::Harmonize(cost)
        | Keyword::Foretell(cost)
        | Keyword::Mutate(cost)
        | Keyword::Disturb(cost)
        | Keyword::Overload(cost)
        | Keyword::Spectacle(cost)
        | Keyword::Surge(cost)
        | Keyword::Encore(cost)
        | Keyword::Outlast(cost)
        | Keyword::Scavenge(cost)
        | Keyword::Fortify(cost)
        | Keyword::Plot(cost)
        | Keyword::Offspring(cost)
        | Keyword::LevelUp(cost)
        | Keyword::Warp(cost)
        | Keyword::Sneak(cost)
        | Keyword::WebSlinging(cost)
        | Keyword::Squad(cost)
        | Keyword::Transmute(cost)
        | Keyword::Transfigure(cost)
        | Keyword::Recover(cost)
        | Keyword::Cleave(cost)
        | Keyword::Replicate(cost)
        | Keyword::MoreThanMeetsTheEye(cost)
        | Keyword::Freerunning(cost)
        | Keyword::Specialize(cost)
        | Keyword::Cycling(CyclingCost::Mana(cost))
        | Keyword::Flashback(FlashbackCost::Mana(cost))
        | Keyword::Bestow(BestowCost::Mana(cost))
        | Keyword::Embalm(EmbalmCost::Mana(cost))
        | Keyword::Eternalize(EternalizeCost::Mana(cost))
        | Keyword::Morph(MorphCost::Mana(cost))
        | Keyword::Escape(EscapeCost::Mana(cost))
        | Keyword::Evoke(EvokeCost::Mana(cost))
        | Keyword::Blitz(BlitzCost::Mana(cost))
        | Keyword::Buyback(BuybackCost::Mana(cost))
        | Keyword::Echo(EchoCost::Mana(cost))
        | Keyword::Entwine(EntwineCost::Mana(cost))
        | Keyword::Escalate(AbilityCost::Mana { cost })
        | Keyword::Ward(WardCost::Mana(cost))
        | Keyword::Disguise(DisguiseCost::Mana(cost))
        | Keyword::Disguise(DisguiseCost::Reduced { cost, .. })
        // The name already says the land type ("islandcycling").
        | Keyword::Typecycling { cost, .. } => with_detail(mana_cost_text(cost)),

        // CR 702.119a + CR 702.119b: Emerge sacrifices a creature, and
        // "Emerge from [quality]" another kind of permanent; only the creature
        // form prints as the bare cost.
        Keyword::Emerge(EmergeCost {
            mana_cost,
            sacrifice_filter,
        }) if *sacrifice_filter == TargetFilter::Typed(TypedFilter::creature()) => {
            with_detail(mana_cost_text(mana_cost))
        }

        Keyword::Dredge(count)
        | Keyword::Modular(count)
        | Keyword::Renown(count)
        | Keyword::Fabricate(count)
        | Keyword::Annihilator(count)
        | Keyword::Bushido(count)
        | Keyword::Frenzy(count)
        | Keyword::Tribute(count)
        | Keyword::Afterlife(count)
        | Keyword::Fading(count)
        | Keyword::Vanishing(count)
        | Keyword::Rampage(count)
        | Keyword::Absorb(count)
        | Keyword::Hideaway(count)
        | Keyword::Ripple(count)
        | Keyword::Poisonous(count)
        | Keyword::Amplify(count)
        | Keyword::Graft(count)
        | Keyword::Toxic(count)
        | Keyword::Saddle(count)
        | Keyword::Teamwork(count)
        | Keyword::Soulshift(count)
        | Keyword::Backup(count)
        | Keyword::Crew { power: count, .. }
        | Keyword::Bloodthirst(BloodthirstValue::Fixed(count)) => with_detail(count.to_string()),
        // CR 702.82a + CR 702.82c: Devour sacrifices creatures, and "Devour
        // [quality]" another kind of permanent; only the creature form prints
        // as the bare count.
        Keyword::Devour {
            n,
            quality: TypeFilter::Creature,
        } => with_detail(n.to_string()),
        Keyword::Bloodthirst(BloodthirstValue::X) => with_detail("X".to_string()),
        // A count that is not a literal is the X the card defines in its text.
        Keyword::Mobilize(count) | Keyword::Firebending(count) => {
            let count = if let QuantityExpr::Fixed { value } = count {
                value.to_string()
            } else {
                "X".to_string()
            };
            with_detail(count)
        }

        // No card prints Suspend 0 or Awaken 0: a count of 0 is the engine's
        // placeholder for one it could not read ("Suspend X", which also
        // leaves its cost as `{0}`), so neither the count nor the cost prints.
        Keyword::Suspend { count: 0, .. } | Keyword::Awaken { count: 0, .. } => name,
        Keyword::Suspend { count, cost } | Keyword::Awaken { count, cost } => {
            with_detail(format!("{count}{}", dashed_cost(cost)))
        }
        // CR 702.24a: "Cumulative upkeep [cost]". The engine's name carries
        // the cost but words only some shapes of it, and prints others as
        // debug text ("Exile { count: 1, .. }"), so the cost is printed here
        // when it is a mana cost or a fixed payment of life, and left to the
        // rules text otherwise.
        Keyword::CumulativeUpkeep(AbilityCost::Mana { cost }) => {
            format!("Cumulative upkeep {}", mana_cost_text(cost))
        }
        Keyword::CumulativeUpkeep(AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value },
        }) => format!("Cumulative upkeep—Pay {value} life"),
        Keyword::CumulativeUpkeep(_) => "Cumulative upkeep".to_string(),
        // The name already says the count ("impending 3", "reinforce 2").
        Keyword::Impending { cost, .. } | Keyword::Reinforce { cost, .. } => {
            format!("{name}{}", dashed_cost(cost))
        }
        // CR 702.160a: the prototype's mana cost, then its power and toughness.
        Keyword::Prototype {
            cost,
            power: Some(power),
            toughness: Some(toughness),
        } => with_detail(format!("{} — {power}/{toughness}", mana_cost_text(cost))),
        Keyword::Prototype { cost, .. } => with_detail(mana_cost_text(cost)),
        Keyword::Splice { subtype, cost } => {
            with_detail(format!("onto {subtype} {}", mana_cost_text(cost)))
        }

        Keyword::Ward(WardCost::PayLife(life)) => format!("{name}—Pay {life} life"),
        Keyword::Ward(WardCost::Waterbend(cost)) => {
            format!("{name}—Waterbend {}", mana_cost_text(cost))
        }

        Keyword::Protection(target) => match target {
            ProtectionTarget::Color(color) => with_detail(format!("from {}", color_word(*color))),
            ProtectionTarget::CardType(quality) | ProtectionTarget::Quality(quality) => {
                with_detail(format!("from {quality}"))
            }
            ProtectionTarget::Multicolored => with_detail("from multicolored".to_string()),
            ProtectionTarget::Everything => with_detail("from everything".to_string()),
            ProtectionTarget::ChosenColor => with_detail("from the chosen color".to_string()),
            ProtectionTarget::ChosenCardType => {
                with_detail("from the chosen card type".to_string())
            }
            ProtectionTarget::ChosenCardName => {
                with_detail("from the chosen card name".to_string())
            }
            ProtectionTarget::ChosenPlayer => with_detail("from the chosen player".to_string()),
            ProtectionTarget::Filter(_)
            | ProtectionTarget::FromPlayer(_)
            | ProtectionTarget::EachColor(_) => name,
        },
        // The name already ends "hexproof from".
        Keyword::HexproofFrom(filter) => match filter {
            HexproofFilter::Color(color) => with_detail(color_word(*color).to_string()),
            HexproofFilter::CardType(quality) | HexproofFilter::Quality(quality) => {
                with_detail(quality.clone())
            }
            HexproofFilter::ChosenColor => with_detail("the chosen color".to_string()),
        },
        Keyword::Enchant(filter) => with_detail(enchant_text(filter).unwrap_or_default()),
        Keyword::Gift(gift) => with_detail(
            match gift {
                GiftKind::Card => "a card",
                GiftKind::Treasure => "a Treasure",
                GiftKind::Food => "a Food",
                GiftKind::TappedFish => "a tapped Fish",
                GiftKind::ExtraTurn => "an extra turn",
            }
            .to_string(),
        ),
        // CR 702.14a: "[type]walk", spelled by the engine's display authority
        // for landwalk ("Islandwalk", "Legendary Landwalk").
        Keyword::Landwalk(_) => keyword.to_string(),
        // Not a printed keyword: the engine's record that a permanent enters
        // with counters, which its card states in a sentence of its own.
        Keyword::EtbCounter {
            counter_type,
            count,
        } => {
            let noun = if *count == 1 { "counter" } else { "counters" };
            format!("Enters with {count} {} {noun}", counter_type.display_phrase())
        }

        // Printed in full by the name: a keyword with no parameter, or one
        // whose engine name carries it ("afflict 2", "partner with Pir").
        Keyword::Flying
        | Keyword::FirstStrike
        | Keyword::DoubleStrike
        | Keyword::Trample
        | Keyword::TrampleOverPlaneswalkers
        | Keyword::Deathtouch
        | Keyword::Lifelink
        | Keyword::Vigilance
        | Keyword::Haste
        | Keyword::Reach
        | Keyword::Defender
        | Keyword::Menace
        | Keyword::Indestructible
        | Keyword::Hexproof
        | Keyword::Shroud
        | Keyword::Flash
        | Keyword::Fear
        | Keyword::Intimidate
        | Keyword::Skulk
        | Keyword::Shadow
        | Keyword::Horsemanship
        | Keyword::Wither
        | Keyword::Infect
        | Keyword::Afflict(_)
        | Keyword::StartingIntensity(_)
        | Keyword::Prowess
        | Keyword::Undying
        | Keyword::Persist
        | Keyword::Cascade
        | Keyword::Exalted
        | Keyword::Flanking
        | Keyword::Evolve
        | Keyword::Extort
        | Keyword::Exploit
        | Keyword::Explore
        | Keyword::Ascend
        | Keyword::Storied
        | Keyword::StartYourEngines
        | Keyword::Soulbond
        | Keyword::Convoke
        | Keyword::Waterbend
        | Keyword::Delve
        | Keyword::Devoid
        | Keyword::Changeling
        | Keyword::Phasing
        | Keyword::Battlecry
        | Keyword::Decayed
        | Keyword::Unleash
        | Keyword::Riot
        | Keyword::LivingWeapon
        | Keyword::JobSelect
        | Keyword::TotemArmor
        | Keyword::Partner(_)
        | Keyword::Casualty(_)
        | Keyword::Banding
        | Keyword::BandsWithOther(_)
        | Keyword::Epic
        | Keyword::Fuse
        | Keyword::Gravestorm
        | Keyword::Haunt
        | Keyword::Improvise
        | Keyword::Ingest
        | Keyword::Melee
        | Keyword::Mentor
        | Keyword::Myriad
        | Keyword::Provoke
        | Keyword::Rebound
        | Keyword::Retrace
        | Keyword::SplitSecond
        | Keyword::Storm
        | Keyword::Totem
        | Keyword::Discover(_)
        | Keyword::Spree
        | Keyword::Tiered
        | Keyword::Ravenous
        | Keyword::Daybound
        | Keyword::Nightbound
        | Keyword::Enlist
        | Keyword::ReadAhead
        | Keyword::Compleated
        | Keyword::Conspire
        | Keyword::Demonstrate
        | Keyword::Dethrone
        | Keyword::DoubleTeam
        | Keyword::LivingMetal
        | Keyword::Bargain
        | Keyword::Sunburst
        | Keyword::Training
        | Keyword::Assist
        | Keyword::Augment
        | Keyword::Aftermath
        | Keyword::JumpStart
        | Keyword::Cipher
        | Keyword::Undaunted
        | Keyword::Paradigm
        | Keyword::Station
        | Keyword::ForMirrodin
        | Keyword::Increment
        | Keyword::Offering(_)
        | Keyword::Unknown(_) => name,

        // The parameter is a filter, a non-mana cost or a deck condition,
        // which the permanent's rules text states in full (see the module
        // doc).
        Keyword::Cycling(CyclingCost::NonMana(_))
        | Keyword::Flashback(FlashbackCost::NonMana(_))
        | Keyword::Bestow(BestowCost::NonMana(_))
        | Keyword::Embalm(EmbalmCost::NonMana(_))
        | Keyword::Eternalize(EternalizeCost::NonMana(_))
        | Keyword::Morph(MorphCost::NonMana(_))
        | Keyword::Escape(EscapeCost::NonMana(_))
        | Keyword::Evoke(EvokeCost::NonMana(_))
        | Keyword::Blitz(BlitzCost::NonMana(_))
        | Keyword::Buyback(BuybackCost::NonMana(_))
        | Keyword::Echo(EchoCost::NonMana(_))
        | Keyword::Entwine(EntwineCost::NonMana(_))
        | Keyword::Escalate(_)
        | Keyword::Ward(_)
        | Keyword::Emerge(_)
        | Keyword::Devour { .. }
        | Keyword::Craft { .. }
        | Keyword::Affinity(_)
        | Keyword::Companion(_)
        | Keyword::Champion(_) => name,
    };
    // Engine data (a land type, a partner's name, an unknown keyword) can
    // carry a line break, which would open a line of its own in the prompt.
    one_line(&text)
}

/// A color as protection and hexproof name it: `red`.
fn color_word(color: ManaColor) -> &'static str {
    match color {
        ManaColor::White => "white",
        ManaColor::Blue => "blue",
        ManaColor::Black => "black",
        ManaColor::Red => "red",
        ManaColor::Green => "green",
    }
}

/// CR 702.5a: an Aura's enchant restriction in the words cards print it with:
/// `creature`, `creature or land`, `nonland permanent`, `player`.
///
/// `None` for a restriction that says more than type-line words, such as a
/// controller ("creature you control") or a property ("tapped creature"). It
/// is not printed in part; the Aura's rules text states it. Every filter shape
/// but a player, a typed filter and a choice among them declines, a shape
/// added later included.
fn enchant_text(filter: &TargetFilter) -> Option<String> {
    match filter {
        TargetFilter::Player => Some("player".to_string()),
        TargetFilter::Typed(typed) => type_line_words(typed),
        TargetFilter::Or { filters } => {
            let choices = filters
                .iter()
                .map(enchant_text)
                .collect::<Option<Vec<String>>>()?;
            Some(match choices.as_slice() {
                [] => String::new(),
                [only] => only.clone(),
                [first, second] => format!("{first} or {second}"),
                [rest @ .., last] => format!("{}, or {last}", rest.join(", ")),
            })
        }
        _ => None,
    }
}

/// A typed filter made only of type-line words, the way a type phrase prints
/// them: negations first, then subtypes, then card types (`non-Wall
/// creature`, `nonland permanent`, `Forest`). `None` when the filter names a
/// controller or a property, or names no type at all.
fn type_line_words(typed: &TypedFilter) -> Option<String> {
    if typed.controller.is_some() || !typed.properties.is_empty() || typed.type_filters.is_empty() {
        return None;
    }
    let mut negations = Vec::new();
    let mut subtypes = Vec::new();
    let mut card_types = Vec::new();
    for type_filter in &typed.type_filters {
        let word = type_word(type_filter)?;
        match type_filter {
            TypeFilter::Non(_) => negations.push(word),
            TypeFilter::Subtype(_) => subtypes.push(word),
            _ => card_types.push(word),
        }
    }
    Some([negations, subtypes, card_types].concat().join(" "))
}

/// One type-line constraint as it prints: `creature`, `Forest`, `nonland`,
/// `non-Wall`. `None` for one with no single printed word.
fn type_word(type_filter: &TypeFilter) -> Option<String> {
    let word = match type_filter {
        TypeFilter::Creature => "creature",
        TypeFilter::Land => "land",
        TypeFilter::Artifact => "artifact",
        TypeFilter::Enchantment => "enchantment",
        TypeFilter::Instant => "instant",
        TypeFilter::Sorcery => "sorcery",
        TypeFilter::Planeswalker => "planeswalker",
        TypeFilter::Battle => "battle",
        TypeFilter::Kindred => "kindred",
        TypeFilter::Permanent => "permanent",
        TypeFilter::Card => "card",
        TypeFilter::Subtype(subtype) => return Some(subtype.clone()),
        // A negated subtype prints hyphenated ("non-Wall"), a negated card
        // type does not ("nonland").
        TypeFilter::Non(negated) => {
            return match &**negated {
                TypeFilter::Subtype(subtype) => Some(format!("non-{subtype}")),
                other => Some(format!("non{}", type_word(other)?)),
            };
        }
        // "Any type", and a choice of types inside a conjunction, have no one
        // word to print.
        TypeFilter::Any | TypeFilter::AnyOf(_) => return None,
    };
    Some(word.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::ability::{ControllerRef, FilterProp};
    use engine::types::counter::CounterType;
    use engine::types::mana::ManaCostShard;

    fn cost(generic: u32, shards: &[ManaCostShard]) -> ManaCost {
        ManaCost::Cost {
            shards: shards.to_vec(),
            generic,
        }
    }

    fn enchant(filter: TargetFilter) -> String {
        keyword_text(&Keyword::Enchant(filter))
    }

    /// Issue #10997: the board printed these as Rust debug text
    /// (`Cycling(Mana(Cost { shards: [], generic: 3 }))`, `DoubleStrike`).
    #[test]
    fn parameterized_keywords_print_their_name_and_parameter() {
        use ManaCostShard::{Blue, White};

        let cases = [
            (Keyword::DoubleStrike, "Double strike"),
            (
                Keyword::Cycling(CyclingCost::Mana(ManaCost::generic(3))),
                "Cycling {3}",
            ),
            (
                Keyword::Ward(WardCost::Mana(ManaCost::generic(2))),
                "Ward {2}",
            ),
            (Keyword::Ward(WardCost::PayLife(3)), "Ward—Pay 3 life"),
            (Keyword::Warp(cost(1, &[White])), "Warp {1}{W}"),
            (
                Keyword::Prototype {
                    cost: cost(1, &[White, White]),
                    power: Some(3),
                    toughness: Some(3),
                },
                "Prototype {1}{W}{W} — 3/3",
            ),
            (
                Keyword::Suspend {
                    count: 4,
                    cost: cost(0, &[Blue]),
                },
                "Suspend 4—{U}",
            ),
            (
                Keyword::Impending {
                    cost: cost(2, &[Blue, Blue]),
                    counters: 3,
                },
                "Impending 3—{2}{U}{U}",
            ),
            (
                Keyword::Typecycling {
                    cost: ManaCost::generic(2),
                    subtype: "Island".to_string(),
                },
                "Islandcycling {2}",
            ),
            (Keyword::Annihilator(2), "Annihilator 2"),
            (
                Keyword::Crew {
                    power: 3,
                    once_per_turn: None,
                },
                "Crew 3",
            ),
            (
                Keyword::Mobilize(QuantityExpr::Fixed { value: 2 }),
                "Mobilize 2",
            ),
            (
                Keyword::Protection(ProtectionTarget::Color(ManaColor::Red)),
                "Protection from red",
            ),
            (
                Keyword::HexproofFrom(HexproofFilter::Color(ManaColor::Black)),
                "Hexproof from black",
            ),
            (Keyword::Landwalk("Island".to_string()), "Islandwalk"),
            (Keyword::Gift(GiftKind::Card), "Gift a card"),
            (
                Keyword::EtbCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: 2,
                },
                "Enters with 2 +1/+1 counters",
            ),
        ];
        for (keyword, printed) in cases {
            assert_eq!(keyword_text(&keyword), printed, "{keyword:?}");
        }
    }

    /// A keyword whose engine name already carries its parameter prints it
    /// once.
    #[test]
    fn a_parameter_the_name_carries_is_not_printed_twice() {
        assert_eq!(keyword_text(&Keyword::Afflict(2)), "Afflict 2");
        assert_eq!(keyword_text(&Keyword::Discover(3)), "Discover 3");
        assert_eq!(
            keyword_text(&Keyword::Reinforce {
                count: 2,
                cost: cost(1, &[ManaCostShard::Green]),
            }),
            "Reinforce 2—{1}{G}"
        );
    }

    /// CR 702.5a: an Aura's enchant restriction prints when it is made only of
    /// type-line words, and is left to the rules text otherwise.
    #[test]
    fn an_enchant_restriction_prints_only_when_all_of_it_can() {
        let creature = TargetFilter::Typed(TypedFilter::creature());
        let land = TargetFilter::Typed(TypedFilter::land());
        // Brink of Disaster: "Enchant creature or land".
        assert_eq!(
            enchant(TargetFilter::Or {
                filters: vec![creature.clone(), land],
            }),
            "Enchant creature or land"
        );
        assert_eq!(enchant(creature), "Enchant creature");
        assert_eq!(
            enchant(TargetFilter::Typed(TypedFilter::creature().with_type(
                TypeFilter::Non(Box::new(TypeFilter::Subtype("Wall".to_string())))
            ))),
            "Enchant non-Wall creature"
        );
        assert_eq!(
            enchant(TargetFilter::Typed(
                TypedFilter::permanent().with_type(TypeFilter::Non(Box::new(TypeFilter::Land)))
            )),
            "Enchant nonland permanent"
        );
        assert_eq!(enchant(TargetFilter::Player), "Enchant player");
        // Aspect of Lamprey: "Enchant creature you control". Entangling Vines:
        // "Enchant tapped creature". Neither prints as "Enchant creature".
        assert_eq!(
            enchant(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You)
            )),
            "Enchant"
        );
        let mut tapped = TypedFilter::creature();
        tapped.properties.push(FilterProp::Tapped);
        assert_eq!(enchant(TargetFilter::Typed(tapped)), "Enchant");
    }

    /// A parameter that is not a mana cost or a number is left to the rules
    /// text rather than printed as debug text.
    #[test]
    fn a_parameter_that_cannot_print_whole_leaves_the_name() {
        let cycling = keyword_text(&Keyword::Cycling(CyclingCost::NonMana(
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 2 },
            },
        )));
        assert_eq!(cycling, "Cycling");
        assert_eq!(
            keyword_text(&Keyword::Devour {
                n: 1,
                quality: TypeFilter::Land,
            }),
            "Devour"
        );
        assert_eq!(
            keyword_text(&Keyword::Devour {
                n: 2,
                quality: TypeFilter::Creature,
            }),
            "Devour 2"
        );
        // Detritivore's suspend X, whose cost is {X}{3}{R}, which the engine
        // holds as the placeholder `Suspend { count: 0, cost: {0} }`.
        assert_eq!(
            keyword_text(&Keyword::Suspend {
                count: 0,
                cost: ManaCost::zero(),
            }),
            "Suspend"
        );
    }

    /// CR 702.24a: a cumulative upkeep cost prints when it is mana or a fixed
    /// payment of life. Any other cost, which the engine's name would print
    /// as debug text, is left to the rules text.
    #[test]
    fn a_cumulative_upkeep_cost_prints_only_when_it_can() {
        let upkeep = |cost: AbilityCost| keyword_text(&Keyword::CumulativeUpkeep(cost));
        assert_eq!(
            upkeep(AbilityCost::Mana {
                cost: ManaCost::generic(1),
            }),
            "Cumulative upkeep {1}"
        );
        assert_eq!(
            upkeep(AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 2 },
            }),
            "Cumulative upkeep—Pay 2 life"
        );
        // Thought Lash's cumulative upkeep cost, "Exile the top card of your
        // library.", in its card data's own encoding.
        let exile: AbilityCost = serde_json::from_value(serde_json::json!({
            "type": "Exile",
            "count": 1,
            "zone": "Library",
            "filter": null,
        }))
        .unwrap();
        assert_eq!(upkeep(exile), "Cumulative upkeep");
    }
}
