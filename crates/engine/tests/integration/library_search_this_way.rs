//! CR 701.23a + CR 701.24a + CR 608.2c: "If you search your library this way, shuffle."
//!
//! The 34 cards of the class search a zone set that includes the library ("your library and/or
//! graveyard", "your graveyard, hand, and/or library", ...) and then shuffle only if the library
//! was searched. A `CantSearchLibrary` effect (printed Mindlock Orb) leaves a graveyard find without
//! a library search, so no shuffle. Every ability-bearing card is built from its verbatim MTGJSON
//! Oracle text and driven through the production cast / activate / trigger path; the found card is a
//! textless filler, or the printed Rune of Sustenance / Arachnus Web where the found card's own text
//! matters. Not asserted: a shuffle after a graveyard or hand find with the library searchable (the
//! engine pools the zones into one search; the printed zone election is a separate gap).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

#[derive(Clone, Copy)]
enum Kind {
    Creature(i32, i32),
    Artifact,
    Enchantment,
    Sorcery,
    Instant,
    Planeswalker(u32),
}

#[derive(Clone, Copy)]
enum Flow {
    Cast,
    PW1,
    Targets1,
    Targets2,
    TargetsFury,
    TargetsPlayer,
    TargetsTapped,
    Gate,
    Supplicant,
    Sanctum,
    LastLight,
    Spinner,
}

#[derive(Clone, Copy)]
enum Found {
    Named(&'static str),
    Filler {
        name: &'static str,
        artifact: bool,
        enchantment: bool,
        subtypes: &'static [&'static str],
        mv: u32,
    },
    Printed(&'static str),
    Oracle {
        name: &'static str,
        oracle: &'static str,
        aura: bool,
        subtypes: &'static [&'static str],
    },
}

#[derive(Clone, Copy)]
struct Spec {
    name: &'static str,
    oracle: &'static str,
    kind: Kind,
    subtypes: &'static [&'static str],
    legendary: bool,
    generic: u32,
    shards: &'static [ManaCostShard],
    flow: Flow,
    found: Found,
    variants: &'static [&'static str],
}

static SPECS: &[Spec] = &[
    Spec {
        name: "Ashiok's Forerunner",
        oracle: r####"Flash
When this creature enters, you may search your library and/or graveyard for a card named Ashiok, Sculptor of Fears, reveal it, and put it into your hand. If you search your library this way, shuffle."####,
        kind: Kind::Creature(3, 3),
        subtypes: &["Human", "Wizard"],
        legendary: false,
        generic: 3,
        shards: &[ManaCostShard::Blue, ManaCostShard::Black],
        flow: Flow::Cast,
        found: Found::Named("Ashiok, Sculptor of Fears"),
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Niambi, Faithful Healer",
        oracle: r####"When Niambi enters, you may search your library and/or graveyard for a card named Teferi, Timebender, reveal it, and put it into your hand. If you search your library this way, shuffle."####,
        kind: Kind::Creature(2, 2),
        subtypes: &["Human", "Cleric"],
        legendary: true,
        generic: 1,
        shards: &[ManaCostShard::White, ManaCostShard::Blue],
        flow: Flow::Cast,
        found: Found::Named("Teferi, Timebender"),
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Runed Crown",
        oracle: r####"When this Equipment enters, you may search your library, hand, and/or graveyard for a Rune card and put it onto the battlefield attached to this Equipment. If you search your library this way, shuffle.
Equipped creature gets +1/+1.
Equip {2}"####,
        kind: Kind::Artifact,
        subtypes: &["Equipment"],
        legendary: false,
        generic: 3,
        shards: &[],
        flow: Flow::Cast,
        found: Found::Printed("Rune of Sustenance"),
        variants: &["G", "H", "L", "GM"],
    },
    Spec {
        name: "Sun-Blessed Mount",
        oracle: r####"When this creature enters, you may search your library and/or graveyard for a card named Huatli, Dinosaur Knight, reveal it, then put it into your hand. If you search your library this way, shuffle."####,
        kind: Kind::Creature(4, 4),
        subtypes: &["Dinosaur"],
        legendary: false,
        generic: 3,
        shards: &[ManaCostShard::Red, ManaCostShard::White],
        flow: Flow::Cast,
        found: Found::Named("Huatli, Dinosaur Knight"),
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Vraska's Stoneglare",
        oracle: r####"Destroy target creature. You gain life equal to its toughness. You may search your library and/or graveyard for a card named Vraska, Regal Gorgon, reveal it, and put it into your hand. If you search your library this way, shuffle."####,
        kind: Kind::Sorcery,
        subtypes: &[],
        legendary: false,
        generic: 4,
        shards: &[ManaCostShard::Black, ManaCostShard::Green],
        flow: Flow::Targets1,
        found: Found::Named("Vraska, Regal Gorgon"),
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Fang-Druid Summoner",
        oracle: r####"Reach
When this creature enters, you may search your library and/or graveyard for a creature card with no abilities, reveal it, and put it into your hand. If you search your library this way, shuffle."####,
        kind: Kind::Creature(2, 4),
        subtypes: &["Ape", "Druid"],
        legendary: false,
        generic: 3,
        shards: &[ManaCostShard::Green],
        flow: Flow::Cast,
        found: Found::Filler {
            name: "Vanilla Bear",
            artifact: false,
            enchantment: false,
            subtypes: &[],
            mv: 0,
        },
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Grand Master of Flowers",
        oracle: r####"As long as Grand Master of Flowers has seven or more loyalty counters on him, he's a 7/7 Dragon God creature with flying and indestructible.
[+1]: Target creature without first strike, double strike, or vigilance can't attack or block until your next turn.
[+1]: Search your library and/or graveyard for a card named Monk of the Open Hand, reveal it, and put it into your hand. If you search your library this way, shuffle."####,
        kind: Kind::Planeswalker(3),
        subtypes: &["Bahamut"],
        legendary: true,
        generic: 2,
        shards: &[ManaCostShard::White, ManaCostShard::White],
        flow: Flow::PW1,
        found: Found::Named("Monk of the Open Hand"),
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Visage of Bolas",
        oracle: r####"When this artifact enters, you may search your library and/or graveyard for a card named Nicol Bolas, the Deceiver, reveal it, and put it into your hand. If you search your library this way, shuffle.
{T}: Add {U}, {B}, or {R}."####,
        kind: Kind::Artifact,
        subtypes: &[],
        legendary: false,
        generic: 4,
        shards: &[],
        flow: Flow::Cast,
        found: Found::Named("Nicol Bolas, the Deceiver"),
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Liliana's Scorn",
        oracle: r####"Destroy target creature. You may search your library and/or graveyard for a card named Liliana, Death Mage, reveal it, and put it into your hand. If you search your library this way, shuffle."####,
        kind: Kind::Sorcery,
        subtypes: &[],
        legendary: false,
        generic: 3,
        shards: &[ManaCostShard::Black, ManaCostShard::Black],
        flow: Flow::Targets1,
        found: Found::Named("Liliana, Death Mage"),
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Runeforge Champion",
        oracle: r####"When this creature enters, you may search your library and/or graveyard for a Rune card, reveal it, and put it into your hand. If you search your library this way, shuffle.
You may pay {1} rather than pay the mana cost for Rune spells you cast."####,
        kind: Kind::Creature(2, 3),
        subtypes: &["Dwarf", "Warrior"],
        legendary: false,
        generic: 2,
        shards: &[ManaCostShard::White],
        flow: Flow::Cast,
        found: Found::Printed("Rune of Sustenance"),
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Oko's Hospitality",
        oracle: r####"Creatures you control have base power and toughness 3/3 until end of turn. You may search your library and/or graveyard for a card named Oko, the Trickster, reveal it, and put it into your hand. If you search your library this way, shuffle."####,
        kind: Kind::Instant,
        subtypes: &[],
        legendary: false,
        generic: 3,
        shards: &[ManaCostShard::Green, ManaCostShard::Blue],
        flow: Flow::Cast,
        found: Found::Named("Oko, the Trickster"),
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Dark Supplicant",
        oracle: r####"{T}, Sacrifice three Clerics: Search your graveyard, hand, and/or library for a card named Scion of Darkness and put it onto the battlefield. If you search your library this way, shuffle."####,
        kind: Kind::Creature(1, 1),
        subtypes: &["Human", "Cleric"],
        legendary: false,
        generic: 0,
        shards: &[ManaCostShard::Black],
        flow: Flow::Supplicant,
        found: Found::Named("Scion of Darkness"),
        variants: &["G", "H", "L", "GM"],
    },
    Spec {
        name: "Sorin's Guide",
        oracle: r####"When this creature enters, you may search your library and/or graveyard for a card named Sorin, Vampire Lord, reveal it, and put it into your hand. If you search your library this way, shuffle."####,
        kind: Kind::Creature(4, 2),
        subtypes: &["Vampire"],
        legendary: false,
        generic: 3,
        shards: &[ManaCostShard::Black, ManaCostShard::Black],
        flow: Flow::Cast,
        found: Found::Named("Sorin, Vampire Lord"),
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Goldmane Griffin",
        oracle: r####"Flying, vigilance
When this creature enters, you may search your library and/or graveyard for a card named Ajani, Inspiring Leader, reveal it, and put it into your hand. If you search your library this way, shuffle."####,
        kind: Kind::Creature(3, 2),
        subtypes: &["Griffin"],
        legendary: false,
        generic: 3,
        shards: &[ManaCostShard::White, ManaCostShard::White],
        flow: Flow::Cast,
        found: Found::Named("Ajani, Inspiring Leader"),
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Domri's Nodorog",
        oracle: r####"Trample
When this creature enters, you may search your library and/or graveyard for a card named Domri, City Smasher, reveal it, and put it into your hand. If you search your library this way, shuffle."####,
        kind: Kind::Creature(5, 2),
        subtypes: &["Beast"],
        legendary: false,
        generic: 3,
        shards: &[ManaCostShard::Red, ManaCostShard::Green],
        flow: Flow::Cast,
        found: Found::Named("Domri, City Smasher"),
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Last Light of Durin's Day",
        oracle: r####"Whenever a Mountain you control enters, put a quest counter on this enchantment. If it has six or more quest counters on it, sacrifice it. If you do, search your hand and/or library for a Dragon card and put it onto the battlefield. If you search your library this way, shuffle.
Mountaincycling {2} ({2}, Discard this card: Search your library for a Mountain card, reveal it, put it into your hand, then shuffle.)"####,
        kind: Kind::Enchantment,
        subtypes: &[],
        legendary: false,
        generic: 1,
        shards: &[ManaCostShard::Red],
        flow: Flow::LastLight,
        found: Found::Filler {
            name: "Dragon Filler",
            artifact: false,
            enchantment: false,
            subtypes: &["Dragon"],
            mv: 0,
        },
        variants: &["H", "L", "HM"],
    },
    Spec {
        name: "Gate to the Afterlife",
        oracle: r####"Whenever a nontoken creature you control dies, you gain 1 life. Then you may draw a card. If you do, discard a card.
{2}, {T}, Sacrifice this artifact: Search your graveyard, hand, and/or library for a card named God-Pharaoh's Gift and put it onto the battlefield. If you search your library this way, shuffle. Activate only if there are six or more creature cards in your graveyard."####,
        kind: Kind::Artifact,
        subtypes: &[],
        legendary: false,
        generic: 3,
        shards: &[],
        flow: Flow::Gate,
        found: Found::Named("God-Pharaoh's Gift"),
        variants: &["G", "H", "L", "GM"],
    },
    Spec {
        name: "Delivery Moogle",
        oracle: r####"Flying
When this creature enters, search your library and/or graveyard for an artifact card with mana value 2 or less, reveal it, and put it into your hand. If you search your library this way, shuffle."####,
        kind: Kind::Creature(3, 2),
        subtypes: &["Moogle"],
        legendary: false,
        generic: 3,
        shards: &[ManaCostShard::White],
        flow: Flow::Cast,
        found: Found::Filler {
            name: "Cheap Artifact",
            artifact: true,
            enchantment: false,
            subtypes: &[],
            mv: 2,
        },
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Angrath's Fury",
        oracle: r####"Destroy target creature. Angrath's Fury deals 3 damage to target player or planeswalker. You may search your library and/or graveyard for a card named Angrath, Minotaur Pirate, reveal it, and put it into your hand. If you search your library this way, shuffle."####,
        kind: Kind::Sorcery,
        subtypes: &[],
        legendary: false,
        generic: 3,
        shards: &[ManaCostShard::Black, ManaCostShard::Red],
        flow: Flow::TargetsFury,
        found: Found::Named("Angrath, Minotaur Pirate"),
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Sanctum of All",
        oracle: r####"At the beginning of your upkeep, you may search your library and/or graveyard for a Shrine card and put it onto the battlefield. If you search your library this way, shuffle.
If a triggered ability of another Shrine you control triggers while you control six or more Shrines, that ability triggers an additional time."####,
        kind: Kind::Enchantment,
        subtypes: &["Shrine"],
        legendary: true,
        generic: 0,
        shards: &[
            ManaCostShard::White,
            ManaCostShard::Blue,
            ManaCostShard::Black,
            ManaCostShard::Red,
            ManaCostShard::Green,
        ],
        flow: Flow::Sanctum,
        found: Found::Filler {
            name: "Shrine Filler",
            artifact: false,
            enchantment: true,
            subtypes: &["Shrine"],
            mv: 0,
        },
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Dovin's Dismissal",
        oracle: r####"Put up to one target tapped creature on top of its owner's library. You may search your library and/or graveyard for a card named Dovin, Architect of Law, reveal it, and put it into your hand. If you search your library this way, shuffle."####,
        kind: Kind::Instant,
        subtypes: &[],
        legendary: false,
        generic: 2,
        shards: &[ManaCostShard::White, ManaCostShard::Blue],
        flow: Flow::TargetsTapped,
        found: Found::Named("Dovin, Architect of Law"),
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Gideon's Resolve",
        oracle: r####"When this enchantment enters, you may search your library and/or graveyard for a card named Gideon, Martial Paragon, reveal it, and put it into your hand. If you search your library this way, shuffle.
Creatures you control get +1/+1."####,
        kind: Kind::Enchantment,
        subtypes: &[],
        legendary: false,
        generic: 4,
        shards: &[ManaCostShard::White],
        flow: Flow::Cast,
        found: Found::Named("Gideon, Martial Paragon"),
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Gideon's Battle Cry",
        oracle: r####"Put a +1/+1 counter on each creature you control. You may search your library and/or graveyard for a card named Gideon, the Oathsworn, reveal it, and put it into your hand. If you search your library this way, shuffle."####,
        kind: Kind::Sorcery,
        subtypes: &[],
        legendary: false,
        generic: 2,
        shards: &[ManaCostShard::White, ManaCostShard::White],
        flow: Flow::Cast,
        found: Found::Named("Gideon, the Oathsworn"),
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Boonweaver Giant",
        oracle: r####"When this creature enters, you may search your graveyard, hand, and/or library for an Aura card and put it onto the battlefield attached to this creature. If you search your library this way, shuffle."####,
        kind: Kind::Creature(4, 4),
        subtypes: &["Giant", "Monk"],
        legendary: false,
        generic: 6,
        shards: &[ManaCostShard::White],
        flow: Flow::Cast,
        found: Found::Printed("Arachnus Web"),
        variants: &["G", "H", "L", "GM"],
    },
    Spec {
        name: "Ethereal Elk",
        oracle: r####"Trample
When this creature enters, you may search your library and/or graveyard for a card named Vivien, Nature's Avenger, reveal it, and put it into your hand. If you search your library this way, shuffle."####,
        kind: Kind::Creature(3, 3),
        subtypes: &["Elk", "Spirit"],
        legendary: false,
        generic: 3,
        shards: &[ManaCostShard::Green, ManaCostShard::Green],
        flow: Flow::Cast,
        found: Found::Named("Vivien, Nature's Avenger"),
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Arachnus Spinner",
        oracle: r####"Reach
Tap an untapped Spider you control: Search your graveyard and/or library for a card named Arachnus Web and put it onto the battlefield attached to target creature. If you search your library this way, shuffle."####,
        kind: Kind::Creature(5, 7),
        subtypes: &["Spider"],
        legendary: false,
        generic: 5,
        shards: &[ManaCostShard::Green],
        flow: Flow::Spinner,
        found: Found::Printed("Arachnus Web"),
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Rowan's Stalwarts",
        oracle: r####"When this creature enters, you may search your library and/or graveyard for a card named Rowan, Fearless Sparkmage, reveal it, and put it into your hand. If you search your library this way, shuffle."####,
        kind: Kind::Creature(5, 2),
        subtypes: &["Human", "Knight"],
        legendary: false,
        generic: 4,
        shards: &[ManaCostShard::Red],
        flow: Flow::Cast,
        found: Found::Named("Rowan, Fearless Sparkmage"),
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Basri's Aegis",
        oracle: r####"Put a +1/+1 counter on each of up to two target creatures. You may search your library and/or graveyard for a card named Basri, Devoted Paladin, reveal it, and put it into your hand. If you search your library this way, shuffle."####,
        kind: Kind::Sorcery,
        subtypes: &[],
        legendary: false,
        generic: 2,
        shards: &[ManaCostShard::White, ManaCostShard::White],
        flow: Flow::Targets2,
        found: Found::Named("Basri, Devoted Paladin"),
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Grasping Current",
        oracle: r####"Return up to two target creatures to their owner's hand.
Search your library and/or graveyard for a card named Jace, Ingenious Mind-Mage, reveal it, and put it into your hand. If you search your library this way, shuffle."####,
        kind: Kind::Sorcery,
        subtypes: &[],
        legendary: false,
        generic: 4,
        shards: &[ManaCostShard::Blue],
        flow: Flow::Targets2,
        found: Found::Named("Jace, Ingenious Mind-Mage"),
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Ral's Dispersal",
        oracle: r####"Return target creature to its owner's hand. You may search your library and/or graveyard for a card named Ral, Caller of Storms, reveal it, and put it into your hand. If you search your library this way, shuffle."####,
        kind: Kind::Instant,
        subtypes: &[],
        legendary: false,
        generic: 3,
        shards: &[ManaCostShard::Blue, ManaCostShard::Blue],
        flow: Flow::Targets1,
        found: Found::Named("Ral, Caller of Storms"),
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Vraska's Scorn",
        oracle: r####"Target opponent loses 4 life. You may search your library and/or graveyard for a card named Vraska, Scheming Gorgon, reveal it, and put it into your hand. If you search your library this way, shuffle."####,
        kind: Kind::Sorcery,
        subtypes: &[],
        legendary: false,
        generic: 2,
        shards: &[ManaCostShard::Black, ManaCostShard::Black],
        flow: Flow::TargetsPlayer,
        found: Found::Named("Vraska, Scheming Gorgon"),
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Tale of Momo",
        oracle: r####"This spell costs {2} less to cast if a creature left the battlefield under your control this turn.
Search your library and/or graveyard for an Ally creature card, reveal it, and put it into your hand. If you search your library this way, shuffle."####,
        kind: Kind::Sorcery,
        subtypes: &[],
        legendary: false,
        generic: 2,
        shards: &[ManaCostShard::White],
        flow: Flow::Cast,
        found: Found::Filler {
            name: "Ally Filler",
            artifact: false,
            enchantment: false,
            subtypes: &["Ally"],
            mv: 0,
        },
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Garruk's Warsteed",
        oracle: r####"Vigilance
When this creature enters, you may search your library and/or graveyard for a card named Garruk, Savage Herald, reveal it, and put it into your hand. If you search your library this way, shuffle."####,
        kind: Kind::Creature(3, 5),
        subtypes: &["Rhino"],
        legendary: false,
        generic: 3,
        shards: &[ManaCostShard::Green, ManaCostShard::Green],
        flow: Flow::Cast,
        found: Found::Named("Garruk, Savage Herald"),
        variants: &["G", "L", "GM"],
    },
    Spec {
        name: "Tower Winder",
        oracle: r####"Reach, deathtouch
When this creature enters, search your library and/or graveyard for a card named Command Tower, reveal it, and put it into your hand. If you search your library this way, shuffle."####,
        kind: Kind::Creature(1, 1),
        subtypes: &["Snake"],
        legendary: false,
        generic: 1,
        shards: &[ManaCostShard::Green],
        flow: Flow::Cast,
        found: Found::Named("Command Tower"),
        variants: &["G", "L", "GM"],
    },
];

const MINDLOCK: &str = r####"Players can't search libraries."####;
const RUNE: (&str, &str) = (
    "Rune of Sustenance",
    r####"Enchant permanent
When this Aura enters, draw a card.
As long as enchanted permanent is a creature, it has lifelink.
As long as enchanted permanent is an Equipment, it has "Equipped creature has lifelink.""####,
);
const WEB: (&str, &str) = (
    "Arachnus Web",
    r####"Enchant creature
Enchanted creature can't attack or block, and its activated abilities can't be activated.
At the beginning of the end step, if enchanted creature's power is 4 or greater, destroy this Aura."####,
);
const GDK: &str = r####"Flying
As Glorious Dragon-Kin enters the battlefield, choose a color.
Artifact creatures and Dragons you control have protection from the chosen color."####;
const FROSTKITE: &str = r####"Flying
You may have this creature enter as a copy of a creature you control with power 4 or greater, except it's a Dragon in addition to its other types and it has flying."####;
const CHROMATIC: &str = r####"Enchant creature
As this Aura enters, choose a color.
This Aura enters with a sleight counter on it.
Prevent all damage that would be dealt to enchanted creature by sources of the last chosen color.
{X}: Put a sleight counter on this Aura and choose a color. X is the number of sleight counters on this Aura."####;

fn pool() -> Vec<ManaUnit> {
    let mut v = Vec::new();
    for t in [
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
    ] {
        for _ in 0..4 {
            v.push(ManaUnit::new(t, ObjectId(0), false, vec![]));
        }
    }
    for _ in 0..40 {
        v.push(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    v
}

fn add_found(scn: &mut GameScenario, f: Found) -> ObjectId {
    match f {
        Found::Named(n) => scn.add_creature_to_hand(P0, n, 1, 1).id(),
        Found::Filler {
            name,
            artifact,
            enchantment,
            subtypes,
            mv,
        } => {
            let mut b = scn.add_creature_to_hand(P0, name, 1, 1);
            if artifact {
                b.as_artifact();
            }
            if enchantment {
                b.as_enchantment();
            }
            if !subtypes.is_empty() {
                b.with_subtypes(subtypes.to_vec());
            }
            if mv > 0 {
                b.with_mana_cost(ManaCost::generic(mv));
            }
            b.id()
        }
        Found::Oracle {
            name,
            oracle,
            aura,
            subtypes,
        } => {
            if aura {
                scn.add_enchantment_to_hand_from_oracle(P0, name, oracle)
                    .with_subtypes(subtypes.to_vec())
                    .id()
            } else {
                scn.add_creature_to_hand_from_oracle(P0, name, 6, 6, oracle)
                    .with_subtypes(subtypes.to_vec())
                    .id()
            }
        }
        Found::Printed(n) => {
            if n == RUNE.0 {
                scn.add_enchantment_to_hand_from_oracle(P0, RUNE.0, RUNE.1)
                    .with_subtypes(vec!["Aura", "Rune"])
                    .id()
            } else if n == WEB.0 {
                scn.add_enchantment_to_hand_from_oracle(P0, WEB.0, WEB.1)
                    .with_subtypes(vec!["Aura"])
                    .id()
            } else {
                unreachable!("only Rune of Sustenance and Arachnus Web are printed finds")
            }
        }
    }
}

fn mana_cost(spec: &Spec) -> ManaCost {
    ManaCost::Cost {
        generic: spec.generic,
        shards: spec.shards.to_vec(),
    }
}

#[derive(Default, Debug, Clone)]
struct Obs {
    found_zone: Option<Zone>,
    pick_offered: bool,
    offered: usize,
    prompts: Vec<String>,
    shuffles: usize,
    searched: usize,
    moves_of_found: usize,
    stuck: Option<String>,
    moves: Vec<String>,
}

fn variant_name(w: &WaitingFor) -> String {
    format!("{w:?}")
        .split(|c: char| !c.is_alphanumeric())
        .next()
        .unwrap_or("")
        .to_string()
}

fn count_events(obs: &mut Obs, events: &[GameEvent], found: ObjectId) {
    for e in events {
        if let GameEvent::PlayerPerformedAction {
            player_id, action, ..
        } = e
        {
            if *player_id == P0 && *action == PlayerActionKind::ShuffledLibrary {
                obs.shuffles += 1;
            } else if *player_id == P0 && *action == PlayerActionKind::SearchedLibrary {
                obs.searched += 1;
            }
        } else if let GameEvent::ZoneChanged {
            object_id,
            from,
            to,
            ..
        } = e
        {
            if *object_id == found {
                obs.moves_of_found += 1;
                obs.moves.push(format!("{from:?}->{to:?}"));
            }
        }
    }
}

/// Answer every prompt the way a player who wants the found card would (or declines the optional search when
/// `accept` is false); there is no zone question to answer.
fn drive(runner: &mut GameRunner, found: ObjectId, obs: &mut Obs, accept: bool) {
    for _ in 0..60 {
        let w = runner.state().waiting_for.clone();
        let name = variant_name(&w);
        let res = match w {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
            WaitingFor::OptionalEffectChoice { player: P0, .. } => {
                obs.prompts.push(name);
                runner.act(GameAction::DecideOptionalEffect { accept })
            }
            WaitingFor::SearchChoice { cards, .. } => {
                obs.prompts.push(name);
                obs.offered = cards.len();
                obs.pick_offered = cards.contains(&found);
                let pick = if obs.pick_offered {
                    vec![found]
                } else {
                    vec![]
                };
                runner.act(GameAction::SelectCards { cards: pick })
            }
            WaitingFor::ReplacementChoice { .. } => {
                obs.prompts.push(name);
                runner.act(GameAction::ChooseReplacement { index: 0 })
            }
            WaitingFor::CopyTargetChoice { valid_targets, .. } => {
                obs.prompts.push(name);
                runner.act(GameAction::ChooseTarget {
                    target: valid_targets
                        .first()
                        .map(|t| engine::types::ability::TargetRef::Object(*t)),
                })
            }
            WaitingFor::NamedChoice { .. } => {
                obs.prompts.push(name);
                runner.act(GameAction::ChooseOption {
                    choice: "Red".to_string(),
                })
            }
            other => {
                obs.stuck = Some(variant_name(&other));
                break;
            }
        };
        match res {
            Ok(r) => count_events(obs, &r.events, found),
            Err(e) => {
                obs.stuck = Some(format!("ERR {e:?}").chars().take(120).collect());
                break;
            }
        }
    }
    obs.found_zone = runner.state().objects.get(&found).map(|o| o.zone);
}

fn run_case(spec: &Spec, variant: &str) -> Obs {
    run_case_deciding(spec, variant, true)
}

/// `variant`: first letter G(raveyard) / H(and) / L(ibrary) / X (exile: no zone the search can see) holds
/// the found card; a trailing M puts a printed Mindlock Orb on the battlefield.
fn run_case_deciding(spec: &Spec, variant: &str, accept: bool) -> Obs {
    let mut scn = GameScenario::new();
    scn.at_phase(Phase::PreCombatMain);
    let muzzled = variant.ends_with('M');
    let zone = match variant.chars().next().unwrap() {
        'G' => Zone::Graveyard,
        'H' => Zone::Hand,
        'X' => Zone::Exile,
        _ => Zone::Library,
    };
    scn.add_card_to_library_top(P0, "Cover One");
    scn.add_card_to_library_top(P0, "Cover Two");
    scn.add_card_to_library_top(P1, "Their Cover");
    let found = add_found(&mut scn, spec.found);
    if let Found::Oracle {
        name: "Deceptive Frostkite",
        ..
    } = spec.found
    {
        scn.add_creature(P0, "Power Four Bear", 4, 4);
    }
    if muzzled {
        scn.add_artifact_from_oracle(P1, "Mindlock Orb", MINDLOCK);
    }
    let opp = |scn: &mut GameScenario, n: &str| scn.add_creature(P1, n, 2, 2).id();
    // (source id, how to start)
    enum Start {
        Cast {
            targets: Vec<ObjectId>,
            player: bool,
        },
        Activate {
            idx: usize,
            targets: Vec<ObjectId>,
            pay: Vec<ObjectId>,
        },
        Trigger,
        PlayLand(ObjectId),
    }
    let mut tapped_target = None;
    let (src, start) = match spec.flow {
        Flow::Cast
        | Flow::Targets1
        | Flow::Targets2
        | Flow::TargetsFury
        | Flow::TargetsPlayer
        | Flow::TargetsTapped => {
            let mut b = match spec.kind {
                Kind::Creature(p, t) => {
                    scn.add_creature_to_hand_from_oracle(P0, spec.name, p, t, spec.oracle)
                }
                Kind::Artifact => scn.add_artifact_to_hand_from_oracle(P0, spec.name, spec.oracle),
                Kind::Enchantment => {
                    scn.add_enchantment_to_hand_from_oracle(P0, spec.name, spec.oracle)
                }
                Kind::Sorcery => {
                    scn.add_spell_to_hand_from_oracle(P0, spec.name, false, spec.oracle)
                }
                Kind::Instant => {
                    scn.add_spell_to_hand_from_oracle(P0, spec.name, true, spec.oracle)
                }
                Kind::Planeswalker(_) => unreachable!(),
            };
            if !spec.subtypes.is_empty() {
                b.with_subtypes(spec.subtypes.to_vec());
            }
            if spec.legendary {
                b.as_legendary();
            }
            b.with_mana_cost(mana_cost(spec));
            let id = b.id();
            let (targets, player) = match spec.flow {
                Flow::Targets1 => (vec![opp(&mut scn, "Opp Bear")], false),
                Flow::Targets2 => (
                    vec![opp(&mut scn, "Opp Bear"), opp(&mut scn, "Opp Cub")],
                    false,
                ),
                Flow::TargetsFury => (vec![opp(&mut scn, "Opp Bear")], true),
                Flow::TargetsPlayer => (vec![], true),
                Flow::TargetsTapped => {
                    let t = opp(&mut scn, "Opp Bear");
                    tapped_target = Some(t);
                    (vec![t], false)
                }
                Flow::Cast => (vec![], false),
                Flow::PW1
                | Flow::Gate
                | Flow::Supplicant
                | Flow::Sanctum
                | Flow::LastLight
                | Flow::Spinner => unreachable!("started below, not cast"),
            };
            (id, Start::Cast { targets, player })
        }
        Flow::PW1 => {
            let sub = spec.subtypes[0];
            let Kind::Planeswalker(l) = spec.kind else {
                unreachable!()
            };
            let id = scn
                .add_planeswalker_from_oracle(P0, spec.name, sub, l, spec.oracle)
                .id();
            let t = opp(&mut scn, "Opp Bear");
            (
                id,
                Start::Activate {
                    idx: 1,
                    targets: vec![t],
                    pay: vec![],
                },
            )
        }
        Flow::Gate => {
            let id = scn
                .add_artifact_from_oracle(P0, spec.name, spec.oracle)
                .id();
            for i in 0..6 {
                scn.add_creature_to_graveyard(P0, &format!("Graveyard Filler {i}"), 1, 1);
            }
            (
                id,
                Start::Activate {
                    idx: 0,
                    targets: vec![],
                    pay: vec![],
                },
            )
        }
        Flow::Supplicant => {
            let Kind::Creature(p, t) = spec.kind else {
                unreachable!()
            };
            let id = scn
                .add_creature_from_oracle(P0, spec.name, p, t, spec.oracle)
                .with_subtypes(spec.subtypes.to_vec())
                .id();
            let c1 = scn
                .add_creature(P0, "Cleric Filler One", 1, 1)
                .with_subtypes(vec!["Cleric"])
                .id();
            let c2 = scn
                .add_creature(P0, "Cleric Filler Two", 1, 1)
                .with_subtypes(vec!["Cleric"])
                .id();
            (
                id,
                Start::Activate {
                    idx: 0,
                    targets: vec![],
                    pay: vec![id, c1, c2],
                },
            )
        }
        Flow::Spinner => {
            let Kind::Creature(p, t) = spec.kind else {
                unreachable!()
            };
            let id = scn
                .add_creature_from_oracle(P0, spec.name, p, t, spec.oracle)
                .with_subtypes(spec.subtypes.to_vec())
                .id();
            let target = scn.add_creature(P0, "Aura Host", 2, 2).id();
            (
                id,
                Start::Activate {
                    idx: 0,
                    targets: vec![target],
                    pay: vec![id],
                },
            )
        }
        Flow::Sanctum => {
            let id = scn
                .add_enchantment_from_oracle(P0, spec.name, spec.oracle)
                .as_legendary()
                .with_subtypes(spec.subtypes.to_vec())
                .id();
            (id, Start::Trigger)
        }
        Flow::LastLight => {
            let id = scn
                .add_enchantment_from_oracle(P0, spec.name, spec.oracle)
                .id();
            scn.with_counter(id, CounterType::Generic("quest".to_string()), 5);
            let mountain = scn
                .add_land_to_hand(P0, "Mountain")
                .with_subtypes(vec!["Mountain"])
                .id();
            (id, Start::PlayLand(mountain))
        }
    };
    scn.with_mana_pool(P0, pool());
    let mut runner = scn.build();
    runner.state_mut().active_player = P0;
    if let Some(t) = tapped_target {
        runner.state_mut().objects.get_mut(&t).unwrap().tapped = true;
    }
    if zone != Zone::Hand {
        assert!(!move_object_for_test(
            runner.state_mut(),
            ZoneMoveRequest::pregame(found, zone),
            &mut Vec::new()
        ));
    }
    let mut obs = Obs::default();
    match start {
        Start::Cast { targets, player } => {
            let mut c = runner.cast(src);
            for t in targets {
                c = c.target_object(t);
            }
            if player {
                c = c.target_player(P1);
            }
            let _ = c.commit();
        }
        Start::Activate { idx, targets, pay } => {
            let mut a = runner.activate(src, idx).stop_at_optional();
            for t in targets {
                a = a.target_object(t);
            }
            if !pay.is_empty() {
                a = a.pay_with(&pay);
            }
            let _ = a.resolve();
        }
        Start::Trigger => {
            process_triggers(
                runner.state_mut(),
                &[GameEvent::PhaseChanged {
                    phase: Phase::Upkeep,
                }],
            );
        }
        Start::PlayLand(land) => {
            let card_id = runner.state().objects[&land].card_id;
            let r = runner
                .act(GameAction::PlayLand {
                    object_id: land,
                    card_id,
                })
                .expect("play Mountain");
            count_events(&mut obs, &r.events, found);
        }
    }
    drive(&mut runner, found, &mut obs, accept);
    obs
}

/// Members whose search starts from an activation: the harness discards the events of the activation
/// call itself, so `SearchedLibrary` is not counted for them (the shuffle column is the proof).
const ACTIVATED_MEMBERS: &[&str] = &[
    "Grand Master of Flowers",
    "Dark Supplicant",
    "Gate to the Afterlife",
    "Arachnus Spinner",
];

fn spec_named(n: &str) -> &'static Spec {
    SPECS.iter().find(|s| s.name == n).unwrap()
}

fn counts_search_events(spec: &Spec) -> bool {
    !ACTIVATED_MEMBERS.contains(&spec.name)
}

/// CR 701.23a + CR 701.24a + CR 608.2c: with a printed Mindlock Orb ("Players can't search libraries.") the
/// graveyard is the only zone searched, so "if you search your library this way" is false and nothing shuffles.
#[test]
fn muzzled_search_does_not_shuffle_for_every_member() {
    let mut failures = Vec::new();
    for spec in SPECS {
        let muzzled = spec
            .variants
            .iter()
            .find(|variant| variant.ends_with('M'))
            .expect("every member has a muzzled variant");
        let o = run_case(spec, muzzled);
        let found_elsewhere = matches!(
            o.found_zone,
            Some(Zone::Hand | Zone::Battlefield | Zone::Graveyard)
        );
        let library_searched = counts_search_events(spec) && o.searched != 0;
        if o.stuck.is_some() || !found_elsewhere || library_searched || o.shuffles != 0 {
            failures.push(format!("{}: {o:?}", spec.name));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// CR 701.24a: the same members with the card in the library search the library and shuffle exactly once
/// (hostile control: the guard must not suppress a real library search).
#[test]
fn library_search_shuffles_once_for_every_member() {
    let mut failures = Vec::new();
    for spec in SPECS.iter().filter(|s| s.variants.contains(&"L")) {
        let o = run_case(spec, "L");
        let found_elsewhere = matches!(o.found_zone, Some(Zone::Hand | Zone::Battlefield));
        let search_counted = !counts_search_events(spec) || o.searched == 1;
        if o.stuck.is_some() || !found_elsewhere || !search_counted || o.shuffles != 1 {
            failures.push(format!("{}: {o:?}", spec.name));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// CR 701.23b + CR 701.24a: a library search that finds no card is still a search of the library, so the
/// shuffle stays (the found card is exiled, outside every searchable zone).
#[test]
fn library_search_that_finds_nothing_still_shuffles() {
    let o = run_case(spec_named("Ashiok's Forerunner"), "X");
    assert_eq!(o.searched, 1, "the library was searched: {o:?}");
    assert_eq!(
        o.shuffles, 1,
        "a failed library search still shuffles: {o:?}"
    );
    assert_eq!(o.found_zone, Some(Zone::Exile), "{o:?}");
}

/// CR 608.2c: a declined "you may search" performs no search, so the gated shuffle does not happen.
#[test]
fn declined_optional_search_does_not_shuffle() {
    let o = run_case_deciding(spec_named("Ashiok's Forerunner"), "L", false);
    assert_eq!(o.searched, 0, "{o:?}");
    assert_eq!(o.shuffles, 0, "{o:?}");
    assert_eq!(o.found_zone, Some(Zone::Library), "{o:?}");
}

fn last_light_finding(found: Found) -> Spec {
    let mut spec = *spec_named("Last Light of Durin's Day");
    spec.found = found;
    spec
}

/// CR 614.1c + CR 707.9a + CR 608.2c: the found permanent enters through a copy choice (Deceptive Frostkite
/// enters as a copy of a power-4 creature), whose completion runs a nested top-level chain between the search
/// and the printed shuffle. That chain clears the per-resolution action ledger; the shuffle must still happen.
#[test]
fn copy_choice_entry_between_search_and_shuffle_keeps_the_shuffle() {
    let spec = last_light_finding(Found::Oracle {
        name: "Deceptive Frostkite",
        oracle: FROSTKITE,
        aura: false,
        subtypes: &["Dragon"],
    });
    let o = run_case(&spec, "L");
    assert!(
        o.prompts.iter().any(|p| p == "CopyTargetChoice"),
        "the copy choice was reached: {o:?}"
    );
    assert_eq!(o.found_zone, Some(Zone::Battlefield), "{o:?}");
    assert_eq!(o.searched, 1, "{o:?}");
    assert_eq!(o.shuffles, 1, "{o:?}");
}

/// The same copy-choice route with the library muzzled: the found card comes from the hand and the
/// shuffle must not happen even though a nested chain ran.
#[test]
fn copy_choice_entry_with_muzzled_library_does_not_shuffle() {
    let spec = last_light_finding(Found::Oracle {
        name: "Deceptive Frostkite",
        oracle: FROSTKITE,
        aura: false,
        subtypes: &["Dragon"],
    });
    let o = run_case(&spec, "HM");
    assert!(
        o.prompts.iter().any(|p| p == "CopyTargetChoice"),
        "the copy choice was reached: {o:?}"
    );
    assert_eq!(o.found_zone, Some(Zone::Battlefield), "{o:?}");
    assert_eq!(o.shuffles, 0, "{o:?}");
}

/// As-enters choices ("As this enters, choose a color") run no nested top-level chain; the shuffle stays.
#[test]
fn as_enters_choice_entry_between_search_and_shuffle_keeps_the_shuffle() {
    let dragon_kin = last_light_finding(Found::Oracle {
        name: "Glorious Dragon-Kin",
        oracle: GDK,
        aura: false,
        subtypes: &["Dragon"],
    });
    let mut boonweaver = *spec_named("Boonweaver Giant");
    boonweaver.found = Found::Oracle {
        name: "Chromatic Armor",
        oracle: CHROMATIC,
        aura: true,
        subtypes: &["Aura"],
    };
    for spec in [dragon_kin, boonweaver] {
        let o = run_case(&spec, "L");
        assert!(
            o.prompts.iter().any(|p| p == "NamedChoice"),
            "{}: the as-enters choice was reached: {o:?}",
            spec.name
        );
        assert_eq!(
            o.found_zone,
            Some(Zone::Battlefield),
            "{}: {o:?}",
            spec.name
        );
        assert_eq!(o.shuffles, 1, "{}: {o:?}", spec.name);
    }
}
