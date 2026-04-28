#![allow(non_camel_case_types)]

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub enum SuperType {
    Basic,
    Legendary,
    Ongoing,
    Snow,
    World,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub enum CardType {
    Artifact,
    Battle,
    Conspiracy,
    Creature,
    Dungeon,
    Enchantment,
    Instant,
    Kindred,
    Land,
    Phenomenon,
    Plane,
    Planeswalker,
    Scheme,
    Sorcery,
    Vanguard,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub enum DungeonType {
    Undercity,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub enum BattleType {
    Siege,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub enum CreatureType {
    Advisor,
    Aetherborn,
    Alien,
    Ally,
    Angel,
    Antelope,
    Ape,
    Archer,
    Archon,
    Armadillo,
    Army,
    Artificer,
    Assassin,
    AssemblyWorker,
    Astartes,
    Atog,
    Aurochs,
    Avatar,
    Azra,
    Badger,
    Balloon,
    Barbarian,
    Bard,
    Basilisk,
    Bat,
    Bear,
    Beast,
    Beaver,
    Beeble,
    Beholder,
    Berserker,
    Bird,
    Bison,
    Blinkmoth,
    Boar,
    Bringer,
    Brushwagg,
    Camarid,
    Camel,
    Capybara,
    Caribou,
    Carrier,
    Cat,
    Centaur,
    Chimera,
    Citizen,
    Cleric,
    Clown,
    Cockatrice,
    Construct,
    Coward,
    Coyote,
    Crab,
    Crocodile,
    Ctan,
    Custodes,
    Cyberman,
    Cyclops,
    Dalek,
    Dauthi,
    Demigod,
    Demon,
    Deserter,
    Detective,
    Devil,
    Dinosaur,
    Djinn,
    Doctor,
    Dog,
    Dragon,
    Drake,
    Dreadnought,
    Drix,
    Drone,
    Druid,
    Dryad,
    Dwarf,
    Echidna,
    Efreet,
    Egg,
    Elder,
    Eldrazi,
    Elemental,
    Elephant,
    Elf,
    Elk,
    Employee,
    Eye,
    Faerie,
    Ferret,
    Fish,
    Flagbearer,
    Fox,
    Fractal,
    Frog,
    Fungus,
    Gamer,
    Gamma,
    Gargoyle,
    Germ,
    Giant,
    Giraffe,
    Gith,
    Glimmer,
    Gnoll,
    Gnome,
    Goat,
    Goblin,
    God,
    Golem,
    Gorgon,
    Graveborn,
    Gremlin,
    Griffin,
    Guest,
    Hag,
    Halfling,
    Hamster,
    Harpy,
    Hedgehog,
    Hellion,
    Hero,
    Hippo,
    Hippogriff,
    Homarid,
    Homunculus,
    Horror,
    Horse,
    Human,
    Hydra,
    Hyena,
    Illusion,
    Imp,
    Incarnation,
    Incubator,
    Inkling,
    Inquisitor,
    Insect,
    Jackal,
    Jellyfish,
    Juggernaut,
    Kangaroo,
    Kavu,
    Kirin,
    Kithkin,
    Knight,
    Kobold,
    Kor,
    Kraken,
    Lamia,
    Lammasu,
    Leech,
    Lemur,
    Leviathan,
    Lhurgoyf,
    Licid,
    Lizard,
    Llama,
    Lobster,
    Manticore,
    Masticore,
    Mercenary,
    Merfolk,
    Metathran,
    Minion,
    Minotaur,
    Mite,
    Mole,
    Monger,
    Mongoose,
    Monk,
    Monkey,
    Moogle,
    Mount,
    Moonfolk,
    Mouse,
    Mutant,
    Myr,
    Mystic,
    Nautilus,
    Necron,
    Nephilim,
    Nightmare,
    Nightstalker,
    Ninja,
    Noble,
    Noggle,
    Nomad,
    Nymph,
    Octopus,
    Ogre,
    Ooze,
    Orb,
    Orc,
    Orgg,
    Otter,
    Ouphe,
    Ox,
    Oyster,
    Pangolin,
    Peasant,
    Pegasus,
    Pentavite,
    Performer,
    Pest,
    Phelddagrif,
    Phoenix,
    Phyrexian,
    Pilot,
    Pincher,
    Pirate,
    Plant,
    Platypus,
    Porcupine,
    Possum,
    Praetor,
    Primarch,
    Prism,
    Processor,
    Qu,
    Rabbit,
    Raccoon,
    Ranger,
    Rat,
    Rebel,
    Reflection,
    Rhino,
    Rigger,
    Robot,
    Rogue,
    Sable,
    Salamander,
    Samurai,
    Sand,
    Saproling,
    Satyr,
    Scarecrow,
    Scientist,
    Scion,
    Scorpion,
    Scout,
    Sculpture,
    Seal,
    Serf,
    Serpent,
    Servo,
    Shade,
    Shaman,
    Shapeshifter,
    Shark,
    Sheep,
    Siren,
    Skeleton,
    Skrull,
    Skunk,
    Slith,
    Sliver,
    Sloth,
    Slug,
    Snail,
    Snake,
    Soldier,
    Soltari,
    Sorcerer,
    Spawn,
    Specter,
    Spellshaper,
    Sphinx,
    Spider,
    Spike,
    Spirit,
    Splinter,
    Sponge,
    Squid,
    Squirrel,
    Starfish,
    Surrakar,
    Survivor,
    Symbiote,
    Synth,
    Tentacle,
    Tetravite,
    Thalakos,
    Thopter,
    Thrull,
    Tiefling,
    TimeLord,
    Toy,
    Treefolk,
    Trilobite,
    Triskelavite,
    Troll,
    Turtle,
    Tyranid,
    Unicorn,
    Utrom,
    Vampire,
    Varmint,
    Vedalken,
    Villain,
    Volver,
    Wall,
    Walrus,
    Warlock,
    Warrior,
    Weasel,
    Weird,
    Werewolf,
    Whale,
    Wizard,
    Wolf,
    Wolverine,
    Wombat,
    Worm,
    Wraith,
    Wurm,
    Yeti,
    Zombie,
    Zubera,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub enum LandType {
    Cave,
    Desert,
    Forest,
    Gate,
    Island,
    Lair,
    Locus,
    Mine,
    Mountain,
    Plains,
    Planet,
    PowerPlant,
    Sphere,
    Swamp,
    Tower,
    Town,
    Urzas,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub enum SpellType {
    Adventure,
    Arcane,
    Lesson,
    Chorus,
    Trap,
    Omen,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub enum EnchantmentType {
    Aura,
    Background,
    Cartouche,
    Case,
    Class,
    Curse,
    Plan,
    Room,
    Rune,
    Saga,
    Shard,
    Shrine,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub enum ArtifactType {
    Attraction,
    Blood,
    Bobblehead,
    Book,
    Clue,
    Equipment,
    Food,
    Fortification,
    Infinity,
    Junk,
    Lander,
    Powerstone,
    Spacecraft,
    Stone,
    Treasure,
    Vehicle,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub enum PlaneswalkerType {
    Ajani,
    Aminatou,
    Angrath,
    Arlinn,
    Ashiok,
    Bahamut,
    Basri,
    Bolas,
    Calix,
    Chandra,
    Comet,
    Dack,
    Dakkon,
    Daretti,
    Davriel,
    Deb,
    Dellian,
    Dihada,
    Domri,
    Dovin,
    Ellywick,
    Elminster,
    Elspeth,
    Estrid,
    Freyalise,
    Garruk,
    Gideon,
    Grist,
    Guff,
    Huatli,
    Jace,
    Jared,
    Jaya,
    Jeska,
    Kaito,
    Karn,
    Kasmina,
    Kaya,
    Kiora,
    Koth,
    Liliana,
    Lolth,
    Lukka,
    Minsc,
    Mordenkainen,
    Nahiri,
    Narset,
    Niko,
    Nissa,
    Nixilis,
    Oko,
    Quintorius,
    Ral,
    Rowan,
    Saheeli,
    Samut,
    Sarkhan,
    Serra,
    Sivitri,
    Sorin,
    Szat,
    Tamiyo,
    Tasha,
    Teferi,
    Teyo,
    Tezzeret,
    Tibalt,
    Tyvar,
    Ugin,
    Urza,
    Venser,
    Vivien,
    Vraska,
    Vronos,
    Will,
    Windgrace,
    Wrenn,
    Xenagos,
    Yanggu,
    Yanling,
    Zariel,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub enum PlaneType {
    Alara,
    AlfavaMetraxis,
    Amonkhet,
    AndrozaniMinor,
    Antausia,
    Apalapucia,
    Arcavios,
    Arkhos,
    Avishkar,
    Azgol,
    Belenon,
    BolassMeditationRealm,
    Capenna,
    Cridhe,
    Darillium,
    Dominaria,
    Earth,
    Echoir,
    Eldraine,
    Equilor,
    Ergamon,
    Fabacin,
    Fiora,
    Gallifrey,
    Gargantikar,
    Gobakhan,
    HorseheadNebula,
    Ikoria,
    Innistrad,
    Iquatana,
    Ir,
    Ixalan,
    Kaldheim,
    Kamigawa,
    Kandoka,
    Karsus,
    Kephalai,
    Kinshala,
    Kolbahan,
    Kylem,
    Kyneth,
    Lorwyn,
    Luvion,
    Mars,
    Mercadia,
    Mirrodin,
    Moag,
    Mongseng,
    Moon,
    Muraganda,
    Necros,
    NewEarth,
    NewPhyrexia,
    OutsideMuttersSpiral,
    Phyrexia,
    Pyrulea,
    Rabiah,
    Rath,
    Ravnica,
    Regatha,
    Segovia,
    SerrasRealm,
    Shadowmoor,
    Shandalar,
    Shenmeng,
    Skaro,
    Spaceship,
    Tarkir,
    TheAbyss,
    TheDalekAsylum,
    TheLibrary,
    Theros,
    Time,
    Trenzalore,
    UnknownPlanet,
    Ulgrotha,
    Valla,
    Vryn,
    Wildfire,
    Xerex,
    Zendikar,
    Zhalfir,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(untagged)]
pub enum SubType {
    // DungeonType
    Undercity,

    // BattleType
    Siege,

    // CreatureType
    Advisor,
    Aetherborn,
    Alien,
    Ally,
    Angel,
    Antelope,
    Ape,
    Archer,
    Archon,
    Armadillo,
    Army,
    Artificer,
    Assassin,
    AssemblyWorker,
    Astartes,
    Atog,
    Aurochs,
    Avatar,
    Azra,
    Badger,
    Balloon,
    Barbarian,
    Bard,
    Basilisk,
    Bat,
    Bear,
    Beast,
    Beaver,
    Beeble,
    Beholder,
    Berserker,
    Bird,
    Bison,
    Blinkmoth,
    Boar,
    Bringer,
    Brushwagg,
    Camarid,
    Camel,
    Capybara,
    Caribou,
    Carrier,
    Cat,
    Centaur,
    Chimera,
    Citizen,
    Cleric,
    Clown,
    Cockatrice,
    Construct,
    Coward,
    Coyote,
    Crab,
    Crocodile,
    Ctan,
    Custodes,
    Cyberman,
    Cyclops,
    Dalek,
    Dauthi,
    Demigod,
    Demon,
    Deserter,
    Detective,
    Devil,
    Dinosaur,
    Djinn,
    Doctor,
    Dog,
    Dragon,
    Drake,
    Dreadnought,
    Drix,
    Drone,
    Druid,
    Dryad,
    Dwarf,
    Echidna,
    Efreet,
    Egg,
    Elder,
    Eldrazi,
    Elemental,
    Elephant,
    Elf,
    Elk,
    Employee,
    Eye,
    Faerie,
    Ferret,
    Fish,
    Flagbearer,
    Fox,
    Fractal,
    Frog,
    Fungus,
    Gamer,
    Gamma,
    Gargoyle,
    Germ,
    Giant,
    Giraffe,
    Gith,
    Glimmer,
    Gnoll,
    Gnome,
    Goat,
    Goblin,
    God,
    Golem,
    Gorgon,
    Graveborn,
    Gremlin,
    Griffin,
    Guest,
    Hag,
    Halfling,
    Hamster,
    Harpy,
    Hedgehog,
    Hellion,
    Hero,
    Hippo,
    Hippogriff,
    Homarid,
    Homunculus,
    Horror,
    Horse,
    Human,
    Hydra,
    Hyena,
    Illusion,
    Imp,
    Incarnation,
    Incubator,
    Inkling,
    Inquisitor,
    Insect,
    Jackal,
    Jellyfish,
    Juggernaut,
    Kangaroo,
    Kavu,
    Kirin,
    Kithkin,
    Knight,
    Kobold,
    Kor,
    Kraken,
    Lamia,
    Lammasu,
    Leech,
    Lemur,
    Leviathan,
    Lhurgoyf,
    Licid,
    Lizard,
    Llama,
    Lobster,
    Manticore,
    Masticore,
    Mercenary,
    Merfolk,
    Metathran,
    Minion,
    Minotaur,
    Mite,
    Mole,
    Monger,
    Mongoose,
    Monk,
    Monkey,
    Moogle,
    Mount,
    Moonfolk,
    Mouse,
    Mutant,
    Myr,
    Mystic,
    Nautilus,
    Necron,
    Nephilim,
    Nightmare,
    Nightstalker,
    Ninja,
    Noble,
    Noggle,
    Nomad,
    Nymph,
    Octopus,
    Ogre,
    Ooze,
    Orb,
    Orc,
    Orgg,
    Otter,
    Ouphe,
    Ox,
    Oyster,
    Pangolin,
    Peasant,
    Pegasus,
    Pentavite,
    Performer,
    Pest,
    Phelddagrif,
    Phoenix,
    Phyrexian,
    Pilot,
    Pincher,
    Pirate,
    Plant,
    Platypus,
    Porcupine,
    Possum,
    Praetor,
    Primarch,
    Prism,
    Processor,
    Qu,
    Rabbit,
    Raccoon,
    Ranger,
    Rat,
    Rebel,
    Reflection,
    Rhino,
    Rigger,
    Robot,
    Rogue,
    Sable,
    Salamander,
    Samurai,
    Sand,
    Saproling,
    Satyr,
    Scarecrow,
    Scientist,
    Scion,
    Scorpion,
    Scout,
    Sculpture,
    Seal,
    Serf,
    Serpent,
    Servo,
    Shade,
    Shaman,
    Shapeshifter,
    Shark,
    Sheep,
    Siren,
    Skeleton,
    Skrull,
    Skunk,
    Slith,
    Sliver,
    Sloth,
    Slug,
    Snail,
    Snake,
    Soldier,
    Soltari,
    Sorcerer,
    Spawn,
    Specter,
    Spellshaper,
    Sphinx,
    Spider,
    Spike,
    Spirit,
    Splinter,
    Sponge,
    Squid,
    Squirrel,
    Starfish,
    Surrakar,
    Survivor,
    Symbiote,
    Synth,
    Tentacle,
    Tetravite,
    Thalakos,
    Thopter,
    Thrull,
    Tiefling,
    TimeLord,
    Toy,
    Treefolk,
    Trilobite,
    Triskelavite,
    Troll,
    Turtle,
    Tyranid,
    Unicorn,
    Utrom,
    Vampire,
    Varmint,
    Vedalken,
    Villain,
    Volver,
    Wall,
    Walrus,
    Warlock,
    Warrior,
    Weasel,
    Weird,
    Werewolf,
    Whale,
    Wizard,
    Wolf,
    Wolverine,
    Wombat,
    Worm,
    Wraith,
    Wurm,
    Yeti,
    Zombie,
    Zubera,

    // LandType
    Cave,
    Desert,
    Forest,
    Gate,
    Island,
    Lair,
    Locus,
    Mine,
    Mountain,
    Plains,
    Planet,
    PowerPlant,
    Sphere,
    Swamp,
    Tower,
    Town,
    Urzas,

    // SpellType
    Adventure,
    Arcane,
    Lesson,
    Chorus,
    Trap,
    Omen,

    // EnchantmentType
    Aura,
    Background,
    Cartouche,
    Case,
    Class,
    Curse,
    Plan,
    Room,
    Rune,
    Saga,
    Shard,
    Shrine,

    // ArtifactType
    Attraction,
    Blood,
    Bobblehead,
    Book,
    Clue,
    Equipment,
    Food,
    Fortification,
    Infinity,
    Junk,
    Lander,
    Powerstone,
    Spacecraft,
    Stone,
    Treasure,
    Vehicle,

    // PlaneswalkerType
    Ajani,
    Aminatou,
    Angrath,
    Arlinn,
    Ashiok,
    Bahamut,
    Basri,
    Bolas,
    Calix,
    Chandra,
    Comet,
    Dack,
    Dakkon,
    Daretti,
    Davriel,
    Deb,
    Dellian,
    Dihada,
    Domri,
    Dovin,
    Ellywick,
    Elminster,
    Elspeth,
    Estrid,
    Freyalise,
    Garruk,
    Gideon,
    Grist,
    Guff,
    Huatli,
    Jace,
    Jared,
    Jaya,
    Jeska,
    Kaito,
    Karn,
    Kasmina,
    Kaya,
    Kiora,
    Koth,
    Liliana,
    Lolth,
    Lukka,
    Minsc,
    Mordenkainen,
    Nahiri,
    Narset,
    Niko,
    Nissa,
    Nixilis,
    Oko,
    Quintorius,
    Ral,
    Rowan,
    Saheeli,
    Samut,
    Sarkhan,
    Serra,
    Sivitri,
    Sorin,
    Szat,
    Tamiyo,
    Tasha,
    Teferi,
    Teyo,
    Tezzeret,
    Tibalt,
    Tyvar,
    Ugin,
    Urza,
    Venser,
    Vivien,
    Vraska,
    Vronos,
    Will,
    Windgrace,
    Wrenn,
    Xenagos,
    Yanggu,
    Yanling,
    Zariel,

    // PlaneType
    Alara,
    AlfavaMetraxis,
    Amonkhet,
    AndrozaniMinor,
    Antausia,
    Apalapucia,
    Arcavios,
    Arkhos,
    Avishkar,
    Azgol,
    Belenon,
    BolassMeditationRealm,
    Capenna,
    Cridhe,
    Darillium,
    Dominaria,
    Earth,
    Echoir,
    Eldraine,
    Equilor,
    Ergamon,
    Fabacin,
    Fiora,
    Gallifrey,
    Gargantikar,
    Gobakhan,
    HorseheadNebula,
    Ikoria,
    Innistrad,
    Iquatana,
    Ir,
    Ixalan,
    Kaldheim,
    Kamigawa,
    Kandoka,
    Karsus,
    Kephalai,
    Kinshala,
    Kolbahan,
    Kylem,
    Kyneth,
    Lorwyn,
    Luvion,
    Mars,
    Mercadia,
    Mirrodin,
    Moag,
    Mongseng,
    Moon,
    Muraganda,
    Necros,
    NewEarth,
    NewPhyrexia,
    OutsideMuttersSpiral,
    Phyrexia,
    Pyrulea,
    Rabiah,
    Rath,
    Ravnica,
    Regatha,
    Segovia,
    SerrasRealm,
    Shadowmoor,
    Shandalar,
    Shenmeng,
    Skaro,
    Spaceship,
    Tarkir,
    TheAbyss,
    TheDalekAsylum,
    TheLibrary,
    Theros,
    Time,
    Trenzalore,
    UnknownPlanet,
    Ulgrotha,
    Valla,
    Vryn,
    Wildfire,
    Xerex,
    Zendikar,
    Zhalfir,
    /// Catch-all for subtype names not yet enumerated above (joke sets,
    /// new releases, etc.). Untagged variant order matters: this MUST be
    /// last so explicit unit variants match first.
    Other(String),
}

type CreatureTypeWord = CreatureType;

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct CardPT {
    pub power: i32,
    pub toughness: i32,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct OracleTypeline {
    pub supertypes: Vec<SuperType>,
    pub cardtypes: Vec<CardType>,
    pub subtypes: Vec<SubType>,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_CreatureTypeVariable", content = "args")]
pub enum CreatureTypeVariable {
    CreatureTypesOfExiled(Box<CardInExile>),
    TheChosenCreatureType,
    TheChosenCreatureTypes,
    TheNotedCreatureType,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_CardtypeVariable", content = "args")]
pub enum CardtypeVariable {
    EachableCardtype,
    TheChosenCardtype,
    CardtypeOfExiled(Box<CardInExile>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_LandTypeVariable", content = "args")]
pub enum LandTypeVariable {
    AnyBasicLandTypeAmongPermanents(Box<Permanents>),
    AnyLandTypeOfPermanent(Box<Permanent>),
    EachBasicLandType,
    TheChosenLandType,
    TheFirstChosenLandType,
    TheSecondChosenLandType,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_PlaneswalkerTypeVariable", content = "args")]
pub enum PlaneswalkerTypeVariable {
    TheChosenPlaneswalkerType,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_CounterType", content = "args")]
pub enum CounterType {
    // PT Counter
    PTCounter(i32, i32),

    // Player Counter
    ExperienceCounter,
    PoisonCounter,

    // Planeswalker / Siege / Saga Counter
    DefenseCounter,
    LoreCounter,
    LoyaltyCounter,

    // Ability Counter
    DeathtouchCounter,
    DoubleStrikeCounter,
    ExaltedCounter,
    FirstStrikeCounter,
    FlyingCounter,
    HasteCounter,
    HexproofCounter,
    IndestructibleCounter,
    LifelinkCounter,
    MenaceCounter,
    ReachCounter,
    ShadowCounter,
    TrampleCounter,
    VigilanceCounter,
    DecayedCounter,

    // Normal Counter
    AcornCounter,
    AegisCounter,
    AgeCounter,
    AimCounter,
    ArrowCounter,
    ArrowheadCounter,
    AwakeningCounter,
    BaitCounter,
    BlazeCounter,
    BlessingCounter,
    BlightCounter,
    BloodCounter,
    BloodlineCounter,
    BloodstainCounter,
    BookCounter,
    BoreCounter,
    BountyCounter,
    BrainCounter,
    BriberyCounter,
    BrickCounter,
    BurdenCounter,
    CageCounter,
    CarrionCounter,
    CellCounter,
    ChargeCounter,
    ChorusCounter,
    CoinCounter,
    CollectionCounter,
    ComponentCounter,
    ConquerorCounter,
    ContestedCounter,
    CorpseCounter,
    CorruptionCounter,
    CreditCounter,
    CroakCounter,
    CrystalCounter,
    CubeCounter,
    CurrencyCounter,
    DeathCounter,
    DelayCounter,
    DepletionCounter,
    DescentCounter,
    DespairCounter,
    DevotionCounter,
    DiscoveryCounter,
    DivinityCounter,
    DoomCounter,
    DreadCounter,
    DreamCounter,
    DutyCounter,
    EchoCounter,
    EggCounter,
    ElixirCounter,
    EmberCounter,
    EnlightenedCounter,
    EonCounter,
    EruptionCounter,
    EverythingCounter,
    ExposureCounter,
    EyeballCounter,
    FadeCounter,
    FateCounter,
    FeatherCounter,
    FeedingCounter,
    FellowshipCounter,
    FetchCounter,
    FilibusterCounter,
    FilmCounter,
    FinalityCounter,
    FireCounter,
    FlameCounter,
    FloodCounter,
    ForeshadowCounter,
    FungusCounter,
    FuryCounter,
    FuseCounter,
    GemCounter,
    GhostformCounter,
    GlyphCounter,
    GoldCounter,
    GrowthCounter,
    HarmonyCounter,
    HatchingCounter,
    HatchlingCounter,
    HealingCounter,
    HitCounter,
    HoneCounter,
    HoofprintCounter,
    HopeCounter,
    HourCounter,
    HourglassCounter,
    HungerCounter,
    IceCounter,
    ImpostorCounter,
    IncarnationCounter,
    IncubationCounter,
    InfectionCounter,
    InfluenceCounter,
    IngenuityCounter,
    IngredientCounter,
    IntelCounter,
    InterventionCounter,
    InvitationCounter,
    IsolationCounter,
    JavelinCounter,
    JudgmentCounter,
    KiCounter,
    KickCounter,
    KnowledgeCounter,
    LandmarkCounter,
    LevelCounter,
    LootCounter,
    LuckCounter,
    MagnetCounter,
    ManifestationCounter,
    MannequinCounter,
    MatrixCounter,
    MemoryCounter,
    MidwayCounter,
    MineCounter,
    MiningCounter,
    MireCounter,
    MusicCounter,
    MusterCounter,
    NecrodermisCounter,
    NestCounter,
    NetCounter,
    NightCounter,
    OilCounter,
    OmenCounter,
    OreCounter,
    PageCounter,
    PainCounter,
    PalliationCounter,
    ParalyzationCounter,
    PetalCounter,
    PetrificationCounter,
    PhylacteryCounter,
    PhyresisCounter,
    PinCounter,
    PlagueCounter,
    PlanCounter,
    PlotCounter,
    PointCounter,
    PolypCounter,
    PossessionCounter,
    PressureCounter,
    PreyCounter,
    PupaCounter,
    QuestCounter,
    RallyCounter,
    RejectionCounter,
    ReprieveCounter,
    RevCounter,
    RevivalCounter,
    RibbonCounter,
    RitualCounter,
    RopeCounter,
    RustCounter,
    SamuraiCounter,
    ScreamCounter,
    ScrollCounter,
    ShellCounter,
    ShredCounter,
    SilverCounter,
    SkewerCounter,
    SleepCounter,
    SleightCounter,
    SlimeCounter,
    SlumberCounter,
    SootCounter,
    SoulCounter,
    SpiteCounter,
    SporeCounter,
    StashCounter,
    StorageCounter,
    StoryCounter,
    StrifeCounter,
    StudyCounter,
    SupplyCounter,
    SuspectCounter,
    TakeoverCounter,
    TaskCounter,
    TheftCounter,
    TideCounter,
    TimeCounter,
    TowerCounter,
    TrapCounter,
    TreasureCounter,
    UnityCounter,
    UnlockCounter,
    ValorCounter,
    VelocityCounter,
    VerseCounter,
    VitalityCounter,
    VoidCounter,
    VortexCounter,
    VowCounter,
    VoyageCounter,
    WageCounter,
    WinchCounter,
    WindCounter,
    WishCounter,
    WreckCounter,

    // Action Counter
    ShieldCounter,
    StunCounter,

    // Variable Counters
    TheChosenCounterType,
    EachableCounterType,
    Or(Vec<CounterType>),
}

// type PlayerId = i32;
type PermanentId = i32;
type EffectId = i32;
type MutateIndex = i32;
type VoteOption = String;
type DungeonRoomName = String;
type NameString = String;
type LetterString = String;
type SpellBookName = String;
type Offerer = String;

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_LoyaltyNumber", content = "args")]
pub enum LoyaltyNumber {
    Integer(i32),
    LoyaltyX,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Expiration", content = "args")]
pub enum Expiration {
    Or(Vec<Expiration>),

    UntilAPlayerIsNoLongerTheMonarch(Box<Players>),
    UntilCardIsNoLongerInGraveyard(CardInGraveyard),
    UntilTheBeginningOfTheNextEndStep,
    DuringPlayersNextTurn(Box<Player>),
    UntilAPlayerExilesACardWithThisAbility(Box<Players>),
    ForAsLongAsPermanentRemainsFaceDown(Box<Permanent>),
    ForAsLongAsPermanentRemainsTapped(Box<Permanent>),
    AsLongAsPlaneIsFaceUp(Plane),
    DuringPlayersNextUntapStep(Box<Player>),
    DuringTheCombatPhaseCreatedThisWay,
    DuringTheExtraTurnCreatedThisWay,
    ForAsLongAsPermanentRemainsAttachedToPermanent(Box<Permanent>, Box<Permanent>),
    UntilAPlayerBecomesTheMonarch(Box<Players>),
    UntilAPlayerCastsASpell(Box<Players>, Box<Spells>),
    UntilAPlayerPlaneswalks(Box<Players>),
    UntilAPlayerRollsValue(Box<Players>, Box<Comparison>),
    UntilCardIsCastFromExile(Box<CardInExile>),
    UntilCardIsNoLongerExiled(Box<CardInExile>),
    UntilCardsAreNoLongerExiled(Box<CardsInExile>),
    UntilEndOfCombat,
    UntilEndOfGame,
    UntilEndOfNextTurn(Box<Player>),
    UntilEndOfTheNextTurn,
    UntilEndOfTurn,
    UntilItDoesntHaveACounterOfType(CounterType),
    UntilItIsNoLongerEnchanted,
    UntilItIsNoLongerExiled,
    UntilItLeavesTheBattlefield,
    UntilNextUpkeep(Box<Player>),
    UntilPermanentChangesControl(Box<Player>, Box<Permanent>),
    UntilPermanentIsTurnedFaceDown(Box<Permanent>),
    UntilPermanentIsTurnedFaceUp(Box<Permanent>),
    UntilPermanentLeavesBattlefield(Box<Permanent>),
    UntilPermanentNoLongerHasACounterOfType(Box<Permanent>, CounterType),
    UntilPermanentNoLongerPassesFilter(Box<Permanent>, Box<Permanents>),
    UntilPlayerExilesAnotherCardWithPermanent(Box<Player>, Box<Permanent>),
    UntilPlayerPaysMana(Box<Player>, ManaCost),
    UntilPlayerRollsValueWhileRollingToVisitAttractions(Box<Player>, Box<Comparison>),
    UntilPlayersNextEndStep(Box<Player>),
    UntilPlayersNextTurn(Box<Player>),
    UntilPlayersNextUntapStep(Box<Player>),
    UntilTheBeginningOfPlayersNextUpkeep(Box<Player>),
    UntilTheEndOfCombatOnPlayersNextTurn(Box<Player>),
    UntilTheEndOfPlayersNextTurn(Box<Player>),
    UntilTopCardOfPlayersLibraryChanges(Box<Player>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_CounterTypes", content = "args")]
pub enum CounterTypes {
    Trigger_ThoseCounterTypes,
    CounterTypeList(Vec<CounterType>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
//#[serde(tag = "_ColorIndicatorColor", content = "args")]
pub enum ColorIndicatorColor {
    White,
    Blue,
    Black,
    Red,
    Green,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub enum SimpleColor {
    White,
    Blue,
    Black,
    Red,
    Green,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_SettableColor", content = "args")]
pub enum SettableColor {
    AllColors,
    Colorless,
    Devoid,
    SimpleColorList(Vec<SimpleColor>),
    TheChosenColor,
    TheChosenColors,
    TheManaColorChosenThisWay,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Color", content = "args")]
pub enum Color {
    TheChosenColor,
    TheChosenColors,
    TheColorChosenByItsController,
    Colorless,
    White,
    Blue,
    Black,
    Red,
    Green,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ManaSymbol", content = "args")]
pub enum ManaSymbol {
    ManaCostGeneric(i32),
    ManaCostW,
    ManaCostU,
    ManaCostB,
    ManaCostR,
    ManaCostG,
    ManaCostC,
    ManaCostS,
    ManaCostWP,
    ManaCostUP,
    ManaCostBP,
    ManaCostRP,
    ManaCostGP,
    // ManaCostRWP,
    // ManaCostRGP,
    // ManaCostGWP,
    // ManaCostGUP,
    // ManaCost2W,
    ManaCost2U,
    ManaCost2B,
    ManaCost2R,
    ManaCost2G,
    // ManaCostCW,
    // ManaCostCU,
    // ManaCostCB,
    // ManaCostCR,
    // ManaCostCG,
    ManaCostWU,
    ManaCostUB,
    ManaCostBR,
    ManaCostRG,
    ManaCostGW,
    ManaCostWB,
    ManaCostUR,
    ManaCostBG,
    ManaCostRW,
    ManaCostGU,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ManaSymbol", content = "args")]
pub enum ManaSymbolX {
    ManaCostGeneric(i32),
    ManaCostW,
    ManaCostU,
    ManaCostB,
    ManaCostR,
    ManaCostG,
    ManaCostC,
    ManaCostS,
    ManaCostWP,
    ManaCostUP,
    ManaCostBP,
    ManaCostRP,
    ManaCostGP,
    ManaCostRWP,
    ManaCostRGP,
    ManaCostGWP,
    ManaCostGUP,
    ManaCost2W,
    ManaCost2U,
    ManaCost2B,
    ManaCost2R,
    ManaCost2G,
    ManaCostCW,
    ManaCostCU,
    ManaCostCB,
    ManaCostCR,
    ManaCostCG,
    ManaCostWU,
    ManaCostUB,
    ManaCostBR,
    ManaCostRG,
    ManaCostGW,
    ManaCostWB,
    ManaCostUR,
    ManaCostBG,
    ManaCostRW,
    ManaCostGU,

    ManaCostX,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ActivateModifier", content = "args")]
pub enum ActivateModifier {
    And(Vec<ActivateModifier>),

    PowerUp,
    CantBeCopied,
    Exhaust,
    ReduceManaCostForEachAlternateCost(Box<Cost>),
    ActivateNoMoreThanNumberTimesEachTurn(Box<GameNumber>),
    ReduceCostIfItTargetsANumberOfPermanent(Box<Comparison>, Box<Permanents>, CostReduction),
    ActivateOnlyAsASorcery,
    ActivateOnlyAsAnInstant,
    ActivateOnlyDuringTheirTurn,
    ActivateOnlyIf(Condition),
    ActivateOnlyOnce,
    ActivateOnlyOnceEachTurn,
    Boast,
    CantActivateIf(Condition),
    Forecast,
    IncreaseManaCostForEach(ManaCost, Box<GameNumber>),
    OnlyOtherPlayersMayActivate(Box<Players>),
    OnlyPlayerMayActivate(Box<Player>),
    OtherPlayersMayActivate(Box<Players>),
    ReduceCostForEach(CostReduction, Box<GameNumber>),
    ReduceCostIf(Condition, CostReduction),
    ReduceCostX(CostReductionX, Box<GameNumber>),
    SpendOnlyColoredManaOnX(Color),
    SpendOnlyColoredMana(Color),
    XCantBeZero,
    XCantBeGreaterThan(Box<GameNumber>),
    XIs(Box<GameNumber>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_CastModifier", content = "args")]
pub enum CastModifier {
    SpendOnlyColoredManaOnXAndAtMostOneManaOfEachColor,
    ReduceCostX(CostReductionX, Box<GameNumber>),
    XCantBeZero,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Craftable", content = "args")]
pub enum Craftable {
    And(Vec<Craftable>),
    Or(Vec<Craftable>),
    IsNonCardtype(CardType),
    HasAbility(CheckHasable),
    IsCreatureType(CreatureType),
    IsLandType(LandType),
    IsColor(Color),
    IsCardtype(CardType),
    AnyCard,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_GameEffect", content = "args")]
pub enum GameEffect {
    CardsCantEnterTheBattlefieldFromExile(Box<Cards>),
    CreaturesCantBlock,
    DamageCantBePrevented,
    DefendingPlayersChooseCreaturesToDefendAttackersAtRandom,
    PermanentCantPhaseIn(Box<Permanent>),
    PermanentsCantPhaseIn(Box<Permanents>),
    PermanentsTappedByPlayerForManaProduceColorlessInstead(Box<Permanents>, Box<Players>),
    PlanarDieBlanksRollsAreChaos,
    SchemesCantBeSetInMotion,
    SpellsAndAbilitiesCantTargetPermanents(SpellsAndAbilities, Box<Permanents>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_PermanentRule", content = "args")]
pub enum PermanentRule {
    StationsPermanentsAsThoughPowerWereGreater(Box<GameNumber>),
    StationsPermanentsUsingToughnessRatherThanPower,
    CanBlockCreaturesWithShadowAsThoughItHadShadow,
    PlaneswalkerCantBeAttacked,
    SaddlesMountsUsingToughnessRatherThanPower,

    CantBecomeUntapped,
    AbilitiesCantBeActivated,
    CantGainAbility(CheckHasable),
    AbilitiesOfTypeCantBeActivated(Box<ActivatedAbilities>),
    AllCreaturesMustBlockIt(Box<Permanents>),
    AssignsCombatDamageAsThoughNotBlocked,
    AssignsNoCombatDamage,
    AssignsToughnessCombatDamage,
    AttackingCausesOthersToAttack(Box<Permanents>),
    CanAttackAPlayerAsThoughItDidntHaveDefender(Box<Players>),
    CanAttackAsThoughItDidntHaveDefender,
    CanAttackAsThoughItHadHaste,
    CanAttackPlayersAndPlaneswalkersAsThoughItHadHaste(Box<Players>),
    CanBeAttachedOnlyToAPermanent(Box<Permanents>),
    CanBeBlockedAsThoughItDidntHave(CheckHasable),
    CanBeTheTargetOfSpellsAndAbilitiesAsThoughTheyDidntHaveHexproof(SpellsAndAbilities),
    CanBeTheTargetOfSpellsOrAbilitiesAsThoughItDidntHaveShroud(SpellsAndAbilities),
    CanBlockAnAdditionalCreature,
    CanBlockAnAdditionalNumberCreatures(Box<GameNumber>),
    CanBlockAnyNumberOfCreatures,
    CanBlockAsThoughUntapped,
    CanBlockCreaturesWithFlyingAsThoughItHadReach(Box<Permanents>),
    CanBlockCreaturesWithLandwalkAbilitiesAsThoughTheyDidntHaveThem,
    CanBlockCreaturesWithShadowAsThoughTheyDidntHaveShadow,
    CanBlockOnly(Box<Permanents>),
    CanBoastTwice,
    CanOnlyAttackAlone,
    CanOnlyBeDestroyedByLethalDamageFromASingleSource,
    CantAttack,
    CantAttackAPermanent(Box<Permanents>),
    CantAttackAPermanentUnlessCost(Box<Permanents>, Box<Cost>),
    CantAttackAPlayer(Box<Players>),
    CantAttackAlone,
    CantAttackAnyPlayerOrPlaneswalkerControlledBy(Box<Players>),
    CantAttackIfDefendingPlayer(Condition),
    CantAttackPlayer(Box<Player>),
    CantAttackPlayerOrPlaneswalkerControlledBy(Box<Player>),
    CantAttackPlayerOrPlaneswalkerControlledByUnlessCost(Box<Player>, Box<Cost>),
    CantAttackPlayerUnlessCost(Box<Player>, Box<Cost>),
    CantAttackUnlessANumberOfOtherCreatureAttacks(Box<Comparison>, Box<Permanents>),
    CantAttackUnlessAnotherCreatureAttacks(Box<Permanents>),
    CantAttackUnlessCost(Box<Cost>),
    CantAttackUnlessDefendingPlayer(Condition),
    CantBeBlocked,
    CantBeBlockedByDefenders(Box<Permanents>),
    CantBeBlockedByMoreThanOne,
    CantBeBlockedExceptByDefenders(Box<Permanents>),
    CantBeBlockedExceptByMultipleDefenders(Box<Comparison>, Box<Permanents>),
    CantBeBlockedIfDefendingPlayer(Box<Players>),
    CantBeBlockedUnlessAllDefendersBlockIt,
    CantBeBlockedUnlessCost(Box<Cost>),
    CantBeBlockedUnlessDefendingPlayer(Box<Players>),
    CantBeEnchanted,
    CantBeEnchantedAndDoesntRemove(Box<Permanents>),
    CantBeEnchantedByAnEnchantment(Box<Permanents>),
    CantBeEquipped,
    CantBeGainedControlOf,
    CantBeRegenerated,
    CantBeSacrificed,
    CantBeTheTargetOfAbilities(Abilities),
    CantBeTheTargetOfSpells(Box<Spells>),
    CantBeTheTargetOfSpellsOrAbilities(SpellsAndAbilities),
    CantBeTurnedFaceUp,
    CantBecomeSuspected,
    CantBecomeTappedUnlessItIsBeingDeclaredAsAnAttacker,
    CantBlock,
    CantBlockAlone,
    CantBlockAttacker(Box<Permanent>),
    CantBlockAttackers(Box<Permanents>),
    CantBlockAttackersUnlessCost(Box<Permanents>, Box<Cost>),
    CantBlockUnlessAnotherDefender(Box<Permanents>),
    CantBlockUnlessAttackingPlayer(Condition),
    CantBlockUnlessCost(Box<Cost>),
    CantBlockUnlessOtherDefenders(Box<Comparison>, Box<Permanents>),
    CantCrew,
    CantHaveAnyCountersOfTypePutOnIt(CounterType),
    CantHaveAnyCountersPutOnIt,
    CantHaveMoreThanNumberCountersOfType(Box<GameNumber>, CounterType),
    CantPhaseOut,
    CantTransform,
    SaddlesMountsAsThoughPowerWereGreater(Box<GameNumber>),
    CrewsVehiclesAsThoughPowerWereGreater(Box<GameNumber>),
    CrewsVehiclesUsingToughnessRatherThanPower,
    DamageDealtToItCantBePreventedOrRedirected,
    DecreaseEquipAbilityCostWhenTargetingAPermanent(CostReduction, Box<Permanents>),
    DetermineLethalDamageUsingPowerRatherThanToughness,
    DoesntUntapDuringControllersUntap,
    IsAColorlessSourceOfDamage,
    IsGoadedByPlayer(Box<Player>),
    MayAssignCombatDamageAsThoughNotBlocked,
    MayAssignCombatDamageDividedAsYouChooseAmongPlayerOrCreaturesAndPlaneswalkers(
        Box<Player>,
        Box<Permanents>,
    ),
    MayAssignCombatDamageToAPermanent(Box<Permanents>),
    MayBeExertedAsItAttacks,
    MayBeExertedAsItAttacksWithTrigger(Box<Actions>),
    MayChooseNotToUntapDuringUntap,
    MustAttack,
    MustAttackAPlayer(Box<Players>),
    MustAttackIfAnotherCreatureAttacks(Box<Permanents>),
    MustAttackPlaneswalker(Box<Permanent>),
    MustAttackPlayer(Box<Player>),
    MustBeBlocked,
    MustBeBlockedByADefender(Box<Permanents>),
    MustBeBlockedByAtLeastNumberDefenders(Box<GameNumber>),
    MustBeBlockedByEachDefender(Box<Permanents>),
    MustBeBlockedByExactlyOneDefender(Box<Permanents>),
    MustBlock,
    MustBlockAttacker(Box<Permanent>),
    MustBlockEachAttacker,
    UntapsDuringOtherPlayersUntapSteps(Box<Players>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ProtectableColor", content = "args")]
pub enum ProtectableColor {
    AnyColor,
    Colored,
    Colors(Vec<Color>),
    ColorsOfPermanent(Box<Permanent>),
    ColorsOfPermanents(Box<Permanents>),
    ColorsWithMostVotesOrTiedForMostVotes,
    ItsOwnColors,
    Monocolored,
    Multicolored,
    NotAColorInCommanderColorIdentity,
    TheChosenColor,
    TheColorsOfSpell(Box<Spell>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Protectable", content = "args")]
pub enum Protectable {
    FromActivatedAndTriggeredAbilities,
    FromCardName(NameFilter),
    FromTypes(Box<Cards>),
    FromColor(ProtectableColor),
    FromEverything,
    FromManaValue(Box<Comparison>),
    FromPermanents(Box<Permanents>),
    FromPlayers(Box<Players>),
    FromSpells(Box<Spells>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_UnspentMana", content = "args")]
pub enum UnspentMana {
    AnyUnspentMana,
    UnspentGreenMana,
    UnspentRedMana,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_PlayerEffect", content = "args")]
pub enum PlayerEffect {
    MayCastExiledCardForAlternateCost(Box<CardInExile>, Box<Cost>),
    MayCastASpellFromAmongExileWithoutPayingOnceEachTurn(Box<Spells>, Box<CardsInExile>),
    MayCastASpellFromHandWithoutPayingOnceEachPlayersTurn(Box<Spells>),
    MayCastExiledCardAndMaySpendManaAsThoughAnyTypeToCastIntoExile(Box<CardInExile>),
    MayCastSpellsFromAmongExiledAndMaySpendManaAsThoughAnyTypeToCastAndAsThoughTheyHadFlash(
        Box<Spells>,
        Box<CardsInExile>,
    ),
    MayCastSpellsFromGraveyardUsingTheirSneakAbility(Box<Spells>, Box<CardsInGraveyard>),
    MayCastSpellsFromGraveyardWithEffect(Box<Spells>, Box<CardsInGraveyard>, Vec<SpellEffect>),
    MayCastSpellsFromTopOfLibraryAndSpellsOfTypeWithEffect(
        Box<Spells>,
        Box<Spells>,
        Vec<SpellEffect>,
    ),
    MayCastTopCardOfLibrary,
    MayPlayCardsMilledThisWay,
    SpellsCastGainAbility(Box<Spells>, Vec<SpellEffect>),

    MayPlayLandsFromAmongExiledWithEffect(Box<CardsInExile>, Vec<ReplacementActionWouldEnter>),
    MayPlayOneCardFromAmongExiledWithoutPaying(Box<CardsInExile>),

    FirstTwoCoinFlipsEachTurnAreHeadsAndYouWin,
    MayCastASpellFromAmongCardsMilledThisWay(Box<Spells>),
    MayCastASpellFromTheirGraveyardOnceEachTurnWithEffect(Box<Spells>, Vec<SpellEffect>),
    MayCastGraveyardCardUsingWarpAbility(Box<CardInGraveyard>),
    MayCastSpellsFromAmongExiledForAdditionalCost(Box<Spells>, Box<CardsInExile>, Box<Cost>),
    MayPlayExiledCardsWithEffect(Box<CardsInExile>, Vec<SpellEffect>),
    MayPlayTopCardOfLibraryForAlternateCost(Box<Cost>),

    MayPlayExiledCardsIf(Box<CardsInExile>, Box<Condition>),
    MayCastASpellFromGraveyardWithAdditionalCostOnceEachPlayersTurn(
        Box<Spells>,
        Box<Cost>,
        Box<Player>,
    ),
    MayPayAlternateCostForFirstUnearthCostEachTurn(ManaCost),
    MayActivateExhaustAbilitiesAsThoughTheyHaventBeenActivated,
    MayCastSpellsFromTopOfLibraryWithEffect(Box<Spells>, Vec<SpellEffect>),
    MayPlayGraveyardCards(Box<CardsInGraveyard>),
    MayCastGraveyardCardAsAnAdventure(Box<CardInGraveyard>),

    DecreaseCostToTurnPermanentsFaceUp(Box<Permanents>, CostReduction),
    MayPlayCardsFromTopOfLibrary,
    DecreaseUnlockCost(CostReduction),
    MayCastSpellsForAlternateCostAsThoughTheyHadFlash(Box<Spells>, Box<Cost>),
    MayCastSpellsFromGraveyardForAdditionalCostWithEffect(
        Box<Spells>,
        Box<Player>,
        Box<Cost>,
        Vec<SpellEffect>,
    ),
    CantPlayCardsFromHand,
    DecreaseFlashbackCosts(ManaCost),
    DecreaseCyclingCosts(ManaCost),
    MayCastSpellsFromGraveyardForAdditionalCost(Box<Spells>, Box<Player>, Box<Cost>),
    DecreasePlotFromHandCost(CostReduction),
    MayCastGraveyardCardUsingBestowAbility(CardInGraveyard),
    MayPlayALandOrCastASpellFromAmongCardsInGraveyardsOnceEachTurn(CardsInGraveyard, Box<Players>),
    MayPlayExiledCardsAndPayAlternateCostToCast(CardsInExile, Box<Cost>),
    MayPlayExiledCardsAndMaySpendManaAsThoughAnyTypeToCastWithTrigger(CardsInExile, Box<Actions>),
    AsLoseUnspentMana(UnspentMana, Vec<Action>),
    MayCastASpellFromAmongCardsInPlayersGraveyardOnceEachTurn(
        Box<Spells>,
        CardsInGraveyard,
        Box<Player>,
    ),
    MayPlayExiledCardsAndMaySpendManaAsThoughAnyTypeToCast(Box<CardsInExile>),
    MayCastExiledCardAndMaySpendColorlessManaAsThoughAnyColorToCast(Box<CardInExile>),
    MayPlotCardsFromTheTopOfTheirLibrary(CardsInLibrary),
    MayPlayLandsFromAmongCardsInPlayersGraveyard(CardsInGraveyard, Box<Player>),
    MayCastSpellsFromAmongCardsInPlayersGraveyard(Box<Spells>, CardsInGraveyard, Box<Player>),
    MayCastSpellsFromAmongCardsInPlayersGraveyardForAlternateCost(
        Box<Spells>,
        CardsInGraveyard,
        Box<Player>,
        Box<Cost>,
    ),
    GainsLifeRatherThanLoseLifeFromRadiation,
    CantAttackPlayerOrPlaneswalkerControlledBy(Box<Player>),
    CantAttackAPermanent(Box<Permanents>),
    CantBeCausedToSacrificePermanentsByAbilities(Box<Permanents>, Abilities),
    CantBeCausedToExilePermanentsByAbilities(Box<Permanents>, Abilities),
    MayCastASpellFromGraveyardWithEffect(Box<Spells>, Box<Player>, Vec<SpellEffect>),
    SpellsCastFromExileHaveAbility(Box<Spells>, Vec<SpellEffect>),
    MayCastASpellFromAmongExiledCardsAndMaySpendManaAsThoughAnyColorToCastOnceEachPlayersTurn(
        Box<Spells>,
        CardsInExile,
        Box<Player>,
    ),
    MayCastASpellFromAmongExiledCardsWithEffect(Box<Spells>, CardsInExile, Vec<SpellEffect>),
    DrawsCardsFromBottomOfTheirLibrary,
    MayPlayExiledCardAndMaySpendManaAsThoughAnyTypeToCast(Box<CardInExile>),
    MayCastOneSpellFromAmongExiledWithoutPaying(Box<Spells>, CardsInExile),
    ReduceManaCostOfActivatedAbilitiesNotLessThanOneX(
        Box<ActivatedAbilities>,
        CostReductionX,
        Box<GameNumber>,
    ),
    AsTheyCascadeTheyMayPutACardFromAmongExiledCardsOnBattlefiled(
        Box<Cards>,
        Vec<ReplacementActionWouldEnter>,
    ),
    AssignsCombatDamageOfAttackingCreatures(Box<Permanents>),
    AttackingDoesntCauseCreaturesToTapIf(Box<Permanents>, Condition),
    CanActivateAbilitiesOnlyDuringTheirTurn,
    CanBeTheTargetOfSpellsAndAbilitiesAsThoughTheyDidntHaveHexproof(SpellsAndAbilities),
    CanCastSpellsFromRevealedHandOfPlayer(Box<Player>),
    CanCastSpellsOnlyAnyTimeTheyCouldCastASorcery,
    CanCastSpellsOnlyDuringTheirTurn,
    CanForetellCardsDuringEachPlayersTurn(Box<Players>),
    CanOnlyCastSpellsFromThierHand,
    CanOnlyUntapCardsOfTypeOfTheirChoiceDuringTheirUntapStep(Vec<CardType>),
    CanPlayLandsFromRevealedHandOfPlayer(Box<Player>),
    CantActivateAbilities(Box<ActivatedAbilities>),
    CantActivateAbilitiesOfCardsInGraveyards,
    CantActivateNonManaAbilities,
    CantAttackPlayer(Box<Player>),
    CantAttackWithCreatures(Box<Permanents>),
    CantBeAttackedExceptBy(Box<Permanents>),
    CantBeCausedToDiscardCardsBySpellAndAbilities(SpellsAndAbilities),
    CantBeCausedToSacrificePermanentsBySpellAndAbilities(Box<Permanents>, SpellsAndAbilities),
    CantBeTheTargetOfSpellsOrAbilities(SpellsAndAbilities),
    CantBecomeTheMonarch,
    CantBlockWithCreatures(Box<Permanents>),
    CantBlockWithMoreThanOneCreature,
    CantCastMoreThanNumberSpellsEachTurn(Box<GameNumber>, Box<Spells>),
    CantCastSpells(Box<Spells>),
    CantCastSpellsFromExile(Box<Spells>),
    CantCastSpellsFromGraveyards(Box<Spells>),
    CantCastSpellsFromLibraries(Box<Spells>),
    CantCastSpellsFromTheirHand(Box<Spells>),
    CantCycleCards,
    CantDrawCards,
    CantDrawMoreThanOneCardEachTurn,
    CantGainLife,
    CantLoseLife,
    CantGetAnyCounters,
    CantGetPoisonCounters,
    CantLoseTheGame,
    CantPayLifeToActivateAbilities(Box<ActivatedAbilities>),
    CantPayLifeToCastSpells(Box<Spells>),
    CantPlayCardInHand(CardInHand),
    CantPlayLands,
    CantPlayLandsFromGraveyards,
    CantPlayLandsFromTheirHand,
    CantPlayLandsOfType(Box<Permanents>),
    CantSacrificePermanents(Box<Permanents>),
    CantSacrificePermanentsToActivateAbilities(Box<Permanents>, Box<ActivatedAbilities>),
    CantSacrificePermanentsToCastSpells(Box<Permanents>, Box<Spells>),
    CantSearchLibraries,
    CantUntapMoreThanNumberPermanentsDuringTheirUntapStep(Box<GameNumber>, Box<Permanents>),
    CantVentureIntoTheDungeonMoreThanOnceEachTurn,
    CantWinTheGame,
    ChoosesHowCreaturesBlock(Box<Permanents>),
    ChoosesHowPlayersVote,
    ChoosesWhichCreaturesAttack,
    ChoosesWhichCreaturesBlockAndHowTheyBlock,
    ControlsPlayersWhileTheyAreSearchingLibraries(Box<Players>),
    DamageDoesntCauseLifeLoss,
    DamageThatWouldReduceLifeTotalToLessThanNumberReducesItToThatNumberInstead(Box<GameNumber>),
    DecreaseAbilityCostOfCardsInPlayersGraveyard(Box<Cards>, Box<Player>, CostReduction),
    DecreaseBlitzCostsForEach(ManaCost, Box<GameNumber>),
    DecreaseBoastAbilityCostForEach(CostReduction, Box<GameNumber>),
    DecreaseCostOfForetellingCardsFromHand(ManaCost),
    DecreaseDashCost(CostReduction),
    DecreaseEquipAbilityCost(CostReduction),
    DecreaseEquipAbilityCostWhenTargetingPermanent(CostReduction, Box<Permanent>),
    DecreaseNinjutsuAbilityCost(CostReduction),
    DecreaseSpellCost(Box<Spells>, CostReduction),
    DecreaseSpellCostForEach(Box<Spells>, CostReduction, Box<GameNumber>),
    DecreaseSpellCostForEachTarget(Box<Spells>, CostReduction),
    DecreaseSpellCostOnlyColored(Box<Spells>, CostReduction),
    DecreaseSpellCostX(Box<Spells>, CostReductionX, Box<GameNumber>),
    DoesntLoseColoredManaAsStepsAndPhasesEnd(UnspentMana),
    DoesntLoseManaAsStepsAndPhasesEnd,
    DoesntLoseTheGameForHaving0OrLessLife,
    DrawsACardDuringEachPlayersUntapStep(Box<Players>),
    GetsAnAdditionalVote,
    GetsAnOptionalAdditionalVote,
    HasNoMaximumHandSize,
    Hexproof,
    HexproofFrom(Protectable),
    IncreaseAbilityCost(Box<ActivatedAbilities>, Box<Cost>),
    IncreaseDevotionToColorAndColorCombinationsByNumber(Box<GameNumber>),
    IncreaseFlashBackCosts(ManaCost),
    IncreaseMaximumHandSize(Box<GameNumber>),
    IncreaseSpellCost(Box<Spells>, Box<Cost>),
    IncreaseSpellCostForEach(Box<Spells>, Box<Cost>, Box<GameNumber>),
    IncreaseSpellCostForEachTarget(Box<Spells>, Box<Cost>),
    LifeTotalCantChange,
    MayActionOnce(Box<Action>),
    MayActivateAbilitiesOfCreaturesAsThoughTheyHadHaste(Box<Permanents>),
    MayActivateEquipAbilitiesAnyTimeTheyCouldCastAnInstant,
    MayActivateLoyaltyAbilitiesOfPlanewalkerTwice(Box<Permanent>),
    MayActivateLoyaltyAbilitiesTwiceEachTurn(Box<Permanents>),
    MayActivateLoyaltyAbilityOfPlaneswalkerAnyTimeTheyCouldCastAnInstant(Box<Permanent>),
    MayActivateLoyaltyAbilityOfPlaneswalkerDuringEachPlayersTurnAndAnyTimeTheyCouldCastAnInstant(
        Box<Permanent>,
        Box<Players>,
    ),
    MayActivateLoyaltyAbilityOfPlaneswalkersDuringEachPlayersTurnAndAnyTimeTheyCouldCastAnInstant(
        Box<Permanents>,
        Box<Players>,
    ),
    MayActivateLoyaltyAbilityOfPlanewalkerAnAdditionalNumberTimes(Box<Permanent>, Box<GameNumber>),
    MayActivateLoyaltyAbilityOfPlanewalkerAnAdditionalTime(Box<Permanent>),
    MayActivateLoyaltyAbilityOfPlanewalkersAnAdditionalTime(Box<Permanents>),
    MayAttackOnlyPlayerOrPlaneswalkersControlledBy(Box<Player>),
    MayCastASpellForAlternateCostOnceDuringEachPlayersTurn(Box<Spells>, Box<Cost>, Box<Players>),
    MayCastASpellFromGraveyard(Box<Spells>, Box<Player>),
    MayCastASpellFromGraveyardIntoExileAndMaySpendManaAsThoughAnyColorToCastOnce(
        Box<Spells>,
        Box<Player>,
    ),
    MayCastASpellFromGraveyardIntoExileWithAdditionalCostOnceEachPlayersTurn(
        Box<Spells>,
        Vec<Action>,
        Box<Player>,
    ),
    MayCastASpellFromGraveyardOnceEachPlayersTurn(Box<Spells>, Box<Player>),
    MayCastASpellFromHandOrTopOfLibraryWithoutPayingOnceEachPlayersTurn(Box<Spells>),
    MayCastASpellFromTopOfLibraryOnceEachTurn(Box<Spells>),
    MayCastASpellOfEachNonlandCardtypeFromAmongExiledCardsWithoutPaying(Box<CardsInExile>),
    MayCastCardFromGraveyardByPayingAddedCost(CardInGraveyard, Box<Cost>),
    MayCastCardInHandWithoutPaying(CardInHand),
    MayCastExiledCard(Box<CardInExile>),
    MayCastExiledCardAndMaySpendManaAsThoughAnyColorToCast(Box<CardInExile>),
    MayCastExiledCardAndMaySpendManaAsThoughAnyColorToCastIf(CardInExile, Condition),
    MayCastExiledCardAndMaySpendManaAsThoughAnyTypeToCast(Box<CardInExile>),
    MayCastExiledCardForAdditionalCost(CardInExile, Box<Cost>),
    MayCastExiledCardIntoExile(Box<CardInExile>),
    MayCastExiledCardWithEffect(CardInExile, Vec<SpellEffect>),
    MayCastExiledCardWithoutPaying(Box<CardInExile>),
    MayCastExiledSpell(CardInExile, Box<Spells>),
    MayCastExiledSpellWithoutPaying(CardInExile, Box<Spells>),
    MayCastGraveyardCard(CardInGraveyard),
    MayCastGraveyardCardForAlternateCastingCost(CardInGraveyard, Box<Cost>),
    MayCastGraveyardCardForAlternateCastingCostWithEnterActions(
        CardInGraveyard,
        Box<Cost>,
        Vec<ReplacementActionWouldEnter>,
    ),
    MayCastGraveyardCardIntoExile(CardInGraveyard),
    MayCastGraveyardCardIntoExileForAlternateCastingCost(CardInGraveyard, Box<Cost>),
    MayCastGraveyardCardIntoExileIfSpell(CardInGraveyard, Box<Spells>),
    MayCastGraveyardCardIntoExileIfSpellForIncreasedCost(CardInGraveyard, Box<Spells>, ManaCost),
    MayCastGraveyardCardUsingBlitzAbility(CardInGraveyard),
    MayCastGraveyardCardUsingMutateAbility(CardInGraveyard),
    MayCastGraveyardCardWithEnterActions(CardInGraveyard, Vec<ReplacementActionWouldEnter>),
    MayCastGraveyardCardWithTrigger(CardInGraveyard, Box<Actions>),
    MayCastGraveyardCardWithoutPayingIntoExile(CardInGraveyard),
    MayCastOneSpellFromAmongExiledEachTurn(Box<Spells>, CardsInExile),
    MayCastSpellsAsThoughTheyHadFlash(Box<Spells>),
    MayCastSpellsFromAmongExiled(Box<Spells>, CardsInExile),
    MayCastSpellsFromAmongExiledAndMaySpendManaAsThoughAnyColorToCast(Box<Spells>, CardsInExile),
    MayCastSpellsFromAmongExiledAndMaySpendManaAsThoughAnyTypeToCast(Box<Spells>, CardsInExile),
    MayCastSpellsFromAmongExiledAndMaySpendManaFromSnowSourcesAsThoughItWereAnyColorToCast(
        Box<CardsInExile>,
    ),
    MayCastSpellsFromAmongExiledForAlternateCastingCost(Box<Spells>, CardsInExile, Box<Cost>),
    MayCastSpellsFromAmongExiledWithoutPaying(Box<Spells>, CardsInExile),
    MayCastSpellsFromGraveyard(Box<Spells>),
    MayCastSpellsFromGraveyardIntoExile(Box<Spells>),
    MayCastSpellsFromHandWithoutPaying,
    MayCastSpellsFromOtherPlayersGraveyards,
    MayCastSpellsFromTheTopOfTheirGraveyardIntoExile(Box<Spells>),
    MayCastSpellsFromTopOfLibrary(Box<Spells>),
    MayCastSpellsFromTopOfLibraryAsThoughTheyHadFlash(Box<Spells>),
    MayCastSpellsFromTopOfLibraryForAlternateCost(Box<Spells>, Box<Cost>),
    MayCastSpellsFromTopOfLibraryWithAdditionalCost(Box<Spells>, Box<Cost>),
    MayCastSpellsFromTopOfLibrary_SpellsWithTrigger(Box<Spells>, Box<Spells>, Box<Actions>),
    MayCastSpellsFromTopOfPlayersLibrary(Box<Spells>, Box<Players>),
    MayCastSpellsWithoutPaying(Box<Spells>),
    MayCastSpellsWithoutPayingAndAsThoughTheyHadFlash(Box<Spells>),
    MayDiscardCardAnyTimeTheyCouldCastAnInstant(CardInHand),
    MayLookAtAnAdditionalNumberCardsAsTheySurveil(Box<GameNumber>),
    MayLookAtAndPlayCardsFromTheTopOfOtherPlayersLibraryAndMaySpendManaAsThoughAnyColorToCast(
        Box<Player>,
    ),
    MayLookAtFaceDownExiledCard(Box<CardInExile>),
    MayLookAtFaceDownExiledCards(Box<CardsInExile>),
    MayLookAtFaceDownPermanents(Box<Permanents>),
    MayLookAtTopCardOfLibraryAnyTime,
    MayPayAdditionalCostToCastSpellsForEffect(
        Box<Spells>,
        Box<Cost>,
        Vec<ReplacementActionWouldEnter>,
    ),
    MayPayAlternateCostForASpellOnceEachTurn(ManaCost, Box<Spells>),
    MayPayAlternateCostForFirstCyclingCostEachTurn(ManaCost),
    MayPayAlternateCostForFirstEquipCostEachTurn(ManaCost),
    MayPayAlternateCostForSpells(Box<Cost>, Box<Spells>),
    MayPayAlternateCyclingCosts(ManaCost),
    MayPayAlternateEchoCosts(Box<Cost>, Box<Permanents>),
    MayPayLifeRatherThanMana(Box<GameNumber>, ManaSymbol),
    MayPayLifeToProduceManaAsManaAbility(Box<GameNumber>, ManaProduceSymbol),
    MayPayManaAnyTimeTheyCouldCastAnInstant(ManaCost, Vec<Action>),
    MayPayToIncrementOrDecrementADiceRoll(Box<Cost>),
    MayPayToReduceColoredManaCostOfSpells(Box<Cost>, ManaCost, Box<Spells>),
    MayPlayALandOrCastASpellFromAmongCardsInTheirGraveyardThatWerePutThereFromTheirLibraryOnceEachPlayers(
        Box<Permanents>,
        Box<Spells>,
        Box<Cards>,
        Box<Player>,
    ),
    MayPlayALandOrCastASpellFromAmongExiledCardsAndMaySpendManaAsThoughAnyColorToCastOnceEachPlayersTurnWithTrigger(
        Box<Permanents>,
        Box<Spells>,
        CardsInExile,
        Box<Player>,
        Box<Actions>,
    ),
    MayPlayALandOrCastASpellFromTheirGraveyardOfEachPermanentTypeDuringPlayersTurn(Box<Player>),
    MayPlayALandOrCastASpellFromTheirGraveyardOnceEachPlayersTurnWithEffect(
        Box<Permanents>,
        Box<Spells>,
        Box<Player>,
        Vec<SpellEffect>,
    ),
    MayPlayAdditionalLands(Box<GameNumber>),
    MayPlayAnAdditionalLand,
    MayPlayAnyNumberOfLandsDuringThierTurn,
    MayPlayColoredCardsFromHandAsRandomBasicLandOfThatCouldProduceOneOfThoseColors,
    MayPlayExiledCard(Box<CardInExile>),
    MayPlayExiledCardAndMaySpendManaAsThoughAnyColorToCast(Box<CardInExile>),
    MayPlayExiledCardAndMaySpendManaAsThoughAnyColorToCastWithTrigger(CardInExile, Box<Actions>),
    MayPlayExiledCardIf(CardInExile, Condition),
    MayPlayExiledCardWithEffect(CardInExile, Vec<SpellEffect>),
    MayPlayExiledCardWithTrigger(CardInExile, Box<Actions>),
    MayPlayExiledCardWithoutPaying(Box<CardInExile>),
    MayPlayExiledCards(Box<CardsInExile>),
    MayPlayExiledCardsAndMaySpendManaAsThoughAnyColorToCast(Box<CardsInExile>),
    MayPlayExiledCardsWithoutPaying(Box<CardsInExile>),
    MayPlayGraveyardCard(CardInGraveyard),
    MayPlayGraveyardCardWithEffect(CardInGraveyard, Vec<SpellEffect>),
    MayPlayLandsFromAmongExiled(Box<CardsInExile>),
    MayPlayLandsFromGraveyard(Box<Cards>),
    MayPlayLandsFromOutsideTheGame(Box<Cards>),
    MayPlayLandsFromTopOfLibrary(Box<Cards>),
    MayPlayLandsFromTopOfPlayersLibrary(Box<Players>),
    MayPlayOneCardFromAmongExiled(Box<CardsInExile>),
    MayPlayOneCardFromAmongExiledAndPayAlternateCostToCast(CardsInExile, Box<Cost>),
    MayPlayOneCardFromAmongExiledAndMaySpendManaAsThoughAnyColorToCast(Box<CardsInExile>),
    MayPlayTopCardOfLibraryWithoutPaying,
    MayPlayTwoCardsFromAmongExiled(Box<CardsInExile>),
    MayPlaysLandsFromOtherPlayersGraveyards,
    MayRemoveACounterOfTypeFromAPermanentToPlayPermanentsCrewCost(
        CounterType,
        Box<Permanents>,
        Box<Permanent>,
    ),
    MayRevealFirstCardDrawnDuringEachPlayersTurn(Box<Players>),
    MaySpendColoredManaAsThoughItWereAnotherColor(Color, Color),
    MaySpendColoredManaAsThoughItWereAnyColor(Color),
    MaySpendColoredManaAsThoughItWereAnyColorAndMaySpendOtherManaOnlyAsThoughItWereColorless(Color),
    MaySpendColoredManaAsThoughItWereAnyColorToPayForAbilities(Color, Abilities),
    MaySpendManaAsThoughItWasAnyColor,
    MaySpendManaAsThoughItWasAnyColorToCastSpells(Box<Spells>),
    MaySpendManaAsThoughItWasAnyColorToPayForAbilities(Abilities),
    MaySpendManaAsThoughItWasAnyTypeToActivateAbilities(Box<ActivatedAbilities>),
    MaySuspendCardsFromHand(Box<Cards>),
    MayTapPermanentsTheyDontControlForManaWithModifiers(Box<Permanents>, ManaUseModifier),
    MustAttackPlaneswalkerWithEachCreature(Box<Permanent>, Box<Permanents>),
    MustAttackPlayerOrPlaneswalkersControlledBy(Box<Player>),
    MustAttackWithANumberOfCreatures(Box<Comparison>, Box<Permanents>),
    MustAttackWithEachCreature(Box<Permanents>),
    NoteManaValueOfExiledCard,
    OnceDuringEachPlayersTurnMayAction(Box<Player>, Box<Action>),
    OnceEachTurnMayAction(Box<Action>),
    OnceEachTurnMayPayToIncrementOrDecrementADiceRoll(Box<Cost>),
    OnceEachTurnMayPayToRerollAnyNumberOfDiceRolled(Box<Cost>),
    PlaysWithCardInHandRevealed(CardInHand),
    PlaysWithHandRevealed,
    PlaysWithTopOfLibraryRevealed,
    Protection(Protectable),
    ReduceActivatedCost(Box<ActivatedAbilities>, CostReduction),
    ReduceManaCostOfActivatedAbilities(Box<ActivatedAbilities>, CostReduction),
    ReduceManaCostOfActivatedAbilitiesNotLessThanOne(Box<ActivatedAbilities>, CostReduction),
    ReduceMaximumHandSize(Box<GameNumber>),
    ReplaceForetellCostOfFirstCardForetoldEachTurn(ManaCost),
    RevealFirstCardDrawnDuringEachPlayersTurn(Box<Players>),
    RevealFirstCardDrawnDuringPlayersTurn(Box<Player>),
    RevealsEachCardDrawn,
    SetMaximumHandSize(Box<GameNumber>),
    SetMinimumSpellCost(Box<Spells>, Box<GameNumber>),
    Shroud,
    SkipsCombatPhase,
    SkipsDrawStep,
    SkipsMainPhase,
    SkipsUntapStep,
    SkipsUpkeepStep,
    SpellsAndAbilitiesTheyCantCantCauseThemToSearchTheirLibrary,
    SpellsCastFromHandHaveAbility(Box<Spells>, Vec<SpellEffect>),
    SpellsCastHaveAbility(Box<Spells>, Vec<SpellEffect>),
    SpellsControlledHaveAbility(Box<Spells>, Vec<SpellEffect>),
    TheNthSpellCastEachTurnHasAbility(Box<GameNumber>, Box<Spells>, Vec<SpellEffect>),
    UnspentManaBecomesColor(Color),
    UnspentManaBecomesColorless,
    UntapsPermanentsDuringEachPlayersUntapSteps(Box<Permanents>, Box<Players>),
    WhileChoosingTargetsMustChooseAtLeastOnePermanentIfAble(Box<Permanents>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_SagaChapter", content = "args")]
pub enum SagaChapter {
    SagaChapter(Vec<i32>, Box<Actions>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_EnterOrFaceUpAction", content = "args")]
pub enum EnterOrFaceUpAction {
    MayActions(Vec<EnterOrFaceUpAction>),
    EntersWithNumberCounters(Box<GameNumber>, CounterType),
    EnterAsACopyOfAPermanentUntil(Box<Permanents>, CopyEffects, Expiration),
    EntersWithPTOfChoice(Vec<PT>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_TransformAction", content = "args")]
pub enum TransformAction {
    ChooseAPlayer(Box<Players>),
    GetAnEmblem(Vec<Rule>),
    TransformsWithNumberCounters(Box<GameNumber>, CounterType),
    AttachPermanentToAPlayer(Box<Permanent>, Box<Players>),
    BecomeACopyOfAnExiledCard(CardsInExile, CopyEffects),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_CastEffect", content = "args")]
pub enum CastEffect {
    AdditionalCastingCost(Box<Cost>),
    AdditionalCastingCostForAllTargets(Box<Cost>),
    AdditionalCastingCostForEachTarget(Box<Cost>),
    AdditionalCastingCostForEachTargetBeyondTheFirst(Box<Cost>),
    AdditionalCastingCostIf(Box<Cost>, Condition),
    AdditionalCastingCostIfItTargetsAPermanent(Box<Cost>, Box<Permanents>),
    AdditionalCastingCostX(Box<Cost>),
    AlternateCastingCost(Box<Cost>),
    AlternateCastingCostIf(Box<Cost>, Condition),
    CantBeCastFromAnywhereOtherThanGraveyard,
    CantBeCastIf(Condition),
    CantBeCastUnless(Condition),
    CantChooseATarget(Box<Permanents>),
    CantSpendManaToCast,
    MayCastAsThoughItHadFlashForAdditionalCost(Box<Cost>),
    MayCastAsThoughItHadFlashIf(Condition),
    MayCastAsThoughItHadFlashIfItTargetsAPermanent(Box<Permanents>),
    MayCastAsThoughItHadFlashIfXIs(Box<Comparison>),
    MayCastAsThoughItHadFlashWithSpecialAction(Vec<Action>),
    MayCastWithoutPayingIf(Condition),
    MaySpendManaAsThoughAnyColorToCast,
    MaySpendManaAsThoughAnyTypeToCast,
    OptionalAdditionalCastingCost(Box<Cost>),
    OptionalAdditionalCastingCostForReflexiveTrigger(Box<Cost>, Box<Actions>),
    PayLifeForEachPreviousCastRatherThanManaForEachPreviousCast(Box<GameNumber>),
    ReduceCastingCost(CostReduction),
    ReduceCastingCostForAlternateCost(CostReduction, Box<Cost>),
    ReduceCastingCostForEach(CostReduction, Box<GameNumber>),
    ReduceCastingCostForEachAlternateCost(CostReduction, Box<Cost>),
    ReduceCastingCostForEachWithMaxReduction(CostReduction, Box<GameNumber>, CostReduction),
    ReduceCastingCostIf(CostReduction, Condition),
    ReduceCastingCostIfItTargetsACard(CostReduction, Box<Cards>),
    ReduceCastingCostIfItTargetsAPermanent(CostReduction, Box<Permanents>),
    ReduceCastingCostIfItTargetsASpell(CostReduction, Box<Spells>),
    ReduceCastingCostIfItTargetsASpellOrAbility(CostReduction, SpellsAndAbilities),
    ReduceCastingCostIfItsBargained(CostReduction),
    ReduceCastingCostX(CostReductionX, Box<GameNumber>),
    SpendOnlyColorManaOnX(Color),
    SpendOnlyColorsOfManaOnX(ColorList),
    SpendOnlyManaFromPermanentsToCast(Box<Permanents>),
    XCantBeZero,
    XIs(Box<Comparison>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ClassAbility", content = "args")]
pub enum ClassAbility {
    ClassAbility(ManaCost, Vec<Rule>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Companion", content = "args")]
pub enum Companion {
    AllCardsPassFilter(Box<Cards>),
    EachCardPassesFilter(Box<Cards>, Box<Cards>),
    EachCardPassesGroupFilter(Box<Cards>, GroupFilter),
    IncreaseStartingDeckSize(Box<GameNumber>),
    NoCardPassesFilter(Box<Cards>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_DeckConstruction", content = "args")]
pub enum DeckConstruction {
    CanBeYourCommander,

    Partner,
    PartnerCharacterSelect,
    PartnerFatherAndSon,
    PartnerFriendsForever,
    PartnerSurvivors,
    PartnerWith(NameString),

    DoctorsCompanion,

    ChooseABackground,

    CanHaveAnyNumberOfThisCard,
    CanHaveUptoNumberOfThisCard(Box<GameNumber>),

    ThisCardIsBanned,
    RemoveFromDeckIfNotPlayingForAnte,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ConspiracyDeck", content = "args")]
pub enum ConspiracyDeck {
    ReduceStartingDeckSize(Box<GameNumber>),
    NoCardPassesFilter(Box<Cards>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ActivatedAbilityEffect", content = "args")]
pub enum ActivatedAbilityEffect {
    IncreaseManaCost(ManaCost),
    AdditionalCostForEachColorManaSymbolInCosts(Box<Cost>, Color),
    AdditionalCost(Box<Cost>),
    ReduceManaCostNotLessThanOne(CostReduction),
    CantBeActivated,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_DeckBuildingAction", content = "args")]
pub enum DeckBuildingAction {
    ChooseAColor(ChoosableColor),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_DraftAction", content = "args")]
pub enum DraftAction {
    DraftFaceUp,
    RevealThisDraftedCard,
    GuessNameOfNextCardAPlayerDraftsFromThisPackAndTheyRevealThatCard,
    MayAddBoosterBackToDraft,
    MayLookAtNextCraftDraftedFromThisPack,
    NoteNumberOfCardsDraftedThisRound,
    NotePlayerWhoPassedPackToYou,
    PlayerToRightChoosesAColor_YouChooseAColor_PlayerToLeftChoosesAColor,
    RevealAndNoteNameOfNextDraftedCard,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_FaceUpDraftEffect", content = "args")]
pub enum FaceUpDraftEffect {
    DraftCardsAtRandomUntilNumberCardsHaveBeenDrafted_TurnThisDraftCardFaceDown(Box<GameNumber>),
    AsDraftACardMayDraftAnAdditionalCardFromPack_PutThisDraftCardIntoThatBooster,
    AsDraftACardMayDraftAnAdditionalCardFromPack_TurnThisDraftCardCardFaceDown_PassNextBoosterWithoutDrafting,
    MayRemoveCardsFromDraftFaceDown,
    MayRemoveCardsFromDraftFaceUp,
    AsDraftACardOfType_MayRevealIt_NoteItsCreatureTypes_TurnThisDraftCardFaceDown(Box<Cards>),
    AsDraftACardOfType_MayRevealIt_NoteItsName_TurnThisDraftCardFaceDown(Box<Cards>),
    AsDraftACard_MayRevealIt_NoteItsName_TurnThisDraftCardFaceDown,
    MayTurnThisDraftCardFaceDown_LookAtAnUnopenedBoosterPackOrABoosterPackNotBeingLookedAt,
    MayTurnThisDraftCardFaceDown_LookAtNextCardDraftedByPlayerOfChoice,
    LastCardInEachBoosterGoesToThisPlayer,
    AfterDraftMayOfferATradeWithOtherPlayers,
    MayTurnThisDraftCardFaceDown_DraftEachCardInCurrentBoosterPackInsteadOfDraftingCardsThisRound,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_PermanentAndSpellEffect", content = "args")]
pub enum PermanentAndSpellEffect {
    ReplaceAllColorWordsWithNewColorWord(Color),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_PermanentsAndSpells", content = "args")]
pub enum PermanentsAndSpells {
    AnyPermanentOrSpell,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_SpreeAction", content = "args")]
pub enum SpreeAction {
    SpreeAction(Box<Cost>, Box<Actions>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_TieredAction", content = "args")]
pub enum TieredAction {
    TieredAction(Box<Cost>, Box<Actions>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_PawMode", content = "args")]
pub enum PawMode {
    PawMode(i32, Box<Actions>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_CDA_Types", content = "args")]
pub enum CDA_Types {
    AddCreatureTypeVariable(CreatureTypeVariable),
    Changeling,
    HasAllCreatureTypes,
    HasAllNonbasicLandTypes,
    AddCreatureTypes(Vec<CreatureType>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct WasAwakened(Box<Actions>);

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct WasntAwakened(Box<Actions>);

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct WasKicked(Box<Actions>);

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct WasntKicked(Box<Actions>);

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct CleavePaid(Box<Actions>);

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct CleaveNotPaid(Box<Actions>);

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct OverloadPaid(Box<Actions>);

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct OverloadNotPaid(Box<Actions>);

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct MadnessXWasPaid(Box<Actions>);

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct MadnessXWasntPaid(Box<Actions>);

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct Gift(Box<Actions>);

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct GiftWasPromised(Box<Actions>);

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct GiftWasntPromised(Box<Actions>);

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct TriggerAndActions(Trigger, Box<Actions>);

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Rule", content = "args")]
pub enum Rule {
    StationChargedAnimate(Range, Vec<Rule>, PT),
    StationCharged(Range, Vec<Rule>),
    Station,

    BasicMayhem,
    Mayhem(Box<Cost>),
    WebSlinging(Box<Cost>),
    WarpX(Box<Cost>),
    Warp(Box<Cost>),
    CombatDamageCantBePrevented,
    Firebending(Box<GameNumber>),
    JobSelect,

    StackSpellsEffect(Box<Spells>, Vec<SpellEffect>),
    StackEffect(Box<SpellsAndAbilities>, Vec<StackEffect>),

    AllDamageIsDealtAsThoughItsSourceHadWither,
    NoMoreThanNumberCreaturesCanBlock(Box<GameNumber>),
    PermanentsCantPhaseIn(Box<Permanents>),
    PermanentsDyingDontCauseAbilitiesToTrigger(Box<Permanents>),
    CardsInEachPlayersLibrariesCantEnterTheBattlefield(Box<Cards>, Box<Players>),

    // Prevent Triggers
    PermanentsEnteringTheBattlefieldDontCauseAbilitiesToTrigger(Box<Permanents>),
    WardAbilitiesOfPermanentsDontTrigger(Box<Permanents>),

    // Duplicate Triggers
    APlayerDrawingACardCausesAbilitiesToTriggerAnAdditionalTime(Box<Players>, Abilities),
    APermanentAttackingCausesAbilitiesToTriggerAnAdditionalTime(Box<Permanents>, Abilities),
    APermanentBecomingTheTargetOfASpellOrAbilityCausesAbilitiesToTriggerAnAdditionalTime(
        Box<Permanents>,
        Box<SpellsAndAbilities>,
        Abilities,
    ),
    APermanentBeingDealtDamageCausesAbilitiesToTriggerAnAdditionalTime(Box<Permanents>, Abilities),
    APermanentDealingCombatDamageToAPlayerCausesAbilitiesToTriggerAnAdditionalTime(
        Box<Permanents>,
        Box<Players>,
        Abilities,
    ),
    APermanentDyingCausesAbilitiesToTriggerAnAdditionalTime(Box<Permanents>, Abilities),
    APermanentEnteringTheBattlefieldCausesAbilitiesToTriggerAnAdditionalTime(
        Box<Permanents>,
        Abilities,
    ),
    APermanentTurningFaceUpCausesAbilitiesToTriggerAnAdditionalTime(Box<Permanents>, Abilities),
    APermanentLeavingTheBattlefieldCausesAbilitiesToTriggerAnAdditionalTime(
        Box<Permanents>,
        Abilities,
    ),
    APlayerCastingOrCopyingASpellCausesAnAbilityToTriggerAnAdditionalTime(
        Box<Players>,
        Box<Spells>,
        Abilities,
    ),
    AbilitiesTriggerAnAdditionalTime(Abilities),

    // Villainous Choice
    APlayerFacingAVillainousChoiceFacesItAnAdditionalTime(Box<Players>),

    // Legends Rule
    TheLegendsRuleDoesntApply,
    TheLegendsRuleDoesntApplyToPermanents(Box<Permanents>),

    // Craft With
    CraftWithCraftables(Vec<Craftable>, ManaCost),
    CraftWithACraftable(Craftable, ManaCost),
    CraftWithANumberOfCraftables(Comparison, Craftable, ManaCost),
    CraftWithANumberOfGroupCraftables(Comparison, Craftable, GroupFilter, ManaCost),

    CardsCantEnterTheBattlefield(Box<Cards>),
    CardsInEachPlayersGraveyardsCantEnterTheBattlefield(Box<Cards>, Box<Players>),
    CombatDamageOfCreaturesCantBePrevented(Box<Permanents>),
    CountersCantBePutOnPermanents(Box<Permanents>),
    CountersOfTypeCantBeRemovedFromPermanents(CounterType, Box<Permanents>),

    DamageCantBePrevented,
    DamageFromPermanentCantBePrevented(Box<Permanent>),
    DamageIsntRemovedFromCreatureDuringCleanup(Box<Permanent>),
    DamageIsntRemovedFromCreaturesDuringCleanup(Box<Permanents>),

    IncreaseBuybackCosts(ManaCost),
    IncreaseMorphCosts(ManaCost),

    ItCantBecomeNight,

    NoMoreThanNumberCreaturesCanAttack(Box<GameNumber>),
    NoMoreThanNumberCreaturesCanAttackPermanent(Box<GameNumber>, Box<Permanent>),
    NoMoreThanNumberCreaturesCanAttackPlayer(Box<GameNumber>, Box<Player>),

    WhilePlayersAreSearchingTheirLibraryTheyExileEachCardTheyFindAndPlayerMayPlayThoseCardsWhileTheyRemainExiledAndMaySpendManaAsThoughItWereAnyColor(
        Box<Players>,
        Box<Player>,
    ),

    ReplaceWouldLearn(ReplacableEventWouldLearn, Vec<ReplacementActionWouldLearn>),

    // Rule: Keyword
    StartYourEngines,
    Harmonize(Box<Cost>),
    HarmonizeX(ManaCostX),
    Mobilize(Box<GameNumber>),
    AnnihilatorX(Box<GameNumber>),
    BestowX(Box<Cost>),
    CrewOnceEachTurn(i32),
    EmergeFromArtifact(Box<Cost>),
    Freerunning(Box<Cost>),
    FreerunningX(Box<Cost>),
    Impending(Box<GameNumber>, Box<Cost>),
    Offspring(Box<Cost>),
    Permanent_Gift(Vec<Action>),
    Saddle(Box<GameNumber>),
    Bargain,
    Plot(Box<Cost>),
    TopCardOfPlayersLibraryEffect(Box<Cards>, Box<Player>, Vec<LibraryCardEffect>),
    Mystery(Condition, Vec<Rule>),
    Disguise(Box<Cost>),
    Aftermath,
    BasicSuspend,
    KickerX(ManaCostX),
    DisguiseX(ManaCostX),
    SpaceSculptor,
    KickerForSpellAbility(Box<Cost>, Box<Rule>),
    SpliceOnto(Box<Spells>, Box<Cost>),
    ReinforceX(ManaCostX),
    MorphX(ManaCostX),
    SurgeX(ManaCostX),
    MiracleX(ManaCostX),
    FlashbackX(ManaCostX),
    // Awaken(Box<GameNumber>, Box<Cost>),
    Offering(Box<Cards>),
    Affinity(Box<Permanents>),
    Devour(Box<Permanents>, Box<GameNumber>),
    Backup(i32, Vec<Rule>),
    Suspend(Box<GameNumber>, Box<Cost>),
    SuspendX(ManaCostX, ActivateModifier),
    ProtectionAndDoesntRemovePermanents(Protectable, Box<Permanents>),

    #[serde(rename_all = "PascalCase")]
    Prototype {
        mana_cost: CardManaCost,
        #[serde(rename = "CardPT")]
        card_pt: CardPT,
    },

    Reinforce(Box<GameNumber>, Box<Cost>),
    Protection(Protectable),
    HexproofFrom(Protectable),
    TypeCycling(Box<Cards>, Box<Cost>),
    EnchantPlayer(Box<Players>),
    Fortify(Box<Cost>),
    Prowl(ManaCost),
    Absorb(i32),
    Equip(Box<Permanents>, Box<Cost>),
    EquipWithModifiers(Box<Permanents>, Box<Cost>, ActivateModifier),
    Annihilator(i32),
    Amplify(i32),
    Afterlife(i32),
    Afflict(i32),
    Surge(ManaCost),
    Ascend,
    Assist,
    AuraSwamp(Box<Cost>),
    Banding,
    BandsWithOthers(Box<Permanents>),
    BattleCry,
    Bestow(Box<Cost>),
    Blitz(Box<Cost>),
    Bloodthirst(Box<GameNumber>),
    BloodthirstX,
    Bushido(Box<GameNumber>),
    Buyback(Box<Cost>),
    Cascade,
    Casualty(Box<GameNumber>),
    CasualtyX,
    Champion(Box<Permanents>),
    // Cipher,
    CommanderNinjutsu(Box<Cost>),
    Compleated,
    Conspire,
    Convoke,
    Crew(i32),
    CumulativeUpkeep(Box<Cost>),
    Cycling(Box<Cost>),
    CyclingX(ManaCostX),
    Dash(Box<Cost>),
    Daybound,
    Deathtouch,
    Decayed,
    Defender,
    Delve,
    Demonstrate,
    Dethrone,
    Disturb(Box<Cost>),
    DoubleAgenda,
    DoubleStrike,
    DoubleTeam,
    Dredge(Box<GameNumber>),
    Echo(Box<Cost>),
    Embalm(Box<Cost>),
    Emerge(Box<Cost>),
    EnchantPermanent(Box<Permanents>),
    EnchantGraveyardCard(CardsInGraveyard, Box<Players>),
    Encore(Box<Cost>),
    Enlist,
    // Epic,
    Escape(Box<Cost>),
    Eternalize(Box<Cost>),
    Evoke(Box<Cost>),
    Evolve,
    Exalted,
    Exploit,
    Extort,
    Fabricate(i32),
    Fading(i32),
    Fear,
    FirstStrike,
    Flanking,
    Flash,
    FlashForCasters(Condition),
    Flashback(Box<Cost>),
    Flying,
    ForMirrodin,
    Foretell(Box<Cost>),
    ForetellX(Box<Cost>),
    Frenzy(i32),
    Fuse,
    Graft(i32),
    Gravestorm,
    Haste,
    Haunt,
    Hexproof,
    HiddenAgenda,
    Hideaway(i32),
    Horsemanship,
    Improvise,
    Increment,
    Indestructible,
    Infect,
    Ingest,
    Intimidate,
    JumpStart,
    Kicker(Box<Cost>),
    KickerXWithModifiers(ManaCostX, CastModifier),
    FlashbackWithModifier(Box<Cost>, CastModifier),
    SpecializeWithModifiers(Box<Cost>, ActivateModifier),
    Landwalk(Box<Permanents>),
    Lifelink,
    LivingMetal,
    LivingWeapon,
    Madness(Box<Cost>),
    Megamorph(Box<Cost>),
    Melee,
    Menace,
    Mentor,
    Miracle(Box<Cost>),
    Modular(Box<GameNumber>),
    MoreThanMeetsTheEye(Box<Cost>),
    Morph(Box<Cost>),
    Multikicker(Box<Cost>),
    Mutate(Box<Cost>),
    Myriad,
    Nightbound,
    Ninjutsu(Box<Cost>),
    Persist,
    Phasing,
    Poisonous(i32),
    Provoke,
    Prowess,
    Rampage(i32),
    Ravenous,
    Reach,
    ReadAhead,
    Rebound,
    Reconfigure(Box<Cost>),
    Recover(Box<Cost>),
    Renown(i32),
    Replicate(Box<Cost>),
    Retrace,
    Riot,
    Ripple(Box<GameNumber>),
    Scavenge(Box<Cost>),
    Shadow,
    Shroud,
    Skulk,
    Sneak(Box<Cost>),
    Soulbond,
    Soulshift(Box<GameNumber>),
    Specialize(Box<Cost>),
    SpecializeFromGraveyard(Box<Cost>),
    Spectacle(Box<Cost>),
    SplitSecond,
    Squad(Box<Cost>),
    StartingIntensity(Box<GameNumber>),
    Storm,
    Sunburst,
    UmbraArmor,
    Toxic(i32),
    Training,
    Trample,
    TrampleOverPlaneswalkers,
    Transmute(Box<Cost>),
    Tribute(i32),
    Undaunted,
    Undying,
    Unearth(Box<Cost>),
    Unleash,
    Vanishing,
    VanishingEnters(i32),
    Vigilance,
    Ward(Box<Cost>),
    Outlast(Box<Cost>),
    Transfigure(Box<Cost>),
    Wither,

    // Rule: CDA
    CDA_ColorButNotColorIdentity(SettableColor),
    CDA_Color(SettableColor),
    CDA_Power(Box<GameNumber>),
    CDA_Toughness(Box<GameNumber>),
    CDA_Types(CDA_Types),

    Companion(Companion),
    DeckConstruction(DeckConstruction),
    ConspiracyDeck(ConspiracyDeck),
    StartingHandSizeIs(Box<GameNumber>),
    SpellActions(Box<Actions>),

    SpellActions_Awaken(Box<Cost>, WasAwakened, WasntAwakened),
    SpellActions_Tiered(Vec<TieredAction>),
    SpellActions_Kicker(Box<Cost>, WasKicked, WasntKicked),
    SpellActions_Cleave(Box<Cost>, CleavePaid, CleaveNotPaid),
    SpellActions_Overload(Box<Cost>, OverloadPaid, OverloadNotPaid),
    SpellActions_MadnessX(Box<Cost>, MadnessXWasPaid, MadnessXWasntPaid),
    SpellActions_Gift(Box<Action>, GiftWasPromised, GiftWasntPromised),

    SpellActions_Spree(Vec<SpreeAction>),
    SpellActions_AdditionalCostOptions(Vec<AdditionalCostOption>),
    SelfEffect(Vec<CardEffect>),
    SelfEffect_NonBattlefield(Vec<CardEffect>),
    AsSelfDraft(Vec<DraftAction>),
    FaceUpDraftEffect(FaceUpDraftEffect),
    AsSchemeIsSetInMotion(SingleScheme, Vec<SetInMotionAction>),
    AsPutIntoAGraveyardFromAnywhere(SingleCard, Vec<PutIntoGraveyardAction>),
    AsPermanentBecomesAttachedToAPermanent(Box<Permanent>, Box<Permanents>, Vec<AttachAction>),
    AsPermanentEnters(Box<Permanent>, Vec<ReplacementActionWouldEnter>),
    AsPermanentEscapes(Box<Permanent>, Vec<ReplacementActionWouldEnter>),
    AsPermanentEntersOrIsTurnedFaceUp(Box<Permanent>, Vec<EnterOrFaceUpAction>),
    AsPermanentIsTurnedFaceUp(Box<Permanent>, Vec<FaceUpAction>),
    AsPermanentTransforms(Box<Permanent>, Vec<TransformAction>),
    PlayerEffect(Box<Player>, Vec<PlayerEffect>),
    PlayerEffect_PlayerMayPayToIgnoreEffectUntil(
        Box<Players>,
        Vec<PlayerEffect>,
        Box<Cost>,
        Expiration,
    ),
    EachPlayerEffect(Box<Players>, Vec<PlayerEffect>),
    ThisSpellEffect(Vec<SpellEffect>),
    CardEffect(Box<Cards>, Vec<CardEffect>),
    EachCardInPlayersLibraryEffect(Box<Cards>, Box<Player>, Vec<LibraryCardEffect>),
    EachCardInPlayersHandEffect(Box<Cards>, Box<Player>, Vec<HandEffect>),
    EachCardInEachPlayersHandEffect(Box<Cards>, Box<Players>, Vec<HandEffect>),
    EachPermanentAndSpellEffect(PermanentsAndSpells, PermanentAndSpellEffect),
    PermanentLayerEffect(Box<Permanent>, Vec<StaticLayerEffect>),
    EachPermanentLayerEffect(Box<Permanents>, Vec<StaticLayerEffect>),
    EachPermanentStickyLayerEffect(Box<Permanents>, Vec<StaticLayerEffect>, Expiration),
    PermanentRuleEffect(Box<Permanent>, Vec<PermanentRule>),
    PermanentRuleEffect_PlayerMayPayToIgnoreEffectUntil(
        Box<Permanent>,
        Vec<PermanentRule>,
        Box<Player>,
        Box<Cost>,
        Expiration,
    ),
    EachPermanentRuleEffect(Box<Permanents>, Vec<PermanentRule>),
    EachCardInGraveyardEffect(Box<CardsInGraveyard>, Box<Player>, Vec<GraveyardCardEffect>),
    EachCardInAGraveyardEffect(
        Box<CardsInGraveyard>,
        Box<Players>,
        Vec<GraveyardCardEffect>,
    ),

    TriggerMayOnceEachTurnI(Trigger, Condition, Box<Actions>),
    TriggerMayOnceEachTurn(Trigger, Box<Actions>),
    TriggerModalA(Vec<TriggerAndActions>),
    TriggerA(Trigger, Box<Actions>),
    TriggerOnce(Trigger, Box<Actions>),
    TriggerOnceEachTurn(Trigger, Box<Actions>),
    TriggerOnceEachTurnI(Trigger, Condition, Box<Actions>),
    TriggerTwiceEachTurn(Trigger, Box<Actions>),
    TriggerI(Trigger, Condition, Box<Actions>),
    TriggerIOnce(Trigger, Condition, Box<Actions>),
    TriggerIOnceEachTurn(Trigger, Condition, Box<Actions>),

    Activated(Box<Cost>, Box<Actions>),
    ActivatedWithModifiers(Box<Cost>, Box<Actions>, ActivateModifier),
    FromExileOrBattlefield(Box<Rule>),
    FromExile(Box<Rule>),
    FromExileIf(Condition, Box<Rule>),
    FromStack(Box<Rule>),
    FromStackIf(Condition, Box<Rule>),
    FromGraveyardOrBattlefield(Box<Rule>),
    FromGraveyard(Box<Rule>),
    FromGraveyardIf(Condition, Box<Rule>),
    FromTopOfLibrary_Digital(Vec<Rule>),
    FromHand(Box<Rule>),
    FromAnyZone(Box<Rule>),
    FromCommandZone(Box<Rule>),
    FromCommandZoneOrBattlefield(Box<Rule>),
    SagaChapters(Vec<SagaChapter>),
    Visit(Box<Actions>),
    VisitAndPrize(Box<Actions>, Box<Actions>),
    DungeonLevel(DungeonRoomName, Box<Actions>, Vec<DungeonRoomName>),
    ClassAbilities(Vec<ClassAbility>),
    LevelUp(Box<Cost>, Vec<Level>),
    If(Condition, Vec<Rule>),
    Unless(Condition, Vec<Rule>),
    IfElse(Condition, Vec<Rule>, Vec<Rule>),
    IfCardIsInOpeningHand(Vec<Action>),
    MaxSpeed(Vec<Rule>),

    AsGameBegins(Vec<Action>),
    BeforeDrawingOpeningHand(Vec<Action>),
    DrawAnAdditionalHandBeforeMulligans,
    YouAreTheStartingPlayer,
    BeforeShufflingDeckToStartTheGame(Vec<PregameAction>),

    ReplaceAPlayerWouldCreateAToken(
        ReplacableEventAPlayerWouldCreateAToken,
        Vec<ReplacementActionAPlayerWouldCreateAToken>,
    ),
    ReplaceAPlayerWouldCreateTokens(
        ReplacableEventAPlayerWouldCreateTokens,
        Vec<ReplacementActionAPlayerWouldCreateTokens>,
    ),
    ReplaceAnEffectWouldCreateAnyNumberOfTokens(
        ReplacableEventAnEffectWouldCreateAnyNumberOfTokens,
        Vec<ReplacementActionAnEffectWouldCreateAnyNumberOfTokens>,
    ),
    ReplaceAnyNumberOfTokensWouldBeCreated(
        ReplacableEventAnyNumberOfTokensWouldBeCreated,
        Vec<ReplacementActionAnyNumberOfTokensWouldBeCreated>,
    ),
    ReplaceWouldBeginATurn(
        ReplacableEventWouldBeginATurn,
        Vec<ReplacementActionWouldBeginATurn>,
    ),
    ReplaceWouldBeginDrawStep(
        ReplacableEventWouldBeginDrawStep,
        Vec<ReplacementActionWouldBeginDrawStep>,
    ),
    ReplaceWouldCopyASpell(
        ReplacableEventWouldCopyASpell,
        Vec<ReplacementActionWouldCopyASpell>,
    ),
    ReplaceWouldCounterASpell(
        ReplacableEventWouldCounterASpell,
        Vec<ReplacementActionWouldCounterASpell>,
    ),
    ReplaceWouldDealDamage(
        ReplacableEventWouldDealDamage,
        Vec<ReplacementActionWouldDealDamage>,
    ),
    ReplaceWouldDestroy(
        ReplacableEventWouldDestroy,
        Vec<ReplacementActionWouldDestroy>,
    ),
    ReplaceWouldDiscard(
        ReplacableEventWouldDiscard,
        Vec<ReplacementActionWouldDiscard>,
    ),
    ReplaceWouldDraw(ReplacableEventWouldDraw, Vec<ReplacementActionWouldDraw>),
    ReplaceWouldEnter(ReplacableEventWouldEnter, Vec<ReplacementActionWouldEnter>),
    ReplaceWouldExplore(
        ReplacableEventWouldExplore,
        Vec<ReplacementActionWouldExplore>,
    ),
    ReplaceWouldFlipACoin(
        ReplacableEventWouldFlipACoin,
        Vec<ReplacementActionWouldFlipACoin>,
    ),
    ReplaceWouldGainLife(
        ReplacableEventWouldGainLife,
        Vec<ReplacementActionWouldGainLife>,
    ),
    ReplaceWouldGetEnergy(
        ReplacableEventWouldGetEnergy,
        Vec<ReplacementActionWouldGetEnergy>,
    ),
    ReplaceWouldLeaveTheBattlefield(
        ReplacableEventWouldLeaveTheBattlefield,
        Vec<ReplacementActionWouldLeaveTheBattlefield>,
    ),
    ReplaceWouldLoseLife(
        ReplacableEventWouldLoseLife,
        Vec<ReplacementActionWouldLoseLife>,
    ),
    ReplaceWouldLoseTheGame(
        ReplacableEventWouldLoseTheGame,
        Vec<ReplacementActionWouldLoseTheGame>,
    ),
    ReplaceWouldMill(ReplacableEventWouldMill, Vec<ReplacementActionWouldMill>),
    ReplaceWouldPayLife(
        ReplacableEventWouldPayLife,
        Vec<ReplacementActionWouldPayLife>,
    ),
    ReplaceWouldPlaneswalk(
        ReplacableEventWouldPlaneswalk,
        Vec<ReplacementActionWouldPlaneswalk>,
    ),
    ReplaceWouldProduceMana(
        ReplacableEventWouldProduceMana,
        Vec<ReplacementActionWouldProduceMana>,
    ),
    ReplaceWouldProliferate(
        ReplacableEventWouldProliferate,
        Vec<ReplacementActionWouldProliferate>,
    ),
    ReplaceWouldPutAPermanentOnTheBattlefield(
        ReplacableEventWouldPutAPermanentOnTheBattlefield,
        Vec<ReplacementActionWouldPutAPermanentOnTheBattlefield>,
    ),
    ReplaceWouldPutCounters(
        ReplacableEventWouldPutCounters,
        Vec<ReplacementActionWouldPutCounters>,
    ),
    ReplaceWouldPutIntoGraveyard(
        ReplacableEventWouldPutIntoGraveyard,
        Vec<ReplacementActionWouldPutIntoGraveyard>,
    ),
    ReplaceWouldReduceLife(
        ReplacableEventWouldReduceLife,
        Vec<ReplacementActionWouldReduceLife>,
    ),
    ReplaceWouldRollDice(
        ReplacableEventWouldRollDice,
        Vec<ReplacementActionWouldRollDice>,
    ),
    ReplaceWouldRollPlanarDice(
        ReplacableEventWouldRollPlanarDice,
        Vec<ReplacementActionWouldRollPlanarDice>,
    ),
    ReplaceWouldScry(ReplacableEventWouldScry, Vec<ReplacementActionWouldScry>),
    ReplaceWouldSearchLibrary(
        ReplacableEventWouldSearchLibrary,
        Vec<ReplacementActionWouldSearchLibrary>,
    ),
    ReplaceWouldUntap(ReplacableEventWouldUntap, Vec<ReplacementActionWouldUntap>),

    CastEffect(CastEffect),
    ActivatedAbilityEffect(Box<ActivatedAbilities>, ActivatedAbilityEffect),
    CantVentureIntoThisDungeonUnlessNamed,
    CountsAsACardWithNameForSpellsNamed(NameString, NameString),
    DeckBuildingIfCommander(Vec<DeckBuildingAction>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(
    tag = "_ReplacementActionWouldPlaneswalkLookAtTheTopNumberCardsOfPlanarDeckAction",
    content = "args"
)]
pub enum ReplacementActionWouldPlaneswalkLookAtTheTopNumberCardsOfPlanarDeckAction {
    PutACardOnBottom,
    PutTheRemainingCardOnTopInAnyOrder,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(
    tag = "_ReplacementActionWouldDrawLookAtTheTopNumberCardsOfLibraryAction",
    content = "args"
)]
pub enum ReplacementActionWouldDrawLookAtTheTopNumberCardsOfLibraryAction {
    PutAGenericCardIntoGraveyard,
    PutAGenericCardIntoHand,
    PutAGenericCardOnTopOfLibrary,
    PutTheRemainingCardsIntoGraveyard,
    PutTheRemainingCardsOnTheBottomOfLibraryInARandomOrder,
    PutTheRemainingCardsOnTheBottomOfLibraryInAnyOrder,
    PutTheRemainingCardsOnTopOfLibraryInAnyOrder,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldPutIntoGraveyardCost", content = "args")]
pub enum ReplacementActionWouldPutIntoGraveyardCost {
    ExileItInstead,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldPayLife", content = "args")]
pub enum ReplacementActionWouldPayLife {
    ExileTheTopNumberCardsOfLibrary(Box<GameNumber>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldPlaneswalk", content = "args")]
pub enum ReplacementActionWouldPlaneswalk {
    ChaosEnsues,
    LookAtTheTopNumberCardsOfPlanarDeck(
        Box<GameNumber>,
        Vec<ReplacementActionWouldPlaneswalkLookAtTheTopNumberCardsOfPlanarDeckAction>,
    ),
    Planeswalk,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldGetEnergy", content = "args")]
pub enum ReplacementActionWouldGetEnergy {
    GetEnergy(Box<GameNumber>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldLeaveTheBattlefield", content = "args")]
pub enum ReplacementActionWouldLeaveTheBattlefield {
    ExileItInstead,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldPutIntoGraveyard", content = "args")]
pub enum ReplacementActionWouldPutIntoGraveyard {
    GainLife(Box<GameNumber>),
    CreateFutureTrigger(FutureTrigger, Box<Actions>),
    CreatePlayerEffectUntil(Box<Player>, Vec<PlayerEffect>, Expiration),
    CreateTokens(Vec<CreatableToken>),
    ExileItInstead,
    ExileItWithACounterInstead(CounterType),
    ExileItWithNumberCountersInstead(Box<GameNumber>, CounterType),
    If(Condition, Vec<ReplacementActionWouldPutIntoGraveyard>),
    LoseLife(Box<GameNumber>),
    MayAction(Box<ReplacementActionWouldPutIntoGraveyard>),
    MustCost(ReplacementActionWouldPutIntoGraveyardCost),
    PlayerAction(Box<Player>, Box<ReplacementActionWouldPutIntoGraveyard>),
    PutACounterOnExiledCard(CounterType, CardInExile),
    PutItInOwnersHandInstead,
    PutItOnBottomOfOwnersLibraryInstead,
    PutItOnTopOfOwnersLibraryInstead,
    PutNumberCountersOfTypeOnPermanent(Box<GameNumber>, CounterType, Box<Permanent>),
    ReflexiveTrigger(Box<Actions>),
    RevealItAndPutItOnBottomOfOwnersLibraryInstead,
    ShuffleItIntoLibraryInstead,
    TakeAnExtraTurn,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldGainLife", content = "args")]
pub enum ReplacementActionWouldGainLife {
    DrawNumberCards(Box<GameNumber>),
    GainLife(Box<GameNumber>),
    GainNoLifeInstead,
    LoseLife(Box<GameNumber>),
    PlayerAction(Box<Player>, Box<ReplacementActionWouldGainLife>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldMill", content = "args")]
pub enum ReplacementActionWouldMill {
    PlayerAction(Box<Player>, Box<ReplacementActionWouldMill>),
    MillNumberCards(Box<GameNumber>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldProduceMana", content = "args")]
pub enum ReplacementActionWouldProduceMana {
    WouldProduceMana_AddMana(ManaProduce),
    WouldProduceMana_ProduceMultiple(Box<GameNumber>),
    WouldProduceMana_ReplaceColor(Color),
    WouldProduceMana_ReplaceType(ManaProduce),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldDrawCost", content = "args")]
pub enum ReplacementActionWouldDrawCost {
    DiscardACard,
    PutAGraveyardCardOntoBattlefield(Box<CardsInGraveyard>, Vec<ReplacementActionWouldEnter>),
    PutACardFromGraveyardIntoHand(Box<CardsInGraveyard>),
    PayLife(Box<GameNumber>),
}
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldDraw", content = "args")]
pub enum ReplacementActionWouldDraw {
    AnyPlayerMayCost(Box<Players>, ReplacementActionWouldDrawCost),
    CastExiledCardWithoutPaying(CardInExile),
    ChooseAPlayer(Box<Players>),
    ChooseAnAction(Vec<Vec<ReplacementActionWouldDraw>>),
    CreatePlayerEffectUntil(Box<Player>, Vec<PlayerEffect>, Expiration),
    CreateTokens(Vec<CreatableToken>),
    DiscardACard,
    DiscardTheCardDrawnThisWay,
    DrawACard,
    MillACard,
    DrawNumberCards(Box<GameNumber>),
    EachPlayerAction(Box<Players>, Box<ReplacementActionWouldDraw>),
    ExileCardsFromTheTopOfLibraryUntilACardOfTypeIsExiled(Box<CardsInLibrary>),
    ExileTheTopCardOfPlayersLibrary(Box<Player>),
    ExileTheTopNumberCardsOfLibrary(Box<GameNumber>),
    ExileTopCardOfLibrary,
    ExileTopCardOfLibraryFaceDown,
    GainLife(Box<GameNumber>),
    IfElse(
        Condition,
        Vec<ReplacementActionWouldDraw>,
        Vec<ReplacementActionWouldDraw>,
    ),
    If(Condition, Vec<ReplacementActionWouldDraw>),
    LookAtTheTopNumberCardsOfLibrary(
        Box<GameNumber>,
        Vec<ReplacementActionWouldDrawLookAtTheTopNumberCardsOfLibraryAction>,
    ),
    LoseLife(Box<GameNumber>),
    LoseTheGame,
    MayAction(Box<ReplacementActionWouldDraw>),
    MustCost(ReplacementActionWouldDrawCost),
    PlayerAction(Box<Player>, Box<ReplacementActionWouldDraw>),
    PlayerActions(Box<Player>, Vec<ReplacementActionWouldDraw>),
    PutACardFromOutsideGameInHand(Box<Cards>),
    PutACounterOfTypeOnPermanent(CounterType, Box<Permanent>),
    PutAPermanentIntoItsOwnersHand(Box<Permanents>),
    PutEachExiledCardOnTheBottomOfTheirOwnersLibraryInARandomOrder(CardsInExile),
    PutExiledCardIntoOwnersHand(CardInExile),
    PutNumberCountersOfTypeOnPermanent(Box<GameNumber>, CounterType, Box<Permanent>),
    PutTheTopCardOfTheExiledPileIntoHand,
    PutTopOfLibraryInGraveyard,
    RevealTheCardDrawnThisWay,
    RevealTopCardOfLibrary,
    SkipThatDraw,
    Unless(Condition, Vec<ReplacementActionWouldDraw>),
    WinTheGame,
    PlayerMustCost(Box<Player>, ReplacementActionWouldDrawCost),
    PermanentDealsDamage(Box<Permanent>, Box<GameNumber>, Box<DamageRecipients>),

    RevealCardsFromTheTopOfLibraryUntilACardOfTypeIsRevealed(
        Box<Cards>,
        Vec<RevealTheTopNumberCardsOfLibraryAction>,
    ),
    RevealTheTopNumberCardsOfLibrary(Box<GameNumber>, Vec<RevealTheTopNumberCardsOfLibraryAction>),
    SearchLibrary(Vec<SearchLibraryAction>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldDealDamageCost", content = "args")]
pub enum ReplacementActionWouldDealDamageCost {
    ExileNumberGraveyardCards(Box<GameNumber>, Box<CardsInGraveyard>),
    PayMana(ManaCost),
}
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldDealDamage", content = "args")]
pub enum ReplacementActionWouldDealDamage {
    If(Condition, Vec<ReplacementActionWouldDealDamage>),
    IfElse(
        Condition,
        Vec<ReplacementActionWouldDealDamage>,
        Vec<ReplacementActionWouldDealDamage>,
    ),
    Unless(Condition, Vec<ReplacementActionWouldDealDamage>),

    LoseTheGame,

    CancelThatDamage,
    ContinueDealingDamage,
    DealDamageAsThoughItHadInfect,

    DealDamageInstead(Box<GameNumber>),
    DealSomeDamageToRecipientInstead(Box<GameNumber>, Box<DamageRecipients>),
    DealToAnyTargetInstead(Box<SingleDamageRecipient>),
    DealToCreatureOrPlaneswalkerInstead(Box<Permanent>),
    DealToPlayerInstead(Box<Player>),

    PreventAllButSomeOfThatDamage(Box<GameNumber>),
    PreventSomeOfThatDamage(Box<GameNumber>),
    PreventThatDamage,

    PermanentDealsDamage(Box<Permanent>, Box<GameNumber>, Box<DamageRecipient>),
    SpellDealsDamage(Box<Spell>, Box<GameNumber>, Box<DamageRecipient>),
    HaveSpellDealDamage(Box<Spell>, Box<GameNumber>, Box<DamageRecipient>),
    VanguardDealsDamage(SingleVanguard, Box<GameNumber>, Box<DamageRecipient>),

    EachPlayerAction(Box<Players>, Box<ReplacementActionWouldDealDamage>),
    MayAction(Box<ReplacementActionWouldDealDamage>),
    MayActions(Vec<ReplacementActionWouldDealDamage>),
    MustCost(ReplacementActionWouldDealDamageCost),
    PlayerMayCost(Box<Player>, ReplacementActionWouldDealDamageCost),

    RemoveNumberCountersOfTypeFromPermanent(Box<GameNumber>, CounterType, Box<Permanent>),

    ChooseAPlayer(Box<Players>),

    CreateFutureTrigger(FutureTrigger, Box<Actions>),
    ReflexiveTrigger(Box<Actions>),

    CreateTokens(Vec<CreatableToken>),
    DestroyPermanent(Box<Permanent>),
    DrawNumberCards(Box<GameNumber>),
    ExileNumberGraveyardCards(Box<GameNumber>, Box<CardsInGraveyard>),
    ExileTheTopNumberCardsOfLibrary(Box<GameNumber>),
    GainControlOfPermanent(Box<Permanent>),
    GainLife(Box<GameNumber>),
    GetNumberRadCounters(Box<GameNumber>),
    MillNumberCards(Box<GameNumber>),
    PlayerAction(Box<Player>, Box<ReplacementActionWouldDealDamage>),
    PutACounterOfTypeOnPermanent(CounterType, Box<Permanent>),
    PutNumberCountersOfTypeOnPermanent(Box<GameNumber>, CounterType, Box<Permanent>),
    RemoveACounterOfTypeFromPermanent(CounterType, Box<Permanent>),
    SacrificeNumberPermanents(Box<GameNumber>, Box<Permanents>),
    ShufflePermanentIntoLibrary(Box<Permanent>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldScry", content = "args")]
pub enum ReplacementActionWouldScry {
    DrawNumberCards(Box<GameNumber>),
    Scry(Box<GameNumber>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldReduceLife", content = "args")]
pub enum ReplacementActionWouldReduceLife {
    TransformPermanent(Box<Permanent>),
    SetLifeTotal(Box<GameNumber>),
    Unless(Box<Condition>, Vec<ReplacementActionWouldReduceLife>),
    LoseTheGame,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldLoseTheGame", content = "args")]
pub enum ReplacementActionWouldLoseTheGame {
    DrawNumberCards(Box<GameNumber>),
    ExilePermanent(Box<Permanent>),
    ShuffleHandGraveyardAndPermanentsIntoLibrary,
    SetLifeTotal(Box<GameNumber>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldBeginDrawStepCost", content = "args")]
pub enum ReplacementActionWouldBeginDrawStepCost {
    SkipThisDrawStep,
}
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldBeginDrawStep", content = "args")]
pub enum ReplacementActionWouldBeginDrawStep {
    MayCost(ReplacementActionWouldBeginDrawStepCost),
    If(Box<Condition>, Vec<ReplacementActionWouldBeginDrawStep>),
    GainLife(Box<GameNumber>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldBeginATurnCost", content = "args")]
pub enum ReplacementActionWouldBeginATurnCost {
    SkipThisTurn,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldBeginATurn", content = "args")]
pub enum ReplacementActionWouldBeginATurn {
    MayCost(ReplacementActionWouldBeginATurnCost),
    SkipTurn,
    UntapPermanent(Box<Permanent>),
    If(Condition, Vec<ReplacementActionWouldBeginATurn>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldSearchLibrary", content = "args")]
pub enum ReplacementActionWouldSearchLibrary {
    SearchTopNumberCardsOfLibraryInstead(Box<GameNumber>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldRollDice", content = "args")]
pub enum ReplacementActionWouldRollDice {
    RollThatManyAndMayExchangeOneWithPermanentsBasePowerOrBaseToughness(Box<Permanent>),
    RollThatManyPlusOneAndIgnoreLowestInstead,
    RollThatManyPlusOneAndPlayerChoosesOneToIgnore(Box<Player>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldRollPlanarDice", content = "args")]
pub enum ReplacementActionWouldRollPlanarDice {
    WouldRollDice_RollThatManyPlusOneAndIgnoreOne,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_FutureReplacableEventWouldAdapt", content = "args")]
pub enum FutureReplacableEventWouldAdapt {
    NextTimeCreatureAdaptsThisTurn(Box<Permanent>),
}
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldAdapt", content = "args")]
pub enum ReplacementActionWouldAdapt {
    AdaptAsThoughtItHadNoCounters(CounterType),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldPutCounters", content = "args")]
pub enum ReplacementActionWouldPutCounters {
    GetAPoisonCounter,
    PutNewAmount(Box<GameNumber>),
    PutNewAmountOfType(Box<GameNumber>, CounterType),
    GetNewAmount(Box<GameNumber>),
    CreatePlayerEffectUntil(Box<Player>, Vec<PlayerEffect>, Expiration),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldUntapCost", content = "args")]
pub enum ReplacementActionWouldUntapCost {
    RemoveACounterOfTypeFromPermanent(CounterType, Box<Permanent>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldUntap", content = "args")]
pub enum ReplacementActionWouldUntap {
    RemoveAllCountersOfTypeFromPermanent(CounterType, Box<Permanent>),
    MustCost(ReplacementActionWouldUntapCost),
    UntapPermanent(Box<Permanent>),
    If(Box<Condition>, Vec<ReplacementActionWouldUntap>),
    PutNumberCountersOfTypeOnPermanent(Box<GameNumber>, CounterType, Box<Permanent>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldDestroy", content = "args")]
pub enum ReplacementActionWouldDestroy {
    RegeneratePermanent(Box<Permanent>),
    CancelDestroy,
    RemoveAllDamageFromPermanent(Box<Permanent>),
    SacrificePermanent(Box<Permanent>),
    CreatePermanentLayerEffectUntil(Box<Permanent>, Vec<LayerEffect>, Expiration),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldDiscard", content = "args")]
pub enum ReplacementActionWouldDiscard {
    DiscardItToTopOfLibraryInstead,
    MayAction(Box<ReplacementActionWouldDiscard>),
    MayActions(Vec<ReplacementActionWouldDiscard>),
    RevealItAndPutItOnTopOfLibraryInstead,
    PutItOntoTheBattlefieldInstead(Vec<ReplacementActionWouldEnter>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldCounterASpell", content = "args")]
pub enum ReplacementActionWouldCounterASpell {
    ExileSpell(Box<Spell>),
    MayAction(Box<ReplacementActionWouldCounterASpell>),
    PlayExiledCardWithoutPaying(Box<CardInExile>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldFlipACoin", content = "args")]
pub enum ReplacementActionWouldFlipACoin {
    FlipTwoCoinsAndIgnoreOne,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(
    tag = "_ReplacementActionWouldPutAPermanentOnTheBattlefield",
    content = "args"
)]
pub enum ReplacementActionWouldPutAPermanentOnTheBattlefield {
    WouldPutPermanentOnBattlefield_PutPermanentOnBattlefield,
    SacrificeAPermanent(Box<Permanents>),
    PlayerAction(
        Box<Player>,
        Box<ReplacementActionWouldPutAPermanentOnTheBattlefield>,
    ),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldCopyASpell", content = "args")]
pub enum ReplacementActionWouldCopyASpell {
    WouldCopyASpell_CopyAnAdditionalTimeAndMayChooseNewTargets,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldProliferate", content = "args")]
pub enum ReplacementActionWouldProliferate {
    ProliferateNumberTimes(Box<GameNumber>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldLearn", content = "args")]
pub enum ReplacementActionWouldLearn {
    PutGraveyardCardOntoBattlefield(CardInGraveyard, Vec<ReplacementActionWouldEnter>),
    Learn,
    ChooseAnAction(Vec<Vec<ReplacementActionWouldLearn>>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldMask", content = "args")]
pub enum ReplacementActionWouldMask {
    TurnItFaceUpInstead,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldEnterCost", content = "args")]
pub enum ReplacementActionWouldEnterCost {
    DiscardACardOfType(Box<Cards>),
    EntersTapped,
    ExileNumberGraveyardCards(Box<GameNumber>, CardsInGraveyard),
    ExileTwoCardsFromAmongPlayersGraveyards(CardsInGraveyard, Box<Players>),
    PayLife(Box<GameNumber>),
    PutANumberOfExiledCardsIntoOwnersGraveyard(Box<GameNumber>, CardsInExile),
    PutAPermanentIntoItsOwnersHand(Box<Permanents>),
    RevealACardOfTypeFromHand(Box<Cards>),
    RevealAnyNumberOfCardsOfTypeFromHand(Box<Cards>),
    SacrificeAPermanent(Box<Permanents>),
    SacrificeNumberPermanents(Box<GameNumber>, Box<Permanents>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldEnter", content = "args")]
pub enum ReplacementActionWouldEnter {
    APlayerAction(Box<Players>, Box<ReplacementActionWouldEnter>),

    ChooseUptoNumberColors(Box<GameNumber>, Box<ChoosableColor>),
    IfCardPassesFilter(Box<Cards>, Vec<ReplacementActionWouldEnter>),
    RemoveAllCountersFromAnyNumberOfPermanents(Box<Permanents>),

    BecomeDay,
    ChooseABasicLandType,
    ChooseACardName(Box<Cards>),
    ChooseACardtype,
    ChooseACardtypeExceptFromList(Vec<CardType>),
    ChooseACardtypeFromList(Vec<CardType>),
    ChooseACardtypeSharedAmongExiledCards(Box<CardsInExile>),
    ChooseAColor(ChoosableColor),
    ChooseAColorAndCreatureType(Vec<ColorAndCreatureType>),
    ChooseACreatureType,
    ChooseACreatureTypeFromList(Vec<CreatureType>),
    ChooseADirection,
    ChooseALandType,
    ChooseANumberBetween(i32, i32),
    ChooseANumberFromAmongAtRandom(Vec<i32>),
    ChooseANumberGreaterThanNumber(i32),
    ChooseAPermanent(Box<Permanents>),
    ChooseAPlaneswalkerType,
    ChooseAPlayer(Box<Players>),
    ChooseEvenOrOdd,
    ChooseLandType(Vec<LandType>),
    ChooseNumberAbilities(Box<GameNumber>, Vec<Rule>),
    ChooseTwoBasicLandTypes,
    ChooseTwoColors,
    ChooseTwoPlayers(Box<Players>),
    ChooseWord(Vec<VoteOption>),
    CreateFutureTrigger(FutureTrigger, Box<Actions>),
    DiscardAnyNumberOfCardsOfType(Box<Cards>),
    DiscardHand,
    DraftACardFromSpellBook(SpellBookName),
    EachPlayerAction(Box<Players>, Box<ReplacementActionWouldEnter>),
    EnterAsACopyOfACardInAPlayersGraveyard(CardsInGraveyard, Box<Players>, CopyEffects),
    EnterAsACopyOfACardInExile(CardsInExile, CopyEffects),
    EnterAsACopyOfAPermanent(Box<Permanents>, CopyEffects),
    EnterAsACopyOfAPermanentUntil(Box<Permanents>, CopyEffects, Expiration),
    EnterAsACopyOfPermanent(Box<Permanent>, CopyEffects),
    EnterAsCopyOfExiled(CardInExile, CopyEffects),
    EntersAsFaceDownArtifactCreature(PT, CreatureType),
    EntersAsFaceDownCreatureWithAbilitiesAndNotedName(PT, Vec<Rule>, NameFilter),
    EntersAsFaceDownLand(LandType),
    EntersAsNonAuraEnchantment,
    EntersAttachedToAPermanent(Box<Permanents>),
    EntersAttachedToPermanent(Box<Permanent>),
    EntersAttachedToPlayer(Box<Player>),
    EntersAttacking,
    EntersAttackingPlayer(Box<Player>),
    EntersAttackingPlayerOrPlaneswalkerControlledBy(Box<Player>),
    EntersBlockingAttacker(Box<Permanent>),
    EntersConverted,
    EntersFaceDown,
    EntersFlipped,
    EntersNormally,
    EntersTapped,
    EntersTransformed,
    EntersPrepared,
    EntersUnderAPlayersControl(Box<Players>),
    EntersUnderOwnersControl,
    EntersUnderPlayersControl(Box<Player>),
    EntersUntapped,
    EntersWithACounter(CounterType),
    EntersWithACounterOfChoice(Vec<CounterType>),
    EntersWithACounterOfTypeForEachKindOfCounterOnPermanent(Box<Permanent>),
    EntersWithAnAbilityCounterForEachAbilityOnACardDiscardedThisWay(Vec<CheckHasable>),
    EntersWithLayerEffect(Vec<LayerEffect>),
    EntersWithLayerEffectOfChoice(Vec<Vec<LayerEffect>>),
    EntersWithLayerEffectUntil(Vec<LayerEffect>, Expiration),
    EntersWithNotedCounters,
    EntersWithNumberCombinationCountersOfChoice(Box<GameNumber>, Vec<CounterType>),
    EntersWithNumberCounters(Box<GameNumber>, CounterType),
    EntersWithNumberCountersForEach(Box<GameNumber>, CounterType, Box<GameNumber>),
    EntersWithNumberDifferentCountersOfChoice(Box<GameNumber>, Vec<CounterType>),
    EntersWithPerpetualEffect(Vec<PerpetualEffect>),
    ExchangeTextBoxesOfTwoPermanents(Box<Permanent>, Box<Permanent>),
    Exile(Vec<Exilable>),
    ExileAnyNumberOfCardsFromPlayersGraveyard(Box<Cards>, Box<Player>),
    ExileCardFromHand(CardInHand),
    ExileItInstead,
    ExileUptoNumberGraveyardCards(Box<GameNumber>, Box<CardsInGraveyard>),
    FlipACoin_OnHeadAndOnTails(
        Vec<ReplacementActionWouldEnter>,
        Vec<ReplacementActionWouldEnter>,
    ),
    GetAnEmblem(Vec<Rule>),
    GetNumberRadCounters(Box<GameNumber>),
    If(Condition, Vec<ReplacementActionWouldEnter>),
    IfElse(
        Box<Condition>,
        Vec<ReplacementActionWouldEnter>,
        Vec<ReplacementActionWouldEnter>,
    ),
    IfElsePassesFilter(
        Box<Permanents>,
        Vec<ReplacementActionWouldEnter>,
        Vec<ReplacementActionWouldEnter>,
    ),
    IfPassesFilter(Box<Permanents>, Vec<ReplacementActionWouldEnter>),
    LookAtAPlayersHand(Box<Players>),
    LoseLife(Box<GameNumber>),
    MayActions(Vec<ReplacementActionWouldEnter>),
    MayCost(ReplacementActionWouldEnterCost),
    MillNumberCards(Box<GameNumber>),
    MillNumberCardsForEach(Box<GameNumber>, Box<GameNumber>),
    MustCost(ReplacementActionWouldEnterCost),
    NoteTheMostPrevalentCreaturTypeInAPlayersLibrary(Box<Players>),
    PayAnyAmountOfLife,
    PayAnyAmountOfLifeUpto(Box<GameNumber>),
    PutACounterOfTypeOnAPermanent(CounterType, Box<Permanents>),
    PutIntoGraveyardInstead,
    ReflexiveTrigger(Box<Actions>),
    RememberLifeTotal,
    RemoveAllCountersFromEachPermanent(Box<Permanents>),
    RevealHand,
    RollNumberD6(Box<GameNumber>),
    RollTwoD6,
    SacrificeAnyNumberOfPermanents(Box<Permanents>),
    SacrificeEachPermanent(Box<Permanents>),
    SecretlyChooseANumberBetween(i32, i32),
    SecretlyChooseAPlayer(Box<Players>),
    ShuffleItIntoLibraryInstead,
    TurnEachPermanentFaceDown(Box<Permanents>),
    Unless(Condition, Vec<ReplacementActionWouldEnter>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionAPlayerWouldCreateAToken", content = "args")]
pub enum ReplacementActionAPlayerWouldCreateAToken {
    CreateTokensInstead(Vec<CreatableToken>),
}
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(
    tag = "_ReplacementActionAPlayerWouldCreateTokensCost",
    content = "args"
)]
pub enum ReplacementActionAPlayerWouldCreateTokensCost {
    ChooseAPermanent(Box<Permanents>),
}
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionAPlayerWouldCreateTokens", content = "args")]
pub enum ReplacementActionAPlayerWouldCreateTokens {
    CreateTokensInstead(Vec<CreatableToken>),
    ChooseAnAction(Vec<Vec<ReplacementActionAPlayerWouldCreateTokens>>),
    MayCost(Box<ReplacementActionAPlayerWouldCreateTokensCost>),
    IfElse(
        Box<Condition>,
        Vec<ReplacementActionAPlayerWouldCreateTokens>,
        Vec<ReplacementActionAPlayerWouldCreateTokens>,
    ),
}
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(
    tag = "_ReplacementActionAnEffectWouldCreateAnyNumberOfTokens",
    content = "args"
)]
pub enum ReplacementActionAnEffectWouldCreateAnyNumberOfTokens {
    CreateTokensInstead(Vec<CreatableToken>),
}
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(
    tag = "_ReplacementActionAnyNumberOfTokensWouldBeCreated",
    content = "args"
)]
pub enum ReplacementActionAnyNumberOfTokensWouldBeCreated {
    CreateTokensInstead(Vec<CreatableToken>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventAPlayerWouldCreateAToken", content = "args")]
pub enum ReplacableEventAPlayerWouldCreateAToken {
    APlayerWouldCreateAToken(Box<Players>, Box<Permanents>),
}
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventAPlayerWouldCreateTokens", content = "args")]
pub enum ReplacableEventAPlayerWouldCreateTokens {
    APlayerWouldCreateTokens(Box<Players>, Box<Permanents>),
    APlayerWouldCreateTokensForTheFirstTimeEachTurn(Box<Players>, Box<Permanents>),
}
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(
    tag = "_ReplacableEventAnEffectWouldCreateAnyNumberOfTokens",
    content = "args"
)]
pub enum ReplacableEventAnEffectWouldCreateAnyNumberOfTokens {
    AnEffectWouldCreateAnyNumberOfTokensUnderAPlayersControl(Box<Permanents>, Box<Players>),
    AnEffectWouldCreateAnyNumberOfTokens(Box<Permanents>),
}
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(
    tag = "_ReplacableEventAnyNumberOfTokensWouldBeCreated",
    content = "args"
)]
pub enum ReplacableEventAnyNumberOfTokensWouldBeCreated {
    AnyNumberOfTokensWouldBeCreatedUnderAPlayersControl(Box<Permanents>, Box<Players>),
    AnyNumberOfTokensWouldBeCreated(Box<Permanents>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldLoseLife", content = "args")]
pub enum ReplacementActionWouldLoseLife {
    LoseLife(Box<GameNumber>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldExplore", content = "args")]
pub enum ReplacementActionWouldExplore {
    ItExplores,
    Scry(Box<GameNumber>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldExplore", content = "args")]
pub enum ReplacableEventWouldExplore {
    APermanentWouldExplore(Box<Permanents>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_FutureReplacableEventWouldEnter", content = "args")]
pub enum FutureReplacableEventWouldEnter {
    NextTimePermanentsWouldEnterTheBattlefield(Box<Permanents>),
}
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldEnter", content = "args")]
pub enum ReplacableEventWouldEnter {
    APermanentWouldEnterTheBattlefieldFromExileOrAfterBeingCastFromExile(Box<Permanents>),
    APermanentWouldEnterTheBattlefieldUnderAPlayersControl(Box<Permanents>, Box<Players>),
    APermanentWouldEnterTheBattlefieldAndWasntCast(Box<Permanents>),
    APermanentWouldEnterTheBattlefield(Box<Permanents>),
    PermanentWouldEnterTheBattlefieldAndWasntCastOrNoManaWasSpentToCast(Box<Permanent>),
    PermanentWouldEnterTheBattlefield(Box<Permanent>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldPayLife", content = "args")]
pub enum ReplacableEventWouldPayLife {
    APlayerWouldPayAnAmountOfLife(Box<Players>, Box<Comparison>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldLoseLife", content = "args")]
pub enum ReplacableEventWouldLoseLife {
    APlayerWouldLoseLife(Box<Players>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldPlaneswalk", content = "args")]
pub enum ReplacableEventWouldPlaneswalk {
    APlayerWouldPlaneswalkAsAResultOfRollingThePlanarDie(Box<Players>),
    APlayerWouldPlaneswalk(Box<Players>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldGetEnergy", content = "args")]
pub enum ReplacableEventWouldGetEnergy {
    APlayerWouldGetEnergy(Box<Players>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(
    tag = "_FutureReplacableEventWouldLeaveTheBattlefield",
    content = "args"
)]
pub enum FutureReplacableEventWouldLeaveTheBattlefield {
    PermanentWouldLeaveTheBattlefield(Box<Permanent>),
}
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldLeaveTheBattlefield", content = "args")]
pub enum ReplacableEventWouldLeaveTheBattlefield {
    PermanentWouldLeaveTheBattlefield(Box<Permanent>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldPutIntoGraveyard", content = "args")]
pub enum ReplacableEventWouldPutIntoGraveyard {
    Or(Vec<ReplacableEventWouldPutIntoGraveyard>),

    APermanentWouldDie(Box<Permanents>),
    APermanentWouldBePutIntoAGraveyard(Box<Permanents>),
    WouldPutAPermanentIntoAPlayersGraveyard(Box<Permanents>, Box<Players>),
    WouldPutACardInPlayersGraveyardFromAnywhereNotCycled(Box<Cards>, Box<Player>),
    WouldPutACardInPlayersGraveyardFromAnywhere(Box<Cards>, Box<Player>),
    WouldPutACardInAPlayersGraveyardFromAnywhereOtherThanBattlefield(Box<Cards>, Box<Players>),
    WouldPutACardInAPlayersGraveyardFromAnywhere(Box<Cards>, Box<Players>),
    WouldPutACardOrTokenInAPlayersGraveyardFromAnywhere(Box<Cards>, Box<Players>),
    WouldPutAPermanentIntoPlayersGraveyard(Box<Permanents>, Box<Player>),
    WouldPutAPermanentASpellOrACardIntoAPlayersGraveyard(
        Box<Permanents>,
        Box<Spells>,
        Box<Cards>,
        Box<Players>,
    ),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldGainLife", content = "args")]
pub enum ReplacableEventWouldGainLife {
    APlayerWouldGainLife(Box<Players>),
    PlayerWouldGainLife(Box<Player>),
    ASpellOrAbilityWouldCauseItsControllerToGainLife(SpellsAndAbilities),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldMill", content = "args")]
pub enum ReplacableEventWouldMill {
    APlayerWouldMillAnyNumberOfCards(Box<Players>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldProduceMana", content = "args")]
pub enum ReplacableEventWouldProduceMana {
    TappingPermanentWouldProduceMana(Box<Permanent>),
    TappingAPermanentWouldProduceMana(Box<Permanents>),
    TappingAPermanentWouldProduceTwoOrMoreMana(Box<Permanents>),
    PlayerTappingAPermanentWouldProduceMana(Box<Player>, Box<Permanents>),
    ASpellOrAbilityWouldProduceColoredMana(SpellsAndAbilities),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_FutureReplacableEventWouldDraw", content = "args")]
pub enum FutureReplacableEventWouldDraw {
    NextTimePlayerWouldDrawACardThisTurn(Box<Player>),
}
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldDraw", content = "args")]
pub enum ReplacableEventWouldDraw {
    APlayerWouldDrawOneOrMoreCards(Box<Players>),
    APlayerWouldDrawACard(Box<Players>),
    PlayerWouldDrawACardForTheFirstTimeEachPlayersTurn(Box<Player>, Box<Players>),
    PlayerWouldDrawDuringTheirDrawStep(Box<Player>),
    APlayerWouldDrawExceptFirstDrawStepDraw(Box<Players>),
    APlayerWouldDrawTwoOrMoreCards(Box<Players>),
    PlayerWouldDrawExceptFirstDrawStepDraw(Box<Player>),
    ACyclingAbilityOfAPermanentWouldCausePlayerToDrawACard(Box<Permanents>, Box<Player>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_FutureReplacableEventWouldDealDamage", content = "args")]
pub enum FutureReplacableEventWouldDealDamage {
    NextDistributedDamageThisTurn,

    NextAmountOfDamageThatWouldBeDealtThisTurnByPermanent(Box<GameNumber>, Box<Permanent>),
    NextAmountOfDamageThatWouldBeDealtThisTurnBySourceToARecipient(
        Box<GameNumber>,
        Box<SingleDamageSource>,
        Box<DamageRecipientsList>,
    ),
    NextAmountOfDamageThatWouldBeDealtThisTurnBySourceToRecipient(
        Box<GameNumber>,
        Box<SingleDamageSource>,
        Box<SingleDamageRecipient>,
    ),
    NextAmountOfDamageThatWouldBeDealtThisTurnBySpellToRecipient(
        Box<GameNumber>,
        Box<Spell>,
        Box<SingleDamageRecipient>,
    ),
    NextAmountOfDamageThatWouldBeDealtThisTurnToARecipient(
        Box<GameNumber>,
        Box<DamageRecipientsList>,
    ),
    NextAmountOfDamageThatWouldBeDealtThisTurnToEachRecipient(
        Box<GameNumber>,
        Box<MultipleDamageRecipients>,
    ),
    NextAmountOfDamageThatWouldBeDealtThisTurnToRecipient(
        Box<GameNumber>,
        Box<SingleDamageRecipient>,
    ),

    NextTimeCombatDamageWouldBeDealtThisTurnByCreature(Box<Permanent>),
    NextTimeCombatDamageWouldBeDealtThisTurnByCreatureToAnyNumberOfRecipients(
        Box<Permanent>,
        Box<DamageRecipientsList>,
    ),
    NextTimeCombatDamageWouldBeDealtThisTurnByCreatureToRecipient(
        Box<Permanent>,
        Box<SingleDamageRecipient>,
    ),
    NextTimeDamageWouldBeDealtThisTurnByAPermanentToRecipient(
        Box<Permanents>,
        Box<SingleDamageRecipient>,
    ),
    NextTimeDamageWouldBeDealtThisTurnByASpellToRecipient(Box<Spells>, Box<SingleDamageRecipient>),
    NextTimeDamageWouldBeDealtThisTurnByPermanent(Box<Permanent>),
    NextTimeDamageWouldBeDealtThisTurnByPermanentToARecipient(
        Box<Permanent>,
        Box<DamageRecipientsList>,
    ),
    NextTimeDamageWouldBeDealtThisTurnByPermanentToRecipient(
        Box<Permanent>,
        Box<SingleDamageRecipient>,
    ),
    NextTimeDamageWouldBeDealtThisTurnBySource(Box<SingleDamageSource>),
    NextTimeDamageWouldBeDealtThisTurnBySourceToARecipient(
        Box<SingleDamageSource>,
        Box<DamageRecipientsList>,
    ),
    NextTimeDamageWouldBeDealtThisTurnBySourceToRecipient(
        Box<SingleDamageSource>,
        Box<SingleDamageRecipient>,
    ),
    NextTimeDamageWouldBeDealtThisTurnToARecipient(Box<DamageRecipientsList>),
    NextTimeDamageWouldBeDealtThisTurnToRecipient(Box<SingleDamageRecipient>),

    NextTimeDamageWouldBeDealtToRecipient(Box<SingleDamageRecipient>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldDealDamage", content = "args")]
pub enum ReplacableEventWouldDealDamage {
    Or(Vec<ReplacableEventWouldDealDamage>),

    // (DamageType)(WouldBeDealt)BySource
    // (DamageType)(WouldBeDealt)BySourceToRecipient
    // (DamageType)(WouldBeDealt)ToRecipient
    LethalLoyaltyDamageWouldBeDealtToAPlaneswalker(Box<Permanents>),

    ASpellOrAbilityWouldCauseASourceToDealDamageToRecipient(
        SpellsAndAbilities,
        DamageSources,
        SingleDamageRecipient,
    ),
    EachDamageWouldBeDealtToRecipient(SingleDamageRecipient),
    AnAmountOfNonCombatDamageWouldBeDealtByASourceToARecipient(
        Box<Comparison>,
        DamageSources,
        DamageRecipientsList,
    ),
    AnAmountOfDamageWouldBeDealtByASourceToARecipient(
        Box<Comparison>,
        DamageSources,
        DamageRecipientsList,
    ),
    AnAmountOfDamageWouldBeDealtByASourceToRecipient(
        Box<Comparison>,
        DamageSources,
        SingleDamageRecipient,
    ),

    CombatDamageWouldBeDealt,
    CombatDamageWouldBeDealtByCreatureToRecipient(Box<Permanent>, SingleDamageRecipient),
    CombatDamageWouldBeDealtByACreature(Box<Permanents>),
    CombatDamageWouldBeDealtByACreatureToASetOfRecipients(Box<Permanents>, DamageRecipientsList),
    CombatDamageWouldBeDealtByACreatureToARecipient(Box<Permanents>, DamageRecipientsList),
    CombatDamageWouldBeDealtByACreatureToRecipient(Box<Permanents>, SingleDamageRecipient),
    CombatDamageWouldBeDealtByCreature(Box<Permanent>),
    CombatDamageWouldBeDealtByCreatureToARecipient(Box<Permanent>, DamageRecipientsList),
    CombatDamageWouldBeDealtToARecipient(DamageRecipientsList),
    CombatDamageWouldBeDealtToRecipient(SingleDamageRecipient),
    DamageWouldBeDealtByAPermanent(Box<Permanents>),
    DamageWouldBeDealtByAPermanentToARecipient(Box<Permanents>, DamageRecipientsList),
    DamageWouldBeDealtByAPermanentToRecipient(Box<Permanents>, SingleDamageRecipient),
    DamageWouldBeDealtByASource(DamageSources),
    DamageWouldBeDealtByASourceToARecipient(DamageSources, DamageRecipientsList),
    DamageWouldBeDealtByASourceToRecipient(DamageSources, SingleDamageRecipient),
    DamageWouldBeDealtByASpell(Box<Spells>),
    DamageWouldBeDealtByASpellToARecipient(Box<Spells>, DamageRecipientsList),
    DamageWouldBeDealtByASpellToRecipient(Box<Spells>, SingleDamageRecipient),
    DamageWouldBeDealtByPermanent(Box<Permanent>),
    DamageWouldBeDealtByPermanentToRecipient(Box<Permanent>, SingleDamageRecipient),
    DamageWouldBeDealtByPermanentToARecipient(Box<Permanent>, DamageRecipientsList),
    DamageWouldBeDealtBySource(SingleDamageSource),
    DamageWouldBeDealtBySourceToRecipient(SingleDamageSource, SingleDamageRecipient),
    DamageWouldBeDealtBySpell(Box<Spell>),
    DamageWouldBeDealtToARecipient(DamageRecipientsList),
    DamageWouldBeDealtToRecipient(SingleDamageRecipient),
    DamageWouldBeDealtByAPlaneToARecipient(Planes, DamageRecipientsList),
    NoncombatDamageWouldBeDealtByASourceToARecipient(DamageSources, DamageRecipientsList),
    NoncombatDamageWouldBeDealtBySpellToARecipient(Box<Spell>, DamageRecipientsList),
    NoncombatDamageWouldBeDealtToARecipient(DamageRecipientsList),
    NoncombatDamageWouldBeDealtToRecipient(SingleDamageRecipient),

    Each1DamagePlayerWouldBeDealt(Box<Player>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldScry", content = "args")]
pub enum ReplacableEventWouldScry {
    PlayerWouldScry(Box<Player>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldReduceLife", content = "args")]
pub enum ReplacableEventWouldReduceLife {
    PlayersLifeTotalWouldBeReducedToNumberOrLess(Box<Player>, Box<GameNumber>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_FutureReplacableEventWouldLoseTheGame", content = "args")]
pub enum FutureReplacableEventWouldLoseTheGame {
    NextTimePlayerWouldLoseTheGameThisTurn(Box<Player>),
}
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldLoseTheGame", content = "args")]
pub enum ReplacableEventWouldLoseTheGame {
    PlayerWouldLoseTheGame(Box<Player>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldBeginDrawStep", content = "args")]
pub enum ReplacableEventWouldBeginDrawStep {
    PlayerWouldBeginTheirDrawStep(Box<Player>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldBeginATurn", content = "args")]
pub enum ReplacableEventWouldBeginATurn {
    PlayerWouldBeginTheirTurn(Box<Player>),
    APlayerWouldBeginAnExtraTurn(Box<Players>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldSearchLibrary", content = "args")]
pub enum ReplacableEventWouldSearchLibrary {
    APlayerWouldSearchTheirLibrary(Box<Players>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_FutureReplacableEventWouldRollDice", content = "args")]
pub enum FutureReplacableEventWouldRollDice {
    NextTimePlayerWouldRollDiceThisTurn(Box<Player>),
}
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldRollDice", content = "args")]
pub enum ReplacableEventWouldRollDice {
    APlayerWouldRollANumberOfDice(Box<Players>),
    APlayerWouldRollANumberOfD6(Box<Players>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldRollPlanarDice", content = "args")]
pub enum ReplacableEventWouldRollPlanarDice {
    APlayerWouldRollANumberOfPlanarDice(Box<Players>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldPutCounters", content = "args")]
pub enum ReplacableEventWouldPutCounters {
    APlayerWouldGetAnyNumberOfPoisonCounters(Box<Players>),
    APlayerWouldGetAnyNumberOfCounters(Box<Players>),
    APlayerWouldPutAnyNumberOfCountersOfTypeOnAPermanent(
        Box<Players>,
        CounterType,
        Box<Permanents>,
    ),
    APlayerWouldPutCountersOnAPermanentOrAPlayer(Box<Players>, Box<Permanents>, Box<Players>),
    AnAbilityWouldPutCountersOfTypeOnAPermanent(Abilities, CounterType, Box<Permanents>),
    AnEffectWouldPutCountersOnAPermanent(Box<Permanents>),
    CountersOfTypeWouldBePointOnAPermanent(CounterType, Box<Permanents>),
    CountersWouldBePutOnAPermanent(Box<Permanents>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldUntap", content = "args")]
pub enum ReplacableEventWouldUntap {
    APermanentWouldUntapDuringsItsControllersUntapStep(Box<Permanents>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_FutureReplacableEventWouldDestroy", content = "args")]
pub enum FutureReplacableEventWouldDestroy {
    NextTimePermanentWouldBeDestroyedThisTurn(Box<Permanent>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldDestroy", content = "args")]
pub enum ReplacableEventWouldDestroy {
    PermanentWouldBeDestroyed(Box<Permanent>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldDiscard", content = "args")]
pub enum ReplacableEventWouldDiscard {
    ASpellOrAbilityWouldCausePlayerToDiscardCard(SpellsAndAbilities, Box<Player>, CardInHand),
    ASpellOrAbilityWouldCausePlayerToDiscardACard(SpellsAndAbilities, Box<Player>),
    AnEffectWouldCausePlayerToDiscardACard(Box<Player>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldCounterASpell", content = "args")]
pub enum ReplacableEventWouldCounterASpell {
    ASpellOrAbilityWouldCounterASpell(SpellsAndAbilities, Box<Spells>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldFlipACoin", content = "args")]
pub enum ReplacableEventWouldFlipACoin {
    PlayerWouldFlipACoin(Box<Player>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(
    tag = "_ReplacableEventWouldPutAPermanentOnTheBattlefield",
    content = "args"
)]
pub enum ReplacableEventWouldPutAPermanentOnTheBattlefield {
    APlayerWouldPutAPermanentOnTheBattlefield(Box<Players>, Box<Permanents>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldCopyASpell", content = "args")]
pub enum ReplacableEventWouldCopyASpell {
    APlayerWouldCopyASpellAnyNumberOfTimes(Box<Players>, Box<Spells>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldProliferate", content = "args")]
pub enum ReplacableEventWouldProliferate {
    APlayerWouldProliferate(Box<Players>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldLearn", content = "args")]
pub enum ReplacableEventWouldLearn {
    APlayerWouldLearn(Box<Players>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacableEventWouldMask", content = "args")]
pub enum ReplacableEventWouldMask {
    PermanentWouldAssignDamageDealDamageBeDealDamageOrBecomeTapped(Box<Permanent>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(
    tag = "_FutureReplacableEventWouldSetASchemeInMotion",
    content = "args"
)]
pub enum FutureReplacableEventWouldSetASchemeInMotion {
    NextTimePlayerWouldSetASchemeInMotion(Box<Player>),
}
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ReplacementActionWouldSetASchemeInMotion", content = "args")]
pub enum ReplacementActionWouldSetASchemeInMotion {
    WouldSetASchemeInMotion_SetNumberSchemesInMotionInstead(Box<GameNumber>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(
    tag = "_ReplacableEventTokensWouldBeCreatedUnderAPlayersControl",
    content = "args"
)]
pub enum ReplacableEventTokensWouldBeCreatedUnderAPlayersControl {
    TokensWouldBeCreatedUnderAPlayersControl(Box<Players>),
}
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(
    tag = "_ReplacementActionTokensWouldBeCreatedUnderAPlayersControl",
    content = "args"
)]
pub enum ReplacementActionTokensWouldBeCreatedUnderAPlayersControl {
    CreateTokensUnderPlayersControlInstead(Vec<CreatableToken>, Box<Player>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_PregameAction", content = "args")]
pub enum PregameAction {
    MayActions(Vec<PregameAction>),
    ExileNumberDraftedCardsNotInDeck(Box<GameNumber>, Box<Cards>),
    RevealCardFromDeck,
    ExileADraftedCardNotInDeck,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_AttackingPlayers", content = "args")]
pub enum AttackingPlayers {
    AttackedPlayerOrPlaneswalkerTheyControl(Box<Player>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Condition", content = "args")]
pub enum Condition {
    // Steps and Phases End
    IsDuringUpkeep,
    IsAfterUpkeep,
    IsDuringDrawStep,
    IsDuringAMainPhase,
    IsBeforeCombat,
    IsBeforeFirstCombatPhaseOfTurn,
    IsFirstCombatPhaseOfTurn,
    IsDuringCombat,
    IsBeforeAttackersDeclared,
    IsDuringDeclareAttackersStep,
    IsBeforeBlockersDeclared,
    IsDuringDeclareBlockersStep,
    IsAfterBlockersAreDeclared,
    IsBeforeCombatDamageStep,
    IsBeforeEndOfCombatStep,
    IsDuringEndOfCombatStep,
    IsAfterCombat,
    IsBeforeEndStep,
    IsFirstEndStepOfTheTurn,
    IsDuringEndStep,

    // Operators
    Or(Vec<Condition>),
    And(Vec<Condition>),

    ANumberOfCardsWerePutIntoExileThisTurn(Box<Comparison>, Box<Cards>),
    TriggerXIs(Box<Comparison>),

    ACardWasntDrawnThisWay,
    ANumberDiceValuesAre(Box<Comparison>, Box<Comparison>),
    APermanentAttackedThisCombat(Box<Permanents>),
    TheNumberOfCountersOfTypePutOnPermanentsThisWayIs(
        CounterType,
        Box<Permanents>,
        Box<Comparison>,
    ),
    ThereAreNumberKindsOfCounterTypesAmongPermanents(Box<Comparison>, Box<Permanents>),
    TotalToughnessOfPermanentsIs(Box<Comparison>, Box<Permanents>),
    ACounterWasPutOnAPermanentThisTurn(Box<Permanents>),
    ASpellWasWarpedThisTurn,
    ASpellWasntCastThisWay,
    IsNotPlayersTurn(Box<Player>),
    MayhemCostWasPaid,

    ANumberOfCountersOfTypeWereMovedThisWay(Box<Comparison>, CounterType),
    TopCardOfPlayersLibraryPassesFilter_Digital(Box<Player>, Box<Cards>),
    ANumberOfTokensWereCreatedThisWay(Box<Comparison>),
    APlayerTurnedAPermanentFaceUpThisTurn(Box<Players>, Box<Permanents>),
    NoCardsWerePutIntoHandThisWay,
    ThereAreNumberUnlockedDoorsAmongPermanents(Box<Comparison>, Box<Permanents>),
    ThereAreNumberNamesAmongUnlockedDoorsOfPermanents(Box<Comparison>, Box<Permanents>),
    ANumberOfGraveyardCardsLeftThisTurn(Box<Comparison>, CardsInGraveyard),
    APermanentWasPutIntoHandThisWay(Box<Permanents>),
    ATokenWasCreatedThisWay,
    AnAmountOfEnergyWasPaidThisWay(Box<Comparison>),
    FreerunningCostWasPaid,
    PlayerDidntDrawACardThisWay,
    ThereAreNumberCardTypesAmongExiled(Box<Comparison>, CardsInExile),
    TheGiftWasPromised,
    TheGiftWasntPromised,
    ValueIs(Box<GameNumber>, Box<Comparison>),
    AllVotesWereForWord(VoteOption),
    ANumberOfCardsWereExildThisWay(Box<Comparison>, Box<Cards>),
    ANumberOfPlayersHaveLostTheGame(Box<Comparison>),
    TheChosenNumbersMatch,
    ANumberOfCardsWerePutIntoPlayersGraveyardFromAnywhereOtherThanTheBattlefieldThisTurn(
        Box<Comparison>,
        Box<Cards>,
        Box<Players>,
    ),
    APermanentWasChosenThisWay,
    NoVotesWereCastThisWay,
    EnergyWasPaidThisWay,
    ANumberOfCreaturesAttackedThisTurn(Box<Comparison>, Box<Permanents>),
    ANumberOfSourcesDealtDamageThisTurn(Box<Comparison>, DamageSources),
    ExcessDamageWasDealtToAPermanentThisWay(Box<Permanents>),
    EvidenceWasCollectedThisWay,
    APermanentOfTypeWasPutOntoTheBattlefieldThisWay(Box<Permanents>),
    ASpellWasCastThisWay(Box<Spells>),
    IsDuringUntapStep,
    ThereAreANumberOfPermanentTypesAmongCardsInPlayersGraveyards(Box<Comparison>, Box<Players>),
    ThisCardIsInExileOrOnTheBattlefield,
    AnAmountOfExcessDamageWasDealtThisWay(Box<Comparison>),
    IsTheNthTurnOfTheGame(Box<Comparison>),
    ACreatureWasExploitedThisWay(Box<Permanents>),
    APermanentExploredThisTurn(Box<Permanents>),
    ACardLeftPlayersGraveyardThisTurn(Box<Cards>, Box<Player>),
    SourcesDealtNonCombatDamageThisTurn(DamageSources, Box<Comparison>),
    ACardOfTypeWasExiledThisTurn(Box<CardsInExile>),
    ACardWasChosenThisWay,
    ACardWasDiscardedThisWay(Box<Cards>),
    ACardWasExiledThisWay(Box<Cards>),
    ACardWasFoundThisWay,
    ACardOfTypeWasFoundThisWay(Box<Cards>),
    ACardWasMilledThisWay(Box<Cards>),
    ACardWasPutIntoGraveyardThisWay(Box<Cards>),
    ACardWasPutIntoHandThisWay(Box<Cards>),
    ACardWasPutIntoPlayersGraveyardFromAnywhereThisTurn(Box<Cards>, Box<Player>),
    ACardWasRevealedByPlayerThisWay(Box<Cards>, Box<Player>),
    ACardWasRevealedThisWay(Box<Cards>),
    AColorIsTheMostCommonColorAmongPermanentsButNotTiedForTheMostComon(Color, Box<Permanents>),
    AColorIsTheMostCommonOrTiedForMostCommonColorAmongPermanents(Color, Box<Permanents>),
    ACombatPermanentPassesFilter(Box<Permanent>, Box<Permanents>),
    ACounterOfTypeWasPutOnAPermanentThisTurn(CounterType, Box<Permanents>),
    ACounterOfTypeWasRemovedFromAPermanentThisTurn(CounterType, Box<Permanents>),
    ACreatureOrPlaneswalkerDiedThisTurn(Box<Permanents>),
    ACreatureOrPlaneswalkerWasDealtDamageThisWay(Box<Permanents>),
    ADiceResultIs(Box<Comparison>),
    AGraveyardCardWasReturnedToHandThisWay(Box<Cards>),
    ALibraryCardWasPutIntoHandThisWay,
    ANumberOfCardsWereDiscardedThisWay(Box<Comparison>),
    ANumberOfCardsWerePutIntoPlayersGraveyardFromAnywhereThisTurn(
        Box<Comparison>,
        Box<Cards>,
        Box<Players>,
    ),
    ANumberOfCardsWereRevealedThisWay(Box<Comparison>, Box<Cards>),
    ANumberOfGraveyardCardsWereReturnedToHandThisWay(Box<Comparison>),
    ANumberOfGroupCardsWereExiledThisWay(Box<Comparison>, Box<Cards>, GroupFilter),
    ANumberOfPermanentsDiedThisTurn(Box<Comparison>, Box<Permanents>),
    APermanentDiesThisWay(Box<Permanents>),
    APermanentEnteredTheBattlefieldThisWay,
    APermanentEnteredTheBattlefieldUnderAPlayersControlThisTurn(Box<Permanents>, Box<Players>),
    APermanentEnteredTheBattlefieldUnderPlayersControlThisTurn(Box<Permanents>, Box<Player>),
    APermanentLeftTheBattlefieldThisTurn(Box<Permanents>),
    APermanentPassesFilter(Box<Permanents>, Box<Permanents>),
    APermanentWasCopiedThisWay(Box<Permanents>),
    APermanentWasDestroyedByASpellOrAbilityThisTurn(Box<Permanents>, SpellsAndAbilities),
    APermanentWasDestroyedThisWay(Box<Permanents>),
    APermanentWasExiledThisWay(Box<Permanents>),
    APermanentWasPutIntoAPlayersGraveyardThisTurn(Box<Permanents>, Box<Players>),
    APermanentWasPutIntoPlayersGraveyardThisTurn(Box<Permanents>, Box<Player>),
    APermanentWasPutOntoTheBattlefieldByPlayerThisWay(Box<Permanents>, Box<Player>),
    APermanentWasPutOntoTheBattlefieldThisWay(Box<Permanents>),
    APermanentWasReturnedToPlayersHandThisTurn(Box<Permanents>, Box<Player>),
    APermanentWasSacrificedThisWay(Box<Permanents>),
    APermanentWasntSacrificedThisWay(Box<Permanents>),
    APermanentsAbilityIsCounteredThisWay(Box<Permanents>),
    APlayerPassesFilter(Box<Players>, Box<Players>),
    APlayerWasDealtDamageThisWay(Box<Players>),
    ASourceDealtDamageThisTurn(DamageSources, Box<Comparison>),
    ASpellWasCountedByASpellOrAbilityThisTurn(Box<Spells>, SpellsAndAbilities),
    ActivatedAbilityPassesFilter(ActivatedAbility, Box<ActivatedAbilities>),
    AllCardsRevealedThisWayAreCardsOfType(Box<Cards>),
    AllCoinsCameUpHeads,
    AllPermanentsPassFilter(Box<Permanents>, Box<Permanents>),
    AllPlayersPassFilter(Box<Players>, Box<Players>),
    AnExiledCardPassesFilter(CardsInExile, CardsInExile),
    AnyCardInAnyPlayersGraveyardPassesFilter(Box<Cards>, Box<Players>),
    AttackingCreaturesPassFilter(Box<Comparison>, Box<Permanents>),
    AttackingPlayerPassesFilter(AttackingPlayers),
    CardIsExiled(Box<CardInExile>),
    CardIsInPlayersGraveyard(CardInGraveyard, Box<Player>),
    CardIsInPlayersGraveyardWithACardAboveIt(CardInGraveyard, Box<Player>, Box<Cards>),
    CardIsInPlayersGraveyardWithANumberOfCardsAboveIt(
        CardInGraveyard,
        Box<Player>,
        Box<Comparison>,
        Box<Cards>,
    ),
    CardIsOnlyCardInPlayersGraveyard(CardInGraveyard, Box<Cards>, Box<Player>),
    CastByAPlayer(Box<Players>),
    CastSpellOrActivatedAbilityPassesFilter(SpellsAndAbilities),
    CastSpellPassesFilter(Box<Spells>),
    CombatDamageWasDealtByACreature(Box<Permanents>),
    CommandPermanentPassesFilter(Box<Permanent>, Box<Permanents>),
    CopiedCardPassesFilter(Box<Cards>),
    CostWasPaid,
    CostWasntPaid,
    DamageFromAPermanentSourceWasPreventedThisWay(Box<Permanents>),
    DamageFromASourceWasPreventedThisWay(DamageSources),
    DamageWasPreventedThisWay,
    DeadCardPassesFilter(Box<Cards>),
    DeadPermanentPassesFilter(Box<Permanents>),
    DestroyedPermanentIsPutInAGraveyardThisWay,
    DiceResultIs(Box<Comparison>),
    DifferenceIs(Box<GameNumber>, Box<GameNumber>, Box<Comparison>),
    DiscardedCardPassesFilter(Box<Cards>),
    EnteringPermanentPassesFilter(Box<Permanents>),
    ExcessDamageWasDealtThisWay,
    ExcessDamageWasDealtToACreatureOrPlaneswalkerThisTurn(Box<Permanents>),
    ExiledCardPassesFilter(CardInExile, CardsInExile),
    ExiledPermanentWasUnearthed,
    GraveyardCardHasNumCounters(CardInGraveyard, Box<Comparison>, CounterType),
    GraveyardCardPassesFilter(CardInGraveyard, Box<Cards>),
    GuestWasUnattachedFromAPermanentThisWay(Box<Permanents>),
    IsAPlayersTurn(Box<Players>),
    IsAnExtraTurn,
    IsNotTheFirstTurnOfTheGame,
    IsPlayersNthTurn(Box<Player>, Box<Comparison>),
    IsPlayersTurn(Box<Player>),
    ItWasAnArtSticker,
    ItsNeitherDayOrNight,
    ItsNight,
    ItsTheFirstTimeCountersOfTypeWerePutOnThatPermanentThisTurn,
    LeavingPermanentPassesFilter(Box<Permanents>),
    ManaFromAPermanentWasSpentToActivateThisAbility(Box<Permanents>),
    MostVotesForWordIs(VoteOption),
    MostVotesOrTiedForMostVotesForWordIs(VoteOption),
    NoCardsOfTypeWereRevealedThisWay(Box<Cards>),
    NoCardsWereRevealedThisWay,
    NoLifeWasPaidThisWay,
    NoOneTookAnActionThisWay,
    NoPermanentsLeftTheBattlefieldThisTurn(Box<Permanents>),
    NoPermanentsPassFilter(Box<Permanents>, Box<Permanents>),
    NoPlayersPassFilter(Box<Players>, Box<Players>),
    NumCardsDiscardedThisWayPassGroupFilter(Box<Comparison>, Box<Cards>, GroupFilter),
    NumCardsFromHandRevealedThisWayPassGroupFilter(Box<Comparison>, Box<Cards>, GroupFilter),
    NumCardsHaveBeenMilledIntoGraveyardThisWay(Box<Comparison>, Box<Cards>),
    NumCardsInExileIs(Box<Comparison>, CardsInExile),
    NumCoinFlipsLostIs(Box<Comparison>),
    NumCoinFlipsWonIs(Box<Comparison>),
    NumCombatPermanentsPassFilter(Box<Comparison>, Box<Permanents>, Box<Permanents>),
    NumDifferentManaValueAmongCardsInPlayersGraveyardIs(Box<Comparison>, Box<Cards>, Box<Player>),
    NumDifferentManaValuesAmongCardsInExileIs(Box<Comparison>, CardsInExile),
    NumGraveyardCardsPassFilter(Box<Comparison>, Box<Cards>, Box<Players>),
    NumGroupCardsWereMilledThisWay(Box<Comparison>, Box<Cards>, GroupFilter),
    NumPermanentsIs(Box<Comparison>, Box<Permanents>),
    NumPermanentsPassFilter(Box<Comparison>, Box<Permanents>, Box<Permanents>),
    NumPlayersPassFilter(Box<Comparison>, Box<Players>, Box<Players>),
    NumSpellsCastLastTurnIs(Box<Comparison>, Box<Spells>),
    NumberDiceAreEqual(Box<Comparison>),
    NumberOfCardTypesAmongThePermanentsSacrificedThisWayIs(Box<Comparison>),
    NumberOfColorsOfManaSpentToActivateThisAbilityIs(Box<Comparison>),
    NumberOfCountersRemovedThisWayIs(Box<Comparison>),
    NumberPermanentsEnteredTheBattlefieldThisWay(Box<Comparison>, Box<Permanents>),
    NumberPermanentsEnteredTheBattlefieldUnderPlayersControlThisTurn(
        Box<Comparison>,
        Box<Permanents>,
        Box<Player>,
    ),
    PermanentDealtDamageToACreatureOrPlaneswalkerThisWay(Box<Permanent>, Box<Permanents>),
    PermanentDealtDamageToPlayerThisWay(Box<Permanent>, Box<Player>),
    PermanentDiesThisWay,
    PermanentPassesFilter(Box<Permanent>, Box<Permanents>),
    PermanentPutInGraveyardPassesFilter(PermanentsAndGraveyardCards),
    PermanentRegeneratedThisWay,
    PermanentTransformedThisWay(Box<Permanent>),
    PermanentWasDestroyedThisWay,
    PermanentsChangedControlThisWay,
    PermanentsPassGroupFilter(Box<Permanents>, GroupFilter),
    PlayerControlledAPermanentAsCast(Box<Player>, Box<Permanents>),
    PlayerGuessedWrong,
    PlayerIsPlayer(Box<Player>, Box<Player>),
    PlayerPassesFilter(Box<Player>, Box<Players>),
    PlayerRevealedACardAsCast(Box<Player>, Box<Cards>),
    PlayersRevealTopCardOfLibraryAndFindHighestManaValue_HasASingleWinner,
    RevealedCardsWerePutInHand,
    SpellOrAbilityPassesFilter(SpellOrAbility, SpellsAndAbilities),
    SpellPassesFilter(Box<Spell>, Box<Spells>),
    SpellXIs(Box<Comparison>),
    TheCardExiledThisWayIsStillExiled,
    TheCardInHandPassesFilter(CardInHand, Box<Cards>),
    TheCardReturnedToHandThisWayPassesFilter(Box<Cards>),
    TheChosenGraveyardCardPassesFilter(Box<Cards>),
    TheChosenWordWas(VoteOption),
    TheLastCardExiledThisWayWasACard(Box<Cards>),
    TheNumberOfCardsOfTypeInPlayersLibraryIs(Box<Cards>, Box<Player>, Box<Comparison>),
    TheNumberOfCountersOfTypeAmongPermanentsIs(CounterType, Box<Permanents>, Box<Comparison>),
    TheNumberOfPermanentsReturnedToHandThisWayIs(Box<Comparison>, Box<Permanents>),
    TheTotalManaValueOfExiledCardsIs(CardsInExile, Box<Comparison>),
    ThereAreANumberOfBasicLandTypesAmongPermanents(Box<Comparison>, Box<Permanents>),
    ThereAreANumberOfCountersAmongPermanents(Box<Comparison>, Box<Permanents>),
    ThereAreNumberCardTypesInPlayersGraveyard(Box<Comparison>, Box<Player>),
    ThisCardIsInTheCommandZone,
    ThisCardIsInTheCommandZoneOrOnTheBattlefield,
    ThisCardIsInYourGraveyard,
    ThisCardIsOnTheBattlefield,
    TimesThisAbilityHasResolvedThisTurnIs(Box<Comparison>),
    TopCardOfPlayersLibraryPassesFilter(Box<Player>, Box<Cards>),
    TotalPowerOfPermanentsIs(Box<Comparison>, Box<Permanents>),
    TriggerChoseCreatureAsRingBearer(Box<Permanents>),
    TriggerDiceResultIs(Box<Comparison>),
    Trigger_WonTheClash,
    WhenAPermanentBecomesTapped_NotTappedForAttacking,
    WhenAPlayerRollsAnyNumberOfDice_AnyDiceResultIs(Box<Comparison>),
    WordWasVotedFor(VoteOption),
}
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_CardInHand", content = "args")]
pub enum CardInHand {
    TheCardChosenThisWay,
    TheCardConjuredIntoHandThisWay,
    TheCardConjuredThisWay,
    TheCardDraftedThisWay,
    TheCardInHandChosenThisWay,
    TheCardInHandRevealedThisWay,
    TheCardPutInHandThisWay,
    TheCardReturnedToHandThisWay,
    TheCardRevealedFromHandThisWay,
    TheCardRevealedThisWay,
    TheCardSeekedThisWay,
    TheChosenCardInHand,
    TheLastCardDrawnThisTurn,
    ThisCardInHand,
    Trigger_ThatCardInHand,
    Trigger_ThatDiscardedCard,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_CostReductionSymbol", content = "args")]
pub enum CostReductionSymbol {
    CostReduceW,
    CostReduceU,
    CostReduceB,
    CostReduceR,
    CostReduceG,
    CostReduceGeneric(i32),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_CostReductionSymbolX", content = "args")]
pub enum CostReductionSymbolX {
    CostReduceX,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_SearchLibraryCost", content = "args")]
pub enum SearchLibraryCost {
    FindACardOfType(Box<Cards>),
    PlayCardWithoutPaying,
    RevealACardOfType(Box<Cards>),
    PutACardOfTypeIntoGraveyard(Box<Cards>),
    PayManaX(ManaCostX, Box<GameNumber>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_SearchLibraryActionValueAction", content = "args")]
pub enum SearchLibraryActionValueAction {
    ValueAction(Range, Vec<SearchLibraryAction>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_SearchLibraryAction", content = "args")]
pub enum SearchLibraryAction {
    RollAD20,
    ValueActions(Vec<SearchLibraryActionValueAction>),

    ChooseAnAction(Vec<SearchLibraryAction>),
    FindNumberGroupCards(Box<GameNumber>, Box<Cards>, GroupFilter),
    FindUptoNumberCardsOfType(Box<GameNumber>, Box<Cards>),

    PutFoundCardsIntoHand,

    PutTheCardsFoundThisWayIntoHand,
    PutACardFoundThisWayOntoTheBattlefield(Vec<ReplacementActionWouldEnter>),
    PutFoundCardsIntoGraveyard,

    APlayerChooseACardExiledThisWay(Box<Players>),
    APlayerChoosesARevealedCard(Box<Players>),
    APlayerChoosesNumberCardsRevealedThisWay(Box<Players>, Box<GameNumber>),
    APlayerChoosesNumberRevealedCards(Box<Players>, Box<GameNumber>),
    ChooseARevealedCardAtRandom,
    CreatePermanentLayerEffectUntil(Box<Permanent>, Vec<LayerEffect>, Expiration),
    CreateTokens(Vec<CreatableToken>),
    DiscardACardAtRandom,
    DontShuffle,
    EachPlayerLosesLife(Box<Players>, Box<GameNumber>),
    ExileAGenericCard,
    ExileAGenericCardFaceDown,
    ExileAllButNumberGenericCards(Box<GameNumber>),
    ExileAllCards(Box<Cards>),
    ExileAnyNumberOfCards(Box<Cards>),
    ExileNumberGenericCards(Box<GameNumber>),
    ExileNumberGenericCardsInShuffledFaceDownPile(Box<GameNumber>),
    ExileTheCardsFoundThisWay,
    ExileUptoNumberCards(Box<GameNumber>, Box<Cards>),
    ExileUptoNumberGenericCards(Box<GameNumber>),
    FindACardOfType(Box<Cards>),
    FindACardOfTypeAtRandom(Box<Cards>),
    FindAGenericCard,
    FindAndRevealUptoNumberGroupCards(Box<GameNumber>, Box<Cards>, GroupFilter),
    FindAnyNumberOfCardsOfType(Box<Cards>),
    FindCardsOfType(Vec<Cards>),
    FindNumberCardsOfType(Box<GameNumber>, Box<Cards>),
    FindNumberGenericCards(Box<GameNumber>),
    FindUptoNumberGroupCards(Box<GameNumber>, Box<Cards>, GroupFilter),
    GainLife(Box<GameNumber>),
    If(Condition, Vec<SearchLibraryAction>),
    IfElse(
        Condition,
        Vec<SearchLibraryAction>,
        Vec<SearchLibraryAction>,
    ),
    MayCastACardOfTypeWithoutPaying(Box<Cards>),
    MayCost(SearchLibraryCost),
    MayExileACardOfType(Box<Cards>),
    MayExileAnyNumberOfCardsOfType(Box<Cards>),
    MayExileUptoNumberGroupCards(Box<GameNumber>, Box<Cards>, GroupFilter),
    MayExileUptoNumberOfCardsOfType(Box<GameNumber>, Box<Cards>),
    MayPutACardOfTypeIntoGraveyard(Box<Cards>),
    MayPutACardOfTypeOntoTheBattlefieldOrAGenericCardIntoHand(
        Box<Cards>,
        Vec<ReplacementActionWouldEnter>,
    ),
    MayPutACardOntoTheBattlefield(Box<Cards>, Vec<ReplacementActionWouldEnter>),
    MayPutAnyNumberOfCardsOfTypeIntoGraveyard(Box<Cards>),
    MayPutAnyNumberOfCardsOntoTheBattlefield(Box<Cards>, Vec<ReplacementActionWouldEnter>),
    MayPutAnyNumberOfGroupCardsOntoBattlefield(
        Box<Cards>,
        GroupFilter,
        Vec<ReplacementActionWouldEnter>,
    ),
    MayPutAnyNumberOfGroupCardsOntoTheBattlefield(
        Box<Cards>,
        GroupFilter,
        Vec<ReplacementActionWouldEnter>,
    ),
    MayPutMultipleCardsOfTypeOntoTheBattlefield(Vec<Cards>, Vec<ReplacementActionWouldEnter>),
    MayPutUptoNumberCardsOntoTheBattlefield(
        Box<GameNumber>,
        Box<Cards>,
        Vec<ReplacementActionWouldEnter>,
    ),
    MayPutUptoNumberGenericCardsIntoHand(Box<GameNumber>),
    MayPutUptoNumberGroupCardsOntoBattlefield(
        Box<GameNumber>,
        Box<Cards>,
        GroupFilter,
        Vec<ReplacementActionWouldEnter>,
    ),
    MayPutUptoNumberOfCardsOfTypeIntoGraveyard(Box<GameNumber>, Box<Cards>),
    MayRevealACardOfType(Box<Cards>),
    MayRevealACardOfTypeAndPutItIntoHand(Box<Cards>),
    MayRevealAnyNumberOfCardsOfTypeAndPutThemIntoHand(Box<Cards>),
    MayRevealMultipleCardsOfTypeAndPutIntoHand(Vec<Cards>),
    MayRevealNumberGroupCards(Box<GameNumber>, Box<Cards>, GroupFilter),
    MayRevealUptoNumberCardsOfTypeAndPutIntoHand(Box<GameNumber>, Box<Cards>),
    MayRevealUptoNumberCardsOfTypeAndPutThemIntoHand(Box<GameNumber>, Box<Cards>),
    MayRevealUptoNumberGroupCardsAndPutIntoHand(Box<GameNumber>, Box<Cards>, GroupFilter),
    MaySetAsideAndRevealACardOfTypeToPutOnTopOfLibrary(Box<Cards>),
    PlayerChoosesACard(Box<Player>),
    PlayerChoosesACardName(Box<Player>, Box<Cards>),
    PlayerChoosesNumberCardsRevealedThisWay(Box<Player>, Box<GameNumber>),
    PutACardOfTypeRevealedThisWayOntoBattlefield(Box<Cards>, Vec<ReplacementActionWouldEnter>),
    PutACardRevealedThisWayIntoHand,
    PutACardRevealedThisWayOntoBattlefield(Vec<ReplacementActionWouldEnter>),
    PutAGenericCardIntoGraveyard,
    PutAGenericCardIntoHand,
    PutChosenCardIntoHand,
    PutChosenCardsIntoGraveyard,
    PutExiledCardOntoBattlefield(CardInExile, Vec<ReplacementActionWouldEnter>),
    PutFoundCardIntoHand,
    PutFoundCardOntoBattlefield(Vec<ReplacementActionWouldEnter>),
    PutFoundCardOntoBattlefieldOrIntoHand(Vec<ReplacementActionWouldEnter>),
    PutFoundCardsOntoBattlefield(Vec<ReplacementActionWouldEnter>),
    PutInLibraryNthFromTheTop(Box<GameNumber>),
    PutNumberCardsRevealedThisWayOntoBattlefield(Box<GameNumber>, Vec<ReplacementActionWouldEnter>),
    PutNumberGenericCardsIntoHand(Box<GameNumber>),
    PutOnBottomOfLibrary,
    PutOnTopOfLibrary,
    PutOnTopOfLibraryInAnyOrder,
    PutRemainingCardsInHand,
    PutRemainingCardsOntoBattlefield(Vec<ReplacementActionWouldEnter>),
    PutRevealedCardsOnTopOfLibraryInAnyOrder,
    PutSetAsideCardIntoHand,
    PutSetAsideCardOnTopOfLibrary,
    PutTheCardFoundThisWayOnTheBattlefield(Vec<ReplacementActionWouldEnter>),
    PutTheChosenCardIntoHand,
    PutTheRemainingCardsBackIntoLibraryAndShuffle,
    PutTheRemainingCardsIntoGraveyard,
    PutTheRemainingCardsIntoHand,
    PutTheRemainingCardsOnTopOfLibraryInAnyOrder,
    PutUptoNumberCardsIntoGraveyard(Box<GameNumber>, Box<Cards>),
    RevealAGenericCardAndPutItIntoHand,
    RevealARandomCardOfTypeAndPutItIntoHand(Box<Cards>),
    RevealFoundCard,
    RevealFoundCardAndSetAside,
    RevealFoundCards,
    RevealNumberGenericCards(Box<GameNumber>),
    RevealUptoNumberCardsOfType(Box<GameNumber>, Box<Cards>),
    RevealUptoNumberGroupCards(Box<GameNumber>, Box<Cards>, GroupFilter),
    SetAsideAGenericCard,
    SetAsideAndRevealAnyNumberOfCardsOfType(Box<Cards>),
    SetAsideNumberGenericCards(Box<GameNumber>),
    SetAsideRevealedCards,
    Shuffle,
    ShuffleAndPutFoundCardOnTop,
    ShuffleAndPutRevealedCardOnTop,
    ShuffleChosenCardsIntoLibrary,
    ShuffleExiledCardIntoLibrary(Box<CardInExile>),
    ShuffleLibraryIfSearched,
    Unless(Condition, Vec<SearchLibraryAction>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Cost", content = "args")]
pub enum Cost {
    And(Vec<Cost>),
    Or(Vec<Cost>),

    BeholdA(Box<CardsInHand>),
    BeholdAndExile(Box<CardsInHand>),
    BeholdNumber(Box<GameNumber>, Box<CardsInHand>),
    BlightX,
    Blight(Box<GameNumber>),

    PutGraveyardCardOnBottomOfLibrary(Box<CardInGraveyard>),

    AttachAPermanentToPermanent(Box<Permanents>, Box<Permanent>),
    AttachPermanentToPermanent(Box<Permanent>, Box<Permanent>),
    // BeholdADragonCreature,
    Earthbend(Box<Permanent>, Box<GameNumber>),
    WaterbendCustomX(ManaCostX, Box<GameNumber>),
    WaterbendX(ManaCostX),
    Waterbend(ManaCost),
    PutGraveyardCardOnTopOfLibrary(Box<CardInGraveyard>),
    RemoveACounterOfTypeFromEachOfNumberPermanents(
        Box<CounterType>,
        Box<GameNumber>,
        Box<Permanents>,
    ),
    RemoveNumberCountersFromAPermanent(Box<GameNumber>, Box<Permanents>),
    RemoveNumberCountersFromPermanent(Box<GameNumber>, Box<Permanent>),

    AnteAPermanent(Box<Permanents>),
    // BeholdADragon,
    CopySpellAndMayChooseNewTargets(Box<Spell>),
    CreateTokensWithFlags(Vec<CreatableToken>, Vec<TokenFlag>),
    ItsManaCostReducedBy(CostReduction),
    PutACardOfTypeMilledThisWayIntoHand(Box<Cards>),
    SacrificeOneOrMorePermanents(Box<Permanents>),
    Surveil(Box<GameNumber>),

    CastCopiedCard,
    ConjureDuplicateOfPermanentIntoHand(Box<Permanent>),
    CreateTokens(Vec<CreatableToken>),
    Forage,
    ItsManaCost,
    PayAnyAmountOfEnergy,
    PayOneOrMoreEnergy,
    PayAnyAmountOfLife,
    SacrificeNumberGroupPermanents(Box<GameNumber>, Box<Permanents>, GroupFilter),
    Exile(Vec<Exilable>),
    ExileNumberGraveyardCards(Box<GameNumber>, CardsInGraveyard),
    ExileAnyNumberOfGroupCardsFromPlayersGraveyard(CardsInGraveyard, Box<Player>, GroupFilter),
    PayManaCostOfPermanentReducedBy(Box<Permanent>, CostReduction),
    AbandonScheme(SingleScheme),
    UntapEachPermanent(Box<Permanents>),
    CollectEvidenceAnyX,
    SuspectAPermanent(Box<Permanents>),
    ChooseAnyNumberPermanentsAndPayManaForEach(Box<Permanents>, ManaCost),
    Amass(Box<GameNumber>, CreatureType),
    TurnPermanentFaceUp(Box<Permanent>),
    GetEnergy(Box<GameNumber>),
    RevealTheSecretlyChosenCreatureType,
    PutExiledCardOnTheBottomOfItsOwnersLibrary(Box<CardInExile>),
    LookAtPlayersHandAndChooseACardToExileUntil(Box<Player>, CardsInHand, Expiration),
    ExileACardOfTypeFromHandWithANumberOfCountersOfType(Box<Cards>, Box<GameNumber>, CounterType),
    PayItsSuspendCost,
    PutACounterOnExiledCard(CounterType, CardInExile),
    ExileGraveyardCardWithNumberCountersOfType(CardInGraveyard, Box<GameNumber>, CounterType),
    LookAtPlayersHandAndChooseACardToExile(Box<Player>, Box<CardsInHand>),
    AddMana(ManaProduce),
    AnteTopCardOfLibrary,
    AttachAPermanentToAPlayer(Box<Permanents>, Box<Players>),
    AttachPermanentToAPermanent(Box<Permanent>, Box<Permanents>),
    BeginGameWithCardOnBattlefield(PregameCard, Vec<ReplacementActionWouldEnter>),
    CastASpellFromHandWithoutPaying(Box<Spells>),
    CastASpellFromPlayersGraveyardWithoutPaying(Box<Spells>, Box<Player>),
    CastCopiedCardWithoutPaying,
    CastGraveyardCard(CardInGraveyard),
    CastGraveyardCardWithAdditionalCostIntoExile(CardInGraveyard, Box<Cost>),
    CastGraveyardCardWithoutPaying(CardInGraveyard),
    CastSpellFromExile(Box<Spells>, CardInExile),
    CastSpellFromExileWithoutPaying(Box<Spells>, CardInExile),
    CastTopCardOfLibraryWithoutPaying,
    CastTopSpellOfLibraryWithoutPaying(Box<Spells>),
    ChooseACardFromPlayersRevealedHand(Box<CardsInHand>, Box<Player>),
    ChooseACardInHand(Box<CardsInHand>),
    ChooseACardInPlayersGraveyard(CardsInGraveyard, Box<Player>),
    ChooseACardtype,
    ChooseACheckableAbility(Vec<CheckHasable>),
    ChooseAColor(ChoosableColor),
    ChooseACreatureType,
    ChooseANumberBetween(i32, i32),
    ChooseAPermanent(Box<Permanents>),
    ChooseAPlayer(Box<Players>),
    ChooseAPlayerAtRandom(Box<Players>),
    ChooseAnExiledCard(Box<CardsInExile>),
    ChooseColors,
    ConvertPermanent(Box<Permanent>),
    CopyAnExiledCard(Box<CardsInExile>),
    CopyExiledCard(Box<CardInExile>),
    CounterSpell(Box<Spell>),
    CreatePermanentLayerEffect(Box<Permanent>, Vec<LayerEffect>),
    CreatePermanentLayerEffectUntil(Box<Permanent>, Vec<LayerEffect>, Expiration),
    CreatePermanentRuleEffectUntil(Box<Permanent>, Vec<PermanentRule>, Expiration),
    CreatePlayerEffectUntil(Box<Player>, Vec<PlayerEffect>, Expiration),
    CreatureConnives(Box<Permanent>),
    DestroyPermanent(Box<Permanent>),
    DiscardACard,
    DiscardACardAtRandom,
    DiscardACardOfType(Box<Cards>),
    DiscardAnyNumberOfCards,
    DiscardAnyNumberOfCardsOfType(Box<Cards>),
    DiscardCard(CardInHand),
    DiscardHand,
    DiscardNumberCards(Box<GameNumber>),
    DiscardNumberCardsAtRandom(Box<GameNumber>),
    DiscardNumberCardsOfType(Box<GameNumber>, Box<Cards>),
    DiscardNumberGroupCards(Box<GameNumber>, GroupFilter),
    DiscardNumberGroupCardsOfType(Box<GameNumber>, Box<Cards>, GroupFilter),
    DrawACard,
    DrawNumberCards(Box<GameNumber>),
    CollectEvidence(Box<GameNumber>),
    ExchangeControl(Box<Permanent>, Box<Permanent>),
    ExchangeControlOfSpellAndPermanent(Box<Spell>, Box<Permanent>),
    ExertPermanent(Box<Permanent>),
    ExileACardFromHand,
    ExileACardFromPlayersGraveyardAndPayItsManaCost(CardsInGraveyard, Box<Player>),
    ExileACardOfTypeFromHand(Box<Cards>),
    ExileAFaceDownPermanentFaceUp(Box<Permanents>),
    ExileAGraveyardCard(Box<CardsInGraveyard>),
    ExileAPermanent(Box<Permanents>),
    ExileAPermanentUntil(Box<Permanents>, Expiration),
    ExileASpell(Box<Spells>),
    ExileAnyNumberOfCardsFromPlayersGraveyard(CardsInGraveyard, Box<Player>),
    ExileAnyNumberOfPermanents(Box<Permanents>),
    ExileCardFromHand(CardInHand),
    ExileEachGraveyardCard(Box<CardsInGraveyard>),
    ExileEachPermanent(Box<Permanents>),
    ExileGraveyardCard(CardInGraveyard),
    ExileGraveyardCardFaceDown(CardInGraveyard),
    ExileHand,
    ExileHandFaceDown,
    ExileNumberCardsFromASinglePlayersGraveyard(Box<GameNumber>, CardsInGraveyard, Box<Players>),
    ExileNumberCardsOfTypeFromHand(Box<GameNumber>, Box<Cards>),
    ExileNumberOrMoreCardsFromPlayersGraveyard(Box<GameNumber>, CardsInGraveyard, Box<Player>),
    ExileNumberOrMoreGroupPermanents(Box<GameNumber>, Box<Permanents>, GroupFilter),
    ExileNumberPermanents(Box<GameNumber>, Box<Permanents>),
    ExilePermanent(Box<Permanent>),
    ExilePermanentUntil(Box<Permanent>, Expiration),
    ExilePlayersGraveyard(Box<Player>),
    ExileSpell(Box<Spell>),
    ExileTheTopNumberCardsOfLibraryFaceDown(Box<GameNumber>),
    ExileTheTopNumberCardsOfPlayersLibrary(Box<GameNumber>, Box<Player>),
    ExileTopCardOfLibrary,
    FlipACoinAndCallIt,
    GainControlOfAPermanent(Box<Permanents>),
    GainControlOfPermanent(Box<Permanent>),
    GainControlOfPermanentUntil(Box<Permanent>, Expiration),
    GainLife(Box<GameNumber>),
    GainLifeForEach(Box<GameNumber>, Box<GameNumber>),
    HaveAPlayerTakeAction(Box<Players>, CostPlayerAction),
    HaveEachPlayerTakeAction(Box<Players>, CostPlayerAction),
    HavePermanentDealDamage(Box<Permanent>, Box<GameNumber>, Box<DamageRecipient>),
    HavePlayerTakeAction(Box<Player>, CostPlayerAction),
    HaveSpellDealDamage(Box<Spell>, Box<GameNumber>, Box<DamageRecipient>),
    Investigate,
    LookAtTheTopCardOfPlayersLibrary(Box<Player>),
    LookAtTopOfLibrary,
    Loyalty(i32),
    LoyaltyMinusX,
    MillACard,
    MillNumberCards(Box<GameNumber>),
    MoveAtLeastOneCounterFromPermanentOntoNewPermanent(Box<Permanent>, Box<Permanent>),
    PayAnyAmountOfMana,
    PayEnergy(Box<GameNumber>),
    PayLife(Box<GameNumber>),
    PayLifeEqualToItsManaValue,
    PayLifeForEach(Box<GameNumber>, Box<GameNumber>),
    PayMana(ManaCost),
    PayManaAnyNumberOfTimes(ManaCost),
    PayManaAnyX(ManaCostX),
    PayManaAnyXRestricted(ManaCostX, Box<Comparison>),
    PayManaCostOfPermanent(Box<Permanent>),
    PayManaCostOfSpell(Box<Spell>),
    PayManaForEach(ManaCost, Box<GameNumber>),
    PayManaReduceForEach(ManaCost, CostReduction, Box<GameNumber>),
    PayManaUptoNumberTimes(ManaCost, Box<GameNumber>),
    PayManaX(ManaCostX, Box<GameNumber>),
    PayMana_OnlyProducedByTreasure(ManaCost),
    PlayALandFromTopOfLibrary(Box<Cards>),
    PlayTopCardOfLibraryWithoutPaying,
    PutACardFromGraveyardIntoHand(Box<CardsInGraveyard>),
    PutACardFromHandOnBattlefield(Box<Cards>, Vec<ReplacementActionWouldEnter>),
    PutACardFromHandOnBottomOfLibrary,
    PutACardFromHandOnTopOfLibrary,
    PutACardFromHandOrGraveyardOnBattlefield(
        Box<Cards>,
        Box<Player>,
        Vec<ReplacementActionWouldEnter>,
    ),
    PutACardFromPlayersGraveyardOnBattlefield(
        CardsInGraveyard,
        Box<Player>,
        Vec<ReplacementActionWouldEnter>,
    ),
    PutAGraveyardCardOnTheBottomOfItsOwnersLibrary(Box<CardsInGraveyard>),
    PutACounterOfTypeOnAPermanent(CounterType, Box<Permanents>),
    PutACounterOfTypeOnPermanent(CounterType, Box<Permanent>),
    PutANameStickerOnPermanent(Box<Permanent>),
    PutANumberOfExiledCardsIntoOwnersGraveyard(Box<GameNumber>, CardsInExile),
    PutAPermanentIntoItsOwnersHand(Box<Permanents>),
    PutAnExiledCardIntoOwnersGraveyard(Box<CardsInExile>),
    PutExiledCardIntoOwnersHand(Box<CardInExile>),
    PutExiledCardOntoBattlefield(CardInExile, Vec<ReplacementActionWouldEnter>),
    PutGraveyardCardIntoHand(CardInGraveyard),
    PutGraveyardCardOntoBattlefield(CardInGraveyard, Vec<ReplacementActionWouldEnter>),
    //PutGraveyardCardToTopOfLibrary(CardInGraveyard),
    PutNumberCardsFromASinglePlayersGraveyardOnBottomOfLibrary(Box<GameNumber>, Box<Players>),
    PutNumberCardsFromPlayersGraveyardOnBottomOfLibrary(Box<GameNumber>, Box<Player>),
    PutNumberCountersOfTypeOnAPermanent(Box<GameNumber>, CounterType, Box<Permanents>),
    PutNumberCountersOfTypeOnPermanent(Box<GameNumber>, CounterType, Box<Permanent>),
    PutNumberPermanentsIntoOwnersHand(Box<GameNumber>, Box<Permanents>),
    PutPermanentIntoItsOwnersHand(Box<Permanent>),
    PutPermanentOnBottomOfOwnersLibrary(Box<Permanent>),
    PutPermanentOnTopOfOwnersLibrary(Box<Permanent>),
    PutSpellOnBottomOfOwnersLibrary(Box<Spell>),
    PutTopCardOfEachPlayersLibraryInGraveyard(Box<Players>),
    PutTopCardOfLibraryOfTypeOnBattlefield(Box<Cards>, Vec<ReplacementActionWouldEnter>),
    RegeneratePermanent(Box<Permanent>),
    RemoveACounterFromAPermanent(Box<Permanents>),
    RemoveACounterFromPermanent(Box<Permanent>),
    RemoveACounterOfTypeFromAPermanent(CounterType, Box<Permanents>),
    RemoveACounterOfTypeFromAnExiledCard(CounterType, CardsInExile),
    RemoveACounterOfTypeFromPermanent(CounterType, Box<Permanent>),
    RemoveAllCountersOfTypeFromPermanent(CounterType, Box<Permanent>),
    RemoveAnyNumberOfCountersOfTypeFromAmongPermanents(CounterType, Box<Permanents>),
    RemoveAnyNumberOfCountersOfTypeFromPermanent(CounterType, Box<Permanent>),
    RemoveNumberCountersFromAmongPermanents(Box<GameNumber>, Box<Permanents>),
    RemoveNumberCountersOfTypeFromAmongPermanents(Box<GameNumber>, CounterType, Box<Permanents>),
    RemoveNumberCountersOfTypeFromPermanent(Box<GameNumber>, CounterType, Box<Permanent>),
    RemoveNumberOrMoreCountersOfTypeFromAmongPermanents(
        Box<GameNumber>,
        CounterType,
        Box<Permanents>,
    ),
    RemoveNumberOrMoreCountersOfTypeFromPermanent(Box<GameNumber>, CounterType, Box<Permanent>),
    RevealACardFromHandAtRandom,
    RevealACardOfTypeFromHand(Box<Cards>),
    RevealAnyNumberOfCardsOfTypeFromHand(Box<Cards>),
    RevealCardFromHand(CardInHand),
    RevealHand,
    RevealHandAndPutEachCardOnBattlefield(Box<Cards>, Vec<ReplacementActionWouldEnter>),
    RevealNumberGroupCardsFromHand(Box<GameNumber>, Box<Cards>, GroupFilter),
    RevealTheChosenPlayer,
    RevealTopCardOfLibrary,
    RevealTopCardOfLibraryAndPutIntoHand(Box<Cards>),
    RevealTopCardOfLibraryOfType(Box<Cards>),
    RollAD6,
    RollAD8,
    SacrificeAPermanent(Box<Permanents>),
    SacrificeAnyNumberOfGroupPermanents(Box<Permanents>, GroupFilter),
    SacrificeAnyNumberOfPermanents(Box<Permanents>),
    SacrificeEachPermanent(Box<Permanents>),
    SacrificeNumberPermanents(Box<GameNumber>, Box<Permanents>),
    SacrificePermanent(Box<Permanent>),
    SacrificeUptoNumberPermanents(Box<GameNumber>, Box<Permanents>),
    SearchLibrary(Vec<SearchLibraryAction>),
    SeekACard(Box<Cards>),
    ShuffleACardFromHandIntoLibrary,
    ShuffleCardsFromHandIntoLibrary(Box<CardsInHand>),
    ShuffleGraveyardCardIntoLibrary(CardInGraveyard),
    ShufflePermanentIntoLibrary(Box<Permanent>),
    TapAPermanent(Box<Permanents>),
    TapAnyNumberOfGroupPermanents(Box<Permanents>, GroupFilter),
    TapAnyNumberOfPermanents(Box<Permanents>),
    TapNumberGroupPermanents(Box<GameNumber>, Box<Permanents>, GroupFilter),
    TapNumberPermanents(Box<GameNumber>, Box<Permanents>),
    TapPermanent(Box<Permanent>),
    TransformPermanent(Box<Permanent>),
    UnattachAPermanentFromAPermanent(Box<Permanents>, Box<Permanents>),
    UnattachPermanent(Box<Permanent>),
    UnspecializeGraveyardCard(CardInGraveyard),
    UntapAPermanent(Box<Permanents>),
    UntapNumberPermanents(Box<GameNumber>, Box<Permanents>),
    UntapPermanent(Box<Permanent>),
}

type ReflexiveAction = Cost;

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Name", content = "args")]
pub enum NameFilter {
    NamedCard(NameString),
    ANameChosenByPermanent(Box<Permanent>),
    TheNamePlayerNotedForCardDuringDraft(Box<Player>, NameString),
    TheNameOfTheSacrificedCreature,
    NameOfGraveyardCard(CardInGraveyard),
    OneOfTheChosenNames,
    TheChosenName,
    TheChosenCardName,
    TheNameChosenByPlayer(Box<Player>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Card", content = "args")]
pub enum SingleCard {
    ThisCard,
    TheCardPutOntoTheBattlefieldThisWay,
    TheCardWithTheChosenName,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Cards", content = "args")]
pub enum Cards {
    Not(Box<Cards>),
    And(Vec<Cards>),
    Or(Vec<Cards>),
    Other(SingleCard),
    SingleCard(SingleCard),

    FromTheLorwynEclipsedExpansion,
    // IsNonSpellType(SpellType),
    TheCardsSeekedThisWay,
    HasNoAbilities,
    IsCardtypeVariable(CardtypeVariable),
    DoesntShareACardtypeWithTheCardsDiscardedThisWay,
    NumCreatureTypesIs(Box<Comparison>),
    HasXInManaCost,
    HasNumberCardTypes(Box<Comparison>),
    AnyCard,
    SharesACardtypeWithThePermanentDestroyedThisWay,
    SharesACreatureTypeWithDeadPermanent,
    CanEnchantPermanent(Box<Permanent>),
    DoesntHaveAbility(CheckHasable),
    DoesntShareALandTypeWithAPermanent(Box<Permanents>),
    DoesntShareANameWithAPermanent(Box<Permanents>),
    DoesntShareANameWithSpell(Box<Spell>),
    HasAbility(CheckHasable),
    HasAnAdventure,
    HasBasicLandType,
    IsArtifactType(ArtifactType),
    IsNonArtifactType(ArtifactType),
    IsCardtype(CardType),
    IsColor(Color),
    IsColored,
    IsColorless,
    IsCreatureType(CreatureType),
    IsCreatureTypeVariable(CreatureTypeVariable),
    IsDoubleFaced,
    IsEnchantmentType(EnchantmentType),
    IsHistoric,
    IsLandType(LandType),
    IsNonLandType(LandType),
    IsMonocolored,
    IsMulticolored,
    IsNamed(NameFilter),
    IsNonCardtype(CardType),
    IsNonColor(Color),
    IsNonCreatureType(CreatureType),
    IsNonEnchantmentType(EnchantmentType),
    IsNonSupertype(SuperType),
    IsNotNamed(NameFilter),
    IsNumberColors(Box<Comparison>),
    IsPermanent,
    // IsAnOutlaw,
    IsPlaneswalkerType(PlaneswalkerType),
    IsSpellType(SpellType),
    IsSupertype(SuperType),
    IsYourCommander,
    ManaCostIs(ManaCost),
    ManaValueIs(Box<Comparison>),
    NumberOfDifferentManaColorSymbolsInCostIs(Box<Comparison>),
    PowerIs(Box<Comparison>),
    SharesACardtypeWithCardDiscardedByPlayerThisWay(Box<Player>),
    SharesACardtypeWithCardsDiscardedThisWay,
    SharesACardtypeWithEachableExiledPermanent,
    SharesACardtypeWithExiledCard(Box<CardInExile>),
    SharesACardtypeWithPermanent(Box<Permanent>),
    SharesACardtypeWithSpell(Box<Spell>),
    SharesAColorWithAPermanent(Box<Permanents>),
    SharesAColorWithPermanent(Box<Permanent>),
    SharesAColorWithIt,
    SharesACreatureTypeWithMostPrevalentCreatureTypeInPlayersLibrary(Box<Player>),
    SharesACreatureTypeWithPermanent(Box<Permanent>),
    SharesACreatureTypeWithPermanents(Box<Permanents>),
    SharesAManaValueWithSpell(Box<Spell>),
    SharesANameWithACardInHandRevealedThisWay,
    SharesANameWithACardSpliceOntoSpell(Box<Spell>),
    SharesANameWithAPermanent(Box<Permanents>),
    SharesANameWithAnExiled(Box<CardsInExile>),
    SharesANameWithCardInPlayersGraveyard(Box<Cards>, Box<Player>),
    SharesANameWithDeadPermanent,
    SharesANameWithExiled(Box<CardInExile>),
    SharesANameWithGraveyardCard(CardInGraveyard),
    SharesANameWithPermanent(Box<Permanent>),
    SharesANameWithSpell(Box<Spell>),
    SharesANameWithTheCardChosenThisWay,
    SharesANameWithTheCardRevealedThisWay,
    SharesTotalPowerAndToughnessWithPermanent(Box<Permanent>),
    ToughnessIs(Box<Comparison>),
    HasMoreThanOneOfTheSameManaSymbolInCost,
    IsAllColors,
    CanEnchantThatEnteringPermanent,
    OwnedByAPlayer(Box<Players>),
    ControlledByAPlayer(Box<Players>),
    TheChosenLibraryFilter,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ExchangeOwnershipCard", content = "args")]
pub enum ExchangeOwnershipCard {
    Ref_TargetPermanent,
    TheCardRevealedFromHandThisWay,
    TheFirstCardExiledThisWay,
    TheSecondCardExiledThisWay,
    ThisPermanent,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Permanents", content = "args")]
pub enum Permanents {
    DoesntShareACreatureTypeWithPermanent(Box<Permanent>),
    IsNotAllColors,
    WasCast,

    SneakCostWasPaid,
    SneakCostWasPaidThisTurn,
    WasCastForItsWarpCost,
    WasCastUsingWebSlinging,
    WasntDealtDamageThisTurn,

    IsHarnessed,

    APermanentWithTheHighestManaValue(Box<Permanents>),
    APermanentWithTheLowestManaValue(Box<Permanents>),
    AdditionalCostWasPaid,
    And(Vec<Permanents>),
    AnyPermanent,
    AttachedToAPermanent(Box<Permanents>),
    AttachedToPermanent(Box<Permanent>),
    AttachedToPlayer(Box<Player>),
    AttackedABattleThisTurn(Box<Permanents>),
    AttackedDuringLastPlayersTurn(Box<Player>),
    AttackedPlayerThisCombat(Box<Player>),
    AttackedPlayerThisTurn(Box<Player>),
    AttackedSincePlayersLastUpkeep(Box<Player>),
    AttackedThisCombat,
    AttackedThisTurn,
    BandedWith(Box<Permanent>),
    BasePowerAndToughnessIsEqualTo(PT),
    BasePowerIs(Box<Comparison>),
    BaseToughnessIs(Box<Comparison>),
    BlockedAnAttackerThisTurn(Box<Permanents>),
    BlockedAttackerThisTurn(Box<Permanent>),
    BlockedDeadAttacker,
    BlockedSincePlayersLastUpkeep(Box<Player>),
    BlockedThisCombat,
    BlockedThisTurn,
    CanBeEnchantedByDeadGuest,
    CastByPlayer(Box<Player>),
    CastByPlayerDuringPlayersMainPhase(Box<Player>, Box<Player>),
    CastByPlayerFromAnyPlayersGraveyard(Box<Player>, Box<Players>),
    CastByPlayerFromHand(Box<Player>, Box<Player>),
    CastByPlayerFromPlayersGraveyard(Box<Player>, Box<Player>),
    CastByPlayerThisTurn(Box<Player>),
    CastFromPlayersLibrary(Box<Player>),
    CoinCameUpTails,
    ControlledByAPlayer(Box<Players>),
    ControlledByPlayer(Box<Player>),
    ControlledSinceBeforeCombatThisTurn,
    ControlledSinceBeginningOfMostRecentTurn,
    ControlledSinceBeginningOfTurn,
    ConvokedPermanent(Box<Permanent>),
    ConvokedSpell(Box<Spell>),
    CouldBeTargetedBySpell(Box<Spell>),
    CouldBeTargetedBySpell_ThoseTargets,
    CouldProduce(ManaProduceSymbol),
    CouldProduceAnyManaColorPermanentCouldProduce(Box<Permanent>),
    CouldntAttackThisTurn,
    CreatedByPermanent(Box<Permanent>),
    CrewedVehicleThisTurn(Box<Permanent>),
    DealtCombatDamageToAPlayerThisCombat(Box<Players>),
    DealtCombatDamageToAPlayerThisTurn(Box<Players>),
    DealtCombatDamageToCreatureThisTurn(Box<Permanents>),
    DealtDamageAmountThisTurn(Box<Comparison>),
    DealtDamageAmountToPlayerThisTurn(Box<Comparison>, Box<Player>),
    DealtDamageThisTurn,
    DealtDamageToAPermanentThisTurn(Box<Permanents>),
    DealtDamageToAnyPlayerThisTurn(Box<Players>),
    DealtDamageToPermanentThisTurn(Box<Permanent>),
    DealtDamageToPlayerThisTurn(Box<Player>),
    DevouredACreature,
    DidntAttackThisTurn,
    DidntEnterTheBattlefieldThisTurn,
    DidntExertThisTurn,
    DoesntHaveAName,
    DoesntHaveAbility(CheckHasable),
    DoesntShareANameWithACardInPlayersGraveyard(Box<Cards>, Box<Player>),
    DoesntShareANameWithAPermanent(Box<Permanents>),
    EmergeCostWasPaid,
    EnlistedAPermanentThisCombat(Box<Permanents>),
    EnteredFromAPlayersGraveyard(Box<Players>),
    EnteredFromPlayersGraveyard(Box<Player>),
    EnteredFromPlayersLibrary(Box<Player>),
    EnteredTheBattlefieldSinceLastTurnOf(Box<Player>),
    EnteredTheBattlefieldThisTurn,
    EnteredTheBattlefieldUnderPlayersControlThisTurn(Box<Player>),
    EnteredUnderPlayersControl(Box<Player>),
    Escaped,
    ExceptFor(Box<Permanents>),
    ExploitedCreature(Box<Permanent>),
    FoughtThisTurn,
    HadACounterOfTypePutOnItThisWay(CounterType),
    HadAnAbilityActivatedThisTurn,
    HadCountersPutOnItThisWay,
    HadToAttackThisCombat,
    HasACounter,
    HasACounterOfType(CounterType),
    HasACounterOfTypeFromPermanent(CounterType, Box<Permanent>),
    HasANameSticker,
    HasAPowerAndToughnessSticker,
    HasASticker,
    HasAbilities,
    HasAbility(CheckHasable),
    HasAnActivatedAbilityOtherThanThisActivatedAbility,
    HasAnAdventure,
    HasAnArtSticker,
    HasAnAttachment(Box<Permanents>),
    HasBeenGoaded,
    HasDealtDamageThisGame,
    HasExiledCards,
    HasExiledNumCards(Box<Comparison>),
    HasNoAbilities,
    HasNoCounters,
    HasNoCountersOfType(CounterType),
    HasNonBasicLandType,
    HasNumCounters(Box<Comparison>),
    HasNumCountersOfType(Box<Comparison>, CounterType),
    HasTheMostVotesOrTiedForTheMostVotes,
    HasXInManaCost,
    HasntBeenPhasedOutWithThisAbility,
    HasntDealtDamageThisGame,
    HasntHadACounterOfTypeRemovedWithThisEffect(CounterType),
    InTheChosenPile,
    InTheChosenPiles,
    InTheChosenSector,
    InThePermanentPileChosenThisWay,
    InThePileChosenForPermanent(Box<Permanent>),
    IntensityIs(Box<Comparison>),
    IsACommander,
    IsARingBearer,
    IsAllColors,
    IsAnOutlaw,
    IsArtifactType(ArtifactType),
    IsAttacking,
    IsAttackingABattle(Box<Permanents>),
    IsAttackingAPermanent(Box<Permanents>),
    IsAttackingAPlayer(Box<Players>),
    IsAttackingAPlayerOrPlaneswalkerTheyControl(Box<Players>),
    IsAttackingAlone,
    IsAttackingPlayer(Box<Player>),
    IsAttackingPlayerOrPlaneswalkerTheyControl(Box<Player>),
    IsAttackingTheSamePlayerOrPlaneswalkerAsCreature(Box<Permanent>),
    IsBlocked,
    IsBlockedByADefender(Box<Permanents>),
    IsBlockedByDefender(Box<Permanent>),
    IsBlocking,
    IsBlockingAlone,
    IsBlockingAnAttacker(Box<Permanents>),
    IsBlockingAttacker(Box<Permanent>),
    IsCardtype(CardType),
    IsCardtypeVariable(CardtypeVariable),
    IsColor(Color),
    IsColored,
    IsColorless,
    IsCreatureType(CreatureType),
    IsCreatureTypePlayerNotedForCardDuringDraft(Box<Player>, NameString),
    IsCreatureTypeVariable(CreatureTypeVariable),
    IsDamaged,
    IsDoubleFaced,
    IsEnchanted,
    IsEnchantedByANumberOfEnchantingPermanents(Box<Comparison>, Box<Permanents>),
    IsEnchantedByAPermanent(Box<Permanents>),
    IsEnchantingPermanent(Box<Permanent>),
    IsEnchantmentType(EnchantmentType),
    IsEquipped,
    IsFaceDown,
    IsFaceUp,
    IsFirstLandPlayedByPlayerThisTurn,
    IsHistoric,
    IsLandType(LandType),
    IsLandTypeVariable(LandTypeVariable),
    IsModified,
    IsMonocolored,
    IsMonstrous,
    IsMulticolored,
    IsNamed(NameFilter),
    IsNonArtifactType(ArtifactType),
    IsNonCardtype(CardType),
    IsNonColor(Color),
    IsNonCreatureType(CreatureType),
    IsNonCreatureTypeVariable(CreatureTypeVariable),
    IsNonEnchantmentType(EnchantmentType),
    IsNonLandType(LandType),
    IsNonOutlaw,
    IsNonPlaneswalkerType(PlaneswalkerType),
    IsNonSupertype(SuperType),
    IsNonToken,
    IsNotACommander,
    IsNotNamed(NameFilter),
    IsNotSuspected,
    IsNumberColors(Box<Comparison>),
    IsPaired,
    IsPairedWithA(Box<Permanents>),
    IsPermanent,
    IsPlaneswalkerType(PlaneswalkerType),
    IsPlaneswalkerTypeVariable(PlaneswalkerTypeVariable),
    IsRenowned,
    IsSaddled,
    IsSupertype(SuperType),
    IsSuspected,
    IsTapped,
    IsTheChosenPermanentFilter,
    IsTheFirstChosenPermanentFilter,
    IsTheSecondChosenPermanentFilter,
    IsTheThirdChosenPermanentFilter,
    IsToken,
    IsTransformed,
    IsUnblocked,
    IsUntapped,
    IsYourCommander,
    IsntAttacking,
    IsntBlocking,
    IsntEnchanted,
    IsntSaddled,
    ItEscaped,
    ItWasCast,
    MadnessCostWasPaid,
    ManaAmountOfSameColorWasSpentToCastIt(Box<Comparison>),
    ManaAmountOfTypeWasSpentToCastIt(Box<Comparison>, Color),
    ManaFromAPermanentWasSpentToCastIt(Box<Permanents>),
    ManaValueIs(Box<Comparison>),
    ManaWasSpentToCastIt(Vec<ManaProduce>),
    NoManaWasSpentToCastIt,
    Not(Box<Permanents>),
    NotChosenByAPlayerThisWay(Box<Players>),
    NotChosenThisWay,
    NotControlledSinceBeginningOfTurn,
    NotInTheChosenPile,
    NotPutOntoBattlefieldByAbility(Ability),
    NumOtherPermanentsAreOnTheBattlefield(Box<Comparison>, Box<Permanents>),
    NumberOfColorsOfManaSpentToCastItIs(Box<Comparison>),
    OnTheBattlefield,
    Or(Vec<Permanents>),
    Other(Box<Permanent>),
    OwnedByAPlayer(Box<Players>),
    PlayedByAPlayer(Box<Players>),
    PlayerControlledAPermanentAsCast(Box<Player>, Box<Permanents>),
    PlayerHasCastAnotherSpellThisTurn(Box<Player>, Box<Spells>),
    PlayerRevealedACardAsCast(Box<Player>, Box<Cards>),
    PowerAndToughnessAreEqual,
    PowerAndToughnessArentEqual,
    PowerAndToughnessIsEqualTo(PT),
    PowerIs(Box<Comparison>),
    PowerIsDifferentFromItsBasePower,
    PowerIsGreaterThanBasePower,
    PowerIsLessThanToughness,
    ProtectedByAPlayer(Box<Players>),
    ProwlCostWasPaid,
    PutOntoBattlefieldByPermanent(Box<Permanent>),
    PutOntoBattlefieldByScheme(SingleScheme),
    ReceivedAVote,
    Ref_TargetPermanents,
    Ref_TargetPermanents1,
    Ref_TargetPermanents2,
    RegeneratedThisTurn,
    RemovedFromCombatThisWay,
    SaddledPermanentThisTurn(Box<Permanent>),
    SharesACardtypeWithExiledCard(Box<CardInExile>),
    SharesACardtypeWithGraveyardCard(CardInGraveyard),
    SharesACardtypeWithPermanent(Box<Permanent>),
    SharesACardtypeWithPermanentFromAmongCardtypes(Box<Permanent>, Vec<CardType>),
    SharesACardtypeWithThatDeadPermanent,
    SharesACardtypeWithTheSacrificedPermanent,
    SharesAColorWithAnyManaColorProduced,
    SharesAColorWithColorChosenByPlayerDuringDraft(Box<Player>, NameString),
    SharesAColorWithPermanent(Box<Permanent>),
    SharesAColorWithTheTopCardOfPlayersLibrary(Box<Player>),
    SharesAColorWithhTheMostCommonColorOrAColorTiedForMostCommonColorAmongPermanents(
        Box<Permanents>,
    ),
    SharesACreatureTypeWithDeadPermanent,
    SharesACreatureTypeWithExiledCard(Box<CardInExile>),
    SharesACreatureTypeWithPermanent(Box<Permanent>),
    SharesACreatureTypeWithPermanents(Box<Permanents>),
    SharesANameOriginallyPrintedInAntiquities,
    SharesANameOriginallyPrintedInArabianNights,
    SharesANameOriginallyPrintedInHomelands,
    SharesANameWithAPermanent(Box<Permanents>),
    SharesANameWithAPermanentThatDealtDamageToPlayerLastTurn(Box<Player>),
    SharesANameWithCardInPlayersGraveyard(Box<Cards>, Box<Player>),
    SharesANameWithExiled(Box<CardInExile>),
    SharesANameWithGraveyardCard(CardInGraveyard),
    SharesANameWithPermanent(Box<Permanent>),
    SharesANameWithSpell(Box<Spell>),
    SharesANameWithTheLeavingPermanent,
    SharesAPermanentCardtypeWithPermanent(Box<Permanent>),
    SharesASectorWithPermanent(Box<Permanent>),
    SharesCardtypeWithPermanent(CardType, Box<Permanent>),
    SinglePermanent(Box<Permanent>),
    SnowManaWasSpentToCastIt,
    SpectacleCostWasPaid,
    StartedThisTurnUntapped,
    SurgeCostWasPaid,
    TargetsAPermanent_ThosePermanents,
    TheCardsConjuredOntoTheBattlefieldThisWay,
    TheChosenCreatures,
    TheChosenPermanents,
    TheCreatedTokens,
    TheNthSpellCastByPlayerThisTurn(Box<GameNumber>, Box<Spells>, Box<Player>),
    ThePermanentsAffectedThisWay,
    ThePermanentsChosenThisWay,
    ThePermanentsExiledThisWay,
    ThePermanentsGainedControlOfThisWay,
    ThePermanentsList,
    ThePermanentsListForPlayer(Box<Player>),
    ThePermanentsNotChosenThisWay,
    ThePermanentsPhasedOutThisWay,
    ThePermanentsPutOnTheBattlefieldThisWay,
    ThePermanentsSacrificedThisWay,
    ThePermanentsTappedThisWay,
    ThePermanentsThatHadCountersPutOnThemThisWay,
    TheSacrificedPermanents,
    TheSecretlyChosenPermanents,
    TheTokensCreatedThisWay,
    TheUnchosenPermanents,
    TotalPowerAndToughnessIs(Box<Comparison>),
    ToughnessIs(Box<Comparison>),
    TributeWasntPaid,
    Trigger_ThoseCreatures,
    Trigger_ThosePermanents,
    WasAttachedToDeadPermanent,
    WasBargained,
    WasBlockedByADefenderThisTurn(Box<Permanents>),
    WasBlockedByDefenderThisCombat(Box<Permanent>),
    WasBlockedByDefenderThisTurn(Box<Permanent>),
    WasBlockedSincePlayersLastUpkeep(Box<Player>),
    WasBlockedThisTurn,
    WasBlockingDeadDefender,
    WasCastFromAPlayersGraveyard(Box<Players>),
    WasCastFromTheirHand,
    WasCastThisTurn,
    WasCrewedByACreatureThisTurn(Box<Permanents>),
    WasCrewedByNumberCreatures(Box<Comparison>),
    WasDealtAnAmountOfDamageThisTurn(Box<Comparison>),
    WasDealtDamageByASourceThisTurn(DamageSources),
    WasDealtDamageByAnyPermanentThisTurn(Box<Permanents>),
    WasDealtDamageByPermanentThisGame(Box<Permanent>),
    WasDealtDamageByPermanentThisTurn(Box<Permanent>),
    WasDealtDamageBySpellThisTurn(Box<Spells>),
    WasDealtDamageByThisSpell,
    WasDealtDamageThisTurn,
    WasDealtDamageThisWay,
    WasDealtExcessDamageThisTurn,
    WasDealtExcessDamageThisWay,
    WasDealtNoncombatDamageThisTurn,
    WasEmbalmed,
    WasGoadedThisWay,
    WasKicked,
    WasKickedTwice,
    WasKickedWithKicker(ManaCost),
    WasTappedToPayForAbilitiesOfPermanent(Box<Permanent>),
    WasTurnedFaceUpThisTurn,
    WasUnearthed,
    WasUntappedThisWay,
    WasntCast,
    WasntCastFromHand,
    WasntCastFromTheirHand,
    WasntKicked,
    XIs(Box<Comparison>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Permanent", content = "args")]
pub enum Permanent {
    // Internal
    ById(PermanentId),

    // Normal
    TheChosenPermanentForPlayer(Box<Player>),
    ThePermanentBlightedThisWay,
    ThePermanentAttachedThisWay,
    ThePermanentBeheldThisWay,
    TheTransformedPermanent,
    ThePermanentThatGrantedThisAbility,
    ThePermanentExiledByPlayerThisWay(Box<Player>),
    TheCardConjuredOntoTheBattlefieldThisWay,
    ThePermanentCloakedThisWay,
    ThePermanentPhasedOutThisWay,
    RefOuter_TargetPermanent,
    TheFirstPermanentChosenByPlayerThisWay(Box<Player>),
    TheSecondPermanentChosenByPlayerThisWay(Box<Player>),
    ThePermanentThatHadCountersPutOnItThisWay,
    ActionPermanent,
    WouldDealDamage_DamageRecipientPermanent,
    TheTokenCreatedThisWay,
    ThePermanentAttachedToThisWay,
    ThisPermanentOrThisCommandCard,
    ThePermanentGainedControlOfThisWay,
    ThePermanentThisSpellBecame,
    Ref_TargetPermanent5,
    ThePermanentPutOnTheBattlefieldByPlayerThisWay(Box<Player>),
    ThePermanentSpellResolvedThisWay,
    WouldDealDamage_ThatPermanent,
    WouldBeDestroyed_ThatPermanent,
    WouldDealDamage_DamageSourceAsPermanent,
    WouldUntapDuringsItsControllersUntapStep_ThatPermanent,
    WouldDie_ThatPermanent,
    PermanentSourceOfAbilityCounteredThisWay,
    PermanentSourceOfAbility(Ability),
    CreatePermanentEffect_It,
    EachPermanentEffect_It,
    ThePermanentThatCreatedThisEmblem,
    TheFirstChosenPermanent,
    ThisExiledPermanentCard,
    TheCreatureHauntedByExiledCard(Box<CardInExile>),
    TheSacrificedPermanent,
    PlayersRingBearer(Box<Player>),
    ThePermanentExiledThisWay,
    Ref_TargetPermanentOfPlayersChoice,
    TheCreatureBolsteredThisWay,
    ThisSacrificedPermanent,
    TheTokenCreatedByPlayerThisWay(Box<Player>),
    ThePermanentThatCreatedIt,
    Ref_TargetPermanentControlledBy(Box<Player>),
    TheSecondChosenPermanent,
    TheCreaturePairedWithPermanent(Box<Permanent>),
    SingleTargetPermanentOfSpell(Box<Spell>),
    TheArmyAmassedThisWay,
    Trigger_ThatLand,
    Ref_TargetPermanents_0,
    Ref_TargetPermanents_1,
    ThePermanentDestroyedThisWay,
    AnyTargetAsAPermanent,
    ApplyPermanentEffect_It,
    DealsDamage_ThatPermanent,
    EachablePermanent,
    GuestPermanent,
    HostPermanent,
    HostPermanentOf(Box<Permanent>),
    Ref_TargetPermanent,
    Ref_TargetPermanent1,
    Ref_TargetPermanent2,
    Ref_TargetPermanent3,
    Ref_TargetPermanent4,
    Self_It,
    ThatEnteringPermanent,
    TheChosenPermanent,
    TheCreatedToken,
    TheCreatureUnequippedThisWay,
    ThePermanentChosenByPlayerThisWay(Box<Player>),
    ThePermanentChosenThisWay,
    ThePermanentPutOnTheBattlefieldThisWay,
    ThePermanentReturnedToHandThisWay,
    ThePermanentSacrificedThisWay,
    ThePermanentTappedThisWay,
    ThePermanentThatDiedThisWay,
    ThisGuest,
    ThisPermanent,
    Trigger_ThatArtifact,
    Trigger_ThatCreature,
    Trigger_ThatCreatureOrPlaneswalker,
    Trigger_ThatDeadPermanent,
    Trigger_ThatOtherCreature,
    Trigger_ThatOtherPermanent,
    Trigger_ThatPermanent,
    Trigger_ThatSacrificedPermanent,
    Trigger_ThatVehicle,
    Trigger_TheAttackingCreature,
    Trigger_TheBlockingCreature,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_SpellCopyEffects", content = "args")]
pub enum SpellCopyEffects {
    NoSpellCopyEffects,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_TokenCopyEffects", content = "args")]
pub enum TokenCopyEffects {
    TokenCopyEffects(Vec<TokenCopyEffect>),
    NoTokenCopyEffects,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_TokenCopyEffect", content = "args")]
pub enum TokenCopyEffect {
    AddSupertypes(Vec<SuperType>),
    RemoveSupertypes(Vec<SuperType>),
    AddCardtypes(Vec<CardType>),
    SetCardtypes(Vec<CardType>),
    AddCreatureTypes(Vec<CreatureType>),
    AddArtifactTypes(Vec<ArtifactType>),
    SetArtifactTypes(Vec<ArtifactType>),
    SetCreatureTypes(Vec<CreatureType>),
    AddAbility(Vec<Rule>),
    LosesAbility(CheckHasable),
    RemoveThisAbility,
    AddAbilityFromEachExiledHasable(CardsInExile, Vec<CheckHasable>),
    AddColor(SettableColor),
    SetColor(SettableColor),
    SetName(NameString),
    SetPT(PT),
    SetLoyalty(Box<GameNumber>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_CopyEffects", content = "args")]
pub enum CopyEffects {
    CopyEffects(Vec<CopyEffect>),
    NoCopyEffects,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_CopyEffect", content = "args")]
pub enum CopyEffect {
    // Typeline
    AddSupertypes(Vec<SuperType>),
    RemoveSupertypes(Vec<SuperType>),
    AddCardtypes(Vec<CardType>),
    SetCardtypes(Vec<CardType>),
    AddArtifactTypes(Vec<ArtifactType>),
    AddCreatureTypes(Vec<CreatureType>),
    AddLandTypes(Vec<LandType>),
    SetArtifactTypes(Vec<ArtifactType>),
    MergeTypeline,

    // Abilities
    AddAbilityVariable(AbilityVariable),
    AddAbility(Vec<Rule>),
    AddAbilityIfItDoesntHaveAbility(Box<Rule>, CheckHasable),

    // Color
    KeepColor,
    AddColor(SettableColor),
    SetColor(SettableColor),

    // Name
    KeepName,
    SetName(NameString),

    // P/T
    KeepPT,
    SetPT(PT),

    // ManaCost
    HasNoManaCost,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_StaticCopyEffect", content = "args")]
pub enum StaticCopyEffect {
    // Name
    KeepName,
    SetName(NameString),

    // Typeline
    MergeTypeline,
    AddSupertypes(Vec<SuperType>),
    RemoveSupertypes(Vec<SuperType>),
    AddCardtypes(Vec<CardType>),
    SetCardtypes(Vec<CardType>),
    AddArtifactTypes(Vec<ArtifactType>),
    AddCreatureTypes(Vec<CreatureType>),
    AddLandTypes(Vec<LandType>),
    SetArtifactTypes(Vec<ArtifactType>),

    // ManaCost
    HasNoManaCost,

    // Color
    KeepColor,
    AddColor(SettableColor),
    SetColor(SettableColor),

    // Abilities
    AddAbility(Vec<Rule>),
    AddAbilityIfItDoesntHaveAbility(Box<Rule>, CheckHasable),
    // P/T
    KeepPT,
    SetPT(CardPT),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_StaticCopyEffects", content = "args")]
pub enum StaticCopyEffects {
    NoStaticCopyEffects,
    StaticCopyEffects(Vec<StaticCopyEffect>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_TokenFlag", content = "args")]
pub enum TokenFlag {
    EntersAttachedToAPermanent(Box<Permanents>),
    EntersWithACounter(CounterType),
    EntersBlockingAttacker(Box<Permanent>),
    EntersWithNumberCounters(Box<GameNumber>, CounterType),
    EntersAttackingPlayerOrPlaneswalkerControlledBy(Box<Player>),
    EntersWithRuleEffectUntil(Vec<PermanentRule>, Expiration),
    EntersAttachedToPermanent(Box<Permanent>),
    EntersTapped,
    EntersAttackingPlayer(Box<Player>),
    EntersAttacking,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_SetInMotionAction", content = "args")]
pub enum SetInMotionAction {
    ChooseAPlayer(Box<Players>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_PutIntoGraveyardAction", content = "args")]
pub enum PutIntoGraveyardAction {
    ExileItInstead,
    RevealItAndShuffleItIntoLibraryInstead,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_AttachAction", content = "args")]
pub enum AttachAction {
    ChooseAColor(ChoosableColor),
    ChooseAnExiledCardToCopy(Box<CardsInExile>),
    ChooseACardName(Box<Cards>),
    ChooseACreatureType,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_FaceUpAction", content = "args")]
pub enum FaceUpAction {
    MayActions(Vec<FaceUpAction>),
    PutNumberCountersOfTypeOnPermanent(Box<GameNumber>, CounterType, Box<Permanent>),
    AttachPermanentToAPermanent(Box<Permanent>, Box<Permanents>),
}

// FINDME FIXME: Replace ReplacementActionWouldEnter with ReplacementActionWouldEnter

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_NameStickerFilter", content = "args")]
pub enum NameStickerFilter {
    TheNameStickerPutOnPermanentThisWay,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Commander", content = "args")]
pub enum Commander {
    TheCommanderChosenThisWay,
    ThisCommandCard,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_PregameCard", content = "args")]
pub enum PregameCard {
    ThisPregameCard,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Letter", content = "args")]
pub enum Letter {
    TheChosenLetter,
    SingleLetter(LetterString),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ManaSpent", content = "args")]
pub enum ManaSpent {
    Or(Vec<ManaProduceSymbol>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ManaSources", content = "args")]
pub enum ManaSources {
    IsCardtype(CardType),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_GameNumber", content = "args")]
pub enum GameNumber {
    TheAmountOfManaFromSourcesSpentToCastIt(Box<ManaSources>),
    TheGreatestManaValueAmongSpellsCastThisTurn(Box<Spells>),
    TheNumberOfCardTypesAmongSpellsCastThisTurn(Box<Spells>),
    TheNumberOfManaSymbolsInManaCostOfSpell(Box<Spell>),
    TheNumberOfPermanentsDealtDamageThisWay(Box<Permanents>),
    TheNumberOfSpellsAndAbilitiesCounteredThisWay,

    EachableNumber,

    WouldDrawACard_ThatMany,

    TotalManaValueOfSpells(Box<Spells>),

    SpeedOfPlayer(Box<Player>),
    TheNumberOfCardsThatWerePutIntoAPlayersLibraryFromTheirHandOrLibraryThisTurn(Box<Players>),
    TheNumberOfCountersOfTypeMovedThisWay(CounterType),

    ManaValueOfTheCardsRevealedByPlayersThisWay(Box<Players>),
    ManaValueOfTheWebslungCreature,
    TheGreatestManaValueAmongCardsInPlayersHand(Box<CardsInHand>, Box<Player>),
    TheGreatestPowerAmongPermanentsAsSpellWasCast(Box<Permanents>),
    TheNumberOfCardsPutIntoAGraveyardThisWay(Box<CardsInGraveyard>),

    TheHighestSpeedAmongPlayers(Box<Players>),
    TheNumberOfCardtypesAmongGraveyardCards(Box<CardsInGraveyard>),
    TheNumberOfGraveyardCards(Box<CardsInGraveyard>),
    TheNumberOfPermanentTypesAmongGraveyardCards(Box<CardsInGraveyard>),

    ManaValueOfThePermanentUnattachedThisWay,
    NumDifferentManaValuesAmongPermanents(Box<Permanents>),
    ManaValueOfYourCommander,
    TheManaValueMinusTheManaSpentOnSpell(Box<Spell>),
    TheNumberOfCardTypesAmongCardsPutInGraveyardThisWay,
    TheNumberOfTokensCreatedThisWay,
    TheNumberOfUnlockedDoorsAmongPermanents(Box<Permanents>),
    AmountOfExcessDamageDealtToPermanentThisTurn(Box<Permanent>),
    ManaValueOfTheCardsRevealedThisWay,
    TheAmountOfEnergyPaidOrLostByPlayersThisTurn(Box<Players>),
    TheAmountOfEnergyPlayerHas(Box<Player>),
    TheAmountOfManaFromPermanentsSpentToCastSpell(Box<Permanents>, Box<Spell>),
    TheGreatestManaValueAmongPermanents(Box<Permanents>),
    TheNumberOfCardTypesAmongPermanents(Box<Permanents>),
    TheNumberOfCardsManifestedThisWay,
    TheNumberOfPermanentsExiledThisTurn(Box<Permanents>),
    TheNumberOfPlayersThatHaveLostTheGame,
    TheNumberOfTimesModeChosenForSpell(Box<Spell>),
    AmountOfManaPaidThisWay,
    TheNumberOfPlayersAttackedByPlayerThisCombat(Box<Players>, Box<Player>),
    TheNumberOfTimesCreatureAttackedThisGame(Box<Permanent>),
    TheNumberOfDifferentColorPairsAmongPermanents(Box<Permanents>),
    TheTotalNumberOfCountersPlayersHave(Box<Players>),
    TheTotalNumberOfCountersOnPermanents(Box<Permanents>),
    TheGreatestManaValueAmongTheCardsDiscardedThisWay,
    TheTotalNumberOfRadCountersAmongPlayers(Box<Players>),
    TheNumberOfCardsInHandRevealedThisWayThatShareAManaValue,
    TheTotalManaValueOfThePermanentsThisSpellTargets,
    TheNumberOfPermanentsGainedControlOfThisWay,
    TheNumberOfSpellsCastByAnyPlayerSinceTheBeginningOfPlayersLastTurn(
        Box<Spells>,
        Box<Players>,
        Box<Player>,
    ),
    AmountOfExcessDamageDealtToPermanentsThisWay(Box<Permanents>),
    TheGreatestManaValueAmongTheCardsThatLeftTheGraveyardThisWay,
    Trigger_TheNumberOfCardsOfTypeMilledThisWay(Box<Cards>),
    TheNumberOfTimesPlayerHasDecendedThisTurn(Box<Player>),
    ManaValueOfThePermanentSacrificedThisWay,
    TheNumberChosenThisWay,
    TheNumberOfSpellsOrAbilitiesThatCausedAnyNumberOfPlayersToGuessOrToGroupCardsOrPermanentsIntoAPileThisTurn(
        SpellsAndAbilities,
        Box<Players>,
    ),
    TheNumberOfColorsAmongTheCardsUsedToCraftPermanent(Box<Permanent>),
    TheNumberOfCardsScryedOnTopOfLibraryThisWay,
    TheAmountOfManaFromATreasureSpentToActivateThisAbility,
    TheNumberOfPermanentsThatLeftTheBattlefieldUnderPlayersControlThisTurn(
        Box<Permanents>,
        Box<Player>,
    ),
    ManaValueOfTheExiledCardUsedToCraftPermanent(Box<Permanent>),
    TheManaValueOfTheCardDiscoveredThisWay,
    TheTotalPowerOfPermanentsThatDiedThisTurn(Box<Permanents>),
    Trigger_DiscoverValue,
    TheHighestManaValueAmongCardsInPlayersLibrary(Box<Cards>, Box<Player>),
    TheLeastToughnessAmongPermanents(Box<Permanents>),
    WouldLoseLife_ThatMuch,
    WhenAPermanentEntersTheBattlefield_AmountOfManaFromAPermanentSpentToCast(Box<Permanents>),
    Emerge_ToughnessOfTheSacrificedCreature,
    TheTotalManaValueOfSpellsCastThisTurn(Box<Spells>),
    TotalManaValueOfExiledCards(Box<CardsInExile>),
    WouldPayLife_ThatMuch,
    TheNumberOfCreaturesThatAttackedThisTurn(Box<Permanents>),
    TheNumberOfCardTypesAmongThePermanentsSacrificedThisTurn,
    TheNumberOfCountersOfTypeOnSpell(CounterType, Box<Spell>),
    HighestNumberPlayerNotedForCardDuringDraft(Box<Player>, NameString),
    NumCardsPlayerRemovedWithCardDuringDraft(Box<Player>, NameString),
    TheNumberOfOtherCardsInPlayersGraveyard(CardInGraveyard, Box<Cards>, Box<Player>),
    NumCountersOfTypeOnExiledCard(CounterType, CardInExile),
    TheNumberOfCardsInHandExiledThisWay,
    HighestNotedValueForPermanent(Box<Permanent>),
    NumberOfCreatureTypesNotedByPermanent(Box<Permanent>),
    LifeTotalOfPlayerAsTurnBegan(Box<Player>),
    WhenPermanentsDealDamageToPlayers_NumPlayersDealtDamage,
    CommandPermanentPassesFilter_NumCounters,
    NumberOfCardsPutIntoGraveyardThisWay,
    TheNumberOfCreatureTypesAmongPermanents(Box<Permanents>),
    TheNumberOfPermanentsSacrificedThisTurn(Box<Permanents>),
    TheAmountOfColorManaSpentOnX(ManaSymbol),
    MinimumOf(Box<GameNumber>, Box<GameNumber>),
    NumCardsPlayerCycledOrDiscardedThisTurn(Box<Player>),
    NumCreaturesPlayerAttackedWithThisTurn(Box<Permanents>, Box<Player>),
    TheClampedAmountOfDamageDealtThisWay,
    TotalNoncombatDamageDealtToPlayersThisTurn(Box<Players>),
    PowerOfTheSelectedPermanent,
    ManaValueOfTheCardFoundThisWay,
    TheNumberOfLibraryCardsRevealedThisWay,
    TheNumberOfLibraryCardsExiledThisWay,
    MaxPermanentsControlledByAPlayer(Box<Permanents>, Box<Players>),
    ManaValueOfTheFoundCard,
    TotalManaValueOfTheCardsDiscardedThisWay,
    TheTotalNumberOfColorManaSymbolsInManaCostsOfTheLibraryCardsRevealedThisWay(Color),
    NumPermanentsShuffledIntoLibraryThisWay,
    NumPermanentsShuffledIntoLibraryThisWayByPlayer(Box<Player>),
    ItsManaValue,
    WhenAPermanentEntersTheBattlefield_AmountOfManaFromTreasureSpentToCast,
    AsLoseUnspentMana_AmountOfUnspentMana,
    AmountOfUnspentManaOfColorPlayerHas(Color, Box<Player>),
    TheGreatestManaValueOfACommanderInTheCommandZoneOrOnTheBattlefield(Commanders),
    EachAdditionalManaCostPaid(ManaCost),
    TheGreatestAmongOfDamageDealtByASourceToAPlayerOrAPermanentThisTurn(
        DamageSources,
        Box<Players>,
        Box<Permanents>,
    ),
    DistributedNumber,
    MaximumOf(Box<GameNumber>, Box<GameNumber>),
    TheTotalNumberOfCardsDrawnByPlayesThisTurn(Box<Players>),
    AManaValueOfAnExiledCard(Box<CardsInExile>),
    ANumberOfCardsInAPlayersHand(Box<Players>),
    NumGroupPermanents(Box<Permanents>, GroupFilter),
    WouldGetCounters_NumberOfCounters,
    Trigger_AmountOfDamagePrevented,
    WouldScry_ThatMuch,
    APlayerWouldMillAnyNumberOfCards_ThatMuch,
    TheAmountOfDamagePreventedThisWay,
    WouldPutCounters_NumberOfCounters,
    WouldDealDamage_ThatMuchDamage,
    WouldGainLife_LifeAmount,
    WouldCreateTokens_NumberTokens,
    NumberOfPermanentsTappedThisWayByPlayer(Box<Player>),
    NumberOfCountersOfTypeOnPlane(CounterType, Plane),
    WhenASpellOrAbilityExilesAnyNumberOfPermanents_AmountOfPermanents,
    TheNumberOfVotesForWord(VoteOption),
    TheNumberOfVotesReceivedByPermanent(Box<Permanent>),
    TheNumberChosenForPermanent(Box<Permanent>),
    TheNumberOfVotesReceivedByPlayer(Box<Player>),
    TheNumberOfAttractionsPlayerHasVisitedThisTurn(Box<Player>),
    TheDiceResult,
    TheFirstDiceResult,
    TheSecondDiceResult,
    TheTotalOfTheDiceResults,
    TheNumberOfDifferentDiceResults,
    TheNumberOfDiceResults(Box<Comparison>),
    TheGreatestNumberOfStoredDiceResultsThatAreEqual,
    TriggerTheDiceResult,
    TotalManaValueOfTheCardsRevealedThisWay,
    Trigger_NumberOfPlayersBeingAttacked,
    TheNumberOfColorsOfTheSacrificedPermanent,
    ToughnessOfCreaturePutOnBattlefieldThisWay,
    NumManaSymbolsInCostOfSpell(ManaSymbol, Box<Spell>),
    TheNotedNumber,
    X_From_Casting,
    AmountOfGenericManaInSpellsManaCost(Box<Spell>),
    NumberOfTimesThisAbilityHasResolvedThisTurn,
    TheGreatestPowerOrToughnessAmongPermanents(Box<Permanents>),
    TheAmountOfDamageDealtToPermanentThisTurnBySources(Box<Permanent>, DamageSources),
    TheGreatestManaValueAmongExiledCards(Box<CardsInExile>),
    TotalToughnessOfExiledCards(Box<CardsInExile>),
    TotalPowerOfExiledCards(Box<CardsInExile>),
    NumSpellsCastByAnyPlayerThisTurn(Box<Spells>, Box<Players>),
    NumDifferentManaValuesAmongCardsInExile(Box<CardsInExile>),
    NumHandCardsExiledFaceDownThisWay,
    TheNumberOfCardTypesItSharesWithAnyExiledCard(Box<CardsInExile>),
    NumCardsOfTypeInPlayersLibrary(Box<Cards>, Box<Player>),
    NumCardsInExile(Box<CardsInExile>),
    WhenAPermanentEntersTheBattlefield_AmountOfManaOfTypeSpentToCast(Vec<ManaProduceSymbol>),
    AmountOfManaOfTypeSpentOnCumulativeUpkeep(ManaSpent),
    TheHighestManaValueAmongPermanentsOrCardsInTheCommandZone(Commanders),
    TheNumberOfCounterTypesAmongPermanents(Box<Permanents>),
    NumSpellsCastByAnyPlayerBeforeSpellThisTurn(Box<Spells>, Box<Players>, Box<Spell>),
    Trigger_ValueXOfThatSpell,
    NumDifferentlyNamedDungeonsPlayerHasComplete(Box<Player>),
    TheNumberOfPermanentsPutOntoTheBattlefieldThisWay(Box<Permanents>),
    LifePaidWithVanguard(SingleVanguard),
    TheNumberOfCardsPlayerDiscardedThisWay(Box<Player>),
    TheGreatestPowerAmongCardsPutIntoGraveyardThisWay(Box<Cards>),
    TotalManaValueOfGraveyardCards(Box<CardsInGraveyard>),
    Trigger_ManaValueOfTheSacrificedPermanent,
    WhenAPlayerDiscardsCardsForTheFirstTimeEachTurn_AmountOfCardsDiscarded,
    WhenAPermanentEntersTheBattlefield_AmountOfManaSpentToCast,
    ManaValueOfCardPutInHandThisWay,
    ToughnessOfExiledCard(Box<CardInExile>),
    ToughnessOfTheExiledCreature,
    ToughnessOfGraveyardCard(CardInGraveyard),
    ManaValueOfGraveyardCard(CardInGraveyard),
    PowerOfGraveyardCard(CardInGraveyard),
    PowerOfExiledCard(Box<CardInExile>),
    FlipACoinUntilLose_NumFlipsWon,
    DeadPermanentPassesFilter_NumCounters,
    TheNumberOfCreaturesThatConvokedPermanent(Box<Permanent>),
    TheHighestManaValueAmongCardsInPlayersGraveyard(Box<Cards>, Box<Player>),
    TheNumberOfPermanentsSacrificedAsPermenantEnteredBattlefield(Box<Permanents>, Box<Permanent>),
    Trigger_AmountOfExcessDamage,
    NumHandCardsExiledThisWay,
    TheAmountOfManaLostThisWay,
    NumPermanentsDevouredByEnteringPermanent(Box<Permanents>),
    NumCardsPutIntoGraveyardFromAnywhereThisTurn(Box<Cards>, Box<Player>),
    TheGreatestNumberOfCardsDrawnByAPlayerThisTurn(Box<Players>),
    NumSpellsCastThisTurn(Box<Spells>),
    NumColorsManaSpentToCastEnteringPermanent,
    WhenAPlayerCastsASpell_ManaValueOfThatSpell,
    TotalManaValueOfCardsInPlayersGraveyard(Box<Cards>, Box<Player>),
    TheGreatestNumberOfPermanentsControlledAmongPlayers(Box<Permanents>, Box<Players>),
    TheNumberOfTurnsPlayerHasBegunSinceItWasForetold,
    TheNumberOfDifferentManaCostsAmongCardsInPlayersGraveyard(Box<Cards>, Box<Player>),
    TheGreatestPowerAmongPermanentsAndCardsInPlayersGraveyard(
        Box<Permanents>,
        Box<Cards>,
        Box<Player>,
    ),
    TheHighestLifeTotalAmongPlayers(Box<Players>),
    WhenAPlayerCastsASpell_ThatSpellX,
    NumTokensCreatedByPlayerThisTurn(Box<Permanents>, Box<Player>),
    TheNumberOfPlayersThatPaidCost,
    NumColorsManaSpentToCastSpell(Box<Spell>),
    TotalPowerOfPermanents(Box<Permanents>),
    TotalToughnessOfPermanents(Box<Permanents>),
    TheNumberOfPermanentsOfTypeSacrificedThisWay(Box<Permanents>),
    NumTimesPermanentRegeneratedThisTurn(Box<Permanent>),
    TheTotalPowerOfThePermanentsSacrificedThisWay,
    NumGraveyardCardsOfTypeExiledThisWay(Box<Cards>),
    TotalPowerOfPermanentsExiledThisWay(Box<Permanents>),
    DifferenceBetween(Box<GameNumber>, Box<GameNumber>),
    DamageDealtToAnyPlayerThisTurn(Box<Players>),
    TheNumberOfCardtypesAmongCardsDiscardedThisWay,
    Trigger_TheAmountOfCounters,
    LoyaltyOfPermanent(Box<Permanent>),
    PowerOfTheDevouredCreature,
    TheNumberOfCardsInHandRevealedByPlayerThisWay(Box<Player>),
    TheNumberOfCreaturesThatDealtCombatDamageToAPlayer(Box<Permanents>, Box<Players>),
    ToxicValueOfPermanent(Box<Permanent>),
    ToughnessOfCreatureDestroyedThisWay,
    ToughnessOfCreatureSacrificedThisWay,
    NumCountersOnDeadPermanent,
    BasePowerOfPermanent(Box<Permanent>),
    TheTotalAmountOfManaPaidThisWay,
    NumCountersOfTypePlayerHasPutOnPermanentsThisTurn(CounterType, Box<Player>, Box<Permanents>),
    PowerOfTheCreatureItTargets,
    NumCoinFlipsLost,
    NumCountersOfTypeOnScheme(CounterType, SingleScheme),
    // FIXME: TheNumberOfCardsInPlayersGraveyardThatWerePutThereFromTheBattlefieldThisTurn -> Cards to CardsInGraveyard, and shouldn't need "ThatWerePutThereFromTheBattlefieldThisTurn"
    TheNumberOfCardsInPlayersGraveyardThatWerePutThereFromTheBattlefieldThisTurn(
        Box<Cards>,
        Box<Player>,
    ),
    TheHighestNumberChosen,
    AmountOfLifePaidThisWay,
    TheAmountOfDamageDealtToPermanentThisTurn(Box<Permanent>),
    NumSpellsCastByPlayerBeforeSpellThisTurn(Box<Spells>, Box<Player>, Box<Spell>),
    ToughnessOfCardInHand(CardInHand),
    TheNumberOfSpellsCounteredThisWay,
    TheAmountOfManaFromATreasureSpentToCastSpell(Box<Spell>),
    AmountOfManaSpentToCastSpell(Box<Spell>),
    TheNumberOfPermanentsPutIntoAPlayersGraveyardThisTurn(Box<Permanents>, Box<Players>),
    ManaValueOfCardPutInGraveyard,
    WhenCountersArePutOnAPermanent_AmountOfCounters,
    NumberOfTurnsPlayerHasBeguan(Box<Player>),
    NumberOfBlocksOfNumberCountersRemovedThisWay(Box<GameNumber>),
    TheNumberOfCoinsThatCameUpHeads,
    TheNumberOfPermanentsOnTheBattlefieldAtBeginningOfTurn(Box<Permanents>),
    TheNumberOfPermanentCardsReturnedToPlayersHandThisWay(Box<Player>),
    TheNumberOfCardsDiscardedByPlayerThisWay(Box<Player>),
    TheNumberOfPermanentsOnTheBattlefieldAsSpellWasCast(Box<Permanents>),
    AmountOfExcessDamageDealtThisWay,
    ManaValueOfTheCardRevealedByPlayerThisWay(Box<Player>),
    TheNumberOfCardsOfTypeInPlayersHand(Box<Cards>, Box<Player>),
    TheNumberOfPermanentsThatDiedThisWay(Box<Permanents>),
    NumberOfCardsOfTypeExiledThisWay(Box<Cards>),
    AmountOfUnspentManaPlayerHas(Box<Player>),
    NumCardsOfTypeDiscardedThisWay(Box<Cards>),
    CurrentStake,
    WhenAPlayerPaysLife_AmountOfLifePaid,
    PowerOfSpell(Box<Spell>),
    CounterSpell_ManaValueOfCounteredSpell,
    DamageDealtByResolvedSpell(Box<Spell>),
    DamageDealtToPlayerThisTurn(Box<Player>),
    DamageDealtToPlayerThisTurnByPermanents(Box<Player>, Box<Permanents>),
    GreatestNumberOfPermanentsThatHaveCreatureTypeInCommon(Box<Permanents>),
    HalfRoundedDown(Box<GameNumber>),
    HalfRoundedUp(Box<GameNumber>),
    TenthRoundedUp(Box<GameNumber>),
    HighestLifeTotalAmongPlayers(Box<Players>),
    HighestManaValueAmongCardsMilledThisWay,
    Integer(i32),
    IntensityOfPermanent(Box<Permanent>),
    IntensityOfSpell(Box<Spell>),
    // ItsManaCost,
    LastNotedLifeTotalForPermanent(Box<Permanent>),
    LifeGainedByPlayerThisTurn(Box<Player>),
    LifeLostByPlayerThisTurn(Box<Player>),
    LifeLostThisWay,
    LifeTotalOfPlayer(Box<Player>),
    LowestLifeTotalAmongPlayers(Box<Players>),
    ManaCostsOfCombatCreatures(Box<Permanents>),
    ManaValueOfCardDiscardedByPlayerThisWay(Box<Player>),
    ManaValueOfDeadPermanent,
    ManaValueOfExiled(Box<CardInExile>),
    ManaValueOfPermanent(Box<Permanent>),
    ManaValueOfSpell(Box<Spell>),
    ManaValueOfTheCardDiscardedThisWay,
    ManaValueOfTheCardExiledThisWay,
    ManaValueOfTheCardMilledThisWay,
    ManaValueOfTheCardRevealedThisWay,
    ManaValueOfTheDiscardedCard,
    ManaValueOfTheSacrificedPermanent,
    MinPermanentsControlledByAPlayer(Box<Permanents>, Box<Players>),
    Minus(Box<GameNumber>, Box<GameNumber>),
    Multiply(Box<GameNumber>, Box<GameNumber>),
    NumAbilityCountersPutOnPermanent,
    NumCardTypesOfCardDiscardedThisWay,
    NumCardsDiscardedThisWay,
    NumCardsDrawnByPlayerThisTurn(Box<Player>),
    NumCardsInPlayersLibrary(Box<Player>),
    NumCardsMilledIntoGraveyardThisWay(Box<Cards>),
    NumCardsMilledThisWay(Box<Cards>),
    NumCardsPlayerDiscardedThisTurn(Box<Player>),
    NumCardsPutIntoLibraryThisWay,
    NumCardsReturnedToHandThisWay,
    NumCardsShuffledIntoLibraryThisWay,
    NumCoinFlipsWon,
    NumColorManaSymbolsInCostsOfCardsInPlayersGraveyard(Color, Box<Cards>, Box<Player>),
    NumColorManaSymbolsInCostsOfPermanent(Color, Box<Permanent>),
    NumColorManaSymbolsInCostsOfPermanents(Color, Box<Permanents>),
    NumColorsAmongPermanents(Box<Permanents>),
    NumColorsManaSpentToCastSelf,
    NumColorsOfPermanent(Box<Permanent>),
    NumCountersOfTypeOnDeadGuestPermanent(CounterType),
    NumCountersOfTypeOnDeadPermanent(CounterType),
    NumCountersOfTypeOnPermanent(CounterType, Box<Permanent>),
    NumCountersOfTypeOnPermanents(CounterType, Box<Permanents>),
    NumCountersOfTypePlayerHas(CounterType, Box<Player>),
    NumCountersOfTypePlayersHave(CounterType, Box<Players>),
    NumCountersOnPermanent(Box<Permanent>),
    NumCreaturesInPlayersParty(Box<Player>),
    NumCreaturesOrPlaneswalkersThatDiedThisTurn(Box<Permanents>),
    NumDifferentManaValueAmongCardsInPlayersGraveyard(Box<Cards>, Box<Player>),
    NumDifferentlyPoweredCreaturesAmongPermanents(Box<Permanents>),
    NumEnteredTheBattlefieldThisTurn(Box<Permanents>),
    NumGraveyardCardsExiledThisWay,
    NumLibraryCardsRevealedThisWay(Box<Cards>),
    NumPermanentsAttachedToDeadPermanent(Box<Permanents>),
    NumPermanentsDestroyedThisWay(Box<Permanents>),
    NumPermanentsExiledThisWay,
    NumPermanentsOfTypeExiledThisWay(Box<Permanents>),
    NumPermanentsPhasedOutThisWay,
    NumPlayers(Box<Players>),
    NumPlayersWhoCreatedATokenThisWay,
    NumPointsOfBushidoPermanentHas(Box<Permanent>),
    NumSpellsCastByPlayerThisTurn(Box<Spells>, Box<Player>),
    NumTimesCreatureHasMutated(Box<Permanent>),
    NumTimesPaidMana,
    NumTimesPermanentAttackedThisTurn(Box<Permanent>),
    NumTimesPlayerHasACastACommanderFromCommandZone(Box<Player>),
    NumTimesPlayerHasCastACommanderFromCommandZone(Box<Player>),
    NumTimesPlayerHasCastTheirCommanderFromCommandZone(Box<Player>),
    NumTimesSpellWasKicked(Box<Spell>),
    NumberOfBasicLandTypesAmongPermanents(Box<Permanents>),
    NumberOfCardsDrawnThisWay,
    NumberOfColorManaSymbolsInManaCostOfTheSacrificedPermanent(Color),
    NumberOfColorsInPlayersCommandersColorIdentity(Box<Player>),
    NumberOfCountersOfTypeOnVanguard(CounterType, SingleVanguard),
    NumberOfCountersOfTypeRemovedThisWay(CounterType),
    NumberOfCountersRemovedThisWay,
    NumberOfPermanentsSacrificedByPlayerThisTurn(Box<Permanents>, Box<Player>),
    PermanentItTargets(Box<Permanents>),
    PlayerDevotionTo(Box<Player>, Color),
    PlayersChosenNumber(Box<Player>),
    Power(Box<GameNumber>, Box<GameNumber>),
    Plus(Box<GameNumber>, Box<GameNumber>),
    Plus3(Box<GameNumber>, Box<GameNumber>, Box<GameNumber>),
    PowerOfDeadPermanent,
    PowerOfPermanent(Box<Permanent>),
    PowerOfTheDiscardedCard,
    PowerOfTheExiledCreature,
    PowerOfTheRevealedCard,
    PowerOfTheSacrificedCreature,
    StartingLifeTotalOfPlayer(Box<Player>),
    TheAmountOfDamageDealtThisWay,
    TheAmountOfEnergyPaidThisWay,
    TheAmountOfSnowManaSpentToCastSpell(Box<Spell>),
    TheChosenNumber,
    TheGreatestNumberOfCardsDiscardedThisWay,
    TheGreatestPowerAmongCardsInPlayersGraveyard(Box<Cards>, Box<Players>),
    TheGreatestPowerAmongPermanents(Box<Permanents>),
    TheGreatestToughnessAmongPermanents(Box<Permanents>),
    TheHighestManaValueAmongCardsInPlayersHand(Box<Cards>, Box<Player>),
    TheHighestManaValueAmongPermanents(Box<Permanents>),
    TheHighestNumberOfCardsInHandAmongPlayers(Box<Players>),
    TheLeastPowerAmongPermanents(Box<Permanents>),
    TheLifePaid,
    TheLowestNumberOfCardsInHandAmongPlayers(Box<Players>),
    TheLowestNumberOfPermanentsControlledAmongPlayers(Box<Permanents>, Box<Players>),
    TheManaValueOfCommander(Commander),
    TheNumberOfAbilitiesAmongPermanents(Vec<CheckHasable>, Box<Permanents>),
    TheNumberOfCardTypesPermanentHas(Box<Permanent>),
    TheNumberOfCardsInHandRevealedThisWay,
    TheNumberOfCardsInPlayersHand(Box<Player>),
    TheNumberOfCardsOfTypeRevealedFromHandThisWay(Box<Cards>),
    TheNumberOfCardsPlayerShuffledIntoLibraryThisWay(Box<Player>),
    TheNumberOfCardsPutIntoHandThisWay,
    TheNumberOfCardsReturnedToTheBattlefieldThisWay,
    TheNumberOfChosenColorsItIs,
    TheNumberOfChosenColorsSpellIs(Box<Spell>),
    TheNumberOfColorsOfManaSpentToCastSpell(Box<Spell>),
    TheNumberOfColorsSpellIs(Box<Spell>),
    TheNumberOfCreaturesGoadedThisWay,
    TheNumberOfLettersInNameStickersOnPermanent(Letter, Box<Permanent>),
    TheNumberOfLibraryCardsOfTypeExiledThisWay(Box<Cards>),
    TheNumberOfNameStickersOnPermanent(Box<Permanent>),
    TheNumberOfNameStickersOnPermanentThatBeginWithLetter(Box<Permanent>, Letter),
    TheNumberOfNameStickersOnPermanentWithLength(Box<Permanent>, Box<Comparison>),
    TheNumberOfPermanentsOnTheBattlefield(Box<Permanents>),
    TheNumberOfPermanentsPutIntoAPlayersGraveyardThisWay(Box<Permanents>, Box<Players>),
    TheNumberOfPermanentsPutIntoPlayersGraveyardThisTurn(Box<Permanents>, Box<Player>),
    TheNumberOfPermanentsReturnedToHandThisWay,
    TheNumberOfPermanentsSacrificedThisWay,
    TheNumberOfPermanentsTappedThisWay,
    TheNumberOfPlayersThatDidntPayCost,
    TheNumberOfPlayersWhoTookAnActionThisWay,
    TheNumberOfPoisonCountersPlayerHas(Box<Player>),
    TheNumberOfRepeatedCostsNotPaid,
    TheNumberOfRepeatedCostsNotPaidByPlayer(Box<Player>),
    TheNumberOfSubtypesPermanentHas(Box<Permanent>),
    TheNumberOfSupertypesPermanentHas(Box<Permanent>),
    TheNumberOfUniqueVowelsOnNameSticker(NameStickerFilter),
    TheTotalNumberOfCardsInPlayersHands(Box<Players>),
    TheTotalNumberOfColorManaSymbolsInManaCostsOfTheCardsInHandRevealedThisWay(Color),
    TheTotalPowerOfAllStickersOnPermanents(Box<Permanents>),
    TheTotalToughnessOfAllStickersOnPermanents(Box<Permanents>),
    TheWinningBid,
    ThirdRoundedUp(Box<GameNumber>),
    Thrice(Box<GameNumber>),
    TotalLifeLostByPlayersThisTurn(Box<Players>),
    TotalManaValueOfEachPermanentDestroyedThisWay(Box<Permanents>),
    TotalManaValueOfMilledCards,
    TotalManaValueOfPermanents(Box<Permanents>),
    ToughnessOfDeadPermanent,
    ToughnessOfPermanent(Box<Permanent>),
    ToughnessOfTheRevealedCard,
    ToughnessOfTheSacrificedCreature,
    Trigger_AmountOfCards,
    Trigger_AmountOfCreatures,
    Trigger_AmountOfDamageDealt,
    Trigger_AmountOfLifeGained,
    Trigger_AmountOfLifeLost,
    Trigger_NumberOfCreatures,
    Trigger_ThatMuch,
    Twice(Box<GameNumber>),
    ValueX,
    WhenAPermanentEntersTheBattlefield_NumberOfColorsOfManaSpentToCast,
    WhenAnyNumberOfCreaturesDealCombatDamageToAnyNumberOfPlayers_AmountOfOpponentsDealtDamage,
    WhenAnyNumberOfPermanentEnterTheBattlefieldUnderControl_AmountOfPermanents,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Schemes", content = "args")]
pub enum Schemes {
    SingleScheme(SingleScheme),
    IsNonSupertype(SuperType),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ColorAndCreatureType", content = "args")]
pub enum ColorAndCreatureType {
    ColorAndCreatureType(Color, CreatureType),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_PlayerOrPermanent", content = "args")]
pub enum PlayerOrPermanent {
    Ref_AnyTarget,
    Ref_TargetPlayerOrPermanent,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Player", content = "args")]
pub enum Player {
    SelfPlayer,
    Trigger_ThatOtherPlayer,
    ThePlayerThatAddedThisAbility,
    TheGiftedPlayer,
    APlayerWouldMillAnyNumberOfCards_ThatPlayer,
    ThePlayerWhoGuessedThisWay,
    ThePlayerChosenThisWay,
    ControllerOfPermanentTheLastTimeItWasBlockedByPermanentThisTurn(Box<Permanent>, Box<Permanent>),
    NextOpponentInTurnOrder,
    AsLoseUnspentMana_ThePlayerLosingMana,
    ControllerOfEachableDestroyedPermanent,
    OwnerOfSpell(Box<Spell>),
    ThePlayerThatChoseTheMode,
    SingleControllerOfTargetPermanents,
    WouldGainLife_ThatPlayer,
    WouldPutAPermanentOnBattlefield_ThatPlayer,
    OwnerOfTheCardReturnedToHandThisWay,
    ActionForEachPlayer_ThatPlayer,
    ActionPlayer,
    AssociatedPlayerForPermanent(Box<Permanent>),
    AttackingPlayer,
    ClashOpponent,
    Condition_ThatPlayer,
    ControllerOfAbility(Ability),
    ControllerOfDeadPermanent,
    ControllerOfDestroyedPermanent,
    ControllerOfEachableExiledPermanent,
    ControllerOfEachableRemovedPermanent,
    ControllerOfLeavingPermanent,
    ControllerOfPermanent(Box<Permanent>),
    ControllerOfSpell(Box<Spell>),
    ControllerOfSpellOrAbility(SpellOrAbility),
    ControllerOfTargetPermanent,
    ControllerOfTargetPermanent2,
    ControllerOfTargetSpell,
    ControllerOfTriggeredAbility(Ability),
    DealsDamage_ThatPlayer,
    DefendingPlayer,
    EachPlayerAction_ThatPlayer,
    EachablePlayer,
    HostController,
    HostPlayer,
    ItsController,
    LoseLifeForEach_ThatPlayer,
    LoseLife_ThatPlayer,
    MillCards_ThatPlayer,
    NearestPlayerInChosenDirection(Box<Players>),
    NumPlayers_ThatPlayer,
    OpponentToTheLeftOfYou,
    OwnerOfDeadPermanent,
    OwnerOfExiledCard(Box<CardInExile>),
    OwnerOfGraveyrdCard(CardInGraveyard),
    OwnerOfPermanent(Box<Permanent>),
    OwnerOfTargetPermanent,
    PlayerAction_ThatPlayer,
    PlayerCreatureIsAttacking(Box<Permanent>),
    PlayerInTheChosenDirectionOf(Box<Player>),
    PlayerOrControllerOfPermanent(PlayerOrPermanent),
    PlayerOrControllerOfPlaneswalkerCreatureIsAttacking(Box<Permanent>),
    PlayerToTheLeftOf(Box<Player>),
    PlayerToTheRightOf(Box<Player>),
    PlayersRevealTopCardOfLibraryAndFindHighestManaValue_SingleWinner,
    Ref_TargetPlayer,
    Ref_TargetPlayer1,
    Ref_TargetPlayer2,
    Ref_TargetPlayer3,
    Ref_TargetPlayers_0,
    Ref_TargetPlayers_1,
    RememberedPlayer,
    SingleGraveyardOwner,
    SingleTargetPlayerOfSpell(Box<Spell>),
    SpellDealsDamage_ThatPlayer,
    ThatSpellsController,
    TheActivePlayer,
    TheAttackingPlayer,
    TheChosenPlayer,
    TheFirstPlayerChosenThisWay,
    TheMonarch,
    TheOtherChosenPlayer(Box<Player>),
    ThePlayerThatChoseAction,
    ThePlayerWhoControlsTheMostPermanents(Box<Permanents>),
    ThePlayerWhoCreatedThisAbility,
    ThePlayerWhoExiledTheCardWithTheHighestManaValue,
    ThePlayerWithTheInitiative,
    ThePlayerWithTheMostCardsInHand,
    ThePlayerWithTheMostLife,
    TheSecondPlayerChosenThisWay,
    TheThirdPlayerChosenThisWay,
    Trigger_ControllerOfThatPermanent,
    Trigger_ControllerOfThatSource,
    Trigger_ControllerOfThatSpell,
    Trigger_ControllerOfThatSpellOrAbility,
    Trigger_ControllerOfThoseCreatures,
    Trigger_DefendingPlayer,
    Trigger_ThatPlayer,
    // WhenAPlayerAttacksAnotherPlayer_ThatOtherPlayer,
    WinningBidder,
    WouldDealDamage_ControllerOfDamageSource,
    WouldDealDamage_DamageRecipientPlayer,
    WouldDrawACard_ThatPlayer,
    You,
    ControllerOfLastSpellThatDealtDamageToPlayerThisTurn(Box<Spells>, Box<Player>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Players", content = "args")]
pub enum Players {
    And(Vec<Players>),
    Or(Vec<Players>),

    CastASpellFromAnywhereOtherThanTheirHandThisTurn(Box<Spells>),

    HasPutACounterOnAPermanentThisTurn(Box<Permanents>),

    HasWaterEarthFireAndAirBendedThisTurn,
    OwnsASpell(Box<Spells>),
    PlayedALandFromAnywhereOtherThanTheirHandThisTurn,
    WasDealtAnAmountOfCombatDamageThisTurn(Box<Comparison>),

    OwnedAPermanentChosenThisWay,

    DoesntHaveMaxSpeed,
    HasMaxSpeed,
    SpeedIs(Box<Comparison>),

    HasNotActivatedAnExhaustAbilityThisTurn,
    WasDealtCombatDamageByAPermanentThisTurn(Box<Permanents>),

    YourTeam,
    AllCardsInHandAre(Box<CardsInHand>, Box<CardsInHand>),
    DrewACardLastTurn,
    OwnersOfExiledCards(Box<CardsInExile>),
    ChoseAFirstPermanentThisWay,
    ChoseASecondPermanentThisWay,
    ShuffledLibraryThisWay,
    PaidOrLostAnAmountOfEnergyThisTurn(Box<Comparison>),
    CommitedACrimeThisTurn,
    HasARadCounter,
    AttackedPlayerThisTurn(Box<Player>),
    AttackedAPlaneswalkerThisTurn(Box<Permanents>),
    IsntBeingAttacked,
    HasABoon,
    HasNoRadCounters,
    IsTheStartingPlayer,
    Descended,
    ControlledAPermanentReturnToHandThisWay,
    GuessedCorrectlyForDraftCard(NameString),
    APlayerNotedByPlayerForCardDuringDraft(Box<Player>, NameString),
    DidntPlayACardFromExileThisTurn(Box<Cards>),
    HasntCastASpellThisGame(Box<Spells>),
    HasNoCardsOfTypeInHand(Box<Cards>),
    HadANumberOfCardsEnterTheirGraveyardFromAnywhereThisTurn(Box<Comparison>, Box<Cards>),
    WasDealtDamageByNumPermantsThisTurn(Box<Comparison>, Box<Permanents>),
    SearchedTheirLibraryThisTurn,
    WasDealtDamageBySpellThisTurn(Box<Spells>),
    ControlledAPermanentExiledThisWay,
    ShuffledAPermanentIntoTheirLibraryThisWay(Box<Permanents>),
    WasDealtDamageThisWay,
    IsAttackingNumberPlayers(Box<Comparison>, Box<Players>),
    DidntLoseLifeThisTurn,
    IsTheirUpkeep,
    DoesntControlPermanent(Box<Permanent>),
    HasntBeenDealtCombatDamageSinceTheirLastTurn,
    IsProtectingBattle(Box<Permanent>),
    OneOfTheChosenPlayers,
    PlaneswalkedToAPlaneThisTurn(Planes),
    VotedForWord(VoteOption),
    VotedForADifferentChoiceThanPlayer(Box<Player>),
    VotedForTheSameChoiceAsPlayer(Box<Player>),
    ReceivedAVote,
    DidntReceiveAVote,
    RolledHighestD20Value,
    RolledNumberDiceThisTurn(Box<Comparison>),
    RolledADiceValueThisTurn(Box<Comparison>),
    NumPlayersPassFilter_ThosePlayers,
    VisitedAnAttractionThisTurn,
    CycledANumberOfCardsThisGame(Box<Comparison>, Box<Cards>),
    ControlledAPermanentShuffledIntoLibraryThisWay,
    NumCardsOwnedInExileIs(Box<Comparison>, CardsInExile),
    DidntActivateAnAbilityThisTurn(Box<ActivatedAbilities>),
    DidntSacrificeAPermanentThisWay(Box<Permanents>),
    DefendingPlayerThisCombat,
    ChoseWord(VoteOption),
    CycledANumberOfCardsThisTurn(Box<Comparison>, Box<Cards>),
    AttackedByPlayerThisCombat(Box<Player>),
    NumOpponentsIs(Box<Comparison>),
    EveryCardInTheirCardPoolStartedTheGameInTheirLibraryOrTheCommandZone,
    CouldMulligan,
    ControlsPermanent(Box<Permanent>),
    SacrificedAPermanentThisTurn(Box<Permanents>),
    SurveilledThisTurn,
    DevotionToColorsIs(ColorList, Box<Comparison>),
    HasPutANumberOfCountersOfTypeOnAPermanentThisTurn(
        Box<Comparison>,
        CounterType,
        Box<Permanents>,
    ),
    DidntAttackPlayerThisTurn(Box<Player>),
    AControllerOfTheLeastPermanentsAmongPlayers(Box<Players>, Box<Permanents>),
    IsAttackingPlayer(Box<Player>),
    PlayerDealtDamageThisWay,
    TheChosenPlayers,
    ControlsLessPermanentsThanEachPlayer(Box<Players>, Box<Permanents>),
    IsAttackedByPlayer(Box<Player>),
    HasntCastASpellThisTurn(Box<Spells>),
    AttackedByPlayerThisTurn(Box<Player>),
    Trigger_IsDefendingPlayer,
    ControlsNumThatShareAName(Box<Comparison>, Box<Permanents>),
    HasNotCompletedDungeon(NameString),
    HasBeenTemptedByTheRingNumberTimes(Box<Comparison>),
    SpellDefendingPlayer,
    WasDealtDamageByPermanentThisTurn(Box<Permanent>),
    IsNotTheStartingPlayer,
    AttackedPlayerLastTurn(Box<Player>),
    WasDealtCombatDamageThisTurn,
    IsNotAttackingAPlayer(Box<Players>),
    NumCardsInHandAtBeginningOfTurnWas(Box<Comparison>),
    ControlsNumBasicLandTypes(Box<Comparison>, Box<Permanents>),
    DidntCastASpellThisTurn(Box<Spells>),
    WasDealtCombatDamageByPermanentThisTurn(Box<Permanent>),
    WasDealtDamageByPermanentThisCombat(Box<Permanent>),
    WasDealtDamageByPermanentThisGame(Box<Permanent>),
    TappedAPermanentForManaThisTurn(Box<Permanents>),
    HadAPermanentEnterTheBattlefieldUnderTheirControlLastTurn(Box<Permanents>),
    HaventAddedManaWithThisAbility,
    Ref_TargetPlayers,
    HasAFullParty,
    CastASpellThisGame(Box<Spells>),
    WasDealtCombatDamageByNumPermanentsThisTurn(Box<Comparison>, Box<Permanents>),
    AttackedPlayerDuringTheirLastTurn(Box<Player>),
    AttackedByACreatureThisTurn(Box<Permanents>),
    IsTheirMainPhase,
    HasNoCardsOfTypeInLibrary(Box<Cards>),
    CastASpellFromAGraveyardThisTurn(Box<Spells>, Box<Players>),
    HasActivatedAnAbilityOfAGraveyardCardThisTurn(Box<Players>),
    CastASpellSincePlayersLastTurnEnded(Box<Spells>, Box<Player>),
    HasANumberOfCardsAmongCardsInGraveyardHandAndLibrary(Box<Comparison>, Box<Cards>),
    DidntAttackWithCreaturesThisTurn,
    NumSpellsCastLastTurnIs(Box<Comparison>, Box<Spells>),
    CastNumSpellsThisTurn(Box<Comparison>, Box<Spells>),
    CastASpellDuringTheirLastTurn(Box<Spells>),
    PutAPermanentOnBattleDuringTheirLastTurn(Box<Permanents>),
    OwnsAndControls(Box<Permanent>),
    OwnsAndControlsA(Box<Permanents>),
    AttackedWithCreaturesThisTurn,
    PlayerWhoTookActionThisWay,
    AttackedWithACreatureThisTurn(Box<Permanents>),
    AttackedWithCreatureThisTurn(Box<Permanent>),
    WasntAttackedByCreatureDuringPlayersLastCombat(Box<Permanent>, Box<Player>),
    ControlsMorePermanentThanPlayer(Box<Player>, Box<Permanents>),
    WasDealtDamageThisTurn,
    AttackedWithNumCreaturesThisTurn(Box<Comparison>, Box<Permanents>),
    DiscardedACardThisTurn,
    CompletedADungeon,
    WasDealtAnAmountOfDamageThisTurn(Box<Comparison>),
    PlayedALandThisTurn,
    NumCardTypesInGraveyardIs(Box<Comparison>),
    GainedLifeThisTurn,
    LostLifeLastTurn,
    HasTheInitiative,
    IsNotTheirTurn,
    ChoseHighestNumber,
    DidntPlayALandThisTurn,
    IsAttacked,
    Poisoned,
    ControlsNumWithDifferentNames(Box<Comparison>, Box<Permanents>),
    ControlsNumThatShareACreatureType(Box<Comparison>, Box<Permanents>),
    WasTheMonarchAsTheTurnBegan,
    GainedLifeAmountThisTurn(Box<Comparison>),
    OwnsACardInExile(Box<CardsInExile>),
    HasACardInHand(Box<CardsInHand>),
    HasACardInGraveyard(Box<CardsInGraveyard>),
    ControlsNumColorsOfPermanents(Box<Comparison>),
    NumCardtypesOnBattlefiendAndInGraveyardIs(Box<Comparison>),
    AttackedWithCreaturesWithTotalPowerThisCombat(Box<Comparison>),
    CastASpellThisTurn(Box<Spells>),
    HadAPermanentEnterTheBattlefieldUnderTheirControlThisTurn(Box<Permanents>),
    ControlsALandOfEachBasicLandType,
    ControlsAPermanentOfEachColor(Box<Permanents>),
    HasTheCitysBlessing,
    IsTheirTurn,
    ControlsNumWithDifferentPowers(Box<Comparison>, Box<Permanents>),
    NumCardsInGraveyardIs(Box<Comparison>, Box<Cards>),
    SacrificedNumPermanentsThisTurn(Box<Comparison>, Box<Permanents>),
    NumCardsOfTypeInHandIs(Box<Comparison>, Box<CardsInHand>),
    NumCardsInLibraryIs(Box<Comparison>),
    AttackedThisTurn,
    CreatedATokenThisTurn,
    LostLifeAmountThisTurn(Box<Comparison>),
    ControlsMorePermanentsThanEachPlayer(Box<Players>, Box<Permanents>),
    HasHighestNumberOfCardsInHandAmongPlayers(Box<Players>),
    IsTheMonarch,
    IsNotTheMonarch,
    AttackedByCreatureThisTurn(Box<Permanent>),
    NumCardsDrawnThisTurnIs(Box<Comparison>),
    HadANumberOfPermanentsEnterTheBattlefieldUnderTheirControlThisTurn(
        Box<Comparison>,
        Box<Permanents>,
    ),
    LifeTotalIs(Box<Comparison>),
    ControlsNum(Box<Comparison>, Box<Permanents>),
    HasANumberOfPoisonCounters(Box<Comparison>),
    NumCardsInHandIs(Box<Comparison>),
    LostLifeThisTurn,
    ControlsLessPermanentThanPlayer(Box<Player>, Box<Permanents>),
    DidntDiscardedACardOfTypeThisWay(Box<Cards>),
    DidntWinTheSubgameThisWay,
    ControlledAPermanentDestroyedThisWay,
    DiscardedACardWithTheHighestManaValueAmongCardsDiscardedThisWay,
    ChoseLowestNumber,
    OwnsAPermanent(Box<Permanents>),
    AControllerOfTheMostPermanentsAmongPlayers(Box<Players>, Box<Permanents>),
    SacrificedAPermanentThisWay(Box<Permanents>),
    OwnerOfACardWithTheLowestManaValueRevealedThisWay(Box<Cards>),
    DiscardedACardOfTypeThisWay(Box<Cards>),
    ChoseAPermanentThisWay,
    ControlsAll(Box<Permanents>),
    ControlsA(Box<Permanents>),
    ControlsNo(Box<Permanents>),
    WasDealtCombatDamageByAPermanentThisGame(Box<Permanents>),
    CoinCameUpTails,
    PaidCost,
    SinglePlayer(Box<Player>),
    OpponentOf(Box<Player>),
    Other(Box<Player>),
    ExceptFor(Box<Players>),
    DidntPayCost,
    Opponent,
    DiscardedACardThisWay,
    AnyPlayer,
    Trigger_ThosePlayers,
    PossibleDefendingPlayerThisCombat,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Spell", content = "args")]
pub enum Spell {
    TheNthSpellCastByPlayerThisTurn(Box<GameNumber>, Box<Spells>, Box<Player>),
    Trigger_ThatSpell,
    TheSpellThatGrantedThisAbility,
    TheSpellExiledThisWay,
    ASpellWouldBeCountered_ThatSpell,
    ThatSpell,
    TheSpellCastThisWay,
    TheResolvedSpellChosenThisWay,
    Ref_TargetSpell,
    TheSpellMostRecentlyCastThisTurn,
    DecreaseSpellCost_ThatSpell,
    ThatEnteringPermanent,
    TheCopiedSpell,
    ThisSpell,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Spells", content = "args")]
pub enum Spells {
    ExceptFor(Box<Spells>),
    And(Vec<Spells>),
    Not(Box<Spells>),
    Or(Vec<Spells>),
    Other(Box<Spell>),
    AnySpell,

    // ManaAmountOfTypeWasSpentToCastIt", Box<Comparison>, ManaProduceSymbol: "Colorless, // FIXME: ManaAmountOfTypeWasSpentToCastIt / Colorless
    // ManaAmountOfTypeWasSpentToCastIt", Box<Comparison>, ManaProduceSymbol: color }       // FIXME: ManaAmountOfTypeWasSpentToCastIt / Color
    ManaAmountOfTypeWasSpentToCastIt(Box<Comparison>, Color),

    HasColorManaSymbolInManaCost(Color),
    HasHybridManaInCost,
    PowerIsLessThanToughness,

    SneakCostWasPaid,
    WasCastForItsWarpCost,
    DoesntHaveAbility(CheckHasable),
    IsNonEnchantmentType(EnchantmentType),
    DoesntShareANameWithACardInPlayersLibrary(Box<Player>),
    HasAbility(CheckHasable),
    ManaSpentIsLessThanManaValue,
    HasAnAdventure,
    HasNoAbilities,
    HasXInManaCost,
    IsACommander,
    IsAllColors,
    IsAnOutlaw,
    IsArtifactType(ArtifactType),
    IsCardtype(CardType),
    IsCreatureTypeVariable(CreatureTypeVariable),
    IsCardtypeVariable(CardtypeVariable),
    IsColor(Color),
    IsColored,
    IsColorless,
    IsCreatureType(CreatureType),
    IsEnchantmentType(EnchantmentType),
    IsHistoric,
    IsMonocolored,
    IsMulticolored,
    IsNamed(NameFilter),
    IsNonCardtype(CardType),
    IsNonColor(Color),
    IsNonCreatureType(CreatureType),
    IsNonSupertype(SuperType),
    IsNumberColors(Box<Comparison>),
    IsParty,
    IsPermanent,
    IsPlaneswalkerType(PlaneswalkerType),
    IsSpellType(SpellType),
    IsSupertype(SuperType),
    IsYourCommander,
    ManaValueIs(Box<Comparison>),
    PowerIs(Box<Comparison>),
    SharesACardtypeWithAnExiledCard(Box<CardsInExile>),
    SharesACardtypeWithExiledCard(Box<CardInExile>),
    SharesACardtypeWithSpell(Box<Spell>),
    SharesACardtypeWithTopOfAnyPlayersLibrary(Box<Players>),
    SharesAColorWith(Color),
    SharesAColorWithACardInPlayersGraveyard(Box<Cards>, Box<Player>),
    SharesAColorWithAPermanent(Box<Permanents>),
    SharesAColorWithExiledCard(Box<CardInExile>),
    SharesAColorWithSpell(Box<Spell>),
    SharesACreatureTypeWithPermanent(Box<Permanent>),
    SharesACreatureTypeWithYourCommander,
    SharesAManaValueWithExiledCard(Box<CardInExile>),
    SharesAManaValueWithSpell(Box<Spell>),
    SharesANameOriginallyPrintedInArabianNights,
    SharesANameWithAPermanent(Box<Permanents>),
    SharesANameWithASpellCastThisTurn,
    SharesANameWithAnExiled(Box<CardsInExile>),
    SharesANameWithCardInPlayersGraveyard(Box<Cards>, Box<Player>),
    SharesANameWithExiled(Box<CardInExile>),
    SharesANameWithPermanent(Box<Permanent>),
    SharesANameWithSpell(Box<Spell>),
    SharesANameWithTheCardRevealedThisWay,
    ToughnessIs(Box<Comparison>),
    AdditionalCostWasPaid,
    AlternateCostWasPaid(ManaCost),
    AmongCardsDrawByAPlayerThisTurn(Box<Players>),
    AmongCardsDrawByPlayerThisTurn(Box<Player>),
    AnAmountOfManaFromPermanentSpentWasToCastIt(Box<Comparison>, Box<Permanents>),
    AnAmountOfManaWasSpentToCastIt(Box<Comparison>),
    CanTargetOnly(Box<Permanents>),
    CastByAPlayer(Box<Players>),
    CastByPlayer(Box<Player>),
    CastByPlayerFromHand(Box<Player>, Box<Player>),
    ControlledByAPlayer(Box<Players>),
    DoesntTargetAPermanent(Box<Permanents>),
    HasASingleTarget,
    HasPhyrexianInManaCost,
    HasXInCost,
    IntensityIs(Box<Comparison>),
    IsCard,
    IsFaceDown,
    IsModal,
    IsntTheTargetOfAnAbility(Abilities),
    ManaFromAPermanentWasSpentToCastIt(Box<Permanents>),
    ManaFromATeasureWasSpentToCastIt,
    ManaFromTeasureWasSpentToCast,
    ManaWasSpentToCastIt(Vec<ManaProduce>),
    ManaWasntSpentToCastIt(ManaProduce),
    NoColoredManaWasSpentToCastIt,
    NoManaWasSpentToCastIt,
    OwnedByAPlayer(Box<Players>),
    PlayerChoseAPermanentAsCast(Box<Player>, Box<Permanents>),
    PlayerControlledAPermanentAsCast(Box<Player>, Box<Permanents>),
    PlayerRevealedACardAsCast(Box<Player>, Box<Cards>),
    ProwlCostWasPaid,
    Ref_TargetSpells,
    SingleSpell(Box<Spell>),
    SnowManaOfSpellsColorWasSpentToCastIt,
    SurgeCostWasPaid,
    TargetsAPermanent(Box<Permanents>),
    TargetsAPlayer(Box<Players>),
    TargetsOnlyASinglePermanent(Box<Permanents>),
    TargetsOnlyASinglePermanentOrPlayer,
    TargetsOnlyASinglePlayer(Box<Players>),
    TargetsOnlyASingleTarget,
    TargetsOnlySinglePermanent(Box<Permanent>),
    TargetsPermanent(Box<Permanent>),
    TargetsPlayer(Box<Player>),
    TargetsSpell(Box<Spell>),
    TheNthSpellCastByPlayerThisTurn(Box<GameNumber>, Box<Spells>, Box<Player>),
    TheNthSpellCastThisTurn(Box<GameNumber>),
    TheSpellsCastThisWay,
    WasBargained,
    WasCastByPlayerDuringTheirMainPhase,
    WasCastFromAPlayersGraveyard(Box<Players>),
    WasCastFromAmongCardsInExile(Box<CardsInExile>),
    WasCastFromAmongCardsPutIntoTheirHandThisTurn,
    WasCastFromExile,
    WasCastFromPlayersHand(Box<Player>),
    WasCastFromTheirHand,
    WasCastFromTheirLibrary,
    WasForetold,
    WasKicked,
    WasKickedWithKicker(ManaCost),
    WasntCast,
    WasntCastFromExile,
    WasntCastFromTheirHand,
    WouldDestroyAPermanent(Box<Permanents>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Planes", content = "args")]
pub enum Planes {
    SinglePlane(Plane),
    IsNamed(NameFilter),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Phenomena", content = "args")]
pub enum Phenomena {
    SinglePhenomenon(Phenomenon),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Phenomenon", content = "args")]
pub enum Phenomenon {
    ThisPhenomenon,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_CardsInExile", content = "args")]
pub enum CardsInExile {
    And(Vec<CardsInExile>),
    Or(Vec<CardsInExile>),
    Not(Box<CardsInExile>),
    Other(Box<CardInExile>),

    SingleExiledCard(Box<CardInExile>),

    AnyCard,
    AnyExiledCard,

    InExile,

    IsFaceUp,
    IsFaceDown,

    IsWarped,
    IsForetold,
    IsSuspended,

    IsNamed(NameFilter),
    SharesANameWithThePlayedCard,
    SharesANameWithSpell(Box<Spell>),
    SharesANameWithAnExiled(Box<CardsInExile>),

    HasAbility(CheckHasable),
    DoesntHaveAbility(CheckHasable),
    HasAnAdventure,

    IsColor(Color),

    ManaValueIs(Box<Comparison>),

    IsCardtype(CardType),
    IsCreatureType(CreatureType),
    IsNonCardtype(CardType),
    IsNonEnchantmentType(EnchantmentType),
    IsPermanent,
    IsSupertype(SuperType),

    OwnedByAPlayer(Box<Players>),

    HadCountersPutOnItThisWay,
    HasACounterOfType(CounterType),
    HasNoCountersOfType(CounterType),

    InTheChosenPile,
    InTheExiledPileChosenThisWay,
    InTheExiledPileNotChosenThisWay,
    TheCardsConjuredThisWay,
    TheCardsExiledByPlayerThisWay(Box<Player>),
    TheCardsExiledThisWay,
    TheExiledCards,
    TheExiledCardsChosenThisWay,
    TheExiledPileChosenThisWay,
    TheNonSpecificCardsExiledThisWay,
    TheOtherPermanentsExiledThisWay,
    ThePilesExiledThisWay,
    TheSpecificCardsExiledThisWay,
    Trigger_ThoseExiledCards,
    UsedToCraftPermanent(Box<Permanent>),
    WasExiledByPlayer(Box<Player>),
    WasExiledByPlayerForDraftCard(Box<Player>, NameString),
    WasExiledByPlayerThisWay(Box<Player>),
    WasExiledByPlayerWithPermanent(Box<Player>, Box<Permanent>),
    WasExiledByPlayerWithPermanentThisTurn(Box<Player>, Box<Permanent>),
    WasExiledThisTurn,
    WasExiledThisWay,
    WasExiledWithAnAbility(Abilities),
    WasExiledWithDeadPermanent,
    WasExiledWithPermanent(Box<Permanent>),
    WasExiledWithPermanentsDelveAbility(Box<Permanent>),
    WasExiledWithPlane(Plane),
    WasTurnedFaceUpThisWay,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_CardInExile", content = "args")]
pub enum CardInExile {
    TheLastExiledCard,
    Ref_TargetExiledCard,
    TheCardConjuredIntoExileThisWay,
    Ref_TargetExiledCard1,
    TheSecondCardExiledThisWay,
    Ref_TargetExiledCard2,
    TheExiledCardChosenThisWay,
    EachableExiled,
    TopCardOfExiledPile,
    WhenAPermanentIsExiled_ThatExiledPermanent,
    TheExiledDeadPermanent,
    TheExiledTopOfLibrary,
    TheOtherExiledCard(Box<CardsInExile>),
    ThisExiledPermanentCard,
    TheCardExiledThisWay,
    TheChosenExiledCard,
    TheExiledCard,
    TheExiledCardFoundThisWay,
    TheFirstCardExiledThisWay,
    TheSingleCardExiledThisWay,
    TheSinglePermanentExiledThisWay,
    TheSpecificCardExiledThisWay,
    ThisExiledCard,
    Trigger_ThatExiledCard,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_CardsInHand", content = "args")]
pub enum CardsInHand {
    And(Vec<CardsInHand>),
    Or(Vec<CardsInHand>),
    AnyCard,
    ExceptFor(Box<CardsInHand>),
    Other(CardInHand),
    SingleCardInHand(CardInHand),

    TotalPowerAndToughnessIs(Box<Comparison>),
    DoesntHaveAbility(CheckHasable),
    IsColorless,
    IsParty,
    IsSpellType(SpellType),
    IsLandType(LandType),
    IsMulticolored,
    IsColor(Color),
    IsArtifactType(ArtifactType),
    IsHistoric,
    IsSupertype(SuperType),
    SharesACreatureTypeWithPermanents(Box<Permanents>),
    HasAbility(CheckHasable),
    IsCardtype(CardType),
    IsCreatureType(CreatureType),
    IsCreatureTypeVariable(CreatureTypeVariable),
    IsEnchantmentType(EnchantmentType),
    IsNonCardtype(CardType),
    IsPermanent,
    IsNamed(NameFilter),
    ManaCostIsSubsetOfManaPaidForThisAbility,
    ManaValueIs(Box<Comparison>),
    ToughnessIs(Box<Comparison>),
    PowerIs(Box<Comparison>),
    SharesACardtypeWithSpell(Box<Spell>),
    SharesANameWithSpell(Box<Spell>),
    IsNonColor(Color),
    SharesANameWithAnotherCardInHandRevealedThisWay,
    TheCardsConjuredIntoHandThisWay,
    TheCardsConjuredThisWay,
    TheCardsDraftedThisWay,
    TheCardsInHandChosenThisWay,
    TheCardsInHandNotChosenThisWay,
    TheCardsOfTypeRevealedThisWay(Box<Cards>),
    TheCardsReturnedToHandThisWay,
    TheCardsSeekedThisWay,
    TheChosenCardsInHand,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_SpellsAndAbilities", content = "args")]
pub enum SpellsAndAbilities {
    AnySpellOrAbility,
    And(Vec<SpellsAndAbilities>),
    Or(Vec<SpellsAndAbilities>),

    Ref_TargetSpellsAndAbilities,
    ControlledByAPlayer(Box<Players>),
    HasXInCost,
    ManaFromATeasureWasSpentToCastItOrActivateIt,
    NotAnAbilityOfAPermanent(Box<Permanents>),
    HasASingleTarget,
    HasOneOrMoreTargets,
    TargetsAPermanent(Box<Permanents>),
    TargetsOnlyASinglePermanentOrPlayer,
    TargetsPermanent(Box<Permanent>),
    TargetsPlayer(Box<Player>),
    ActivatedAbility,
    LoyaltyAbility,
    TriggeredAbility,
    IsSpell(Box<Spells>),
}

// FIXME: Can I replace this with check_hasable? Most places are saying you can't activate abilities, which aren't on the stack or activated yet...}
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ActivatedAbility", content = "args")]
pub enum ActivatedAbility {
    Trigger_ThatActivatedAbility,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ActivatedAbilities", content = "args")]
pub enum ActivatedAbilities {
    And(Vec<ActivatedAbilities>),
    Or(Vec<ActivatedAbilities>),
    AnyAbility,

    NonManaAbility,
    ManaAbility,

    TargetsAPlayer(Box<Players>),
    TargetsOnlySinglePermanent(Box<Permanent>),
    TargetsAPermanent(Box<Permanents>),
    TargetsPermanent(Box<Permanent>),

    NinjutsuAbility,
    AbilityOfACardInPlayersGraveyard(Box<Cards>, Box<Player>),
    AbilityOfASource(AbilitySources),
    FirstAbilityActivatedByPlayerThisTurn(Box<ActivatedAbilities>, Box<Player>),
    AbilityOfAPermanent(Box<Permanents>),
    AbilityOfPermanent(Box<Permanent>),
    EternalizeAbility,
    EmbalmAbility,
    DoesntHaveTapSelfInCost,
    EquipAbility,
    BoastAbility,
    OutlastAbility,
    ExhaustAbility,

    HasXInCost,
    HasTapSelfInCost,

    NonLoyaltyAbility,
    LoyaltyAbility,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_AbilitySources", content = "args")]
pub enum AbilitySources {
    And(Vec<AbilitySources>),
    Or(Vec<AbilitySources>),

    IsAnOutlaw,
    IsCreatureType(CreatureType),
    IsCreatureTypeVariable(CreatureTypeVariable),
    IsNamed(NameFilter),
    ControlledByAPlayer(Box<Players>),
    IsCardtype(CardType),
    IsColor(Color),
    IsNonColor(Color),
    IsColorless,
    IsNotACommander,
    IsSupertype(SuperType),
    IsNonCardtype(CardType),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Dungeon", content = "args")]
pub enum SingleDungeon {
    OwnedByAPlayer(Box<Players>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Abilities", content = "args")]
pub enum Abilities {
    Other(Ability),
    And(Vec<Abilities>),
    Or(Vec<Abilities>),
    AnyAbility,

    TargetsAPermanent(Box<Permanents>),
    TargetsOnlySinglePermanent(Box<Permanent>),
    HasASingleTarget,

    CanTargetOnly(Box<Permanents>),

    AbilityOfAnEmblem(Box<Emblem>),
    AbilityOfPermanent(Box<Permanent>),
    AbilityOfAPermanent(Box<Permanents>),
    AbilityOfASource(AbilitySources),
    AbilityOfASpell(Box<Spells>),

    IsCardtype(CardType),
    ControlledByAPlayer(Box<Players>),
    RoomAbilityOfDungeon(SingleDungeon),
    BackupAbility,
    ModularAbility,
    LoyaltyAbility,
    TriggeredAbility,
    ActivatedAbility,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Ability", content = "args")]
pub enum Ability {
    Trigger_ThatTriggeredAbility,
    Trigger_ThatActivatedAbility,
    Ref_TargetAbility,
    ThisAbility,
    Trigger_ThatAbility,
}
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_DamageSource", content = "args")]
pub enum DamageSource {
    Trigger_ThatPermanent,
    ThisDamageSource,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_DamageSources", content = "args")]
pub enum DamageSources {
    And(Vec<DamageSources>),
    Or(Vec<DamageSources>),
    Other(DamageSource),
    AnyDamageSource,

    IsCreatureTypeVariable(CreatureTypeVariable),
    ManaValueIs(Box<Comparison>),
    SharesAColorWithAColorOfManaSpendOnActivationCost,
    IsNonCreatureType(CreatureType),
    SharesAColorWithExiledCard(Box<CardInExile>),
    IsCreatureType(CreatureType),
    IsCardtype(CardType),
    IsNonCardtype(CardType),
    IsNotPermanentSource(Box<Permanent>),
    IsNamed(NameFilter),
    ControlledByAPlayer(Box<Players>),
    IsColor(Color),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Comparison", content = "args")]
pub enum Comparison {
    TheChosenQuality,
    NotTheChosenQuality,
    AnyManaValueAmongPermanents(Box<Permanents>),
    ANumberOfCardsInAPlayersGraveyard(Box<Cards>, Box<Players>),
    AnyManaValueAmongCardsInPlayersGraveyard(CardsInGraveyard, Box<Player>),
    OneOf(Vec<i32>),
    AnyNumber,
    Even,
    Odd,
    Prime,
    LessThanOrEqualTo(Box<GameNumber>),
    GreaterThanOrEqualTo(Box<GameNumber>),
    GreaterThan(Box<GameNumber>),
    LessThan(Box<GameNumber>),
    EqualTo(Box<GameNumber>),
    NotEqualTo(Box<GameNumber>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_CheckHasable", content = "args")]
pub enum CheckHasable {
    // ReplaceEvent(ReplacableEvent, Vec<Action>),
    And(Vec<CheckHasable>),

    ReplaceWouldDealDamage(
        ReplacableEventWouldDealDamage,
        Vec<ReplacementActionWouldDealDamage>,
    ),

    ThisAbility,
    OtherThanThisAbility,

    AbilityStickerAbility,
    StickerAbility,

    ActivatedAbility,
    LoyaltyAbility,
    NonManaAbility,
    HasTapSelfInCost,
    ExhaustAbility,

    AnyWarp,
    AnyAwaken,
    AnyBandsWithOthers,
    AnyBlitz,
    AnyCumulativeUpkeep,
    AnyCycling,
    AnyDisturb,
    AnyEmbalm,
    AnyEternalize,
    AnyFading,
    AnyFlashback,
    AnyForetell,
    AnyFreerunning,
    AnyHexproof,
    AnyKicker,
    AnyLandwalk,
    AnyMadness,
    AnyModular,
    AnyMorph,
    AnyMutate,
    AnyPartner,
    AnyProtection,
    AnyProtectionFromColor,
    AnyRampage,
    AnySuspend,
    AnyToxic,
    AnyUnearth,
    AnyVanishing,
    AnyWard,

    Banding,
    Cascade,
    Convoke,
    Deathtouch,
    Decayed,
    Defender,
    Devoid,
    Disguise,
    DoctorsCompanion,
    DoubleStrike,
    Fear,
    FirstStrike,
    Flanking,
    Flash,
    Flying,
    Haste,
    Horsemanship,
    Indestructible,
    Infect,
    LevelUp,
    Lifelink,
    ManaAbility,
    Menace,
    Phasing,
    Reach,
    Shadow,
    Shroud,
    Skulk,
    Soulbond,
    StartYourEngines,
    Trample,
    Vigilance,

    Enchant(Box<Permanents>),
    Landwalk(Box<Permanents>),
    ProtectionFromColor(Color),

    EachableAbility,
    TheChosenAbility,
    TriggeredAbility,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ChoosableColor", content = "args")]
pub enum ChoosableColor {
    AnyColor,

    Other(Color),
    ColorList(Vec<Color>),

    ColorsInPlayersHand(Box<Player>),
    ColorsOfCardsInPlayersGraveyard(Box<Cards>, Box<Player>),

    ColorAmoungPermanents(Box<Permanents>),
    NotColorAmoungPermanents(Box<Permanents>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ManaProduceSymbol", content = "args")]
pub enum ManaProduceSymbol {
    ManaProduceW,
    ManaProduceU,
    ManaProduceB,
    ManaProduceR,
    ManaProduceG,
    ManaProduceC,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ManaProduce", content = "args")]
pub enum ManaProduce {
    ManaProduceW,
    ManaProduceU,
    ManaProduceB,
    ManaProduceR,
    ManaProduceG,
    ManaProduceC,

    Or(Vec<ManaProduce>),
    And(Vec<ManaProduce>),

    ManaOfAChosenColor,
    EachColorAmongCardsUsedToCraftPermanent(Box<Permanent>),
    AnyManaColorChosenByPlayerDuringDraft(Box<Player>, NameString),
    LastNotedManaTypeAndAmount,
    ManaOfTheLastNotedType,
    OneManaOfEachColorInManaCostOfTheMilledCard,
    AnyOtherManaColor,
    AnyTwoDifferentManaColors,
    AnyColorManaSymbolInTheCardRevealedThisWay,
    AnyManaColorAmongCardsInAPlayersGraveyard(CardsInGraveyard, Box<Players>),
    AnyManaColorCircled,
    AnyManaColorOfPermanent(Box<Permanent>),
    TheManaLostThisWay,
    AnyManaColorOfExiledCard(Box<CardInExile>),
    AnyManaColorOfAnExiledCard(Box<CardsInExile>),
    Trigger_AnyManaTypeProduced,
    ManaCostOfPermanent(Box<Permanent>),
    AnyManaColorAmongPermanents(Box<Permanents>),
    AnyManaColorAPermanentCouldProduce(Box<Permanents>),
    AnyManaTypeAPermanentCouldProduce(Box<Permanents>),
    AnyManaTypePermanentCouldProduce(Box<Permanent>),
    EachManaColorAmongPermanents(Box<Permanents>),
    ManaOfTheChosenColor,
    AnyManaTypeTheSacrificedPermanentCouldProduce,
    AnyManaColor,
    AnyManaColorInCommanderColorIdentity,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ColorWord", content = "args")]
pub enum ColorWord {
    TheFirstChosenColorWord,
    TheSecondChosenColorWord,
}

type ColorWordVariable = ColorWord;

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_LandTypeWord", content = "args")]
pub enum LandTypeWord {
    TheFirstChosenLandType,
    TheSecondChosenLandType,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_AbilityVariable", content = "args")]
pub enum AbilityVariable {
    ThisAbility,
    TheChosenAbility,
    TheChosenAbilities,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_LayerEffect", content = "args")]
pub enum LayerEffect {
    // Layer 1
    SetCopiablePT(PT),
    AddCopiableCardtype(CardType),
    AddCopiableCreatureType(CreatureType),
    AddCopiableAbility(Vec<Rule>),
    IsACopyOfPermanentSpell(Box<Spell>, CopyEffects),
    IsACopyOfPermanent(Box<Permanent>, CopyEffects),
    IsACopyOfLibraryCard(CardInLibrary, CopyEffects),
    IsACopyOfGraveyardCard(CardInGraveyard, CopyEffects),
    IsACopyOfExiledCard(CardInExile, CopyEffects),
    IsACopyOfTheRevealedCard(CopyEffects),
    IsACopyOfThatCard(CopyEffects),

    // Layer 2
    SetController(Box<Player>),

    // Layer 3
    ReplaceColorWordVariableWithNewColorWordVariable(ColorWord, ColorWord),
    ReplaceLandTypeVariableWithNewLandTypeVariable(LandTypeWord, LandTypeWord),
    ReplaceCreatureTypeVariableWithNewCreatureType(CreatureTypeVariable, CreatureTypeWord),
    SetName(NameString),

    // Layer 4
    AddCardtype(CardType),
    RemoveCardtype(CardType),
    HasAllCreatureTypes,

    AddCreatureTypeVariable(CreatureTypeVariable),
    AddLandTypeVariable(LandTypeVariable),
    SetCreatureTypeVariable(CreatureTypeVariable),
    SetLandTypeVariable(LandTypeVariable),

    AddCreatureType(CreatureType),
    AddArtifactType(ArtifactType),
    AddEnchantmentType(EnchantmentType),
    AddLandType(LandType),
    AddSupertype(SuperType),
    RemoveSupertype(SuperType),
    SetArtifactType(ArtifactType),
    SetCardtype(CardType),
    SetCardtypes(Vec<CardType>),
    SetCreatureType(CreatureType),
    SetCreatureTypes(Vec<CreatureType>),
    SetLandType(LandType),
    RemoveArtifactType(ArtifactType),
    RemoveAllCreatureTypes,
    RemoveAllLandTypes,
    RemoveCreatureType(CreatureType),

    // Layer 5
    AddColor(SettableColor),
    SetColor(SettableColor),

    // Layer 6
    AddAbility(Vec<Rule>),
    AddAbilityVariable(AbilityVariable),
    AddAbility_ActivatedWithModifiers(Box<Cost>, Box<Actions>, ActivateModifier),
    AddActivatedAbilitiesAndMaySpendColorManaAsThoughAnyColorToActivate(
        Box<ActivatedAbilities>,
        Color,
    ),
    AddAbilityFromGraveyardCardHasable(CardInGraveyard, Vec<CheckHasable>),
    AddAbilityFromCardsInHandHasable(Box<CardsInHand>, Vec<CheckHasable>),
    AddAbilityAndLoseAllOtherAbilities(Vec<Rule>),
    AddAbilityIfItDoesntHaveIt(Vec<Rule>),
    AddAbilityFromCardsInPlayersGraveyardHasable(Box<Cards>, Box<Player>, Vec<CheckHasable>),
    AddAbilityFromPermanentHasable(Box<Permanent>, Vec<CheckHasable>),
    AddAbilityFromEachPermanentHasable(Box<Permanents>, Vec<CheckHasable>),
    LosesAbility(CheckHasable),
    LosesAllAbilities,

    // Layer 7
    SetPower(Box<GameNumber>),
    SetToughness(Box<GameNumber>),
    AdjustPTXY(ModX, ModY, Box<GameNumber>, Box<GameNumber>),
    SetPowerAndToughnessBoth(Box<GameNumber>),
    SwitchPT,
    SetPT(PT),
    #[serde(rename = "AdjustPT_TheChosenPTMod")]
    AdjustPTTheChosenPTMod,
    AdjustPT(i32, i32),
    AdjustPTX(ModX, ModX, Box<GameNumber>),
    AdjustPTForEach(i32, i32, Box<GameNumber>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_PTXValue", content = "args")]
pub enum PTXValue {
    Integer(i32),
    X,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_PT", content = "args")]
pub enum PT {
    PTX(PTXValue, PTXValue, Box<GameNumber>),
    PTOfGraveyardCard(CardInGraveyard),
    PTOfExiledCard(Box<CardInExile>),
    ManualPT(Box<GameNumber>, Box<GameNumber>),
    ZeroPT,
    PT(i32, i32),
    PTOfPermanent(Box<Permanent>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ModX", content = "args")]
pub enum ModX {
    Integer(i32),
    PlusX,
    MinusX,
    Zero,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ModY", content = "args")]
pub enum ModY {
    PlusY,
    MinusY,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_PTMod", content = "args")]
pub enum PTMod {
    PTMod(i32, i32),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Commanders", content = "args")]
pub enum Commanders {
    IsCardtype(CardType),
    ManaValueIs(Box<Comparison>),
    And(Vec<Commanders>),
    OwnedByAPlayer(Box<Players>),
    IsYourCommander,
    IsACommander,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_GraveyardCard", content = "args")]
pub enum CardInGraveyard {
    TheGraveyardCardChosenThisWay,
    TheChosenGraveyardCard,
    TheCardMilledThisWay,
    TheSacrificedPermanent,
    EnchantedGraveyardCard,
    TheCardConjuredIntoGraveyardThisWay,
    TopCardOfPlayersGraveyard(Box<Player>),
    Ref_TargetGraveyardCardInPlayersGraveyard(Box<Player>),
    ThisSacrificedPermanent,
    TheCardPutIntoGraveyardThisWay,
    TheLastGraveyardCardChosenThisWay,
    ThePermanentSacrificedThisWay,
    Trigger_ThatSacrificedPermanent,
    TopCardOfTypeOfPlayersGraveyard(Box<Cards>, Box<Player>),
    Trigger_ThatGraveyardCard,
    TheCardDiscardedThisWay,
    Ref_TargetGraveyardCard,
    Ref_TargetGraveyardCard1,
    Ref_TargetGraveyardCard2,
    Ref_TargetGraveyardCard3,
    Ref_TargetGraveyardCard4,
    Ref_TargetGraveyardCard5,
    ThisGraveyardCard,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_CardsInGraveyard", content = "args")]
pub enum CardsInGraveyard {
    AnyCardInAnyGraveyard,

    And(Vec<CardsInGraveyard>),
    Not(Box<CardsInGraveyard>),
    Or(Vec<CardsInGraveyard>),

    IsNonSpellType(SpellType),
    IsNumberColors(Box<Comparison>),
    SharesANameWithGraveyardCard(CardInGraveyard),

    Other(CardInGraveyard),
    SingleGraveyardCard(CardInGraveyard),

    DoesntHaveAbility(CheckHasable),
    Ref_TargetGraveyardCards1,
    Ref_TargetGraveyardCards2,

    InTheGraveyardPileChosenThisWay,
    TheDiscardedCardsChosenThisWay,
    ThePermanentsSacrificedThisWay,
    TheCardsPutIntoAGraveyardThisWay,
    DiedThisTurn(Box<Permanents>),
    TheGraveyardCardsNotChosenThisWay,
    GraveyardCardWithMostVotesOrTiedForMostVotes,
    TheChosenGraveyardCards,
    InTheGraveyardPileNotChosenThisWay,

    TheGraveyardCardsChosenThisWay,
    WasPutIntoGraveyardFromAnywhereOtherThanTheBattlefieldThisTurn,

    CanEnchantAPermanent(Box<Permanents>),
    DoesntSharesACardtypeWithSpell(Box<Spell>),
    HasASticker,
    HasAbility(CheckHasable),
    HasAnAdventure,
    HasAnArtSticker,
    HasNoAbilities,
    InAPlayersGraveyard(Box<Players>),
    IsAnOutlaw,
    IsArtifactType(ArtifactType),
    IsCardtype(CardType),
    IsColor(Color),
    IsColorless,
    IsCreatureType(CreatureType),
    IsCreatureTypeVariable(CreatureTypeVariable),
    IsEnchantmentType(EnchantmentType),
    IsHistoric,
    IsLandType(LandType),
    IsMonocolored,
    IsMulticolored,
    IsNamed(NameFilter),
    IsNonCardtype(CardType),
    IsNonCreatureType(CreatureType),
    IsNonEnchantmentType(EnchantmentType),
    IsNonSupertype(SuperType),
    IsNotNamed(NameFilter),
    IsPermanent,
    IsPlaneswalkerType(PlaneswalkerType),
    IsSpellType(SpellType),
    IsSupertype(SuperType),
    ManaValueIs(Box<Comparison>),
    Ref_TargetGraveyardCards,
    SharesANameWithSpell(Box<Spell>),
    PowerIs(Box<Comparison>),
    ToughnessIs(Box<Comparison>),
    TheTopNumberCardsOfTypeInPlayersGraveyard(Box<GameNumber>, Box<Cards>, Box<Player>),
    CardsOfTypeMilledThisWay(Box<Cards>),
    WasPutIntoGraveyardByPlayerThisWay(Box<Player>),
    WasntPutIntoGraveyardThisWay,
    WasPutIntoGraveyardThisWay,
    Trigger_ThoseGraveyardCards,
    WasAttachedToDeadPermanent,
    WasDiscardedIntoGraveyardThisTurn,
    WasMilledIntoGraveyardThisTurn,
    WasPutIntoGraveyardFromAnywhereThisTurn,
    WasPutIntoGraveyardFromLibraryThisTurn,
    WasPutIntoGraveyardFromTheBattlefieldThisTurn,
    WasSurveilledThisTurn,
    WasntPutIntoGraveyardThisCombat,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ColorList", content = "args")]
pub enum ColorList {
    AllColors,
    TheChosenColor,
    Colors(Vec<Color>),
    Colorless,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_CreatureTokenType", content = "args")]
pub enum CreatureTokenType {
    ArtifactCreatureToken,
    CreatureToken,
    EnchantmentArtifactCreatureToken,
    EnchantmentCreatureToken,
    LandCreatureToken,
}

type CreatureTokenSubtype = SubType;

// type CreatureTokenSubtypes = Vec<CreatureTokenSubtype>;
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_CreatureTokenSubtypes", content = "args")]
pub enum CreatureTokenSubtypes {
    CreatureTokenSubtypesList(Vec<CreatureTokenSubtype>),
    TheChosenCreatureType,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_LandTokenType", content = "args")]
pub enum LandTokenType {
    LandToken,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_LandTokenSubtypes", content = "args")]
pub enum LandTokenSubtypes {
    AllBasicLandTypes,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_CostPlayerAction", content = "args")]
pub enum CostPlayerAction {
    GainControlOfPermanentUntil(Box<Permanent>, Expiration),
    CreateTokens(Vec<CreatableToken>),
    GainControlOfPermanent(Box<Permanent>),
    DrawACard,
    LoseLife(Box<GameNumber>),
    DrawNumberCards(Box<GameNumber>),
    PutExiledCardIntoOwnersHand(Box<CardInExile>),
    PutGraveyardCardIntoHand(CardInGraveyard),
    ActivateAManaAbilityOfEachPermanentAndLoseUnspentMana(Box<Permanents>),
    PutTopOfLibraryInGraveyard,
    GainLife(Box<GameNumber>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_AttackAssignment", content = "args")]
pub enum AttackAssignment {
    ThePlayerOrPlaneswalkerChosenThisWay,
    Player(Box<Player>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Scheme", content = "args")]
pub enum SingleScheme {
    ThisScheme,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Vanguard", content = "args")]
pub enum SingleVanguard {
    ThisVanguard,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Plane", content = "args")]
pub enum Plane {
    ThisPlane,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_PlayersAndPermanents", content = "args")]
pub enum PlayersAndPermanents {
    APlayerOrAPermanent(Box<Players>, Box<Permanents>),
    Ref_AnyTargets,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_FutureTrigger", content = "args")]
pub enum FutureTrigger {
    WhenAPlayerNextAttacksThisTurn(Box<Players>),
    WhenPlayerNextActivatesAnAbilityThisTurn(Box<Player>, Box<ActivatedAbilities>),
    AtTheBeginningOfTheNextUpkeep,
    AtTheBeginningOfPlayersNextDeclareAttackersStep(Box<Player>),

    Or(Vec<FutureTrigger>),
    WhenAPlayerPlaneswalks(Box<Players>),
    WhenPlayerNextActivatesAnAbilityBySpendingAnAmountOfMana(
        Box<Player>,
        Box<ActivatedAbilities>,
        Box<Comparison>,
    ),
    WhenAPlayerNextActivatesAnAbilityThisTurn(Box<Players>, Box<ActivatedAbilities>),
    AtTheBeginningOfPlayersDeclareAttackersStepOnTheirNextTurn(Box<Player>),
    AtTheBeginningOfPlayersNextDrawStep(Box<Player>),
    AtTheBeginningOfPlayersNextMainPhase(Box<Player>),
    AtTheBeginningOfPlayersNextFirstMainPhase(Box<Player>),
    AtTheBeginningOfPlayersFirstMainPhaseOfTheGame(Box<Player>),
    AtNextEndOfCombatThisTurn,
    AtTheEndOfThisCombat,
    AtTheNextEndOfCombat,
    AtTheBeginningOfTheEndStepOfTheExtraTurnCreatedThisWay,
    AtTheBeginningOfPlayersEndStepNextTurn(Box<Player>),
    AtTheBeginningOfPlayersFirstUpkeep(Box<Player>),
    AtTheBeginningOfPlayersNextEndStep(Box<Player>),
    AtTheBeginningOfPlayersNextUpkeep(Box<Player>),
    AtTheBeginningOfTheFirstUpkeep,
    AtTheBeginningOfTheNextCleanupStep,
    AtTheBeginningOfTheNextCombatPhaseThisTurn,
    AtTheBeginningOfTheNextCombat,
    AtTheBeginningOfTheNextEndStep,
    AtTheBeginningOfTheNextMainPhaseThisTurn,
    AtTheBeginningOfTheNextTurnsUpkeep,
    WhenPlayerCastsTheirNextSpellOrActivatesTheirNextAbilityThisTurn(
        Box<Player>,
        SpellsAndAbilities,
    ),
    WhenPlayerCastsTheirNextSpellThisGame(Box<Player>, Box<Spells>),
    WhenPlayerCastsTheirNextSpellThisTurn(Box<Player>, Box<Spells>),
    WhenPlayerCastsTheirNextSpellFromTheirHandThisTurn(Box<Player>, Box<Spells>),
    WhenCreatureOrPlaneswalkerDies(Box<Permanent>),
    WhenPermanentBecomesUntapped(Box<Permanent>),
    WhenPermanentLeavesTheBattlefield(Box<Permanent>),
    WhenPermanentIsPutIntoAPlayersGraveyard(Box<Permanent>, Box<Players>),
    WhenPlayerLosesControlOfPermanent(Box<Player>, Box<Permanent>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_DamageRecipients", content = "args")]
pub enum DamageRecipients {
    EachPermanent(Box<Permanents>),
    EachPlayer(Box<Players>),
    Permanent(Box<Permanent>),

    Ref_AnyTarget,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_MultipleDamageRecipients", content = "args")]
pub enum MultipleDamageRecipients {
    MultipleRecipients(Vec<DamageRecipients>),
    EachPermanent(Box<Permanents>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_DamageRecipientsList", content = "args")]
pub enum DamageRecipientsList {
    APermanent(Box<Permanents>),
    APlayer(Box<Players>),
    APlayerOrAPermanent(Box<Players>, Box<Permanents>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_SingleDamageRecipient", content = "args")]
pub enum SingleDamageRecipient {
    Player(Box<Player>),
    DistributedAnyTarget,
    Ref_AnyTargets_1,
    Ref_AnyTargets_2,
    Ref_AnyTarget,
    Ref_TargetPlayerOrPermanent,
    Permanent(Box<Permanent>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_SingleDamageSource", content = "args")]
pub enum SingleDamageSource {
    TheChosenDamageSource,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_LookAtTopOfLibraryCost", content = "args")]
pub enum LookAtTopOfLibraryCost {
    And(Vec<LookAtTopOfLibraryCost>),

    PayLife(Box<GameNumber>),
    SacrificePermanent(Box<Permanent>),
    SacrificeAPermanent(Box<Permanents>),
    PayMana(ManaCost),
    PutTheRemainingCardsOnTheBottomOfLibraryInAnyOrder,
    PutACardOfTypeOntoTheBattlefield(Box<Cards>, Vec<ReplacementActionWouldEnter>),
    RevealACardOfType(Box<Cards>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_PerpetualEffect", content = "args")]
pub enum PerpetualEffect {
    AddAbilityFromPermanentHasable(Box<Permanent>, Vec<CheckHasable>),
    AddSupertype(SuperType),
    SetColor(SettableColor),
    SetManaCost(CardManaCost),

    Incorporate(ManaCost),
    SetCreatureTypes(Vec<CreatureType>),
    AddColor(SettableColor),

    AddAbility(Vec<Rule>),
    AddAbilityVariable(AbilityVariable),
    AddAbilityFromCardsHasable(Vec<CheckHasable>),

    AddArtifactType(ArtifactType),
    AddCardtype(CardType),
    AddCreatureType(CreatureType),
    AddLandType(LandType),
    AdjustPT(i32, i32),
    AdjustPTX(ModX, ModX, Box<GameNumber>),
    DoubleCreaturesPowerAndToughness,
    LosesAbility(CheckHasable),
    LosesAllAbilities,
    SetCardtype(CardType),
    SetCreatureType(CreatureType),
    SetPT(PT),
    SetPower(Box<GameNumber>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_LookAtTopOfLibraryAction", content = "args")]
pub enum LookAtTopOfLibraryAction {
    PutRemainingSetAsideCardsIntoHand,
    PutSetAsideCardsOfTypeOntoBattlefield(Box<Cards>, Vec<ReplacementActionWouldEnter>),
    MayPutUptoNumberGroupCardsOntoTheBattlefield(
        Box<GameNumber>,
        Box<Cards>,
        GroupFilter,
        Vec<ReplacementActionWouldEnter>,
    ),
    MayRevealUptoNumberCardsOfTypeAndSetAside(Box<GameNumber>, Box<Cards>),
    ExileNumberGenericCardsFaceDown(Box<GameNumber>),
    PutAnyNumberOfCardsOntoTheBattlefield(Box<Cards>, Vec<ReplacementActionWouldEnter>),
    MayAction(Box<LookAtTopOfLibraryAction>),
    PutTheRemainingCardsOnTopOfLibraryInAnyOrder,
    ShuffleAndPutTheRemainingCardsOnTopOfLibraryInAnyOrder,
    ConjureADuplicateOfCardOntoTheBattlefield(SingleCard, Vec<ReplacementActionWouldEnter>),
    RevealACardOfType(Box<Cards>),
    PutFoundCardOntoBattlefield(Vec<ReplacementActionWouldEnter>),
    MayRevealAndPutACardOfTypeOntoTheBattlefield(Box<Cards>, Vec<ReplacementActionWouldEnter>),
    MayPutFoundCardOntoBattlefield(Vec<ReplacementActionWouldEnter>),
    ExileAGenericCard,
    PutFoundCardIntoHand,
    ExileTheRemainingCardsFaceDown,
    CloakNumberGenericCards(Box<GameNumber>),
    CreateExiledCardEffect(CardInExile, Vec<ExiledCardEffect>),
    PutRemainingCardsInHand,
    ExileAnyNumberOfGenericCardsInAFaceDownPile,
    ExileTheRemainingCardsInAFaceUpPile,
    PutUptoNumberGenericCardsOnTopOfLibraryInAnyOrder(Box<GameNumber>),
    MayPutAnyNumberOfGroupCardsOntoTheBattlefield(
        Box<Cards>,
        GroupFilter,
        Vec<ReplacementActionWouldEnter>,
    ),
    APlayerChoosesAPileTopPutIntoHand(Box<Players>),
    ExileAGenericCardWithACounter(CounterType),
    MayExileUptoNumberCardsOfType(Box<GameNumber>, Box<Cards>),
    PutAGenericCardAndAllCardsWithTheSameNameIntoHand,
    LoseLifeForEach(Box<GameNumber>, Box<GameNumber>),
    ExileAGenericCardFaceDown,
    ExileAnyNumberOfGenericCards,
    ExileTheRemainingCards,
    ManifestAGenericCard,
    MayExileACardOfType(Box<Cards>),
    MayExileAGenericCard,
    MayExileAnyNumberOfGenericCards,
    MayPutACardOfTypeOntoTheBattlefield(Box<Cards>, Vec<ReplacementActionWouldEnter>),
    MayPutAGenericCardIntoHand,
    MayPutAnyNumberOfCardsOntoTheBattlefield(Box<Cards>, Vec<ReplacementActionWouldEnter>),
    MayRevealACardOfTypeAndPutIntoHand(Box<Cards>),
    MayRevealACardOfTypeAndPutOnTopOfLibrary(Box<Cards>),
    MayRevealAnyNumberOfCardOfTypeAndPutOnTopOfLibrary(Box<Cards>),
    MayRevealAnyNumberOfCardsOfTypeAndPutOnTopOfLibraryInAnyOrder(Box<Cards>),
    MayRevealAnyNumberOfCardsOfTypeAndPutThemIntoHand(Box<Cards>),
    PutAGenericCardIntoGraveyard,
    PutAGenericCardIntoHand,
    PutAGenericCardOnBottomOfLibrary,
    PutAGenericCardOnTopOfLibrary,
    PutAnyNumberOfGenericCardsIntoHand,
    PutAnyNumberOfGenericCardsOnBottomOfLibraryAnyOrder,
    PutNumberGenericCardsIntoHand(Box<GameNumber>),
    PutRemainingCardsOnTheTopOrBottomOfLibraryInAnyOrder,
    PutTheRemainingCardsBackIntoLibraryAndShuffle,
    PutTheRemainingCardsIntoGraveyard,
    PutTheRemainingCardsIntoHand,
    PutTheRemainingCardsOnTheBottomOfLibraryInARandomOrder,
    PutTheRemainingCardsOnTheBottomOfLibraryInAnyOrder,
    MayRevealUptoNumberCardsOfTypeAndPutIntoHand(Box<GameNumber>, Box<Cards>),
    SeparateIntoFaceUpFileAndFaceDownPile,
    PlayerChoosesPileTopPutIntoHand(Box<Player>),
    LeaveRemainingCardsOnTopOfLibraryInSameOrder,
    SeparateIntoTwoFaceDownPiles,
    PlayerExilesAPile(Box<Player>),
    PlayerLooksAtRemainingCardsAndPutsAGenericCardIntoHand(Box<Player>),
    MayRevealMultipleCardsOfTypeAndPutIntoHand(Vec<Cards>),
    CreatePermanentLayerEffectUntil(Box<Permanent>, Vec<LayerEffect>, Expiration),
    If(Condition, Vec<LookAtTopOfLibraryAction>),
    Unless(Condition, Vec<LookAtTopOfLibraryAction>),
    MayActions(Vec<LookAtTopOfLibraryAction>),
    IfElse(
        Condition,
        Vec<LookAtTopOfLibraryAction>,
        Vec<LookAtTopOfLibraryAction>,
    ),
    AttachPermanentToAPermanent(Box<Permanent>, Box<Permanents>),
    RepeatableActions(Vec<LookAtTopOfLibraryAction>),
    MayCost(LookAtTopOfLibraryCost),
    LookAtTheNextNumberCardsOnTopOfLibrary(Box<GameNumber>),
    RepeatThisProcess,
    PutUptoNumberGenericCardsIntoHand(Box<GameNumber>),
    MayCastASpellFromAmongThemWithoutPaying(Box<Spells>),
    ForEachCardPutIntoGraveyardUnlessCost(LookAtTopOfLibraryCost),
    ExileNumberGenericCardsAtRandom(Box<GameNumber>),
    CreatePerpetualPermanentEffect(Box<Permanent>, Vec<PerpetualEffect>),
    MayPutUptoNumberCardsOntoTheBattlefield(
        Box<GameNumber>,
        Box<Cards>,
        Vec<ReplacementActionWouldEnter>,
    ),
    GainLife(Box<GameNumber>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ResolveAction", content = "args")]
pub enum ResolveAction {
    ExileResolvingSpell,
    ExileResolvingSpellAndPlotIt,
    ExileResolvingSpellWithNumberCountersOfTypeAndEffects(
        Box<GameNumber>,
        CounterType,
        Vec<ExiledCardEffect>,
    ),
    CreateFutureTrigger(FutureTrigger, Box<Actions>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ManaUseModifier", content = "args")]
pub enum ManaUseModifier {
    // ManaBonus
    CreatePermanentSpellLayerEffect(Expiration, Box<Spells>, Vec<LayerEffect>),
    FlagSpellsCastWith(Box<Spells>, Vec<SpellEffect>),
    DontLoseAsStepsAndPhasesEnd(Box<Expiration>),
    TriggerSpentOnSpell(Box<Spells>, Box<Actions>),
    TriggerSpentOnSpellOrAbility(SpellsAndAbilities, Box<Actions>),
    FlagPermanentsCastWith(Box<Permanents>, Vec<ReplacementActionWouldEnter>),

    // Mana Restrictions
    CanOnlySpendOnCumulativeUpkeepCosts,
    CanOnlySpendOnMorphCosts,
    CanOnlySpendOnSpells(Box<Spells>),
    CanOnlySpendOnXCost,
    CanOnlySpendToActivateAbilities,
    CanOnlySpendToActivateAbilitiesOfPermanents(Box<Permanents>),
    CanOnlySpendToActivateAbilitiesOfSources(AbilitySources),
    CanOnlySpendToActivateEquipAbilities,
    CanOnlySpendToCastExiledCard(Box<CardInExile>),
    CanOnlySpendToCastForetoldSpells,
    CanOnlySpendToCastGraveyardSpells(Box<Spells>),
    CanOnlySpendToCastSpellsFromAPlayersGraveyard(Box<Spells>, Box<Players>),
    CanOnlySpendToCastSpellsFromAnywhereOtherThanPlayersHand(Box<Spells>, Box<Players>),
    CanOnlySpendToCastSpellsFromExile(Box<Spells>),
    CanOnlySpendToCastTheirCommander,
    CanOnlySpendToForetellCards,
    CanOnlySpendToGainAClassLevel,
    CanOnlySpendToPayACostThatContainsManaSymbol(ManaSymbol),
    CanOnlySpendToPayDisturbCosts,
    CanOnlySpendToTurnACreatureFaceUp,
    CanOnlySpendToTurnAManifestedCreatureFaceUp,
    CanOnlySpendToTurnAPermanentFaceUp,
    CanOnlySpendToUnlockDoors,
    CantSpendOnGenericCosts,
    CantSpendOnSpells(Box<Spells>),

    And(Vec<ManaUseModifier>),
    Or(Vec<ManaUseModifier>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_PermanentsAndGraveyardCards", content = "args")]
pub enum PermanentsAndGraveyardCards {
    IsCardtype(CardType),
    Ref_TargetPermanentsAndGraveyardCards,
    WasntSacrificed,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_PermanentOrExiledCard", content = "args")]
pub enum PermanentOrExiledCard {
    Ref_TargetPermanentOrExiledCard,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_PhasedOutEffect", content = "args")]
pub enum PhasedOutEffect {
    TapAsPhasesIn,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_GroupExiledEffect", content = "args")]
pub enum GroupExiledEffect {
    OneMayBePlayedBy(Box<Player>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_SpellOrPermanent", content = "args")]
pub enum SpellOrPermanent {
    Ref_TargetSpellOrPermanent,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_SpellOrAbility", content = "args")]
pub enum SpellOrAbility {
    Trigger_ThatSpellOrAbility,
    Ref_TargetSpellOrAbility,
    EachableSpellOrAbility,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_CardsInLibrary", content = "args")]
pub enum CardsInLibrary {
    SharesACardtypeWithTheCycledCard,
    IsCardtype(CardType),
    IsLandType(LandType),
    IsCreatureType(CreatureType),
    SharesANameWithSpell(Box<Spell>),
    IsNonCardtype(CardType),
    TheCardsConjuredInLibraryThisWay,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_CardInLibrary", content = "args")]
pub enum CardInLibrary {
    TheLibraryCardFoundThisWay,
    TheCardConjureIntoLibraryThisWay,
    TheTopCardOfTypeInPlayersLibrary(Box<Cards>, Box<Player>),
    ARandomCardOfTypeFromPlayersLibrary(Box<Cards>, Box<Player>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_FutureSpell", content = "args")]
pub enum FutureSpell {
    TheNextSpellPlayerCasts(Box<Spells>, Box<Player>),
    TheNextSpellPlayerCastsThisTurn(Box<Spells>, Box<Player>),
    TheNextSpellPlayerCastsFromTheirHandThisTurn(Box<Spells>, Box<Player>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ChoosableCreatureType", content = "args")]
pub enum ChoosableCreatureType {
    AnyCreatureType,
    CreatureTypesOfSpell(Box<Spell>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Level", content = "args")]
pub enum Level {
    Level(Range, PT, Vec<Rule>),
    LevelNoRules(Range, PT),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Range", content = "args")]
pub enum Range {
    BetweenValues(i32, i32),
    ValueOrBigger(i32),
    ValueOrSmaller(i32),
    ExactValue(i32),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ValueAction", content = "args")]
pub enum ValueAction {
    ValueAction(Range, Vec<Action>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_NamedAction", content = "args")]
pub enum NamedAction {
    NamedAction(VoteOption, Vec<Action>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_FuturePlayerEffect", content = "args")]
pub enum FuturePlayerEffect {
    CanCastOnlyOneMoreSpellThisTurn,
    MayCastTheirNextSpellThisTurnWithoutPaying(Box<Spells>),
    MayCastTheirNextSpellThisTurnAsThoughItHadFlashWithEffects(Box<Spells>, Vec<SpellEffect>),
    MayCastTheirNextSpellThisTurnAsThoughItHadFlash(Box<Spells>),
    NextCardPlayedThisCanCanBePlayedAsThoughItHadFlash(Box<Cards>),
    MayCastTheirNextSpellThisTurnForAlternateCost(Box<Spells>, Box<Cost>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_SpellOrPermanentEffect", content = "args")]
pub enum SpellOrPermanentEffect {
    SetColor(SettableColor),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_RevealTheTopNumberCardsOfLibraryCost", content = "args")]
pub enum RevealTheTopNumberCardsOfLibraryCost {
    PayLife(Box<GameNumber>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_RevealTheTopNumberCardsOfLibraryAction", content = "args")]
pub enum RevealTheTopNumberCardsOfLibraryAction {
    PlayerAction(Box<Player>, Box<RevealTheTopNumberCardsOfLibraryAction>),
    PutAGenericCardOnBottomOfLibrary,
    PutRemainingCardsInHand,

    ForEachColorAmongPermanentsYouMayExileACardOfThatColorFoundThisWay(Box<Permanents>),
    PutAnyNumberOfFoundCardsOntoBattlefield(Vec<ReplacementActionWouldEnter>),
    ChooseAPlayer(Box<Players>),
    ReflexiveTrigger(Box<Actions>),
    APlayerChoosesACardOfType(Box<Players>, Box<Cards>),
    MayPutAnyNumberOfGroupCardsIntoHand(Box<Cards>, GroupFilter),
    MayExileACardOfEachCardType,
    PutEachCardOfTypeIntoGraveyard(Box<Cards>),
    CreateExiledCardEffect(CardInExile, Vec<ExiledCardEffect>),
    ExileTheCardFoundThisWayWithNumberCountersOfType(Box<GameNumber>, CounterType),
    SacrificePermanent(Box<Permanent>),
    SpellDealsDamage(Box<Spell>, Box<GameNumber>, Box<DamageRecipient>),
    APlayerChoosesMultipleCardsOfType(Box<Players>, Vec<Cards>),
    PutACardFoundThisWayOntoTheBattlefield(Vec<ReplacementActionWouldEnter>),
    APlayerChoosesAGenericCard(Box<Players>),
    APlayerChoosesAPile(Box<Players>),
    APlayerChoosesFinalDestination(
        Box<Players>,
        Box<RevealTheTopNumberCardsOfLibraryAction>,
        Box<RevealTheTopNumberCardsOfLibraryAction>,
    ),
    APlayerExilesACardOfType(Box<Players>, Box<Cards>),
    APlayerMayCastASpellFromAmongThemWithoutPaying(Box<Players>, Box<Spells>),
    APlayerMayCastUptoNumberSpellsFromAmongThemWithoutPaying(
        Box<Players>,
        Box<GameNumber>,
        Box<Spells>,
    ),
    APlayerMayPutACardOfTypeOntoTheBattlefield(
        Box<Players>,
        Box<Cards>,
        Vec<ReplacementActionWouldEnter>,
    ),
    APlayerPutsAGenericCardIntoGraveyard(Box<Players>),
    APlayerPutsNumberGenericCardsIntoGraveyard(Box<Players>, Box<GameNumber>),
    APlayerPutsTheRemainingCardsOnTheTopOfLibraryInAnyOrder(Box<Players>),
    APlayerSeparatesThoseCardsIntoTwoPiles(Box<Players>),
    ChooseACardThatsExactlyEachColorPair,
    ChooseAnyNumberOfCards(Box<Cards>),
    ChooseMultipleCardsOfType(Vec<Cards>),
    CreatePermanentLayerEffect(Box<Permanent>, Vec<LayerEffect>),
    CreatePermanentLayerEffectUntil(Box<Permanent>, Vec<LayerEffect>, Expiration),
    EachPlayerStartingWithChoosesADifferentCardToPutIntoHand(Box<Players>, Box<Player>, Box<Cards>),
    ExileTheCardFoundThisWay,
    ExileTheRemainingCards,
    ExileTheRemainingCardsWithACounterOfType(CounterType),
    ForEachCardPutIntoHandUnlessAnyPlayerAction(Box<Players>, RevealTheTopNumberCardsOfLibraryCost),
    IfElse(
        Condition,
        Vec<RevealTheTopNumberCardsOfLibraryAction>,
        Vec<RevealTheTopNumberCardsOfLibraryAction>,
    ),
    If(Condition, Vec<RevealTheTopNumberCardsOfLibraryAction>),
    LeaveTheRemainingCardsOnTopOfLibraryInTheSameOrder,
    MayCastASpellFromAmongThemWithoutPaying(Box<Spells>),
    MayCastTheCardFoundThisWayWithoutPaying,
    MayPutACardOfEachCardtypeAmongSpellsCastThisTurnIntoHand(Box<Spells>),
    MayPutACardOfEachCardtypeIntoHand,
    MayPutACardOfTypeIntoHand(Box<Cards>),
    MayPutACardOfTypeOntoTheBattlefield(Box<Cards>, Vec<ReplacementActionWouldEnter>),
    MayPutAnyNumberOfCardsOfTypeIntoHand(Box<Cards>),
    MayPutAnyNumberOfCardsOfTypeOntoTheBattlefield(Box<Cards>, Vec<ReplacementActionWouldEnter>),
    MayPutMultipleCardsOfTypeIntoHand(Vec<Cards>),
    MayPutMultipleCardsOfTypeIntoHandOrOntoTheBattlefield(
        Vec<Cards>,
        Vec<ReplacementActionWouldEnter>,
    ),
    MayPutTheCardFoundThisWayOntoTheBattlefield(Vec<ReplacementActionWouldEnter>),
    MayPutUptoNumberCardsOfTypeOntoTheBattlefield(
        Box<GameNumber>,
        Box<Cards>,
        Vec<ReplacementActionWouldEnter>,
    ),
    MayPutUptoNumberGroupCardsOntoTheBattlefield(
        Box<GameNumber>,
        Box<Cards>,
        GroupFilter,
        Vec<ReplacementActionWouldEnter>,
    ),
    PlayerMillsNumberCards(Box<Player>, Box<GameNumber>),
    PutACardOfTypeIntoHand(Box<Cards>),
    PutACardOfTypeOntoTheBattlefield(Box<Cards>, Vec<ReplacementActionWouldEnter>),
    PutAChosenCardOntoTheBattlefield(Vec<ReplacementActionWouldEnter>),
    PutAGenericCardFromTheChosenPileIntoHand,
    PutAGenericCardIntoHand,
    PutAPileIntoHand,
    PutEachCardOfTypeChosenThisWayOntoTheBattlefield(Box<Cards>, Vec<ReplacementActionWouldEnter>),
    PutEachCardOfTypeIntoHand(Box<Cards>),
    PutEachCardOfTypeOntoTheBattlefield(Box<Cards>, Vec<ReplacementActionWouldEnter>),
    PutTheCardFoundThisWayIntoHand,
    PutTheCardFoundThisWayOnTopOfLibrary,
    PutTheCardFoundThisWayOntoTheBattlefield(Vec<ReplacementActionWouldEnter>),
    PutTheCardsFoundThisWayIntoExile,
    PutTheCardsFoundThisWayIntoGraveyard,
    PutTheCardsFoundThisWayIntoHand,
    PutTheCardsFoundThisWayOntoTheBattlefield(Vec<ReplacementActionWouldEnter>),
    PutTheCardsNotFoundThisWayIntoGraveyard,
    PutTheChosenCardIntoGraveyard,
    PutTheChosenCardIntoHand,
    PutTheChosenCardOntoTheBattlefield(Vec<ReplacementActionWouldEnter>),
    PutTheChosenCardsIntoHand,
    PutTheChosenPileIntoHand,
    PutTheRemainingCardsBackIntoLibraryAndShuffle,
    PutTheRemainingCardsIntoGraveyard,
    PutTheRemainingCardsIntoHand,
    PutTheRemainingCardsOnTheBottomOfLibraryInARandomOrder,
    PutTheRemainingCardsOnTheBottomOfLibraryInAnyOrder,
    PutUptoNumberCardsOfTypeIntoHand(Box<GameNumber>, Box<Cards>),
    SeperateThoseCardsIntoTwoPiles,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_AnteCards", content = "args")]
pub enum AnteCards {
    OwnedByAPlayer(Box<Players>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_CreatableToken", content = "args")]
pub enum CreatableToken {
    // Number Tokens
    NumberTokens(Box<GameNumber>, Box<CreatableToken>),
    NumberTokensForEach(Box<GameNumber>, Box<GameNumber>, Box<CreatableToken>),

    // Manually Defined Tokens
    ArtifactToken(
        NameString,
        Vec<SuperType>,
        Vec<SubType>,
        ColorList,
        Vec<Rule>,
    ),
    ArtifactTokenWithNoRules(NameString, Vec<SuperType>, Vec<SubType>, ColorList),
    NamedArtifactVehicleToken(
        NameString,
        Vec<SuperType>,
        Vec<SubType>,
        ColorList,
        Vec<Rule>,
        PT,
    ),
    ArtifactVehicleToken(Vec<SuperType>, Vec<SubType>, ColorList, Vec<Rule>, PT),

    EnchantmentToken(
        NameString,
        Vec<SuperType>,
        Vec<SubType>,
        ColorList,
        Vec<Rule>,
    ),
    CreatureToken(PT, CreatureTokenType, ColorList, CreatureTokenSubtypes),
    CreatureTokenWithAbilities(
        PT,
        CreatureTokenType,
        ColorList,
        CreatureTokenSubtypes,
        Vec<Rule>,
    ),
    LegendaryNamedCreatureTokenWithCopyEffects(
        NameString,
        PT,
        CreatureTokenType,
        ColorList,
        CreatureTokenSubtypes,
        TokenCopyEffects,
    ),
    LegendaryNamedCreatureToken(
        NameString,
        PT,
        CreatureTokenType,
        ColorList,
        CreatureTokenSubtypes,
    ),
    LegendaryNamedCreatureTokenWithAbilities(
        NameString,
        PT,
        CreatureTokenType,
        ColorList,
        CreatureTokenSubtypes,
        Vec<Rule>,
    ),
    NamedCreatureToken(
        NameString,
        PT,
        CreatureTokenType,
        ColorList,
        CreatureTokenSubtypes,
    ),
    NamedCreatureTokenWithAbilities(
        NameString,
        PT,
        CreatureTokenType,
        ColorList,
        CreatureTokenSubtypes,
        Vec<Rule>,
    ),
    NamedLandTokenWithNoAbilities(NameString, LandTokenType, ColorList, LandTokenSubtypes),

    // Token Copies of Things
    TokenCopyOfDiscardedCard(Box<CardInHand>, TokenCopyEffects),
    TokenCopyOfEachCardOfTypeRevealedThisWay(Box<Cards>, TokenCopyEffects),
    TokenCopyOfAPermanent(Box<Permanents>, TokenCopyEffects),
    TokenCopyOfEachExiledCard(CardsInExile, TokenCopyEffects),
    TokenCopyOfAnExiledCard(CardsInExile, TokenCopyEffects),
    TokenCopyOfEachPermanentDestroyedThisWay(TokenCopyEffects),
    TokenCopyOfExiledCard(CardInExile, TokenCopyEffects),
    TokenCopyOfNamedCard(NameString, TokenCopyEffects),
    TokenCopyOfSpell(Box<Spell>, TokenCopyEffects),
    TokenFromCopy,
    TokenCopyOfGraveyardCard(CardInGraveyard, TokenCopyEffects),
    TokenCopyOfACardAtRandom(Box<Cards>),
    TokenCopyOfCommander(TokenCopyEffects),
    TokenCopyOfEachGraveyardCard(CardsInGraveyard, TokenCopyEffects),
    TokenCopyOfEachPermanent(Box<Permanents>, TokenCopyEffects),
    TokenCopyOfPermanent(Box<Permanent>, TokenCopyEffects),
    TokenCopyOfAnEnteringPermanent(Box<Permanents>, TokenCopyEffects),

    // Replacement-Effect Tokens
    ThoseTokens,

    // Oracle Tokens
    MutavaultToken,
    SpellgorgerWeirdToken,
    TarmogoyfToken,

    // Pre-defined
    VirtuousRoleToken,
    WickedRoleToken,
    YoungHeroRoleToken,
    CursedRoleToken,
    MonsterRoleToken,
    RoyalRoleToken,
    SorcererRoleToken,

    BloodToken,
    ClueToken,
    FishToken,
    FoodToken,
    GoldToken,
    JunkToken,
    LanderToken,
    MapToken,
    MutagenToken,
    OctopusToken,
    PowerstoneToken,
    ShardToken,
    TreasureToken,
    VibraniumToken,
    WalkerToken,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Exilable", content = "args")]
pub enum Exilable {
    // Cards In Graveyards
    AGraveyardCard(Box<CardsInGraveyard>),
    AGraveyardCardAtRandom(Box<CardsInGraveyard>),
    AGraveyardCardAtRandomInEachPlayersGraveyard(Box<CardsInGraveyard>, Box<Players>),
    AnyNumberOfGraveyardCards(Box<CardsInGraveyard>),
    AnyNumberOfGroupGraveyardCards(CardsInGraveyard, GroupFilter),
    GraveyardCards(Box<CardsInGraveyard>),
    GraveyardCard(Box<CardInGraveyard>),
    NumberGraveyardCards(Box<GameNumber>, CardsInGraveyard),
    UptoOneGraveyardCard(Box<CardsInGraveyard>),

    // Cards In Hand
    ARandomCardFromPlayersHand(Box<Player>),
    CardInHand(Box<CardInHand>),

    // Cards In Library
    TheTopCardOfPlayersLibrary(Player),
    TheTopNumberCardsOfPlayersLibrary(Box<GameNumber>, Box<Player>),
    ARandomCardFromPlayersLibrary(Player),
    ARandomCardOfTypeFromPlayersLibrary(CardsInLibrary, Box<Player>),

    // Permanents
    APermanent(Box<Permanents>),
    Permanent(Box<Permanent>),
    Permanents(Box<Permanents>),
    UptoOnePermanent(Box<Permanents>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_DamageRecipient", content = "args")]
pub enum DamageRecipient {
    MultipleRecipients(Vec<DamageRecipient>),

    CreatureOrPlaneswalkerChosenAtRandom(Box<Permanents>),
    EachPermanent(Box<Permanents>),
    EachPlayer(Box<Players>),
    EachableTarget,
    Permanent(Box<Permanent>),
    Player(Box<Player>),
    PlayerOrPlaneswalkerPermanentIsAttacking(Box<Permanent>),
    Ref_AnyTarget,
    Ref_AnyTarget1,
    Ref_AnyTarget2,
    Ref_AnyTargets,
    Ref_AnyTargets_1,
    Ref_AnyTargets_2,
    Ref_AnyTargets_3,
    Ref_TargetPlayerOrPermanent,
    TheChosenDamageRecipient,
    Trigger_ThatRecipient,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_DamageToRecipients", content = "args")]
pub enum DamageToRecipients {
    DamageToRecipients(Box<GameNumber>, Box<DamageRecipient>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Emblem", content = "args")]
pub enum Emblem {
    OwnedByAPlayer(Box<Players>),
    ThisEmblem,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Boon", content = "args")]
pub enum Boon {
    ThisBoon,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Direction", content = "args")]
pub enum Direction {
    TheChosenDirection,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ActionOption", content = "args")]
pub enum ActionOption {
    ActionOption(Box<Cost>, Vec<Action>),
    DoNothingOption(Vec<Action>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Targets", content = "args")]
pub enum Targets {
    Ref_TargetPlayersAndPermanents,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_PutCounterAction", content = "args")]
pub enum PutCounterAction {
    ACounterOfTypeOnPermanent(CounterType, Box<Permanent>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Action", content = "args")]
pub enum Action {
    AwakenPermanent(Box<GameNumber>, Box<Permanent>),
    ChooseARandomColor(ChoosableColor),
    PutACardAndOrACardFromHandOnBattlefield(
        Box<CardsInHand>,
        Box<CardsInHand>,
        Vec<ReplacementActionWouldEnter>,
    ),
    PutCountersOfDeadPermanentOnEachPermanent(Box<Permanents>),

    CastUptoNumberCopiedCardsWithoutPaying(Box<GameNumber>),
    AirbendSpell(Box<Spell>),
    Ascend,
    Epic,
    Blight(Box<GameNumber>),
    Cipher(Box<Spell>),
    Paradigm(Box<Spell>),
    CastExiledCardWithoutPayingIntoExile(Box<CardInExile>),
    ConjureARandomCardIntoExile(Box<Cards>),
    ConjureNumberCardsIntoLibrary(Box<GameNumber>, NameString),
    CounterEachSpellAndAbility(SpellsAndAbilities),
    CreatePerpetualCardsInPlayersHandAndCardsInPlayersLibraryEffect(
        CardsInHand,
        Box<Players>,
        CardsInLibrary,
        Box<Players>,
        Vec<PerpetualEffect>,
    ),
    PreparePermanent(Box<Permanent>),
    UnpreparePermanent(Box<Permanent>),
    SkipNextNumberTurns(Box<GameNumber>),
    MayReflexiveActionTrigger(ReflexiveAction, Box<Actions>),
    ReflexiveActionTrigger(ReflexiveAction, Box<Actions>),
    ReflexiveActionTriggerI(ReflexiveAction, Condition, Box<Actions>),
    ExileTheTopNumberCardsOfLibraryInFaceUpPile(Box<GameNumber>),
    GetNumberExperienceCounters(Box<GameNumber>),
    GainControlOfPlayerDuringTheirNextCombatStep(Box<Player>),
    PutACardDiscardedThisWayOntoTheBattlefield(Box<Cards>, Vec<ReplacementActionWouldEnter>),
    PutACardOfTypeMilledThisWayOnTopOfLibrary(Box<Cards>),
    RemoveAnyNumberOfCountersFromAmongPermanents(Box<Permanents>),
    RevealNumberCardsFromHandAndPlayerChoosesACardToExile(Box<GameNumber>, Box<Player>, Box<Cards>),
    PutCounters(Vec<PutCounterAction>),
    SearchLibraryAndOrOutsideTheGame(Vec<SearchLibraryAction>),

    CastAnExiledCardAndMaySpendManaAsThoughAnyTypeToCast(Box<CardsInExile>),
    HarnessPermanent(Box<Permanent>),
    PutACounterOfTypeOnEachOfUptoNumberPermanents(CounterType, Box<GameNumber>, Box<Permanents>),
    PutAnExiledCardOntoBattlefield(Box<CardsInExile>, Vec<ReplacementActionWouldEnter>),
    PutCountersOfTypeOfDeadPermanentOnPermanent(CounterType, Box<Permanent>),
    PutEachCardOfTypeMilledThisWayOntoTheBattlefield(
        Box<CardsInLibrary>,
        Vec<ReplacementActionWouldEnter>,
    ),
    PutGraveyardCardOnBottomOfLibrary(Box<CardInGraveyard>),
    SecretlyChooseLibraryFilter(Vec<Cards>),
    ThereIsAnAdditionalEndStep,
    TripleCreaturesPowerAndToughnessUntilEndOfTurn(Box<Permanent>),
    AirbendPermanent(Box<Permanent>),
    AirbendPermanents(Box<Permanents>),
    AttachAPermanentToAPermanent(Box<Permanents>, Box<Permanents>),
    ChooseAGraveyardCardThatHasntBeenChosen(Box<CardsInGraveyard>),
    ConjureADuplicateOfPermanentOntoTheBattlefield(
        Box<Permanent>,
        Vec<ReplacementActionWouldEnter>,
    ),
    ConjureADuplicateOfSpellCardOntoTheBattlefield(Box<Spell>, Vec<ReplacementActionWouldEnter>),
    ConjureARandomCardFromSpellbookIntoTheTopNumberCardsOfLibrary(SpellBookName, Box<GameNumber>),
    ConjureDuplicateOfGraveyardCardOnTopOfPlayersLibrary(Box<CardInGraveyard>, Box<Player>),
    ConjureEachCardFromSpellBookIntoLibrary(SpellBookName),
    CopyAbilityNumberTimesAndMayChooseNewTargets(Box<Ability>, Box<GameNumber>),
    CreateCardInHandEffectUntil(Box<CardInHand>, Vec<CardEffect>, Box<Expiration>),
    Earthbend(Box<Permanent>, Box<GameNumber>),
    ExileACardFromEachPlayersGraveyard(Box<CardsInGraveyard>, Box<Players>),
    ForEachPermanentPutUptoNumberCountersOfTypeOnIt(
        Box<Permanents>,
        Box<GameNumber>,
        Box<CounterType>,
    ),
    ManifestCardFromHand(Box<CardInHand>),
    MayCastGraveyardCardIntoExileAndMaySpendManaAsThoughAnyType(Box<CardInGraveyard>),
    MayReflexiveAction(Box<Cost>),
    PutGraveyardCardOnTopOfLibrary(Box<CardInGraveyard>),

    APlayerActions(Box<Players>, Vec<Action>),
    AfterThisMainPhaseThereAreAnAdditionalNumberCombatPhases(Box<GameNumber>),
    AttachPermanentsToPermanent(Box<Permanents>, Box<Permanent>),
    ConjureDuplicateOfPermanentIntoExile(Box<Permanent>),
    CopyCard(Box<SingleCard>),

    ReducePlayersSpeed(Box<Player>, Box<GameNumber>),
    ExchangeOwnershipOfTwoCards(ExchangeOwnershipCard, ExchangeOwnershipCard),
    VoteForACardInGraveyard(Box<CardsInGraveyard>),

    CreateFutureReplaceWouldAdapt(
        FutureReplacableEventWouldAdapt,
        Vec<ReplacementActionWouldAdapt>,
    ),
    CreateFutureReplaceWouldDealDamage(
        FutureReplacableEventWouldDealDamage,
        Vec<ReplacementActionWouldDealDamage>,
    ),
    CreateFutureReplaceWouldDestroy(
        FutureReplacableEventWouldDestroy,
        Vec<ReplacementActionWouldDestroy>,
    ),
    CreateFutureReplaceWouldDraw(
        FutureReplacableEventWouldDraw,
        Vec<ReplacementActionWouldDraw>,
    ),
    CreateFutureReplaceWouldEnter(
        FutureReplacableEventWouldEnter,
        Vec<ReplacementActionWouldEnter>,
    ),
    CreateFutureReplaceWouldLeaveTheBattlefield(
        FutureReplacableEventWouldLeaveTheBattlefield,
        Vec<ReplacementActionWouldLeaveTheBattlefield>,
    ),
    CreateFutureReplaceWouldLoseTheGame(
        FutureReplacableEventWouldLoseTheGame,
        Vec<ReplacementActionWouldLoseTheGame>,
    ),
    CreateFutureReplaceWouldRollDice(
        FutureReplacableEventWouldRollDice,
        Vec<ReplacementActionWouldRollDice>,
    ),
    CreateFutureReplaceWouldSetASchemeInMotion(
        FutureReplacableEventWouldSetASchemeInMotion,
        Vec<ReplacementActionWouldSetASchemeInMotion>,
    ),

    CreateReplaceAnyNumberOfTokensWouldBeCreatedUntil(
        ReplacableEventAnyNumberOfTokensWouldBeCreated,
        Vec<ReplacementActionAnyNumberOfTokensWouldBeCreated>,
        Expiration,
    ),
    CreateReplaceTokensWouldBeCreatedUnderAPlayersControlUntil(
        ReplacableEventTokensWouldBeCreatedUnderAPlayersControl,
        Vec<ReplacementActionTokensWouldBeCreatedUnderAPlayersControl>,
        Expiration,
    ),

    CreateReplaceWouldGainLifeUntil(
        ReplacableEventWouldGainLife,
        Vec<ReplacementActionWouldGainLife>,
        Expiration,
    ),
    CreateReplaceWouldDealDamageUntil(
        ReplacableEventWouldDealDamage,
        Vec<ReplacementActionWouldDealDamage>,
        Expiration,
    ),
    CreateReplaceWouldDrawUntil(
        ReplacableEventWouldDraw,
        Vec<ReplacementActionWouldDraw>,
        Expiration,
    ),
    CreateReplaceWouldEnterUntil(
        ReplacableEventWouldEnter,
        Vec<ReplacementActionWouldEnter>,
        Expiration,
    ),
    CreateReplaceWouldLeaveTheBattlefieldUntil(
        ReplacableEventWouldLeaveTheBattlefield,
        Vec<ReplacementActionWouldLeaveTheBattlefield>,
        Expiration,
    ),
    CreateReplaceWouldMaskUntil(
        ReplacableEventWouldMask,
        Vec<ReplacementActionWouldMask>,
        Expiration,
    ),
    CreateReplaceWouldPlaneswalkUntil(
        ReplacableEventWouldPlaneswalk,
        Vec<ReplacementActionWouldPlaneswalk>,
        Expiration,
    ),
    CreateReplaceWouldProduceManaUntil(
        ReplacableEventWouldProduceMana,
        Vec<ReplacementActionWouldProduceMana>,
        Expiration,
    ),
    CreateReplaceWouldPutCountersUntil(
        ReplacableEventWouldPutCounters,
        Vec<ReplacementActionWouldPutCounters>,
        Expiration,
    ),
    CreateReplaceWouldPutIntoGraveyardUntil(
        ReplacableEventWouldPutIntoGraveyard,
        Vec<ReplacementActionWouldPutIntoGraveyard>,
        Expiration,
    ),

    CreatePerpetualAllCardsEffect(Box<Cards>, Vec<PerpetualEffect>),
    DoubleCountersOfEachTypePlayersHave(Box<Players>),
    EachPlayerActions(Box<Players>, Vec<Action>),
    EndureWithPermanent(Box<GameNumber>, Box<Permanent>),

    ForEachValueInRangeConjureDuplicateOfARandomCardOfTypeOntoBattlefield(
        Box<GameNumber>,
        Box<GameNumber>,
        Box<Cards>,
        Vec<ReplacementActionWouldEnter>,
    ),

    PutAPermanentIntoItsOwnersHand(Box<Permanents>),

    PutACounterOfTypeOnAPermanentOfEachColor(CounterType, Box<Permanents>),
    PutEachGraveyardCardOntoBattlefieldFaceDownAsAnArtifactCreature(
        Box<CardsInGraveyard>,
        Vec<ReplacementActionWouldEnter>,
        PT,
        CreatureType,
    ),

    PutEachPermanentInOwnersLibraryNthFromTheTop(Box<Permanents>, Box<GameNumber>),
    PutNumberGraveyardCardsOnTopOfLibraryInAnyOrder(Box<GameNumber>, Box<CardsInGraveyard>),
    RemoveUptoNumberCountersOfTypeFromAmongPermanents(
        Box<GameNumber>,
        CounterType,
        Box<Permanents>,
    ),
    RevealHandAndPlayerMayChooseACardToExile(Box<Player>, Box<Cards>),
    SaddlePermanent(Box<Permanent>),
    SeekCards(Vec<Cards>),
    TurnPermanentFaceDownAsArtifactCreature(Box<Permanent>, PT, CreatureType),

    // Create Tokens
    ForEachPlayerCreateTokens(Box<Players>, Vec<CreatableToken>),
    ForEachPlayerCreateTokensWithFlags(Box<Players>, Vec<CreatableToken>, Vec<TokenFlag>),
    ForEachPermanentCreateTokensWithFlags(Box<Permanents>, Vec<CreatableToken>, Vec<TokenFlag>),
    ForEachPermanentCreateTokens(Box<Permanents>, Vec<CreatableToken>),
    CreateTokensWithFlags(Vec<CreatableToken>, Vec<TokenFlag>),
    CreateTokens(Vec<CreatableToken>),

    // Conjure
    ConjureACardIntoGraveyard(NameString),
    ConjureACardOfChoiceFromSpellBookIntoHand(SpellBookName),
    ConjureACardOfChoiceFromSpellBookOntoBattlefield(
        SpellBookName,
        Vec<ReplacementActionWouldEnter>,
    ),
    ConjureACardOfTypeFromSpellBookOntoBattlefield(
        SpellBookName,
        Box<Cards>,
        Vec<ReplacementActionWouldEnter>,
    ),
    ConjureADuplicateOfEachPermanentOntoTheBattlefield(
        Box<Permanents>,
        Vec<ReplacementActionWouldEnter>,
    ),
    ConjureARandomCardFromSpellBookIntoExile(SpellBookName),
    ConjureARandomCardFromSpellBookIntoExileFaceDown(SpellBookName),
    ConjureARandomCardFromSpellBookIntoHand(SpellBookName),
    ConjureCardIntoHand(NameString),
    // ConjureCardIntoLibrary(NameString),
    ConjureCardIntoLibraryNthFromTop(NameString, Box<GameNumber>),
    ConjureCardIntoTheTopNumberCardsOfLibraryAtRandom(NameString, Box<GameNumber>),
    ConjureCardOntoBattlefield(NameString, Vec<ReplacementActionWouldEnter>),
    ConjureCardOrCardIntoHand(NameString, NameString),
    ConjureCardsIntoGraveyard(Box<GameNumber>, NameString),
    ConjureCardsOntoTheBattlefield(Vec<NameString>, Vec<ReplacementActionWouldEnter>),
    ConjureDuplicateOfARandomCardOfTypeFromAPlayersLibraryIntoHand(Box<Cards>, Box<Players>),
    ConjureDuplicateOfARandomCardOfTypeFromPlayersLibraryIntoHand(Box<Cards>, Box<Player>),
    ConjureDuplicateOfARandomCardOfTypeIntoHand(Box<Cards>),
    ConjureDuplicateOfCardInHandIntoHand(CardInHand),
    ConjureDuplicateOfEachDestroyedPermanentIntoHand(Box<Permanents>),
    ConjureDuplicateOfEachExiledCardIntoHand(Box<CardsInExile>),
    ConjureDuplicateOfEachPermanentIntoHand(Box<Permanents>),
    ConjureDuplicateOfExiledCardIntoHand(Box<CardInExile>),
    ConjureDuplicateOfExiledCardIntoPlayersGraveyard(CardInExile, Box<Player>),
    ConjureDuplicateOfExiledCardIntoTheTopNumberCardsOfLibraryAtRandom(
        CardInExile,
        Box<GameNumber>,
    ),
    ConjureDuplicateOfGraveyardCardIntoHand(CardInGraveyard),
    ConjureDuplicateOfGraveyardCardIntoPlayersGraveyard(CardInGraveyard, Box<Player>),
    ConjureDuplicateOfPermanentIntoHand(Box<Permanent>),
    ConjureDuplicateOfPermanentIntoPlayersGraveyard(Box<Permanent>, Box<Player>),
    ConjureDuplicateOfPermanentIntoTopNumberCardsOfPlayersLibraryAtRandom(
        Box<Permanent>,
        Box<GameNumber>,
        Box<Player>,
    ),
    ConjureDuplicateOfSpellIntoHand(Box<Spell>),
    ConjureDuplicateOfThePermanentSacrificedThisWayIntoHand,
    ConjureDuplicateOfTheTopCardOfPlayersIntoHand(Box<Player>),
    ConjureDuplicatesOfNumberRandomCardsOfTypeFromPlayersLibraryIntoHand(
        Box<GameNumber>,
        Box<Cards>,
        Box<Player>,
    ),
    ConjureEachCardFromSpellBookOntoTheBattlefield(SpellBookName, Vec<ReplacementActionWouldEnter>),
    ConjureMultipleCardsIntoLibraryAndShuffle(NameString, NameString, NameString),
    ConjureNumberCardsIntoHand(Box<GameNumber>, NameString),
    ConjureNumberCardsIntoLibraryAndShuffle(Box<GameNumber>, NameString),
    ConjureNumberCardsIntoPlayersGraveyard(Box<GameNumber>, NameString, Box<Player>),
    ConjureNumberCardsIntoTheTopNumberCardsOfEachPlayersLibraryAtRandom(
        Box<GameNumber>,
        NameString,
        Box<GameNumber>,
        Box<Players>,
    ),
    ConjureNumberCardsOfChoiceFromSpellBookIntoHand(Box<GameNumber>, SpellBookName),
    ConjureNumberCardsOnTopOfLibrary(Box<GameNumber>, NameString),
    ConjureNumberCardsOntoBattlefield(
        Box<GameNumber>,
        NameString,
        Vec<ReplacementActionWouldEnter>,
    ),
    ConjureNumberDuplicateCardsIntoGraveyard(Box<GameNumber>, CardInHand),
    ConjureNumberDuplicatesOfAnOutsideCardIntoHand(Box<GameNumber>, Box<Cards>),
    ConjureNumberDuplicatesOfGraveyardCardIntoExile(Box<GameNumber>, CardInGraveyard),
    ConjureNumberRandomCardsFromSpellBookIntoHand(Box<GameNumber>, SpellBookName),
    ConjureThePowerNineIntoLibraryAndShuffle,
    ForEachPermanentConjureCardOntoBattlefield(
        Box<Permanents>,
        NameString,
        Vec<ReplacementActionWouldEnter>,
    ),
    ConjureADuplicateOfEachPermanentIntoGraveyard(Box<Permanents>),
    ConjureDuplicateOfARandomCardOfTypeOntoBattlefield(
        Box<Cards>,
        Vec<ReplacementActionWouldEnter>,
    ),
    ConjureDuplicateOfEachCardSeekedThisWayIntoHand,

    // Remaining
    ExileNumberGraveyardCards(Box<GameNumber>, CardsInGraveyard),

    ExileAGraveyardCard(Box<CardsInGraveyard>),
    // MayCastASpellFromGraveyardWithEffect(Box<Spells>, Box<Player>, Vec<SpellEffect>),
    ChooseAnOrderForCardTypes(Vec<CardType>),
    CreatePerpetualPermanentOrGraveyardCardEffect(
        Box<Permanent>,
        Box<CardInGraveyard>,
        Vec<PerpetualEffect>,
    ),
    ManifestEachCardOfTypeFromHand(Box<CardsInHand>),
    LockOrUnlockADoorOfPermanent(Box<Permanent>),
    ManifestDreadNumberTimes(Box<GameNumber>),
    PutAGraveyardCardIntoHand(Box<CardsInGraveyard>),
    PutAnyNumberOfCardsOfTypeMilledThisWayIntoHand(Box<Cards>),
    PutNumberCountersOfTypeAndACounterOfTypeOnPermanent(
        Box<GameNumber>,
        CounterType,
        CounterType,
        Box<Permanent>,
    ),
    PutUptoOneCardOfEachCardtypeAmongPermanentsFromHandOnTheBattlefield(
        Box<Permanents>,
        Vec<ReplacementActionWouldEnter>,
    ),
    UnlockADoorOfAPermanent(Box<Permanents>),
    UnlockADoorOfPermanent(Box<Permanent>),
    CastASpellFromTopOfLibraryWithoutPaying(Box<Spells>),
    AfterTheSecondMainPhaseThisTurnThereIsAnAdditionalCombatPhaseAndAnAdditionalMainPhaseWithAtTheBeginningOfCombatTrigger(
        Box<Actions>,
    ),
    CastExiledCardForAlternateCost(CardInExile, Box<Cost>),
    ChooseANonBasicLandType,
    CopyEachAbilityAndMayChooseNewTargets(Abilities),
    Exile(Vec<Exilable>),
    Forage,
    LoseAllCounters,
    ExileInShuffledFaceDownPile(Vec<Exilable>),
    ExilePlayersHandFaceDown(Box<Player>),
    HeistPlayersLibrary(Box<Player>),
    ManifestDread,
    ManifestNumberCardsFromHand(Box<GameNumber>),
    PutACardOfTypeAndOrACardOfTypeMilledThisWayOntoTheBattlefield(
        Vec<Cards>,
        Vec<ReplacementActionWouldEnter>,
    ),
    PutACardOfTypeMilledThisWayIntoHand(Box<Cards>),
    RemoveACounterFromPlayer(Box<Player>),
    PutAnyNumberOfCardsOfTypeMilledThisWayOntoTheBattlefield(
        Box<Cards>,
        Vec<ReplacementActionWouldEnter>,
    ),
    PutUptoNumberCardsOfTypeMilledThisWayIntoHand(Box<GameNumber>, Box<Cards>),
    SearchLibraryAndGraveyardAndHand(Vec<SearchLibraryAction>),
    ShuffleLibraryIfSearched,
    ThereAreNumberAdditionalUpkeepSteps(Box<GameNumber>),
    SetAttackAssignmentOfCreatures(Box<Permanents>, AttackAssignment),
    CastASpellFromExileOntoBottomOfLibrary(Box<Spells>, CardsInExile),
    DestroyUptoOnePermanentEachPlayerControls(Box<Permanents>, Box<Players>),
    MayCastASpellFromEachPlayersGraveyardWithoutPayingIntoExile(Box<Spells>, Box<Players>),
    ExileEachPermanentInAShuffledFaceDownPile(Box<Permanents>),
    CloakEachExiledCard(CardsInExile, Vec<ReplacementActionWouldEnter>),
    SecretlyChooseANumberBetween(i32, i32),
    ChooseACardInHandOrAPermanent(Box<Cards>, Box<Permanents>),
    CloakCardFromHand(CardInHand),
    LoseAllRadCounters,
    MayCastGraveyardCardAndMaySpendManaAsThoughAnyType(CardInGraveyard),
    PutUptoNumberCardsFromHandAndOrGraveyardOnBattlefield(
        Box<GameNumber>,
        Box<Cards>,
        Box<Player>,
        Vec<ReplacementActionWouldEnter>,
    ),
    PutNumberCardsFromExileOntoBattlefield(
        Box<GameNumber>,
        CardsInExile,
        Vec<ReplacementActionWouldEnter>,
    ),
    RevealUptoNumberCardsOfTypeFromHand(Box<GameNumber>, Box<CardsInHand>),
    CloakACardFromHand,
    SecretlyVoteForUptoOnePermanent(Box<Permanents>),
    PlayACardFromExileWithoutPaying(Box<CardsInExile>),
    CloakTheTopCardOfPlayersLibrary(Box<Player>),
    CastASpellFromPlayersGraveyardWithoutPaying(Box<Spells>, Box<Player>),
    ChooseNewTargetsForAnyNumberOfSpellsOrAbilities(SpellsAndAbilities),
    PutACardOfTypeMilledThisWayOntoTheBattlefield(Box<Cards>, Vec<ReplacementActionWouldEnter>),
    SecretlyChooseACreatureTypeFromList(Vec<CreatureType>),
    SecretlyChooseAGraveyardCard(Box<CardsInGraveyard>),
    GetOneTimeConditionalBoon(Trigger, Condition, Box<Actions>),
    ShuffleAPermanentIntoLibrary(Box<Permanents>),
    GetARadCounter,
    PhaseInPermanent(Box<Permanent>),
    RevealTheSecretlyChosenNumber,
    GuessWhichNumberWasSecretlyChosen,
    SecretlyChooseANumberBetweenThatHasntBeenChosen(i32, i32),
    ExileACardOfTypeFromHandWithANumberOfCountersOfType(Box<Cards>, Box<GameNumber>, CounterType),
    PutEachCardOfTypeMilledThisWayOntoTheBattlefieldAsFaceDownArtifactCreatures(
        Box<Cards>,
        PT,
        CreatureType,
    ),
    MayPutADuplicateCounterOnEachPermanent(Box<Permanents>),
    RevealFaceDownPermanent(Box<Permanent>),
    PhaseInEachPermanent(Box<Permanents>),
    ExploreWithPermanentNumberTimes(Box<Permanent>, Box<GameNumber>),
    ChooseOneOrTwoPermanents(Box<Permanents>),
    OnlyAllowedAttackersDuringTheAdditionalCombatStepAddedThisWay(Box<Permanents>),
    CastTheCardDiscardedThisWayWithoutPaying,
    PerpetuallyExchangePowerOfPermanentAndPermanent(Box<Permanent>, Box<Permanent>),
    PutTheTopNumberCardsOfPlayersLibraryOntoBattlefieldAsFaceDownArtifactCreatures(
        Box<GameNumber>,
        PT,
        CreatureType,
    ),
    PlayExiledCard(Box<CardInExile>),
    ExileAllCardsOfTypeFromLibrary(CardsInLibrary),
    Discover(Box<GameNumber>),
    RemoveNumberCountersOfTypeFromAnExiledCard(Box<GameNumber>, CounterType, CardsInExile),
    PutNumberCountersOfTypeOnAPermanent(Box<GameNumber>, CounterType, Box<Permanents>),
    MayReselectWhichPlayerOrPermanentEachCreatureIsAttacking(Box<Permanents>),
    PlayALandFromTopOfLibraryOrCastASpellFromTopOfLibraryWithTrigger(
        Box<Permanents>,
        Box<Spells>,
        Box<Actions>,
    ),
    ExilePermanentFaceDown(Box<Permanent>),
    CastASpellFromHandForAlternateCost(Box<Spells>, Box<Cost>),
    PutExiledCardOnTheBottomOfItsOwnersLibrary(Box<CardInExile>),
    PutNumberCardsFromAmongPlayersGraveyardsOntoTheBattlefield(
        Box<GameNumber>,
        CardsInGraveyard,
        Box<Players>,
        Vec<ReplacementActionWouldEnter>,
    ),
    MayPutACardFromHandOrGraveyardOnBattlefieldForEachPermanent(
        Box<Cards>,
        Vec<ReplacementActionWouldEnter>,
        Box<Permanents>,
    ),
    ReselectWhichPlayerOrPermanentCreatureIsAttacking(Box<Permanent>),
    PutEachExiledCardOntoTheBattlefieldFaceDownAsAnArtifactCreature(CardsInExile, PT, CreatureType),
    PutAGraveyardCardOntoBattlefield(Box<CardsInGraveyard>, Vec<ReplacementActionWouldEnter>),
    PutEachCardOfTypeMilledThisWayIntoHand(Box<Cards>),
    PutNumberCountersOfTypeOnSpell(Box<GameNumber>, CounterType, Box<Spell>),
    DistributeNumberCountersOfTypeAmongAnyNumberOfPermanentsGreaterThanZero(
        Box<GameNumber>,
        CounterType,
        Box<Permanents>,
    ),
    ChooseACreatureType_And_ChooseACreatureTypeOtherThan(CreatureType),
    PutCommanderFromCommandZoneOntoBattlefield(Commander, Vec<ReplacementActionWouldEnter>),
    CreateEachCardInPlayersGraveyardEffectUntil(
        Box<Cards>,
        Box<Player>,
        Vec<GraveyardCardEffect>,
        Expiration,
    ),
    ReturnThisExiledCardToBattlefield(Vec<ReplacementActionWouldEnter>),
    CreateGraveyardCardEffectUntil(CardInGraveyard, Vec<GraveyardCardEffect>, Expiration),
    CastSpellFromExileWithoutPayingAndFlagSpellsCastWithEffect(
        Box<Spells>,
        CardInExile,
        Box<Spells>,
        Vec<SpellEffect>,
    ),
    RemoveACounterOfTypeFromEachExiledCard(CounterType, CardsInExile),
    CastExiledCardForReducedCost(CardInExile, CostReduction),
    TimeTravelNumberTimes(Box<GameNumber>),
    TimeTravel,
    CreateExiledCardEffect(CardInExile, Vec<ExiledCardEffect>),
    CreateEachExiledCardEffect(CardsInExile, Vec<ExiledCardEffect>),
    ExileTopCardOfOtherLibrariesFaceDown(Box<Players>),
    ExileUptoOneCardFromEachPlayersGraveyard(Box<Cards>, Box<Players>),
    NoteManaTypeAndAmountSpentToActivateThisAbility,
    ExileBottomCardOfOtherLibrariesFaceDown(Box<Players>),
    CopyAnExiledCardNumberTimes(CardsInExile, Box<GameNumber>),
    CounterSpellIntoExileWithANumberOfCountersAndWithEffects(
        Box<Spell>,
        Box<GameNumber>,
        CounterType,
        Vec<ExiledCardEffect>,
    ),
    ExileTopNumberCardsOfOtherLibraryFaceDown(Box<GameNumber>, Box<Player>),
    AttachAnyNumberOfPermanentsToPlayerOrPermanent(Box<Permanents>, PlayerOrPermanent),
    AttachAnyNumberOfPermanentsToAnyPermanents(Box<Permanents>, Box<Permanents>),
    PutEachCommanderFromCommandZoneOntoBattlefield(Commanders, Vec<ReplacementActionWouldEnter>),
    PutPermanentIntoOwnersGraveyard(Box<Permanent>),
    PutCardFromAnywhereIntoPlayersGraveyard(SingleCard),
    AbandonScheme(SingleScheme),
    AcceptARandomCondition(Offerer),
    AcceptARandomOffer(Offerer),
    ActivateAManaAbilityOfEachPermanentAndLoseUnspentMana(Box<Permanents>),
    Adapt(Box<GameNumber>),
    AddCombinationMana(ManaProduce, Box<GameNumber>),
    AddCombinationManaWithModifers(ManaProduce, Box<GameNumber>, ManaUseModifier),
    AddMana(ManaProduce),
    AddManaRepeated(ManaProduce, Box<GameNumber>),
    AddManaRepeatedTwiceWithModifiers(
        ManaProduce,
        Box<GameNumber>,
        ManaProduce,
        Box<GameNumber>,
        ManaUseModifier,
    ),
    AddManaRepeatedWithModifiers(ManaProduce, Box<GameNumber>, ManaUseModifier),
    AddManaWithModifiers(ManaProduce, ManaUseModifier),
    Amass(Box<GameNumber>, CreatureType),
    AnteTopCardOfLibrary,
    AttachAPermanentToPermanent(Box<Permanents>, Box<Permanent>),
    AttachAnyNumberOfPermanentsToPermanent(Box<Permanents>, Box<Permanent>),
    AttachEachPermanentToAPermanent(Box<Permanents>, Box<Permanents>),
    AttachEachPermanentToPermanent(Box<Permanents>, Box<Permanent>),
    AttachPermanentToACardInAPlayersGraveyard(Box<Permanent>, Box<Cards>, Box<Players>),
    AttachPermanentToAPermanent(Box<Permanent>, Box<Permanents>),
    AttachPermanentToPermanent(Box<Permanent>, Box<Permanent>),
    AttachPermanentToPlayer(Box<Permanent>, Box<Player>),
    AttachUptoOnePermanentToEachPermanent(Box<Permanents>, Box<Permanents>),
    BecomeDay,
    BecomeNight,
    BecomeTheMonarch,
    BeginGameWithCardOnBattlefield(PregameCard, Vec<ReplacementActionWouldEnter>),
    Bolster(Box<GameNumber>),
    CastACopiedCardWithoutPaying,
    CastASpellAndMaySpendManaAsThoughAnyColorToCast(Box<Spells>),
    CastASpellDrawnThisWayWithoutPaying,
    CastASpellFromAPlayersGraveyardWithoutPayingIntoExile(Box<Spells>, Box<Players>),
    CastASpellFromExile(Box<Spells>, CardsInExile),
    CastASpellFromExileWithEffect(Box<Spells>, CardsInExile, Vec<SpellEffect>),
    CastASpellFromExileWithoutPaying(Box<Spells>, CardsInExile),
    CastASpellFromHandGraveyardOrExileWithoutPaying(Box<Spells>, Box<Cards>, Cards, CardsInExile),
    CastASpellFromHandWithoutPaying(Box<Spells>),
    CastASpellFromMilledCardsWithoutPaying(Box<Spells>),
    CastASpellFromPlayersGraveyardWithoutPayingIntoExile(Box<Spells>, Box<Player>),
    CastASpellFromRevealedCardInHandsWithoutPaying(Box<Spells>, Box<Cards>),
    CastAnyNumberOfCardsInPlayersGraveyardWithoutPaying(Box<Cards>, Box<Player>),
    CastAnyNumberOfCopiedCards,
    CastAnyNumberOfCopiedCardsWithoutPaying,
    CastAnyNumberOfExiledCardsWithoutPaying(Box<CardsInExile>),
    CastAnyNumberOfGraveyardCardsWithoutPayingIntoExile(Box<CardsInGraveyard>),
    CastAnyNumberOfGroupSpellsFromExileWithoutPaying(Box<Spells>, GroupFilter, CardsInExile),
    CastAnyNumberOfSpellsFromExileWithoutPaying(Box<Spells>, CardsInExile),
    CastAnyNumberOfSpellsFromHandWithoutPaying(Box<Spells>),
    CastAnyNumberOfSpellsFromOutsideTheGameWithoutPaying(Box<Spells>),
    CastCardInHandWithoutPaying(CardInHand),
    CastCardInHandWithoutPayingAsAFacedownCreatureSpell(CardInHand, PT),
    CastCommanderFromCommandZoneWithoutPaying,
    CastCopiedCard,
    CastCopiedCardForAlternateCost(Box<Cost>),
    CastCopiedCardForReducedCost(ManaCost),
    CastCopiedCardWithoutPaying,
    CastEachCopiedCardWithoutPaying,
    CastExiledCardAndMaySpendManaAsThoughAnyColorToCast(Box<CardInExile>),
    CastExiledCardWithoutPaying(Box<CardInExile>),
    CastExiledCardWithoutPayingOntoBottomOfLibrary(Box<CardInExile>),
    CastGraveyardCard(CardInGraveyard),
    CastGraveyardCardIntoExile(CardInGraveyard),
    CastGraveyardCardWithoutPaying(CardInGraveyard),
    CastGraveyardCardWithoutPayingIntoExile(CardInGraveyard),
    CastSpellFromExile(Box<Spells>, CardInExile),
    CastSpellFromExileWithoutPaying(Box<Spells>, CardInExile),
    CastSpellsFromExileWithoutPaying(Box<Spells>, CardsInExile),
    CastSpellFromGraveyardWithoutPayingIntoExile(Box<Spells>, CardInGraveyard),
    CastSpellFromHandOrGraveyardAlternateCost(Box<Spells>, Box<Cost>),
    CastTheCardRevealedThisWayWithoutPaying,
    CastTopCardOfLibraryForAlternateCost(ManaCost),
    CastTopCardOfLibraryWithoutPaying,
    CastTopCardOfPlayersLibraryWithoutPaying(Box<Player>),
    CastTopSpellOfLibraryWithoutPaying(Box<Spells>),
    CastUptoNumberExiledCardsWithoutPaying(Box<GameNumber>, CardsInExile),
    CastUptoNumberGroupSpellsFromGraveyardOrHandWithoutPayingIntoExile(
        Box<GameNumber>,
        Box<Spells>,
        GroupFilter,
    ),
    CastUptoNumberSpellsFromExileWithoutPaying(Box<GameNumber>, Box<Spells>, CardsInExile),
    CastUptoNumberSpellsFromHandWithoutPaying(Box<GameNumber>, Box<Spells>),
    ChangeATargetOfSpellOrAbilityToPermanent(SpellOrAbility, Box<Permanent>),
    ChangeTargetsOfAbility(Ability),
    ChangeTargetsOfSpell(Box<Spell>),
    ChangeTargetsOfSpellOrAbility(SpellOrAbility),
    ChangeTheTargetOfAbility(Ability),
    ChangeTheTargetOfSpell(Box<Spell>),
    ChangeTheTargetOfSpellOrAbility(SpellOrAbility),
    ChangeTheTargetOfSpellToAPermanent(Box<Spell>, Box<Permanents>),
    ChangeTheTargetOfSpellToPermanent(Box<Spell>, Box<Permanent>),
    ChangeTheTargetsOfSpellToAPlayer(Box<Spell>, Box<Players>),
    ChooseABasicLandType,
    ChooseACardFromAmongCardsDiscardedThisWay(Box<Cards>),
    ChooseACardFromAmongTheTopNumberCardsInPlayersGraveyard(
        Box<Cards>,
        Box<GameNumber>,
        Box<Player>,
    ),
    ChooseACardFromPlayersRevealedHand(Box<Cards>, Box<Player>),
    ChooseACardInEachPlayersGraveyard(Box<Cards>, Box<Players>),
    ChooseACardInHand(Box<CardsInHand>),
    ChooseACardInHandAtRandom(Box<Cards>),
    ChooseACardInHandOfEachColor(Box<Cards>),
    ChooseACardInPlayersGraveyard(Box<Cards>, Box<Player>),
    ChooseACardInPlayersGraveyardAtRandom(Box<Cards>, Box<Player>),
    ChooseACardName(Box<Cards>),
    ChooseACardNameThatHasntBeenChosen(Box<Cards>),
    ChooseACardOfTypeInPlayersHandAtRandom(Box<Cards>, Box<Player>),
    ChooseACardtype,
    ChooseACardtypeFromList(Vec<CardType>),
    ChooseACheckableAbility(Vec<CheckHasable>),
    ChooseAColor(ChoosableColor),
    ChooseAColorOrColorless(ChoosableColor),
    ChooseACommanderOnTheBattlefieldOrInTheCommandZone(Commanders),
    ChooseACounterTypeOnAPermanent(Box<Permanents>),
    ChooseACounterTypeOnPermanent(Box<Permanent>),
    ChooseACounterTypeOnPlayer(Box<Player>),
    ChooseACreatureType,
    ChooseACreatureTypeOtherThan(CreatureType),
    ChooseADamageNumber,
    ChooseADamageRecipient(DamageRecipientsList),
    ChooseADamageSource(DamageSources),
    ChooseAGraveyardCard(Box<CardsInGraveyard>),
    ChooseAGraveyardPile,
    ChooseALandType,
    ChooseALandTypeAndABasicLandType,
    ChooseALetter,
    ChooseANamedAction(Vec<NamedAction>),
    ChooseANumber,
    ChooseANumberBetween(i32, i32),
    ChooseANumberBetweenAtRandom(Box<GameNumber>, Box<GameNumber>),
    ChooseANumberFromAmongAtRandom(Vec<i32>),
    ChooseANumberGreaterThanNumber(i32),
    ChooseAPartyFromAmongPermanents(Box<Permanents>),
    ChooseAPermanent(Box<Permanents>),
    ChooseAPermanentAtRandom(Box<Permanents>),
    ChooseAPermanentForEachPlayer(Box<Players>, Box<Permanents>),
    ChooseAPermanentOfEachBasicLandTypeAvailable(Box<Permanents>),
    ChooseAPermanentOfEachPermanentTypeAvailable(Box<Permanents>),
    ChooseAPermanentOfEachPowerAvailable(Box<Permanents>),
    ChooseAPermanentPile,
    ChooseAPermanentThatHasntBeenChosen(Box<Permanents>),
    ChooseAPermanentType,
    ChooseAPileCreatedByEachPlayer(Box<Players>),
    ChooseAPileCreatedByEachPlayerAtRandom(Box<Players>),
    ChooseAPlaneswalkerType,
    ChooseAPlayer(Box<Players>),
    ChooseAPlayerAtRandom(Box<Players>),
    ChooseAPlayerOrPlaneswalkerCurrentlyAttackedByPlayer(Box<Player>),
    ChooseASecondPermanentAtRandom(Box<Permanents>),
    ChooseASector,
    ChooseASpellThatResolvedThisTurn(Box<Spells>),
    FaceAVillianousChoice(Vec<Vec<Action>>),
    ChooseActionAtRandom(Vec<Vec<Action>>),
    ChooseAnAbility(Vec<Rule>),
    ChooseAnAbilityAtRandom(Vec<Rule>),
    ChooseAnAction(Vec<Vec<Action>>),
    ChooseAnAttackingCreatureForBlockerToBlock(Box<Permanents>, Box<Permanent>),
    ChooseAnExiledCard(Box<CardsInExile>),
    ChooseAnExiledCardAtRandom(Box<CardsInExile>),
    ChooseAnExiledPile,
    ChooseAnUnchosenCardInPlayersGraveyard(Box<Cards>, Box<Player>),
    ChooseAnyNumberOfGroupPermanents(Box<Permanents>, GroupFilter),
    ChooseAnyNumberOfPermanents(Box<Permanents>),
    ChooseAnyNumberPermanentsAndPayManaForEach(Box<Permanents>, ManaCost),
    ChooseColors,
    ChooseCopyFromCopies,
    ChooseCounterAtRandomPermanentDoesntHave(Box<Permanent>, Vec<CounterType>),
    ChooseEvenOrOdd,
    ChooseLandType(Vec<LandType>),
    ChooseLeftOrRight,
    ChooseLibraryFilter(Vec<Cards>),
    ChooseMultiplePermanentsAmoungPermanents(Vec<Permanents>, Box<Permanents>),
    ChooseMultiplePermanentsAmoungPermanentsForEachPlayer(
        Box<Players>,
        Vec<Permanents>,
        Box<Permanents>,
    ),
    ChooseNamedPileForPermanent(Vec<VoteOption>, Box<Permanent>),
    SwapWordChoice(VoteOption, VoteOption),
    ChooseNewTargetsForSpell(Box<Spell>),
    ChooseNewTargetsForSpellOrAbility(SpellOrAbility),
    ChooseNumberAbilitiesAtRandom(Box<GameNumber>, Vec<Rule>),
    ChooseNumberCardsFromAmongCardsInHandRevealedThisWay(Box<GameNumber>, Box<Cards>),
    ChooseNumberCardsInEachPlayersGraveyard(Box<GameNumber>, Box<Cards>, Box<Players>),
    ChooseNumberCardsInHand(Box<GameNumber>),
    ChooseNumberCardsInPlayersGraveyard(Box<GameNumber>, Box<Cards>, Box<Player>),
    ChooseNumberGraveyardCards(Box<GameNumber>, Box<CardsInGraveyard>),
    ChooseNumberPermanents(Box<GameNumber>, Box<Permanents>),
    ChoosePTMod(Vec<PTMod>),
    ChoosePermanentFilter(Vec<Permanents>),
    ChooseProtectionFromAColorOrFromArtifact,
    ChooseRandomColorPermanentDoesntHaveProtectionFrom(Box<Permanent>),
    ChooseTwoBasicLandTypes,
    ChooseTwoColorWords,
    ChooseUptoNumberCardsFromAmongCardsInPlayersHandRevealedThisWay(
        Box<GameNumber>,
        Box<Cards>,
        Box<Player>,
    ),
    ChooseUptoNumberCardsInHand(Box<GameNumber>, Box<Cards>),
    ChooseUptoNumberExiledCards(Box<GameNumber>, CardsInExile),
    ChooseUptoNumberPermanents(Box<GameNumber>, Box<Permanents>),
    ChooseUptoNumberPermanentsForEach(Box<GameNumber>, Box<Permanents>, Box<GameNumber>),
    ChooseUptoOnePermanent(Box<Permanents>),
    ChooseUptoOnePermanentForEachPlayer(Box<Permanents>),
    ChooseWord(Vec<VoteOption>),
    CircleNumberColors(Box<GameNumber>, ChoosableColor),
    ClaimThePrize,
    Clash(Box<Players>, Vec<Action>, Vec<Action>),
    ControllersSacrificeEachPermanent(Box<Permanents>),
    ConvertPermanent(Box<Permanent>),
    CopyAbilityAndMayChooseNewTargets(Ability),
    CopyAbilityForEachPermanentItCouldTarget(Ability, Box<Permanents>),
    CopyActivatedAbilityAndMayChooseNewTargets(ActivatedAbility),
    CopyAnExiledCard(Box<CardsInExile>),
    CopyCardInHand(CardInHand),
    CopyCardWithTheNotedName,
    CopyEachSpellAndMayChooseNewTargets(Box<Spells>),
    CopyEnchantedGraveyardCard,
    CopyExiledCard(Box<CardInExile>),
    CopyExiledCardNumberTimes(CardInExile, Box<GameNumber>),
    CopyExiledCards(Box<CardsInExile>),
    CopyNumberCardsAtRandom(Box<GameNumber>, Box<Cards>),
    CopySpell(Box<Spell>),
    CopySpellAndMayChooseNewTargets(Box<Spell>),
    CopySpellAndMayChooseNewTargetsWithEffects(Box<Spell>, Vec<SpellEffect>),
    CopySpellAndMustChooseNewTarget(Box<Spell>, Box<Permanent>),
    CopySpellAndRandomlyChooseNewTargetsExceptFor(Box<Spell>, PlayersAndPermanents),
    CopySpellForEach(Box<Spell>, Box<GameNumber>),
    CopySpellForEachAndMayChooseNewTargets(Box<Spell>, Box<GameNumber>),
    CopySpellForEachOtherPermanentOrPlayerAndMustChooseThemAsNewTarget(Box<Spell>),
    CopySpellForEachPermanentAndMustChooseItAsNewTarget(Box<Spell>, Box<Permanents>),
    CopySpellForEachPlayerAndMustChooseNewTargetPermanentTheyControl(Box<Spell>, Box<Players>),
    CopySpellForEachPlayerAndMustChooseThemAsNewTarget(Box<Spell>, Box<Players>),
    CopySpellForEachSpellPermanentCardAndOrPlayerItCouldTarget(Box<Spell>),
    CopySpellNumberTimes(Box<Spell>, Box<GameNumber>),
    CopySpellNumberTimesAndMayChooseNewTargets(Box<Spell>, Box<GameNumber>),
    CopySpellOrAbilityAndMayChooseNewTargets(SpellOrAbility),
    CopySpellOrAbilityForEachPermanentOrPlayerItCouldTarget(SpellOrAbility),
    CopySpellOrAbilityNumberTimesAndMayChooseNewTargets(SpellOrAbility, Box<GameNumber>),
    CopySpellWithModifiers(Box<Spell>, Vec<SpellEffect>),
    CounterAbility(Ability),
    CounterEachAbility(Abilities),
    CounterEachSpell(Box<Spells>),
    CounterSpell(Box<Spell>),
    CounterSpellAndSpellsOfTypeAreCounteredOntoTheBattlefield(
        Box<Spell>,
        Box<Spells>,
        Vec<ReplacementActionWouldEnter>,
    ),
    CounterSpellIntoBottomOfLibrary(Box<Spell>),
    CounterSpellIntoExile(Box<Spell>),
    CounterSpellIntoHand(Box<Spell>),
    CounterSpellIntoTopOfLibrary(Box<Spell>),
    CounterSpellIntoTopOrBottomOfLibrary(Box<Spell>),
    CounterSpellOrAbility(SpellOrAbility),
    GetOneTimeBoon(Trigger, Box<Actions>),
    GetANumberTimeBoon(Box<GameNumber>, Trigger, Box<Actions>),
    CreateCopiesOfRandomCardsWithManaCosts(Box<GameNumber>),
    CreateEachPermanentLayerEffect(Box<Permanents>, Vec<LayerEffect>),
    CreateEachPermanentLayerEffectUntil(Box<Permanents>, Vec<LayerEffect>, Expiration),
    CreateEachPermanentRuleEffect(Box<Permanents>, Vec<PermanentRule>),
    CreateEachPermanentRuleEffectUntil(Box<Permanents>, Vec<PermanentRule>, Expiration),
    CreateEachPlayerEffectUntil(Box<Players>, Vec<PlayerEffect>, Expiration),
    CreateEachSpellEffect(Box<Spells>, Vec<SpellEffect>, Expiration),
    CreateFuturePlayerEffect(Box<Player>, FuturePlayerEffect),
    CreateFutureSpellEffect(FutureSpell, Vec<SpellEffect>),
    CreateFutureTrigger(FutureTrigger, Box<Actions>),
    CreateFutureTriggerI(FutureTrigger, Condition, Box<Actions>),
    CreateFutureTrigger_UnlessPlayerPaysManaBefore(
        FutureTrigger,
        Box<Actions>,
        Box<Player>,
        ManaCost,
    ),
    CreateGameEffect(Expiration, GameEffect),
    CreateGroupExileEffect(Expiration, CardsInExile, Vec<GroupExiledEffect>),
    CreateLimitedSpellEffect(Expiration, Box<Spell>, Vec<SpellEffect>),
    CreatePermanentLayerEffect(Box<Permanent>, Vec<LayerEffect>),
    CreatePermanentLayerEffectUntil(Box<Permanent>, Vec<LayerEffect>, Expiration),
    CreatePermanentRuleEffect(Box<Permanent>, Vec<PermanentRule>),
    CreatePermanentRuleEffectUntil(Box<Permanent>, Vec<PermanentRule>, Expiration),
    CreatePermanentSpellLayerEffect(Expiration, Box<Spell>, Vec<LayerEffect>),
    CreatePermanentsList(Box<Permanents>),
    CreatePerpetualCardInHandEffect(CardInHand, Vec<PerpetualEffect>),
    CreatePerpetualCardInLibraryEffect(CardInLibrary, Vec<PerpetualEffect>),
    CreatePerpetualCardsInHandEffect(Box<CardsInHand>, Vec<PerpetualEffect>),
    CreatePerpetualCardsInEachPlayersHandEffect(
        Box<CardsInHand>,
        Box<Players>,
        Vec<PerpetualEffect>,
    ),
    CreatePerpetualCardsInPlayersGraveyardEffect(
        CardsInGraveyard,
        Box<Player>,
        Vec<PerpetualEffect>,
    ),
    CreatePerpetualCardsInPlayersHandEffect(Box<CardsInHand>, Box<Player>, Vec<PerpetualEffect>),
    CreatePerpetualCardsInPlayersLibraryEffect(CardsInLibrary, Box<Player>, Vec<PerpetualEffect>),
    CreatePerpetualDeadCardEffect(Vec<PerpetualEffect>),
    CreatePerpetualEachExiledCardEffect(CardsInExile, Vec<PerpetualEffect>),
    CreatePerpetualEachGraveyardCardEffect(Box<CardsInGraveyard>, Vec<PerpetualEffect>),
    CreatePerpetualEachPermanentEffect(Box<Permanents>, Vec<PerpetualEffect>),
    CreatePerpetualExiledCardEffect(CardInExile, Vec<PerpetualEffect>),
    CreatePerpetualGraveyardCardEffect(CardInGraveyard, Vec<PerpetualEffect>),
    CreatePerpetualPermanentEffect(Box<Permanent>, Vec<PerpetualEffect>),
    CreatePerpetualSacrificedCardEffect(Vec<PerpetualEffect>),
    CreatePerpetualSpellEffect(Box<Spell>, Vec<PerpetualEffect>),
    CreatePlayerEffect(Box<Player>, Vec<PlayerEffect>),
    CreatePlayerEffectUntil(Box<Player>, Vec<PlayerEffect>, Expiration),
    CreateSpellEffect(Box<Spell>, Vec<SpellEffect>),
    CreateSpellOrPermanentEffect(Expiration, SpellOrPermanent, Vec<SpellOrPermanentEffect>),
    CreateTrigger(Trigger, Box<Actions>),
    CreateTriggerUntilI(Trigger, Condition, Box<Actions>, Expiration),
    CreateTriggerOnce(Expiration, Trigger, Box<Actions>),
    CreateTriggerUntil(Trigger, Box<Actions>, Expiration),
    CreatureConnives(Box<Permanent>),
    CreatureConnivesNumber(Box<Permanent>, Box<GameNumber>),
    CreatureMustAttackDuringControllersNextCombatPhase(Box<Permanent>),
    DestroyAPermanentAtRandom(Box<Permanents>),
    DestroyAPermanentNoRegen(Box<Permanents>),
    DestroyEachPermanent(Box<Permanents>),
    DestroyEachPermanentNoRegen(Box<Permanents>),
    DestroyEachPermanentNoRegenSubset(Box<Permanents>, Box<Permanents>),
    DestroyNumberPermanents(Box<GameNumber>, Box<Permanents>),
    DestroyPermanent(Box<Permanent>),
    DestroyPermanentNoRegen(Box<Permanent>),
    DestroyUptoNumberPermanents(Box<GameNumber>, Box<Permanents>),
    DetainEachPermanent(Box<Permanents>),
    DetainPermanent(Box<Permanent>),
    DigitallySearchLibrary(Vec<SearchLibraryAction>),
    DiscardACard,
    DiscardACardAtRandom,
    DiscardACardOfType(Box<Cards>),
    DiscardAllButNumberCards(Box<GameNumber>),
    DiscardAnyNumberOfCards,
    DiscardAnyNumberOfCardsAtRandom,
    DiscardAnyNumberOfCardsOfType(Box<Cards>),
    DiscardCard(CardInHand),
    DiscardCards(Box<CardsInHand>),
    DiscardEachCard(Box<CardsInHand>),
    DiscardHand,
    DiscardNumberCards(Box<GameNumber>),
    DiscardNumberCardsAtRandom(Box<GameNumber>),
    DiscardNumberCardsDrawnThisTurn(Box<GameNumber>),
    DiscardNumberCardsOfType(Box<GameNumber>, Box<Cards>),
    DiscardTheCardDrawnThisWay,
    DiscardTheCardRevealedThisWay,
    DiscardUptoNumberCards(Box<GameNumber>),
    DistrbuteUptoNumberArtStickersAmongAnyNumberOfPermanents(Box<GameNumber>, Box<Permanents>),
    DistributeNumberCountersOfTypeAmongAnyNumberOfPermanents(
        Box<GameNumber>,
        CounterType,
        Box<Permanents>,
    ),
    DoubleCountersOfEachTypeOnEachPermanent(Box<Permanents>),
    DoubleCountersOfEachTypeOnPermanent(Box<Permanent>),
    DoubleCountersOfTypeOnEachPermanent(CounterType, Box<Permanents>),
    DoubleCountersOfTypeOnPermanent(CounterType, Box<Permanent>),
    DoubleCreaturesPowerAndToughnessUntilEndOfTurn(Box<Permanent>),
    DoubleCreaturesPowerNumberTimesUntilEndOfTurn(Box<Permanent>, Box<GameNumber>),
    DoubleCreaturesPowerUntilEndOfTurn(Box<Permanent>),
    DoubleEachCreaturesPowerAndToughnessUntilEndOfTurn(Box<Permanents>),
    DoubleEachCreaturesPowerUntilEndOfTurn(Box<Permanents>),
    DoubleEachCreaturesToughnessUntilEndOfTurn(Box<Permanents>),
    DoubleEachTypeOfUnspentMana,
    DoubleTheStake,
    DoubleXValueOfSpell(Box<Spell>),
    DraftACardFromSpellBook(SpellBookName),
    DraftACardFromSpellBookNumberTimes(SpellBookName, Box<GameNumber>),
    CollectEvidence(Box<GameNumber>),
    DrawACard,
    DrawNumberCards(Box<GameNumber>),
    DrawNumberCardsForEach(Box<GameNumber>, Box<GameNumber>),
    DrawNumberCardsThenDiscardNumberOfThem(Box<GameNumber>, Box<GameNumber>),
    DrawUptoNumberCards(Box<GameNumber>),
    EachCreatureConnives(Box<Permanents>),
    EachPermanentDealsDamage(Box<Permanents>, Box<GameNumber>, DamageRecipient),
    EachPermanentDoesntUntapDuringControllersNextUntap(Box<Permanents>),
    EachPlayerCantCastSpellsDuringTheirNextTurn(Box<Players>, Box<Spells>),
    EachPlayerRevealsCardsFromTheTopOfTheirLibraryUntilTheyRevealACardOfType(
        Box<Players>,
        Box<Cards>,
        Vec<RevealTheTopNumberCardsOfLibraryAction>,
    ),
    EndTheCombatPhase,
    EndTheTurn,
    ExchangeAnteCardWithTopCardOfPlayersLibrary(AnteCard, Box<Player>),
    ExchangeControl(Box<Permanent>, Box<Permanent>),
    ExchangeControlOfEachPermanentWithPlayer(Box<Permanents>, Box<Player>),
    ExchangeControlOfEachPermanentWithPlayerUntil(Box<Permanents>, Box<Player>, Expiration),
    ExchangeControlOfSpellAndPermanent(Box<Spell>, Box<Permanent>),
    ExchangeGraveyardAndLibrary,
    ExchangeHandAndGraveyard,
    ExchangeHandAndLibraryThenShuffle,
    ExchangeLifeTotalWithPermanentsPower(Box<Permanent>),
    ExchangeLifeTotalWithPermanentsToughness(Box<Permanent>),
    ExchangeLifeTotalWithPlayer(Box<Player>),
    ExchangePowerOfTwoCreaturesUntil(Box<Permanent>, Box<Permanent>, Expiration),
    ExchangeTextBoxesOfTwoPermanentsUntil(Box<Permanent>, Box<Permanent>, Expiration),
    ExileACardFromHand,
    ExileACardFromHandAtRandom,
    ExileACardFromHandFaceDown,
    ExileACardFromHandOrGraveyard(Box<Cards>),
    ExileACardFromHandUntil(Expiration),
    ExileACardFromPlayersGraveyardAtRandom(CardsInGraveyard, Box<Player>),
    ExileACardFromPlayersHandOrGraveyard(Box<Cards>, Box<Player>),
    ExileACardFromPlayersRevealedHand(Box<Cards>, Box<Player>),
    ExileACardOfTypeFromHand(Box<Cards>),
    ExileACardOfTypeFromPlayersLibraryAtRandom(Box<Cards>, Box<Player>),
    ExileAPermanent(Box<Permanents>),
    ExileAPermanentUntil(Box<Permanents>, Expiration),
    ExileAllCardsInPlayersLibrary(Box<Player>),
    ExileAllLibraryCards,
    ExileAllLibraryCardsFaceDown,
    ExileAnyNumberOfCardsFromHandFaceDown,
    ExileAnyNumberOfCardsFromPlayersGraveyard(Box<Cards>, Box<Player>),
    ExileAnyNumberOfPermanents(Box<Permanents>),
    ExileAnyNumberOfPermanentsUntil(Box<Permanents>, Expiration),
    ExileBottomCardOfPlayersGraveyard(Box<Cards>, Box<Player>),
    ExileBottomCardOfTypeFromLibrary(Box<Cards>),
    ExileCardFromHand(CardInHand),
    ExileCardFromHandAndGraveyardCard(CardInHand, CardInGraveyard),
    ExileCardFromHandFaceDown(CardInHand),
    ExileCardsFromHand(Box<CardsInHand>),
    ExileCardsFromTheTopOfLibraryUntilACardOfTypeIsExiled(Box<Cards>),
    ExileCardsFromTheTopOfLibraryUntilANumberOfCardsOfTypeAreExiled(Box<GameNumber>, Box<Cards>),
    ExileCardsInGraveyardDiscardedThisWay,
    ExileEachCardFromEachPlayersGraveyard(Box<Cards>, Box<Players>),
    ExileEachCardFromHandAndGraveyard(Box<Cards>),
    ExileEachCardFromPlayersGraveyard(Box<Cards>, Box<Player>),
    ExileEachCardFromPlayersGraveyardInShuffledFaceDownPile(Box<Cards>, Box<Player>),
    ExileEachCardFromPlayersGraveyardUntil(Box<Cards>, Box<Player>, Expiration),
    ExileEachCardOfTypeFromPlayersHand(Box<Cards>, Box<Player>),
    ExileEachGraveyardCard(Box<CardsInGraveyard>),
    ExileEachPermanent(Box<Permanents>),
    ExileEachPermanentAndGraveyardCard(PermanentsAndGraveyardCards),
    ExileEachPermanentUntil(Box<Permanents>, Expiration),
    ExileEachPermanentUntilWithTriggerEntersUnderPlayersControl(
        Box<Permanents>,
        Expiration,
        Box<Player>,
        Box<Actions>,
    ),
    ExileEachPlayersGraveyard(Box<Players>),
    ExileEachPlayersHand(Box<Players>),
    ExileEachSpell(Box<Spells>),
    ExileEnchantedGraveyardCard,
    ExileGraveyardCard(CardInGraveyard),
    ExileGraveyardCardEachCardInEachPlayersGraveyardAndEachPermanent(
        CardInGraveyard,
        Box<Cards>,
        Box<Players>,
        Box<Permanents>,
    ),
    ExileGraveyardCardWithACounterOfType(CardInGraveyard, CounterType),
    ExileGraveyardCardWithNumberCountersOfType(CardInGraveyard, Box<GameNumber>, CounterType),
    ExileHand,
    ExileHandFaceDown,
    ExileNumberCardsFromHand(Box<GameNumber>),
    ExileNumberCardsFromLibraryFaceDownAtRandom(Box<GameNumber>),
    ExileNumberPermanents(Box<GameNumber>, Box<Permanents>),
    ExileNumberPermanentsCardsFromHandOrCardsFromGraveyard(
        Box<GameNumber>,
        Box<Permanents>,
        Box<Cards>,
        Box<Cards>,
    ),
    ExilePermanent(Box<Permanent>),
    ExilePermanentAndEachPermanentAndEachCardFromEachPlayersGraveyard(
        Box<Permanent>,
        Box<Permanents>,
        Box<Cards>,
        Box<Players>,
    ),
    ExilePermanentAndEachPermanentUntil(Box<Permanent>, Box<Permanents>, Expiration),
    ExilePermanentAndTheTopCardOfPlayersLibraryInShuffledFaceDownPile(Box<Permanent>, Box<Player>),
    ExilePermanentUntil(Box<Permanent>, Expiration),
    ExilePermanentWithACounter(Box<Permanent>, CounterType),
    ExilePermanentWithANumberOfCounters(Box<Permanent>, Box<GameNumber>, CounterType),
    ExilePermanentsAndMeldIntoNewPermanent(
        Box<Permanent>,
        Box<Permanents>,
        NameString,
        Vec<ReplacementActionWouldEnter>,
    ),
    ExilePlayersGraveyard(Box<Player>),
    ExilePlayersHand(Box<Player>),
    ExileSinglePermanentAndEachPermanent(Box<Permanent>, Box<Permanents>),
    ExileSpell(Box<Spell>),
    ExileSpellWithANumberOfCountersOnIt(Box<Spell>, Box<GameNumber>, CounterType),
    ExileTheBottomNumberCardsOfLibrary(Box<GameNumber>),
    ExileTheCardRevealedThisWay,
    ExileTheTopCardOfPlayersLibrary(Box<Player>),
    ExileTheTopCardOfPlayersLibraryFaceDown(Box<Player>),
    ExileTheTopNumberCardsOfLibrary(Box<GameNumber>),
    ExileTheTopNumberCardsOfLibraryFaceDown(Box<GameNumber>),
    ExileTheTopNumberCardsOfLibraryInFaceDownPile(Box<GameNumber>),
    ExileTheTopNumberCardsOfPlayersLibrary(Box<GameNumber>, Box<Player>),
    ExileTopCardOfEachPlayersLibraries(Box<Players>),
    ExileTopCardOfEachPlayersLibrariesFaceDown(Box<Players>),
    ExileTopCardOfEachPlayersLibrariesWithACounterOfType(Box<Players>, CounterType),
    ExileTopCardOfLibrary,
    ExileTopCardOfLibraryFaceDown,
    ExileTopCardOfOtherLibraries(Box<Players>),
    ExileTopCardsOfLibraryUntilASingleCardOfTypeIsExiled(Box<Cards>),
    ExileTopCardsOfLibraryUntilGroupCardsAreExiled(GroupFilter),
    ExileTopOfLibraryForEachPlayerOrPermanentWithAction(PlayersAndPermanents, Box<Action>),
    ExileTwoPermanents(Box<Permanent>, Box<Permanent>),
    ExileUptoNumberCardsOfTypeMilledThisWay(Box<GameNumber>, Box<Cards>),
    ExileUptoOneCardOfEachCardTypeFromPlayersGraveyard(Box<Player>),
    ExploreWithPermanent(Box<Permanent>),
    Fateseal(Box<GameNumber>),
    Fight(Box<Permanent>, Box<Permanent>),
    FlipACoin,
    FlipACoinForEachPermanent(Box<Permanents>),
    FlipACoinNumberTimesOrUntilLose(Box<GameNumber>),
    FlipACoinUntilLose,
    FlipACoinUntilLoseOrStop,
    FlipACoin_OnHeadAndOnTails(Vec<Action>, Vec<Action>),
    FlipACoin_OnLose(Vec<Action>),
    FlipACoin_OnWin(Vec<Action>),
    FlipACoin_OnWinAndLose(Vec<Action>, Vec<Action>),
    FlipCoins(Box<GameNumber>),
    FlipPermanent(Box<Permanent>),
    ForEachPermanentPutANumberOfCountersOfTypeOnIt(Box<Permanents>, Box<GameNumber>, CounterType),
    ForEachPlayerChooseAWord(Box<Players>, Vec<VoteOption>),
    GainControlOfAPermanentControlledByEachPlayer(Box<Permanents>, Box<Players>),
    GainControlOfAPermanentUntil(Box<Permanents>, Expiration),
    GainControlOfEachPermanent(Box<Permanents>),
    GainControlOfEachPermanentUntil(Box<Permanents>, Expiration),
    GainControlOfPermanent(Box<Permanent>),
    GainControlOfPermanentUntil(Box<Permanent>, Expiration),
    GainControlOfPlayerDuringTheirNextTurn(Box<Player>),
    GainControlOfSpellAndMayChooseNewTargets(Box<Spell>),
    GainControlOfSpellAndRandomlyChooseNewTargetsExceptFor(Box<Spell>, PlayersAndPermanents),
    GainLife(Box<GameNumber>),
    GainLifeAndLifeForEach(Box<GameNumber>, Box<GameNumber>, Box<GameNumber>),
    GainLifeForEach(Box<GameNumber>, Box<GameNumber>),
    GetAPoisonCounter,
    GetAnEmblem(Vec<Rule>),
    GetAnExperienceCounter,
    GetCounterOfType(CounterType),
    GetEnergy(Box<GameNumber>),
    GetExperienceCounter,
    GetNumberRadCounters(Box<GameNumber>),
    GetNumberPoisonCounters(Box<GameNumber>),
    GetNumberTickets(Box<GameNumber>),
    GoadCreature(Box<Permanent>),
    GoadEachCreature(Box<Permanents>),
    GoadCreatureUntil(Box<Permanent>, Expiration),
    UngoadEachCreature(Box<Permanents>),
    GuessIfACardIsInPlayersHand(Box<Cards>, Box<Player>),
    GuessIfCardInHandPassesFilter(CardInHand, Box<Cards>),
    HaveCreaturesFight(Box<Permanent>, Box<Permanent>),
    HaveCreaturesFight_OnWin(Box<Permanent>, Box<Permanent>, Vec<Action>, Vec<Action>),
    HaveDeadPermanentDealDamage(Box<GameNumber>, Box<DamageRecipient>),
    HaveDiscardedCardDealDamage(CardInHand, Box<GameNumber>, Box<DamageRecipient>),
    HaveEachPlayerLoseLife(Box<Players>, Box<GameNumber>),
    HavePermanentDealDamage(Box<Permanent>, Box<GameNumber>, Box<DamageRecipient>),
    HavePlayerTakeAction(Box<Player>, Box<Action>),
    Incubate(Box<GameNumber>),
    IncubateNumberTimes(Box<GameNumber>, Box<GameNumber>),
    Investigate,
    InvestigateTimes(Box<GameNumber>),
    Learn,
    LookAtARandomCardInPlayersHand(Box<Player>),
    LookAtCardsOfTypeInPlayersHand(Box<Cards>, Box<Player>),
    LookAtEachFaceDownPermanent(Box<Permanents>),
    LookAtFaceDownExiledCards(Box<CardsInExile>),
    LookAtFaceDownPermanent(Box<Permanent>),
    LookAtPlayersHand(Box<Player>),
    LookAtPlayersHandAndChooseACardForPlayerToPlayControllingThemToDoSo(Box<Player>, Box<Cards>),
    LookAtPlayersHandAndChooseACardToDiscard(Box<Player>, Box<Cards>),
    LookAtPlayersHandAndChooseACardToPutOnBattlefield(
        Box<Player>,
        Box<Cards>,
        Vec<ReplacementActionWouldEnter>,
    ),
    LookAtPlayersHandAndChooseNumCardsToDiscard(Box<Player>, Box<GameNumber>, Box<Cards>),
    LookAtPlayersHandAndChooseNumberCardsToPutOnTopOfTheirLibraryInAnyOrder(
        Box<Player>,
        Box<GameNumber>,
        Box<Cards>,
    ),
    LookAtPlayersHandAndMayChooseACardToCastWithoutPaying(Box<Player>, Box<Cards>),
    LookAtTheTopCardOfEachPlayersLibrary(Box<Players>),
    LookAtTheTopCardOfPlayersLibrary(Box<Player>),
    LookAtTheTopNumberCardsOfLibrary(Box<GameNumber>, Vec<LookAtTopOfLibraryAction>),
    LookAtTheTopNumberCardsOfPlayersLibrary(
        Box<Player>,
        Box<GameNumber>,
        Vec<LookAtTopOfLibraryAction>,
    ),
    LookAtTopOfLibrary,
    LoseAllPoisonCounters,
    LoseLife(Box<GameNumber>),
    LoseLifeAndLifeForEach(Box<GameNumber>, Box<GameNumber>, Box<GameNumber>),
    LoseLifeForEach(Box<GameNumber>, Box<GameNumber>),
    LoseTheGame,
    LoseUnspentMana,
    Loyalty(i32),
    ManifestACardFromHand,
    ManifestEachExiledCard(Box<CardsInExile>),
    ManifestTheTopCardOfPlayersLibrary(Box<Player>),
    ManifestTheTopNumberCardsOfPlayersLibrary(Box<GameNumber>, Box<Player>),
    MayCastASpellFromGraveyardIntoExile(Box<Spells>, Box<Player>),
    MayPutTheTopCardOfPlayersLibraryOfTypeInGraveyardForCost(Box<Player>, Box<Cards>, Box<Cost>),
    MillACard,
    MillCardsUntilACardOfTypeIsMilledOrUntilNumberCardsHaveBeenPutIntoGraveyardThisWay(
        Box<Cards>,
        Box<GameNumber>,
    ),
    MillNumberCards(Box<GameNumber>),
    Monstrosity(Box<GameNumber>),
    MoveACounterFromPermanentOntoAnotherPermanent(Box<Permanent>, Box<Permanent>),
    MoveACounterOfEachTypeNotOnPermanentFromPermanent(Box<Permanent>, Box<Permanent>),
    MoveACounterOfTypeFromPermanentOntoAnotherPermanent(
        CounterType,
        Box<Permanent>,
        Box<Permanent>,
    ),
    MoveACounterOfTypeFromPermanentOntoNewPermanent(CounterType, Box<Permanent>, Box<Permanent>),
    MoveAllCountersFromPermanentOntoAnotherPermanent(Box<Permanent>, Box<Permanent>),
    MoveAllCountersOfTypeFromEachPermanentOntoPermanent(
        CounterType,
        Box<Permanents>,
        Box<Permanent>,
    ),
    MoveAllCountersOfTypeFromPermanentOntoPermanent(CounterType, Box<Permanent>, Box<Permanent>),
    MoveAnyNumberOfCountersFromPermanentOntoNewPermanent(Box<Permanent>, Box<Permanent>),
    MoveAnyNumberOfCountersFromPermanentsOntoNewPermanent(Box<Permanents>, Box<Permanent>),
    MoveAnyNumberOfCountersOfTypeFromPermanentOntoNewPermanent(
        CounterType,
        Box<Permanent>,
        Box<Permanent>,
    ),
    MoveAnyNumberOfCountersOfTypeFromPermanentOntoNewPermanents(
        CounterType,
        Box<Permanent>,
        Box<Permanents>,
    ),
    MoveAnyNumberOfCountersOfTypeFromPermanentsOntoNewPermanent(
        CounterType,
        Box<Permanents>,
        Box<Permanent>,
    ),
    MoveNumberCountersOfTypeFromPermanentOntoAnotherPermanent(
        Box<GameNumber>,
        CounterType,
        Box<Permanent>,
        Box<Permanent>,
    ),
    NoteAnUnnotedCreatureType(ChoosableCreatureType),
    NoteCountersOnPermanent(Box<Permanent>),
    NoteNumber(Box<GameNumber>),
    NoteTypeOfManaSpentToActivateThisAbility,
    NumberEachPermanentStartingFromZero(Box<Permanents>),
    OnlyAllowedAttackersUntilEndOfTurn(Box<Permanents>),
    OnlyAllowedAttackingPlayersUntilEndOfTurn(Box<Players>),
    OnlyAllowedBlockersUntilEndOfTurn(Box<Permanents>),
    OnlyAllowedCastingPlayersUntilEndOfTurn(Box<Players>),
    OpenAnAttraction,
    OpenNumberAttractions(Box<GameNumber>),
    PayAnyAmountOfEnergy,
    PayAnyAmountOfLife,
    PayAnyAmountOfMana,
    PermanentDealsDamageAndPermanentDealsDamage(
        Box<Permanent>,
        Box<GameNumber>,
        DamageRecipient,
        Box<Permanent>,
        Box<GameNumber>,
        DamageRecipient,
    ),
    PermanentDoesntUntapDuringControllersNextNumberUntaps(Box<Permanent>, Box<GameNumber>),
    PermanentDoesntUntapDuringControllersNextUntap(Box<Permanent>),
    PerpetuallyIncreaseIntensityOfCardsOwnedByPlayer(Box<Cards>, Box<Player>, Box<GameNumber>),
    PerpetuallyIncreaseIntensityOfPermanent(Box<Permanent>, Box<GameNumber>),
    PhaseInEachPermanentAndPhaseOutEachPermanent(Box<Permanents>, Box<Permanents>),
    PhaseOutAnyNumberOfPermanents(Box<Permanents>),
    PhaseOutEachPermanent(Box<Permanents>),
    PhaseOutEachPermanentUntil(Box<Permanents>, Expiration),
    PhaseOutPermanent(Box<Permanent>),
    PhaseOutPermanentUntil(Box<Permanent>, Expiration),
    PhaseOutPermanentUntilWithEffects(Box<Permanent>, Expiration, PhasedOutEffect),
    Planeswalk,
    PlayACardFromOutsideGame,
    PlayALandOrCastASpellFromAmongExiledCardsWithoutPaying(
        Box<Permanents>,
        Box<Spells>,
        CardsInExile,
    ),
    PlayAMagicSubgame,
    PlayAnExiledCardAndMaySpendManaAsThoughAnyColorToCast(Box<CardsInExile>),
    PlayAnyNumberOfExiledCards(Box<CardsInExile>),
    PlayAnyNumberOfLandsOrCastAnyNumberOfSpellsFromExileWithoutPaying(Box<Spells>, CardsInExile),
    PlayExiledCardWithoutPaying(Box<CardInExile>),
    PlayGraveyardCardWithoutPaying(CardInGraveyard),
    PlayTopCardOfLibraryWithoutPaying,
    PlayersDiscardCards(Box<CardsInHand>),
    PlayersExchangeLifeTotals(Box<Player>, Box<Player>),
    Populate,
    PopulateNumberTimes(Box<GameNumber>),
    PopulateWithFlags(Vec<ReplacementActionWouldEnter>),
    Proliferate,
    ProliferateNumberTimes(Box<GameNumber>),
    PutACardFromAGraveyardOnBattlefield(
        CardsInGraveyard,
        Box<Players>,
        Vec<ReplacementActionWouldEnter>,
    ),
    PutACardFromGraveyardIntoHand(Box<CardsInGraveyard>),
    PutACardFromGraveyardIntoHandAtRandom(Box<CardsInGraveyard>),
    PutACardFromGraveyardIntoHandExceptForGraveyardCard(CardsInGraveyard, CardInGraveyard),
    PutACardFromHandIntoGraveyard(Box<Cards>),
    PutACardFromHandOnBattlefield(Box<CardsInHand>, Vec<ReplacementActionWouldEnter>),
    PutACardFromHandOnBottomOfLibrary,
    PutACardFromHandOnTopOfLibrary,
    PutACardFromHandOrGraveyardOnBattlefield(
        Box<Cards>,
        Box<Player>,
        Vec<ReplacementActionWouldEnter>,
    ),
    PutACardFromOutsideGameInHand(Box<Cards>),
    PutACardFromOutsideGameOnTopOfLibrary(Box<Cards>),
    PutACardFromPlayersGraveyardOnBattlefield(
        CardsInGraveyard,
        Box<Player>,
        Vec<ReplacementActionWouldEnter>,
    ),
    PutACardFromPlayersGraveyardOnTopOfLibrary(Box<Cards>, Box<Player>),
    PutACommanderFromCommandZoneIntoHand(Commanders),
    PutACommanderFromCommandZoneOntoBattlefield(Commanders, Vec<ReplacementActionWouldEnter>),
    PutACounterOfChoiceOnPermanent(Vec<CounterType>, Box<Permanent>),
    PutACounterOfEachTypeOnPermanent(CounterTypes, Box<Permanent>),
    PutACounterOfTypeAndACounterOfTypeOnPermanent(CounterType, CounterType, Box<Permanent>),
    PutACounterOfTypeOnAPermanent(CounterType, Box<Permanents>),
    PutACounterOfTypeOnEachPermanent(CounterType, Box<Permanents>),
    PutACounterOfTypeOnOrRemoveACounterOfTypeFromPermanent(CounterType, Box<Permanent>),
    PutACounterOfTypeOnPermanent(CounterType, Box<Permanent>),
    PutACounterOfTypeOnPlayer(CounterType, Box<Player>),
    PutACounterOnEachExiledCard(CounterType, Box<CardsInExile>),
    PutACounterOnExiledCard(CounterType, Box<CardInExile>),
    PutACounterOnOrRemoveACounterOfTypeFromExiledCard(CounterType, Box<CardInExile>),
    PutACounterOnPlane(CounterType, Plane),
    PutACounterOnScheme(CounterType, SingleScheme),
    PutACounterOnVanguard(CounterType, SingleVanguard),
    PutADuplicateCounterOnPermanent(Box<Permanent>),
    PutANameStickerOnAPermanent(Box<Permanents>),
    PutANameStickerOnPermanent(Box<Permanent>),
    PutAPowerAndToughnessStickerOnAPermanent(Box<Permanents>),
    PutARandomCardFromLibraryIntoGraveyard(Box<Cards>),
    PutARandomCardFromLibraryOntoBattlefield(Box<Cards>, Vec<ReplacementActionWouldEnter>),
    PutARandomCardFromPlayersLibraryOntoBattlefield(
        Box<Cards>,
        Box<Player>,
        Vec<ReplacementActionWouldEnter>,
    ),
    PutARandomCardOfTypeFromAmongTheTopNumberCardsOfLibraryIntoHand(Box<Cards>, Box<GameNumber>),
    PutAStickerOnACardInPlayersGraveyard(CardsInGraveyard, Box<Player>),
    PutAStickerOnAPermanent(Box<Permanents>),
    PutAbilityCountersOnAPermanentFromAbilitiesOnCardsInPlayersGraveyard(
        Box<Permanents>,
        Vec<CheckHasable>,
        Box<Cards>,
        Box<Player>,
    ),
    PutAllCardsFromHandOnBottomOfLibraryAnyOrder,
    PutAllCardsFromHandOnTopOfLibraryRandomOrder,
    PutAnAbilityStickerWithTicketCostOnPermanentWithoutPaying(Box<Comparison>, Box<Permanent>),
    PutAnArtStickerOnAPermanent(Box<Permanents>),
    PutAnExiledCardIntoOwnersGraveyard(Box<CardsInExile>),
    PutAnExiledCardIntoOwnersHand(Box<CardsInExile>),
    PutAnotherCounterOnExiledCard(Box<CardInExile>),
    PutAnotherCounterOnPermanent(Box<Permanent>),
    PutAnyNumberOfCardsFromAmongExileOntoBattlefield(
        Box<Cards>,
        CardsInExile,
        Vec<ReplacementActionWouldEnter>,
    ),
    PutAnyNumberOfCardsFromExileOntoBattlefield(CardsInExile, Vec<ReplacementActionWouldEnter>),
    PutAnyNumberOfCardsFromHandOnBottomOfLibraryInAnyOrder,
    PutAnyNumberOfCardsFromHandOntoBattlefield(Box<Cards>, Vec<ReplacementActionWouldEnter>),
    PutAnyNumberOfCardsFromHandOntoBattlefieldAsFaceDownArtifactCreatures(PT),
    PutAnyNumberOfCardsFromHandOrFromPlayersGraveyardOnBattlefield(
        Box<Cards>,
        Box<Player>,
        Vec<ReplacementActionWouldEnter>,
    ),
    PutCardFromHandIntoGraveyard(CardInHand),
    PutCardFromHandIntoPlayersHand(CardInHand, Box<Player>),
    PutCardFromHandOnBattlefield(CardInHand, Vec<ReplacementActionWouldEnter>),
    PutCardFromHandOnBottomOfLibrary(CardInHand),
    PutCardFromHandOnTopOfLibrary(CardInHand),
    PutCardInHandIntoLibraryNthFromTop(CardInHand),
    PutCardsFromHandOnBattlefield(Box<CardsInHand>, Vec<ReplacementActionWouldEnter>),
    PutCopyOfEachCounterOnPermanentOnPermanent(Box<Permanent>, Box<Permanent>),
    PutCountersOfDeadPermanentOnPermanent(Box<Permanent>),
    PutDeadPermanentOnBottomOfLibrary,
    PutDeadPermanentOnTopOfLibrary,
    PutDeadPermanentOnTopOfLibraryOrOnBottomOfLibrary,
    PutDistributedCounters(CounterType),
    PutDuplicateCountersOnPermanent(Box<Permanent>),
    PutEachCardFromAnteInGraveyard(AnteCards),
    PutEachCardFromEachPlayersGraveyardOntoTheBattlefield(
        Box<Cards>,
        Box<Players>,
        Vec<ReplacementActionWouldEnter>,
    ),
    PutEachCardInGraveyardOntoBottomOfLibraryInRandomOrder(Box<Cards>),
    PutEachCommanderFromCommandZoneIntoHand(Commanders),
    PutEachCommanderFromGraveyardIntoHand,
    PutEachExiledCardOnTheBottomOfTheirOwnersLibraryInARandomOrder(Box<CardsInExile>),
    PutEachExiledCardOnTheBottomOfTheirOwnersLibraryInAnyOrder(Box<CardsInExile>),
    PutEachExiledCardOntoTheBattlefield(CardsInExile, Vec<ReplacementActionWouldEnter>),
    PutEachGraveyardCardIntoHand(Box<CardsInGraveyard>),
    PutEachGraveyardCardOntoBattlefield(Box<CardsInGraveyard>, Vec<ReplacementActionWouldEnter>),
    PutEachPermanentIntoItsOwnersHand(Box<Permanents>),
    PutEachPermanentOnTopOfOwnersLibrariesThenShuffleThoseLibraries(Box<Permanents>),
    PutEachPermanentToTopOrBottomOfLibrary(Box<Permanents>),
    PutExiledCardInOwnersLibraryNthFromTheTop(CardInExile, Box<GameNumber>),
    PutExiledCardIntoOwnersGraveyard(Box<CardInExile>),
    PutExiledCardIntoOwnersHand(Box<CardInExile>),
    PutExiledCardOnStackAsCopyOfSpell(CardInExile, Box<Spell>, SpellCopyEffects),
    PutExiledCardOntoBattlefield(CardInExile, Vec<ReplacementActionWouldEnter>),
    PutExiledCardsIntoOwnersGraveyards(Box<CardsInExile>),
    PutExiledCardsIntoOwnersHand(Box<CardsInExile>),
    PutExiledCardsOnTopOfLibraryInAnyOrder(Box<CardsInExile>),
    PutExiledPileIntoOwnersHand(Box<CardsInExile>),
    PutFormerCountersOnPermanent(Box<Permanent>),
    PutGraveyardCardInOwnersLibraryNthFromTheTop(CardInGraveyard, Box<GameNumber>),
    PutGraveyardCardIntoHand(CardInGraveyard),
    PutGraveyardCardIntoHandOrOntoBattlefield(CardInGraveyard, Vec<ReplacementActionWouldEnter>),
    PutGraveyardCardOntoBattlefield(CardInGraveyard, Vec<ReplacementActionWouldEnter>),
    // PutGraveyardCardToTopOfLibrary(CardInGraveyard),
    PutNumCardsFromHandOnBottomOfLibraryAnyOrder(Box<GameNumber>),
    PutNumCardsFromHandOnTopOfLibraryAnyOrder(Box<GameNumber>),
    PutNumberCountersOfChoiceOnAPermanent(Box<GameNumber>, Vec<CounterType>, Box<Permanents>),
    PutNumberCountersOfTypeOnEachPermanent(Box<GameNumber>, CounterType, Box<Permanents>),
    PutNumberCountersOfTypeOnExiledCard(Box<GameNumber>, CounterType, CardInExile),
    PutNumberCountersOfTypeOnPermanent(Box<GameNumber>, CounterType, Box<Permanent>),
    PutNumberCountersOfTypeOnPermanentForEach(
        Box<GameNumber>,
        CounterType,
        Box<Permanent>,
        Box<GameNumber>,
    ),
    PutNumberCountersOfTypeOnPermanentOrExiled(Box<GameNumber>, CounterType, PermanentOrExiledCard),
    PutNumberCountersOnEachExiledCard(Box<GameNumber>, CounterType, CardsInExile),
    PutNumberCountersOnExiledCard(Box<GameNumber>, CounterType, CardInExile),
    PutNumberCountersOnGraveyardCard(Box<GameNumber>, CounterType, CardInGraveyard),
    PutNumberPermanentsIntoOwnersHand(Box<GameNumber>, Box<Permanents>),
    PutPermanentInOwnersLibraryBeneathNumberCards(Box<Permanent>, Box<GameNumber>),
    PutPermanentInOwnersLibraryNthFromTheTop(Box<Permanent>, Box<GameNumber>),
    PutPermanentIntoItsOwnersHand(Box<Permanent>),
    PutPermanentOnBottomOfOwnersLibrary(Box<Permanent>),
    PutPermanentOnTopOfOwnersLibrary(Box<Permanent>),
    PutSpellInOwnersLibraryNthFromTheTop(Box<Spell>, Box<GameNumber>),
    PutTheBottomCardOfPlayersLibraryIntoGraveyard(Box<Player>),
    PutTheTopCardOfPlayersLibraryInGraveyard(Box<Player>),
    PutTheTopCardOfPlayersLibraryOnTheBottomOfTheirLibrary(Box<Player>),
    PutTheTopNumberCardsOfLibraryInHand(Box<GameNumber>),
    PutTopCardOfLibraryOfTypeOnBattlefield(Box<Cards>, Vec<ReplacementActionWouldEnter>),
    PutTopOfLibraryInGraveyard,
    PutTopOfLibraryInHand,
    PutTopOfLibraryOnBattlefield(Vec<ReplacementActionWouldEnter>),
    PutTopOfLibraryOnBottomOfLibrary,
    PutTopOfOtherLibraryInGraveyard(Box<Player>),
    PutTopOfPlanarDeckOnBottomOfPlanarDeck,
    PutUptoNumberCardsFromGraveyardToHand(Box<GameNumber>, Box<Cards>),
    PutUptoNumberCardsFromHandOntoBattlefield(
        Box<GameNumber>,
        Box<Cards>,
        Vec<ReplacementActionWouldEnter>,
    ),
    PutUptoNumberCountersOfTypeOnPermanentWithMaxTotal(
        Box<GameNumber>,
        CounterType,
        Box<Permanent>,
        Box<GameNumber>,
    ),
    PutUptoNumberGraveyardCardsOntoTheBattlefield(
        Box<GameNumber>,
        Box<CardsInGraveyard>,
        Vec<ReplacementActionWouldEnter>,
    ),
    PutUptoNumberNameStickersOnPermanent(Box<GameNumber>, Box<Permanent>),
    RedistributeLifeTotalsOfPlayers(Box<Players>),
    RegenerateEachPermanent(Box<Permanents>),
    RegeneratePermanent(Box<Permanent>),
    RememberLifeTotal,
    RememberPlayer(Box<Player>),
    RemoveACounterFromExiledCard(Box<CardInExile>),
    RemoveACounterFromPermanent(Box<Permanent>),
    RemoveACounterOfTypeFromEachOfAnyNumberOfPermanents(CounterType, Box<Permanents>),
    RemoveACounterOfTypeFromEachPermanent(CounterType, Box<Permanents>),
    RemoveACounterOfTypeFromExiledCard(CounterType, CardInExile),
    RemoveACounterOfTypeFromPermanent(CounterType, Box<Permanent>),
    RemoveAllCountersFromEachPermanent(Box<Permanents>),
    RemoveAllCountersFromPermanent(Box<Permanent>),
    RemoveAllCountersFromPlayer(Box<Player>),
    RemoveAllCountersOfTypeFromAPermanent(CounterType, Box<Permanents>),
    RemoveAllCountersOfTypeFromEachPermanent(CounterType, Box<Permanents>),
    RemoveAllCountersOfTypeFromPermanent(CounterType, Box<Permanent>),
    RemoveCountersDistributedThisWay,
    RemoveCreatureFromCombat(Box<Permanent>),
    RemoveCreatureFromCombatAndUnblockBlockers(Box<Permanent>),
    RemoveNumCountersFromPermanent(Box<GameNumber>, Box<Permanent>),
    RemoveNumCountersOfTypeFromEachExiled(Box<GameNumber>, CounterType, CardsInExile),
    RemoveNumberCountersOfTypeFromEachPermanent(Box<GameNumber>, CounterType, Box<Permanents>),
    RemoveNumberCountersOfTypeFromPermanent(Box<GameNumber>, CounterType, Box<Permanent>),
    RemoveNumberCountersOfTypeFromPermanentOrExiled(
        Box<GameNumber>,
        CounterType,
        PermanentOrExiledCard,
    ),
    RemoveNumberCountersOfTypeOnExiledCard(Box<GameNumber>, CounterType, CardInExile),
    RemoveUptoNumberCountersFromPermanent(Box<GameNumber>, Box<Permanent>),
    RemoveUptoNumberCountersFromPlayer(Box<GameNumber>, Box<Player>),
    RemoveUptoNumberCountersOfTypeFromPermanent(Box<GameNumber>, CounterType, Box<Permanent>),
    ReorderPlayersGraveyard(Box<Player>),
    RepeatThisProcess,
    RerollAnyNumberOfTheStoredD6Results,
    ReselectTargetOfSpellOrAbilityAtRandom(SpellOrAbility),
    ReselectWhichPlayerCreatureIsAttacking(Box<Permanent>),
    RestartTheGameLeavingEachExiledCardInExileThenPutThoseCardsOntoTheBattlefield(
        CardsInExile,
        Vec<ReplacementActionWouldEnter>,
    ),
    ReturnACardFromAnyPlayersGraveyardToBattlefield(
        CardsInGraveyard,
        Box<Players>,
        Vec<ReplacementActionWouldEnter>,
    ),
    ReturnACardFromGraveyardToBattlefield(Box<Cards>, Vec<ReplacementActionWouldEnter>),
    ReturnACardFromGraveyardToBattlefieldAtRandom(Box<Cards>, Vec<ReplacementActionWouldEnter>),
    ReturnACardFromGraveyardToHandAtRandom(Box<Cards>),
    ReturnACardFromGraveyardToTopOfLibrary(Box<Cards>),
    ReturnACardFromPlayersGraveyardToBattlefield(
        Box<Cards>,
        Box<Player>,
        Vec<ReplacementActionWouldEnter>,
    ),
    ReturnACardMilledThisWayToBattlefield(Box<Cards>, Vec<ReplacementActionWouldEnter>),
    ReturnAGraveyardCardToHand(Box<Cards>),
    ReturnAPermanentToTopOfLibrary(Box<Permanents>),
    ReturnAnExiledCardToBattlefield(CardsInExile, Vec<ReplacementActionWouldEnter>),
    ReturnAnExiledCardToOwnersHand(Box<CardsInExile>),
    ReturnAnyNumberCardsMilledThisWayToHand(Box<Cards>),
    ReturnAnyNumberOfCardsFromGraveyardToBattlefield(
        CardsInGraveyard,
        Box<Player>,
        Vec<ReplacementActionWouldEnter>,
    ),
    ReturnAnyNumberOfCardsFromGraveyardToHand(Box<Cards>),
    ReturnAnyNumberOfGroupCardsFromGraveyardToHand(Box<Cards>, GroupFilter),
    ReturnAnyNumberOfGroupCardsFromPlayersGraveyardToBattlefield(
        CardsInGraveyard,
        GroupFilter,
        Box<Player>,
        Vec<ReplacementActionWouldEnter>,
    ),
    ReturnAnyNumberOfGroupCardsMilledThisWayToBattlefield(
        Box<Cards>,
        GroupFilter,
        Vec<ReplacementActionWouldEnter>,
    ),
    ReturnAnyNumberOfPermanentsToTheirOwnersHands(Box<Permanents>),
    ReturnDeadGraveyardCardToBattlefield(Vec<ReplacementActionWouldEnter>),
    ReturnDeadGraveyardCardToBottomOfLibrary,
    ReturnDeadGraveyardCardToHand,
    ReturnDeadGraveyardCardToTopOfLibrary,
    ReturnDeadGuestGraveyardCardToBattlefield(Vec<ReplacementActionWouldEnter>),
    ReturnDeadGuestGraveyardCardToHand,
    ReturnEachCardFromEachPlayersGraveyardToBattlefieldThatWasPutThereFromAnywhereThisTurn(
        Box<Cards>,
        Box<Players>,
        Vec<ReplacementActionWouldEnter>,
    ),
    ReturnEachCardFromEachPlayersGraveyardToBattlefieldThatWasPutThereFromTheBattlefieldThisTurn(
        Box<Cards>,
        Box<Players>,
        Vec<ReplacementActionWouldEnter>,
    ),
    ReturnEachCardFromEachPlayersGraveyardToOwnersHand(Box<Cards>, Box<Players>),
    ReturnEachCardFromGraveyardToHand(Box<Cards>),
    ReturnEachCardFromGraveyardToHandThatWasCycledOrDiscardedThisTurn(Box<Cards>),
    ReturnEachCardFromGraveyardToHandThatWasPutThereFromAnywhereThisTurn(Box<Cards>),
    ReturnEachCardFromGraveyardToHandThatWasPutThereFromBattlefieldThisTurn(Box<Cards>),
    ReturnEachCardFromPlayersGraveyardToBattlefield(
        CardsInGraveyard,
        Box<Player>,
        Vec<ReplacementActionWouldEnter>,
    ),
    ReturnEachCardFromPlayersGraveyardToBattlefieldThatWasDestroyThisWay(
        Box<Cards>,
        Box<Player>,
        Vec<ReplacementActionWouldEnter>,
    ),
    ReturnEachCardFromPlayersGraveyardToBattlefieldThatWasPutThereFromBattlefieldThisTurn(
        Box<Cards>,
        Box<Player>,
        Vec<ReplacementActionWouldEnter>,
    ),
    ReturnEachCardFromPlayersGraveyardToHand(Box<Cards>, Box<Player>),
    ReturnEachCardFromPlayersGraveyardToHandThatWasPutThereFromBattlefieldThisTurn(
        Box<Cards>,
        Box<Player>,
    ),
    ReturnEachExiledCardToBottomOfOwnersLibraryRandomOrder(Box<CardsInExile>),
    ReturnEachExiledCardToGraveyard(Box<CardsInExile>),
    ReturnEachExiledCardToOwnersHand(Box<CardsInExile>),
    ReturnEachGraveyardCardToBattlefield(Box<CardsInGraveyard>, Vec<ReplacementActionWouldEnter>),
    ReturnEachGraveyardCardToBottomOfLibraryInAnyOrder(Box<CardsInGraveyard>),
    ReturnEachPermanentToCommandZone(Box<Permanents>),
    ReturnEnchantingGraveyardCardToBattlefield(Vec<ReplacementActionWouldEnter>),
    ReturnEnchantingGraveyardCardToHand,
    //ReturnGraveyardCardToBottomOfLibrary(CardInGraveyard),
    ReturnGraveyardCardToHand(CardInGraveyard),
    ReturnGraveyardCardToTopOrBottomOfLibrary(CardInGraveyard),
    ReturnGraveyardCardsToHand(Box<CardsInGraveyard>),
    ReturnGraveyardCardsToTopOfLibraryInAnyOrder(Box<CardsInGraveyard>),
    ReturnMultipleCardsFromPlayersGraveyardToBattlefield(
        Vec<Cards>,
        Box<Player>,
        Vec<ReplacementActionWouldEnter>,
    ),
    ReturnNewGraveyardCardToBattlefield(Vec<ReplacementActionWouldEnter>),
    ReturnNumberCardsFromGraveyardToBattlefield(
        Box<GameNumber>,
        Box<Cards>,
        Vec<ReplacementActionWouldEnter>,
    ),
    ReturnNumberCardsFromGraveyardToBattlefieldAtRandom(
        Box<GameNumber>,
        Box<Cards>,
        Vec<ReplacementActionWouldEnter>,
    ),
    ReturnNumberCardsFromPlayersGraveyardToHand(Box<GameNumber>, Box<Cards>, Box<Player>),
    ReturnNumberCardsFromPlayersGraveyardToHandAtRandom(Box<GameNumber>, Box<Cards>, Box<Player>),
    ReturnNumberGraveyardCardsToBattlefieldAtRandom(
        Box<GameNumber>,
        Box<CardsInGraveyard>,
        Vec<ReplacementActionWouldEnter>,
    ),
    ReturnPermanentOrExiledCardToOwnersHand(PermanentOrExiledCard),
    ReturnPermanentToBottomOfLibrary(Box<Permanent>),
    ReturnPermanentToCommandZone(Box<Permanent>),
    ReturnPermanentToLibraryUnderNumberCards(Box<Permanent>, Box<GameNumber>),
    ReturnPermanentToTopOrBottomOfLibrary(Box<Permanent>),
    ReturnSpellOrPermanentToOwnersHand(SpellOrPermanent),
    ReturnSpellToBottomOfLibrary(Box<Spell>),
    ReturnSpellToOwnersHand(Box<Spell>),
    ReturnSpellToTopOrBottomOfLibrary(Box<Spell>),
    ReturnTheExiledDeadPermanentToGraveyard,
    ReturnUptoNumberCardsFromAmongPlayersGraveyardsToBattlefield(
        Box<GameNumber>,
        CardsInGraveyard,
        Box<Players>,
        Vec<ReplacementActionWouldEnter>,
    ),
    ReturnUptoNumberCardsFromExileToOwnersHand(Box<GameNumber>, CardsInExile),
    ReturnUptoNumberCardsFromPlayersGraveyardToBattlefield(
        Box<GameNumber>,
        Box<Cards>,
        Box<Player>,
        Vec<ReplacementActionWouldEnter>,
    ),
    ReturnUptoNumberPermanentsToOwnersHand(Box<GameNumber>, Box<Permanents>),
    ReturnUptoOneCardOfEachPermanentTypeInPlayersGraveyardToBattlefield(
        Box<Player>,
        Vec<ReplacementActionWouldEnter>,
    ),
    RevealACardFromHand,
    RevealACardFromHandAtRandom,
    RevealACardFromHandAtRandomAndDiscardIfItIsACardOfType(Box<Cards>),
    RevealACardOfTypeFromHand(Box<Cards>),
    RevealACardOfTypeFromHandAtRandom(Box<Cards>),
    RevealANumberOfCardsFromHandAndPlayerChoosesACardToDiscard(Box<GameNumber>, Box<Player>),
    RevealANumberOfCardsFromHandAndPlayerChoosesACardToExile(
        Box<GameNumber>,
        Box<Player>,
        Box<Cards>,
    ),
    RevealANumberOfCardsFromHandAndPlayerMayCastASpellFromAmongThemWithoutPaying(
        Box<GameNumber>,
        Box<Player>,
        Box<Spells>,
    ),
    RevealAllCardsOfTypeFromHand(Box<Cards>),
    RevealAllCardsOfTypeFromHandAndPlayerChoosesACardToExile(Box<Cards>, Box<Player>),
    RevealAllCardsOfTypeFromHandAndPlayerChoosesCard(Box<Cards>, Box<Player>),
    RevealAnyNumberOfCardsOfTypeFromHand(Box<Cards>),
    RevealCardFromHand(CardInHand),
    RevealCardFromOutsideGameAndPutInHand(Box<Cards>),
    RevealCardFromOutsideGameAndPutInHandOrPutAnExiledCardInOwnersHand(Box<Cards>, CardsInExile),
    RevealCardsFromTheTopOfLibraryUntilACardOfTypeIsRevealed(
        Box<Cards>,
        Vec<RevealTheTopNumberCardsOfLibraryAction>,
    ),
    RevealCardsFromTheTopOfLibraryUntilACardOfTypeIsRevealedOrUntilNumberCardsAreRevealed(
        Box<Cards>,
        Box<GameNumber>,
        Vec<RevealTheTopNumberCardsOfLibraryAction>,
    ),
    RevealCardsFromTheTopOfLibraryUntilANumberOfCardsOfTypeAreRevealed(
        Box<GameNumber>,
        Box<Cards>,
        Vec<RevealTheTopNumberCardsOfLibraryAction>,
    ),
    RevealCardsFromTheTopOfPlanarDeckUntilRevealAPlaneCardThenChaosEnsuresOnThatPlaneThenPutAllCardsOnBottomInAnyOrder,
    RevealCardsFromTheTopOfPlanarDeckUntilRevealAPlaneCardThenPlaneswalkToItWhileNotPlaneswalkingAwayAndPutTheRestOnBottomInAnyOrder,
    RevealCardsFromTheTopOfPlanarDeckUntilRevealNumberPlaneCardsAndPutAPlaneCardFromAmongOnTopAndTheRestOnBottomInAnyOrder(
        Box<GameNumber>,
    ),
    RevealCardsFromTheTopOfPlanarDeckUntilRevealNumberPlaneCardsSimultaneouslyPlaneswalkToThemThenPutOnBottomInAnyOrder(
        Box<GameNumber>,
    ),
    RevealHand,
    RevealHandAndAlternateExilingCardsWithPlayer(Box<Player>),
    RevealHandAndDiscardACardOfTypeAtRandom(Box<Cards>),
    RevealHandAndDiscardEachCard(Box<Cards>),
    RevealHandAndExileEachCardOfType(Box<Cards>),
    RevealHandAndPlayerChoosesACard(Box<Player>, Box<Cards>),
    RevealHandAndPlayerChoosesACardToDiscard(Box<Player>, Box<Cards>),
    RevealHandAndPlayerChoosesACardToExile(Box<Player>, Box<Cards>),
    RevealHandAndPlayerChoosesACardToExileUntil(Box<Player>, Box<Cards>, Expiration),
    RevealHandAndPlayerChoosesACardToPutOnBattlefield(
        Box<Player>,
        Box<Cards>,
        Vec<ReplacementActionWouldEnter>,
    ),
    RevealHandAndPlayerChoosesMultipleCardsToDiscard(Box<Player>, Vec<Cards>),
    RevealHandAndPlayerChoosesNumberCardsToDiscard(Box<Player>, Box<GameNumber>),
    RevealHandAndPlayerChoosesNumberCardsToExile(Box<Player>, Box<GameNumber>, Box<Cards>),
    RevealHandAndPlayerMayCastASpellFromAmongThemWithoutPaying(Box<Player>, Box<Spells>),
    RevealHandAndPlayerMayChooseACardToDiscard(Box<Player>, Box<Cards>),
    RevealLibrary(Vec<RevealTheTopNumberCardsOfLibraryAction>),
    RevealNumberCardsFromHand(Box<GameNumber>),
    RevealNumberCardsFromHandAtRandom(Box<GameNumber>),
    RevealNumberCardsFromHandAtRandomAndDiscardEachCardOfType(Box<GameNumber>, Box<Cards>),
    RevealSecretlyChosenNumbers,
    RevealSecretlyChosenPermanents,
    RevealTheCardDrawnThisWay,
    RevealTheCardPutInHandThisWay,
    RevealTheCardsDrawnThisWay,
    RevealTheTopCardOfPlayersLibrary(Box<Player>),
    RevealTheTopNumberCardsOfLibrary(Box<GameNumber>, Vec<RevealTheTopNumberCardsOfLibraryAction>),
    RevealTheTopNumberCardsOfPlanarDeckAndTriggerEachCHAOSAbilityThenPutOnBottomOfPlanarDeckInAnyOrder(
        Box<GameNumber>,
    ),
    RevealTopCardOfLibrary,
    RevealTopCardOfLibraryAndPutIntoHand(Box<Cards>),
    RevealTopCardOfPlanarDeck,
    RevealVotesForPermanent,
    RevealVotesForPlayer,
    RevealVotesForWord,
    RollAD10,
    RollAD12,
    RollAD20,
    RollAD4,
    RollAD6,
    RollAD8,
    RollNumberD20AndIgnoreAllButHighest(Box<GameNumber>),
    RollNumberD6(Box<GameNumber>),
    RollNumberD6AndStoreTheResult(Box<GameNumber>),
    RollThePlanarDie,
    RollToVisitAttractions,
    RollTwoD10AndChooseAnOrder,
    RollTwoD12AndChooseAnOrder,
    RollTwoD4AndChooseAnOrder,
    RollTwoD6AndChooseAnOrder,
    RollTwoD8AndChooseAnOrder,
    SacrificeAPermanent(Box<Permanents>),
    SacrificeAPermanentOfAPlayersChoice(Box<Permanents>, Box<Players>),
    SacrificeAllPermanentsExceptForAPermanentOfEachLandType(Box<Permanents>),
    SacrificeAllPermanentsExceptForNum(Box<Permanents>, Box<GameNumber>),
    SacrificeAnyNumberOfPermanents(Box<Permanents>),
    SacrificeEachPermanent(Box<Permanents>),
    SacrificeHalfOfThePermanentsRoundedUp(Box<Permanents>),
    SacrificeNumberPermanents(Box<GameNumber>, Box<Permanents>),
    SacrificePermanent(Box<Permanent>),
    SacrificePermanents(Vec<Permanents>),
    SacrificeAllPermanentsExceptForASpecificPermanentOfEachTypeOfTheirChoice(
        Box<Permanents>,
        Vec<Permanents>,
    ),
    Scry(Box<GameNumber>),
    SearchEachPlayersLibrary(Box<Players>, Vec<SearchLibraryAction>),
    SearchHandAndLibrary(Vec<SearchLibraryAction>),
    SearchHandAndOrLibrary(Vec<SearchLibraryAction>),
    SearchLibrary(Vec<SearchLibraryAction>),
    SearchLibraryAndGraveyard(Vec<SearchLibraryAction>),
    SearchLibraryAndOrGraveyard(Vec<SearchLibraryAction>),
    SearchLibraryAndOrGraveyardAndOrHand(Vec<SearchLibraryAction>),
    SearchLibraryAndOrGraveyardAndOrOutsideTheGame(Vec<SearchLibraryAction>),
    SearchPlayersLibrary(Box<Player>, Vec<SearchLibraryAction>),
    SearchPlayersLibraryAndGraveyardAndHand(Box<Player>, Vec<SearchLibraryAction>),
    SearchTheTopNumberCardsOfLibrary(Box<GameNumber>, Vec<SearchLibraryAction>),
    SecretlyChooseANumber,
    SecretlyChooseANumberGreaterThanNumber(i32),
    SecretlyChooseAPermanent(Box<Permanents>),
    SecretlyChooseAPlayer(Box<Players>),
    SecretlyVoteForAPermanent(Box<Permanents>),
    SecretlyVoteForAPlayer(Box<Players>),
    SecretlyVoteForAWord(Vec<VoteOption>),
    SeekACard(Box<Cards>),
    SeekNumberCardsFromTheTopNumberCardsOfLibrary(Box<GameNumber>, Box<Cards>, Box<GameNumber>),
    SeekACardFromTheTopNumberCardsOfLibrary(Box<Cards>, Box<GameNumber>),
    SeekNumberCards(Box<GameNumber>, Box<Cards>),
    SeparateCardsInPlayersGraveyardIntoTwoPiles(Box<Cards>, Box<Player>),
    SeparateExiledCardsIntoTwoPiles(Box<CardsInExile>),
    SeparatePermanentsIntoNamedPiles(Box<Permanents>, Vec<VoteOption>),
    SeparatePermanentsIntoNumberPiles(Box<Permanents>, Box<GameNumber>),
    SeparatePermanentsIntoTwoPilesAndAPlayerChoosesAPile(Box<Permanents>, Box<Players>),
    SetAttackAssignmentOfCreature(Box<Permanent>, AttackAssignment),
    SetCreatureAsBlocked(Box<Permanent>),
    SetEachCreatureAsBlocked(Box<Permanents>),
    SetLifeTotal(Box<GameNumber>),
    SetSchemeInMotion(SingleScheme),
    SetStake(Box<GameNumber>),
    Shuffle,
    ShuffleAllButNumberCardsInHandIntoLibrary(Box<GameNumber>),
    ShuffleAnyNumberOfCardsFromHandIntoLibrary,
    ShuffleCardFromHandIntoLibrary(CardInHand),
    ShuffleCardsFromHandIntoLibrary(Box<CardsInHand>),
    ShuffleEachCardInGraveyardIntoLibrary(Box<Cards>),
    ShuffleEachCardInPlayersGraveyardIntoLibrary(Box<Cards>, Box<Player>),
    ShuffleEachExiledCardIntoLibrary(Box<CardsInExile>),
    ShuffleEachGraveyardCardIntoLibrary(Box<CardsInGraveyard>),
    ShuffleEachPermanentIntoLibrary(Box<Permanents>),
    ShuffleExiledCardIntoLibrary(Box<CardInExile>),
    ShuffleExiledCardsAndPutOnTopOfLibrary(Box<CardsInExile>),
    ShuffleGraveyard,
    ShuffleGraveyardCardIntoLibrary(CardInGraveyard),
    ShuffleGraveyardIntoLibrary,
    ShuffleHandAndGraveyardIntoLibrary,
    ShuffleHandAndPermanentsIntoLibrary,
    ShuffleHandGraveyardAndPermanentsIntoLibrary,
    ShuffleHandIntoLibrary,
    ShufflePermanentAndGraveyardIntoLibrary(Box<Permanent>),
    ShufflePermanentIntoLibrary(Box<Permanent>),
    ShuffleSpellAndGraveyardCardsIntoLibraries(Box<Spell>, Box<CardsInGraveyard>),
    ShuffleSpellIntoLibrary(Box<Spell>),
    ShuffleUptoNumberCardsFromOutsideTheGameIntoLibrary(Box<GameNumber>),
    ShuffleUptoNumberCardsFromPlayersGraveyardIntoLibrary(Box<GameNumber>, Box<Cards>, Box<Player>),
    SimultaneouslySacrificePermanentAndPutGraveyardCardOntoBattlefield(
        Box<Permanent>,
        CardInGraveyard,
        Vec<ReplacementActionWouldEnter>,
    ),
    SimultaneouslyTapEachPermanentAndUntapEachPermanent(Box<Permanents>, Box<Permanents>),
    SkipAllCombatPhasesTheirNextTurn,
    SkipNextCombatPhase,
    SkipNextCombatPhaseThisTurn,
    SkipNextDrawStep,
    SkipNextTurn,
    SkipNextUntapStep,
    StartBiddingWarAmongPlayersAtNumber(Box<Players>, Box<GameNumber>),
    StartBiddingWarAmongPlayersAtAnyNumber(Box<Players>),
    StartBiddingWarWithPlayer(Box<Player>, Box<GameNumber>),
    Support(Box<Permanents>),
    Surveil(Box<GameNumber>),
    TakeANumberOfExtraTurns(Box<GameNumber>),
    TakeAnExtraTurn,
    TakeAnExtraTurnAfterNextTurn,
    TakeAnExtraTurnAndSkipUntapStepOfThatTurn,
    TakeTheInitiative,
    TapAllButNumberPermanents(Box<GameNumber>, Box<Permanents>),
    TapAnyNumberOfPermanents(Box<Permanents>),
    ExploreWithEachPermanent(Box<Permanents>),
    TapEachPermanent(Box<Permanents>),
    TapNumberPermanents(Box<GameNumber>, Box<Permanents>),
    TapOrUntapPermanent(Box<Permanent>),
    SuspectPermanent(Box<Permanent>),
    SuspectEachPermanent(Box<Permanents>),
    UnsuspectPermanent(Box<Permanent>),
    UnsuspectEachPermanent(Box<Permanents>),
    TapPermanent(Box<Permanent>),
    TemptWithRing,
    ThereIsAnAdditionalBeginningPhase,
    ThereIsAnAdditionalCombatPhase,
    ThereIsAnAdditionalCombatPhaseAndAnAdditionalMainPhase,
    ThereIsAnAdditionalCombatPhaseWithTrigger(Box<Actions>),
    ThereIsAnAdditionalUpkeepStep,
    TransformAPermanent(Box<Permanents>),
    TransformAnyNumberOfPermanents(Box<Permanents>),
    TransformEachPermanent(Box<Permanents>),
    TransformPermanent(Box<Permanent>),
    TrySwappingBlockAssignmentsOfTwoAttackingCreatures(Box<Permanent>, Box<Permanent>),
    TrySwappingBlockAssignmentsOfTwoBlockingCreatures(Box<Permanent>, Box<Permanent>),
    TurnAPermanentFaceUp(Box<Permanents>),
    TurnAnExiledPileFaceUp,
    TurnEachExiledCardFaceUp(Box<CardsInExile>),
    TurnEachMorphPermanentFaceDown(Box<Permanents>),
    TurnEachPermanentFaceDownAsCreature(Box<Permanents>, PT, CreatureType),
    TurnExiledCardFaceUp(Box<CardInExile>),
    TurnPermanentFaceDown(Box<Permanent>),
    TurnPermanentFaceUp(Box<Permanent>),
    UnattachEachPermanentFromEachPermanent(Box<Permanents>, Box<Permanents>),
    UnattachEachPermanentFromPermanent(Box<Permanents>, Box<Permanent>),
    UnattachPermanent(Box<Permanent>),
    UntapAPermanent(Box<Permanents>),
    UntapEachPermanent(Box<Permanents>),
    UntapPermanent(Box<Permanent>),
    UntapUptoNumberPermanents(Box<GameNumber>, Box<Permanents>),
    ValueActions(Box<GameNumber>, Vec<ValueAction>),
    VentureIntoTheDungeon,
    VoteForAPermanent(Box<Permanents>),
    VoteForAWord(Vec<VoteOption>),
    VoteForColor(Vec<Color>),
    WinTheGame,
    WouldDealExcessDamage_DealExcessToPlayerInstead(Box<Player>),

    // High Level Actions
    PermanentOrDeadPermanentDealsDamage(Box<GameNumber>, Box<DamageRecipient>),
    GraveyardCardDealsDamage(CardInGraveyard, Box<GameNumber>, Box<DamageRecipient>),
    ExiledCardDealsDamage(CardInExile, Box<GameNumber>, Box<DamageRecipient>),
    EmblemDealsDamage(Emblem, Box<GameNumber>, Box<DamageRecipient>),
    BoonDealsDamage(Boon, Box<GameNumber>, Box<DamageRecipient>),
    SacrificedPermanentDealsDamage(Box<GameNumber>, Box<DamageRecipient>),
    DeadPermanentDealsDamage(Box<GameNumber>, Box<DamageRecipient>),
    DeadGuestPermanentDealsDamage(Box<GameNumber>, Box<DamageRecipient>),
    PermanentDealsDamage(Box<Permanent>, Box<GameNumber>, Box<DamageRecipient>),
    DiscardedCardDealsDamage(CardInHand, Box<GameNumber>, Box<DamageRecipient>),
    VanguardDealsDamage(SingleVanguard, Box<GameNumber>, Box<DamageRecipient>),
    SchemeDealsDamage(SingleScheme, Box<GameNumber>, Box<DamageRecipient>),
    PlaneDealsDamage(Plane, Box<GameNumber>, Box<DamageRecipient>),
    SpellDealsDamage(Box<Spell>, Box<GameNumber>, Box<DamageRecipient>),
    APlayerGainsControlOfPermanent(Box<Players>, Box<Permanent>),
    ReflexiveAction(Box<Cost>),
    EachPlayerMakesAVillainousChoice(Box<Players>, Vec<Vec<Action>>),
    PlayerChoosesCostActionForEachPermanent(Box<Player>, Box<Permanents>, Vec<ActionOption>),
    ActionForEachPermanentThatDiedThisWay(Vec<Action>),
    RemoveEachCreatureFromCombat(Box<Permanents>),
    ActionForEachExiledCard(CardsInExile, Vec<Action>),
    ActionForEachPermanentByController(Box<Permanents>, Vec<Action>),
    ActionForEachTarget(Targets, Vec<Action>),
    ActionForEachCheckableAbility(Vec<CheckHasable>, Vec<Action>),
    SacrificedPermanentDealsDistributedDamage,
    ActionForEachDistributedAnyTarget(Vec<Action>),
    SpellDealsDistributedDamage(Box<Spell>),
    DeadPermanentDealsDistributedDamage,
    PermanentDealsDistributedDamage(Box<Permanent>),
    ActionForEachCounterTypePlayerHas(Box<Player>, Vec<Action>),
    PlayerMayActions(Box<Player>, Vec<Action>),
    ActionForEachSpellAndAbility(SpellsAndAbilities, Vec<Action>),
    PlayersRevealTopCardOfLibraryAndFindHighestManaValue(Box<Players>),
    ChaosEnsues,
    ActionForEachPermanentPutInGraveyardThisWay(Vec<Action>),
    ActionForEachPermanentExiledThisWayByController(Vec<Action>),
    PlayerRepeatedMayCost(Box<Player>, Box<GameNumber>, Box<Cost>),
    EachPlayerChoosesAnAction(Box<Players>, Vec<Action>),
    PlayerChooseAnAction(Box<Player>, Vec<Action>),
    SpellDealsDamageEachPlayer(
        Box<Players>,
        Box<Spell>,
        Box<GameNumber>,
        Box<DamageRecipient>,
    ),
    SpellDealsDamageEachPlayerForEach(
        Box<Players>,
        Box<Spell>,
        Box<GameNumber>,
        Box<DamageRecipient>,
        Box<GameNumber>,
    ),
    AnyPlayerMayPayMana(Box<Players>, ManaCost, Vec<Action>, Vec<Action>),
    DoNothing,
    SpellDealsDamageExcessReplacable(
        Box<Spell>,
        Box<GameNumber>,
        Box<DamageRecipient>,
        Vec<Action>,
    ),
    EachPlayerStartingWithMayAction(Box<Players>, Box<Player>, Box<Action>),
    PermanentDealsDamageAndPreventSomeOfIt(
        Box<Permanent>,
        Box<GameNumber>,
        Box<DamageRecipient>,
        Box<GameNumber>,
    ),
    ActionForEachCardtype(Vec<CardType>, Vec<Action>),
    SpellDealsDamageDividedAmongRecipientsRoundedDown(
        Box<Spell>,
        Box<GameNumber>,
        Box<DamageRecipient>,
    ),
    MayHavePlayerAction(Box<Player>, Box<Action>),
    SpellDealsMultipleDamage(Box<Spell>, Vec<DamageToRecipients>),
    PermanentDealsMultipleDamage(Box<Permanent>, Vec<DamageToRecipients>),
    GraveyardCardDealsMultipleDamage(Box<CardInGraveyard>, Vec<DamageToRecipients>),
    ReflexiveTriggerI(Condition, Box<Actions>),
    ReflexiveTriggerNumberTimes(Box<GameNumber>, Box<Actions>),
    PermanentDealsDamageDividedAsPlayerChooses(
        Box<Permanent>,
        Box<GameNumber>,
        Box<DamageRecipient>,
        Box<Player>,
    ),
    PermanentDealsDamageExcessReplacable(
        Box<Permanent>,
        Box<GameNumber>,
        Box<DamageRecipient>,
        Vec<Action>,
    ),
    PermanentDealsDamageForEach(
        Box<Permanent>,
        Box<GameNumber>,
        Box<DamageRecipient>,
        Box<GameNumber>,
    ),
    SpellDealsDamageForEach(
        Box<Spell>,
        Box<GameNumber>,
        Box<DamageRecipient>,
        Box<GameNumber>,
    ),
    SpellDealsDamageCantBePrevented(Box<Spell>, Box<GameNumber>, Box<DamageRecipient>),
    IfHavePlayerAction(Box<Player>, Box<Action>, Vec<Action>),
    ActionForEachPlayerInTurnOrder(Box<Players>, Vec<Action>),
    ActionForEachPermanentDestroyedThisWay(Vec<Action>),
    ReflexiveTrigger(Box<Actions>),
    If(Condition, Vec<Action>),
    PlayerMayCost(Box<Player>, Box<Cost>),
    PlayerMustCost(Box<Player>, Box<Cost>),
    MayCost(Box<Cost>),
    Unless(Condition, Vec<Action>),
    IfElse(Condition, Vec<Action>, Vec<Action>),
    ExilePermanentsControlledByOrCardsFromHand(
        Box<Players>,
        Box<Permanents>,
        Box<Cards>,
        Box<GameNumber>,
    ),
    PutEachPermanentOnTheTopOfOwnersLibraryInOrderOfOwnersChoice(Box<Permanents>),
    EachPlayerStartingWithActionInDirection(Box<Players>, Box<Player>, Direction, Vec<Action>),
    ActionForEachPermanent(Box<Permanents>, Vec<Action>),
    PutEachPermanentOnBottomOfOwnersLibraryInOrderOfOwnersChoice(Box<Permanents>),
    ActionForEachPlayer(Box<Players>, Vec<Action>),
    ActionForEachCounterTypeOnPermanent(Box<Permanent>, Vec<Action>),
    ActionForEachCounterTypeOnPermanents(Box<Permanents>, Vec<Action>),
    RepeatableActionsNumTimes(Box<GameNumber>, Vec<Action>),
    ActionNumberTimes(Box<GameNumber>, Vec<Action>),
    RepeatableActions(Vec<Action>),
    MayAction(Box<Action>),
    MayActionOnceEachTurn(Box<Action>),
    DuringNextUntap(Box<Player>, Vec<Action>),
    EachPlayerAction(Box<Players>, Box<Action>),
    EachPlayerStartingWithAction(Box<Players>, Box<Player>, Vec<Action>),
    EachPlayerMayAction(Box<Players>, Box<Action>),
    EachPlayerMayActions(Box<Players>, Vec<Action>),
    APlayerMayAction(Box<Players>, Box<Action>),
    APlayerAction(Box<Players>, Box<Action>),
    EachPlayerRepeatedMayCost(Box<Players>, Box<GameNumber>, Box<Cost>),
    RepeatedMayCost(Box<GameNumber>, Box<Cost>),
    AnyPlayerMayCost(Box<Players>, Box<Cost>),
    EachPlayerMayCost(Box<Players>, Box<Cost>),
    EachPlayerMustCost(Box<Players>, Box<Cost>),
    MustCost(Box<Cost>),
    MayActions(Vec<Action>),
    PlayerMayAction(Box<Player>, Box<Action>),
    PlayerAction(Box<Player>, Box<Action>),
    PlayerActions(Box<Player>, Vec<Action>),
    PlayersExileTopCardOfLibraryAndFindHighestManaValueUntilSingleWinner(Box<Players>),
    ReverseTurnOrder,
    DrawTheGame,
    CreateValueX(Box<GameNumber>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_OtherTarget", content = "args")]
pub enum OtherTarget {
    Ref_TargetPermanent,
    Ref_AnyTarget1,
    Ref_AnyTarget,
    ThisPermanent,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_SpellsAndPermanents", content = "args")]
pub enum SpellsAndPermanents {
    // ManaValueIs(Box<Comparison>),
    AnySpellOrPermanent,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_GroupFilter", content = "args")]
pub enum GroupFilter {
    HasNumberSymbolsAmongCosts(Box<Comparison>, ManaProduceSymbol),
    DifferentControllers,
    ShareAGraveyard,
    ANumberOfDifferentCardTypes(Box<Comparison>),
    ControlledByDifferentPlayers,
    SameToughness,
    ControlledByTheSamePlayer,
    DifferentManaValues,
    DifferentNames,
    DifferentPowers,
    EachBasicLandType,
    HasAColorNotInCommon,
    SameNames,
    ShareAllCardTypes,
    ShareACardType,
    ShareAColor,
    ShareACreatureType,
    ShareACreatureTypeOfChoice,
    ShareALandType,
    SharesANameWithEachPermanent(Box<Permanents>),
    SharesNoCreatureTypes,
    TotalManaValueIs(Box<Comparison>),
    TotalPowerIs(Box<Comparison>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_AnteCard", content = "args")]
pub enum AnteCard {
    Ref_TargetAnteCard,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Target", content = "args")]
pub enum Target {
    BetweenOneAndNumberAnyTargets(Box<GameNumber>),
    BetweenOneAndNumberTargetGraveyardCards(Box<GameNumber>, Box<CardsInGraveyard>),
    UptoNumberTargetSpellsOrAbilities(Box<GameNumber>, Box<SpellsAndAbilities>),
    OneOrTwoTargetGraveyardCards(Box<CardsInGraveyard>),
    AnyNumberOfTargetGraveyardCards(Box<CardsInGraveyard>),
    AnyNumberOfTargetGroupGraveyardCards(CardsInGraveyard, GroupFilter),
    NumberTargetGraveyardCards(Box<GameNumber>, CardsInGraveyard),
    NumberTargetGroupGraveyardCards(Box<GameNumber>, CardsInGraveyard, GroupFilter),
    TargetGraveyardCard(Box<CardsInGraveyard>),
    TargetGraveyardCardInEachPlayersGraveyard(CardsInGraveyard, Box<Players>),
    UptoNumberTargetGraveyardCards(Box<GameNumber>, CardsInGraveyard),
    UptoNumberTargetGroupGraveyardCards(Box<GameNumber>, CardsInGraveyard, GroupFilter),
    UptoOneTargetGraveyardCard(Box<CardsInGraveyard>),
    UptoOneTargetGraveyardCardInEachPlayersGraveyard(CardsInGraveyard, Box<Players>),
    UptoOneTargetGraveyardCardOfEachColor(Box<CardsInGraveyard>),
    AnyNumberOfAnyTargets,
    AnyNumberOfTargetGroupPermanents(Box<Permanents>, GroupFilter),
    AnyNumberOfTargetPermanents(Box<Permanents>),
    AnyNumberOfTargetPlayers(Box<Players>),
    AnyNumberOfTargetPlayersOrPermanents(Box<Players>, Box<Permanents>),
    AnyNumberOfTargetSpells(Box<Spells>),
    AnyOtherTarget(OtherTarget),
    AnyTarget,
    AnyTargetChosenAtRandom,
    AnyTargetExceptAPermanent(Box<Permanents>),
    AnyTargetExceptPermanent(Box<Permanent>),
    AnyTargetOfAPlayersChoice(Box<Players>),
    AnyTargetOfPlayersChoice(Box<Player>),
    AnyTargetThatWasDealtDamageThisTurn,
    NumberAnyTargets(Box<GameNumber>),
    NumberTargetGroupPermanents(Box<GameNumber>, Box<Permanents>, GroupFilter),
    NumberTargetPermanents(Box<GameNumber>, Box<Permanents>),
    NumberTargetPlayers(Box<GameNumber>, Box<Players>),
    OneOrMoreTargetPermanents(Box<Permanents>),
    OneOrTwoTargetPermanents(Box<Permanents>),
    TargetAbility(Abilities),
    TargetAnteCard(Box<Cards>),
    TargetGraveyardCardOfAPlayersChoice(CardsInGraveyard, Box<Players>),
    TargetExiledCard(Box<CardsInExile>),
    TargetPermanent(Box<Permanents>),
    TargetPermanentAtRandom(Box<Permanents>),
    TargetPermanentEachPlayerControls(Box<Permanents>, Box<Players>),
    TargetPermanentOfAPlayersChoice(Box<Permanents>, Box<Players>),
    TargetPermanentOfAPlayersChoiceTheyControl(Box<Permanents>, Box<Players>),
    TargetPermanentOfPlayersChoice(Box<Permanents>, Box<Player>),
    TargetPermanentOrExiledCard(Box<Permanents>, CardsInExile),
    TargetPlayer(Box<Players>),
    TargetPlayerAtRandom(Box<Players>),
    TargetPlayerAtTime(Box<Players>, Box<Players>),
    TargetPlayerOfPlayersChoice(Box<Players>, Box<Player>),
    TargetPlayerOrPermanent(Box<Players>, Box<Permanents>),
    TargetSpell(Box<Spells>),
    TargetSpellOrAbility(SpellsAndAbilities),
    TargetSpellOrPermanent(SpellsAndPermanents),
    TargetSpellOrTargetPermanent(Box<Spells>, Box<Permanents>),
    UptoNumberAnyTargets(Box<GameNumber>),
    UptoNumberAnyTargetsExcept(Box<GameNumber>, OtherTarget),
    UptoNumberTargetGroupPermanents(Box<GameNumber>, Box<Permanents>, GroupFilter),
    UptoNumberTargetPermanents(Box<GameNumber>, Box<Permanents>),
    UptoNumberTargetPermanentsAndOrCardsInAnyPlayersGraveyard(
        Box<GameNumber>,
        Box<Permanents>,
        Box<Cards>,
        Box<Players>,
    ),
    UptoNumberTargetPermanentsTargetPlayerControls(Box<GameNumber>, Box<Permanents>, Box<Players>),
    UptoNumberTargetPlayers(Box<GameNumber>, Box<Players>),
    UptoNumberTargetSpells(Box<GameNumber>, Box<Spells>),
    UptoOneTargetAbility(Abilities),
    UptoOneTargetExiledCard(Box<CardsInExile>),
    UptoOneTargetPermanent(Box<Permanents>),
    UptoOneTargetPermanentEachPlayerControls(Box<Permanents>, Box<Players>),
    UptoOneTargetPermanent_Optional(Box<Permanents>),
    UptoOneTargetPlayer(Box<Players>),
    UptoOneTargetPlayerOrPermanent(Box<Players>, Box<Permanents>),
    UptoOneTargetSpell(Box<Spells>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_DistributedTarget", content = "args")]
pub enum DistributedTarget {
    BetweenOneAndNumberTargetPermanents(Box<GameNumber>, Box<Permanents>),
    AnyNumberOfTargetPermanents(Box<Permanents>),
    NumberTargetPermanents(Box<GameNumber>, Box<Permanents>),
    TargetPlayer(Box<Players>),
    UptoNumberAnyTargets(Box<GameNumber>),
    AnyNumberOfAnyTargets,
    BetweenOneAndNumberAnyTargets(Box<GameNumber>),
    UptoNumberTargetPermanents(Box<GameNumber>, Box<Permanents>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Distribution", content = "args")]
pub enum Distribution {
    DistributeNumberAmongAnyTargets(Box<GameNumber>),
    DistributeNumberAmongTargets(Box<GameNumber>),
    IfElse(Condition, Box<Distribution>, Box<Distribution>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_AdditionalCostOption", content = "args")]
pub enum AdditionalCostOption {
    AdditionalCost(Box<Cost>, Box<Actions>),
    NoAdditionalCost(Box<Actions>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Actions", content = "args")]
pub enum Actions {
    AdditionalCost_Modal(Vec<AdditionalCostOption>),

    Targeted_Modal(Vec<Actions>),
    Targeted_DifferentTargets(Vec<Target>, Box<Actions>),
    TargetedDistributed(Vec<DistributedTarget>, Box<Distribution>, Box<Actions>),
    Targeted(Vec<Target>, Box<Actions>),

    ActionList(Vec<Action>),

    WithX(Box<Comparison>, Box<Actions>),
    X(Box<Comparison>, Box<Actions>),

    Modal_ChooseUptoNumberPawsMayChooseSameModeMoreThanOnce(Box<GameNumber>, Vec<PawMode>),
    Modal_ChooseOneOrChooseOneOrMoreIf(Condition, Vec<Actions>),
    Modal_ChooseOneAtRandom(Vec<Actions>),
    Modal_APlayerChoosesOne(Box<Players>, Vec<Actions>),
    Modal_ChooseAnyNumber(Vec<Actions>),
    Modal_ChooseNumberMayChooseSameModeMoreThanOnce(Box<GameNumber>, Vec<Actions>),
    Modal_ChooseOne(Vec<Actions>),
    Modal_ChooseBoth(Vec<Actions>),
    Modal_ChooseOneOrBoth(Vec<Actions>),
    Modal_ChooseOneOrBothIf(Condition, Vec<Actions>),
    Modal_ChooseOneOrMayChooseTwoIf(Condition, Vec<Actions>),
    Modal_ChooseOneOrMore(Vec<Actions>),
    Modal_ChooseOneOrMore_DifferentTargets(Vec<Actions>),
    Modal_ChooseOneOrMore_Escalate(Box<Cost>, Vec<Actions>),
    Modal_ChooseOneThatHasntBeenChosen(Vec<Actions>),
    Modal_ChooseOneThatHasntBeenChosenThisTurn(Vec<Actions>),
    Modal_ChooseOneThatWasntChosenDuringPlayersLastCombat(Box<Player>, Vec<Actions>),
    Modal_ChooseOne_Entwine(Box<Cost>, Vec<Actions>),
    Modal_ChooseThree(Vec<Actions>),
    Modal_ChooseTwo(Vec<Actions>),
    Modal_ChooseTwo_DifferentTargets(Vec<Actions>),
    Modal_ChooseTwo_Entwine(Box<Cost>, Vec<Actions>),
    Modal_ChooseUptoNumber(Box<GameNumber>, Vec<Actions>),
    Modal_ChooseUptoOne(Vec<Actions>),
    Modal_IfElse(Condition, Box<Actions>, Box<Actions>),
    Modal_MayChooseTwo_DifferentTargets(Vec<Actions>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_LibraryCardEffect", content = "args")]
pub enum LibraryCardEffect {
    AddSupertype(SuperType),
    AddAbility(Vec<Rule>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_HandEffect", content = "args")]
pub enum HandEffect {
    AddAbility(Vec<Rule>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_ExiledCardEffect", content = "args")]
pub enum ExiledCardEffect {
    AddAbility(Vec<Rule>),
    AddAbilityIfItDoesntHaveIt(Vec<Rule>),
    IsPlotted,
    IsForetold,
    IsForetoldForCost(Box<Cost>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_CardEffect", content = "args")]
pub enum CardEffect {
    SetPT(PT),

    AddLandType(LandType),
    SetCreatureTypeVariable(CreatureTypeVariable),
    AddCreatureType(CreatureType),
    AddCreatureTypeVariable(CreatureTypeVariable),
    AddCardtype(CardType),

    AddColor(SettableColor),
    SetColor(SettableColor),

    AddAbility(Vec<Rule>),
    HasAllCreatureTypes,

    MayCastFromLibraryWhileSearchingLibrary,
    CountersRemainOnCardAsItMovesBetweenZonesExceptforHandAndLibrary,
    CantBePlayed,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_StackEffect", content = "args")]
pub enum StackEffect {
    CantBeCountered,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_SpellEffect", content = "args")]
pub enum SpellEffect {
    EntersWithLayerEffectUntil(Vec<LayerEffect>, Box<Expiration>),
    WebSlinging(Box<Cost>),

    ResolvesIntoExileInsteadOfGraveyardWithACounter(CounterType),
    Wither,
    Evoke(Box<Cost>),
    Delve,
    CastWithPerpetualEffect(Vec<PerpetualEffect>),
    AsResolves(Vec<ResolveAction>),
    Offspring(ManaCost),
    Emerge(Box<Cost>),
    Freerunning(ManaCost),
    EntersWithLayerEffect(Vec<LayerEffect>),
    MayCastAsThoughItHadFlash,
    Prowl(ManaCost),
    SplitSecond,
    Conspire,
    CantBeCopied,
    CantBeCountered,
    AdditionalCostForEachColorManaSymbolInCosts(Box<Cost>, Color),
    ReplaceLandTypeVariableWithNewLandTypeVariable(LandTypeWord, LandTypeWord),
    DamageToPermanentsCantBePreventedOrRedirected(Box<Permanents>),
    IfPermanentSpell(Vec<SpellEffect>),
    ReplaceColorWordVariableWithNewColorWordVariable(ColorWordVariable, ColorWordVariable),
    Undaunted,
    ResolvesIntoExileInsteadOfGraveyard,
    If(Condition, Box<SpellEffect>),
    DecreaseManaCostForEach(CostReduction, Box<GameNumber>),
    EntersWithNumberCounters(Box<GameNumber>, CounterType),
    IncreaseManaCostForEach(ManaCost, Box<GameNumber>),
    SetCreatureTypeVariable(CreatureTypeVariable),
    EntersWithACounterOfChoice(Vec<CounterType>),
    IncreaseManaCost(ManaCost),
    DecreaseManaCost(CostReduction),
    DecreaseManaCostX(CostReductionX, Box<GameNumber>),
    EntersWithACounter(CounterType),
    CantBeCounteredBySpells(Box<Spells>),
    Ripple(Box<GameNumber>),
    StickerKicker(Box<Cost>),
    Devour(Box<Permanents>, Box<GameNumber>),
    Blitz(Box<Cost>),
    Affinity(Box<Permanents>),
    Replicate(Box<Cost>),
    AddColor(SettableColor),
    SetColor(SettableColor),
    AddCreatureType(CreatureType),
    AddCreatureTypeVariable(CreatureTypeVariable),
    AddAbilityUntil(Box<Rule>, Expiration),
    AddAbility(Vec<Rule>),
    SetPT(PT),
    Bloodthirst(Box<GameNumber>),
    Casualty(Box<GameNumber>),
    AddCardtype(CardType),
    ResolvesIntoHandInsteadOfGraveyard,
    Storm,
    HasAllCreatureTypes,
    Demonstrate,
    Improvise,
    EntersTapped,
    CantBeCast,
    Sunburst,
    IsAColorlessSourceOfDamage,
    RemoveSupertypes(Vec<SuperType>),
    Cascade,
    Deathtouch,
    Convoke,
    Riot,
    Lifelink,
    Rebound,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_GraveyardCardEffect", content = "args")]
pub enum GraveyardCardEffect {
    AddAbility(Vec<Rule>),
    CantBeTheTargetOfSpellsOrAbilities(SpellsAndAbilities),
    LosesAllAbilities,
    AddCreatureTypeVariable(CreatureTypeVariable),
}

// ------------------------------------------------------------------------- //
// --                        Static Layer Effects                         -- //
// ------------------------------------------------------------------------- //

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_StaticLayerEffect", content = "args")]
pub enum StaticLayer1Effect {
    IsACopyOf_TheObjectChosenToCopy(StaticCopyEffects),

    // Layer 1 Effect - Copy Of (Internal)
    IsACopyOf(NormalObject, StaticCopyEffects),

    // Layer 1 Effect - Copiable (Internal)
    SetCopiableManaCost(CardManaCost),
    SetCopiablePT(CardPT),
    AddCopiableCardtype(CardType),
    AddCopiableSubtype(SubType),
    AddCopiableAbility(Vec<Rule>),

    // Layer 1 Effect -- Mutate (Internal)
    MutateOnTop(MutateIndex),
    MutateUnder(MutateIndex),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_StaticLayerEffect", content = "args")]
pub enum StaticLayer2Effect {
    // Layer 2 Effect
    SetController(Box<Player>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_StaticLayerEffect", content = "args")]
pub enum StaticLayer3Effect {
    // Layer 3 Effect
    HasAllNamesOfNonlegendaryCreatures,
    HasTextOfGraveyardCardAndTheText(CardInGraveyard, Vec<Rule>),
    SetNameToTheChosenName,
    SetName(NameString),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_StaticLayerEffect", content = "args")]
pub enum StaticLayer4Effect {
    // Layer 4 Effect
    AddCardtype(CardType),
    RemoveCardtype(CardType),
    HasAllCreatureTypes,
    AddCreatureType(CreatureType),
    AddArtifactType(ArtifactType),
    AddCreatureTypeVariable(CreatureTypeVariable),
    SetCreatureTypeVariable(CreatureTypeVariable),
    AddLandTypeVariable(LandTypeVariable),
    SetLandTypeVariable(LandTypeVariable),
    AddLandType(LandType),
    AddSupertype(SuperType),
    RemoveSupertype(SuperType),
    SetArtifactType(ArtifactType),
    SetCardtype(CardType),
    SetCardtypes(Vec<CardType>),
    SetCreatureType(CreatureType),
    SetCreatureTypes(Vec<CreatureType>),
    SetLandType(LandType),
    SetLandTypes(Vec<LandType>),
    HasAllLandTypes,
    RemoveAllCreatureTypes,
    RemoveAllLandTypes,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_StaticLayerEffect", content = "args")]
pub enum StaticLayer5Effect {
    // Layer 5 Effect
    AddColor(SettableColor),
    SetColor(SettableColor),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_StaticLayerEffect", content = "args")]
pub enum StaticLayer6Effect {
    // Layer 6 Effect
    AddAbilityVariable(AbilityVariable),
    AddAbility(Vec<Rule>),
    AddAbilityFromAnExiledHasable_MayOnlyActivateOnecEachTurn(CardsInExile, Vec<CheckHasable>),
    AddAbilityFromAnExiledHasable(CardsInExile, Vec<CheckHasable>),
    AddAbilityFromExiledHasable(CardInExile, Vec<CheckHasable>),
    AddActivatedAbilitiesAndMaySpendManaAsThoughItWasAnyColorToActivate(Box<ActivatedAbilities>),
    AddAbilityFromTopOfLibraryHasable(Vec<CheckHasable>),
    AddAbilityAndLoseAllOtherAbilities(Vec<Rule>),
    AddAbilityFromCardsRemovedFromDraftWithCardsNamedHasable(
        Box<Cards>,
        NameString,
        Vec<CheckHasable>,
    ),
    AddAbilityFromPermanentHasable(Box<Permanent>, Vec<CheckHasable>),
    AddAbilityFromCardsInAPlayersGraveyardHasable(Box<Cards>, Box<Players>, Vec<CheckHasable>),
    AddAbilityFromEachPermanentHasable(Box<Permanents>, Vec<CheckHasable>),
    AddAbilityFromCardsInPlayersGraveyardHasable(Box<Cards>, Box<Player>, Vec<CheckHasable>),
    LosesAbility(CheckHasable),
    LosesAllAbilities,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_StaticLayerEffect", content = "args")]
pub enum StaticLayer7Effect {
    // Layer 7 Effect
    SetPower(Box<GameNumber>),
    SetToughness(Box<GameNumber>),
    AdjustPTX(ModX, ModX, Box<GameNumber>),
    AdjustPTXY(ModX, ModY, Box<GameNumber>, Box<GameNumber>),
    SetPowerAndToughnessBoth(Box<GameNumber>),
    AdjustPT(i32, i32),
    SetPT(PT),
    AdjustPTForEach(i32, i32, Box<GameNumber>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_StaticLayerEffect", content = "args")]
pub enum StaticLayerEffect {
    // Layer 1 Effect - Copy Of
    IsACopyOf(NormalObject, StaticCopyEffects),

    IsACopyOf_TheObjectChosenToCopy(StaticCopyEffects),

    // Layer 1 Effect - Copiable (Internal)
    SetCopiableManaCost(CardManaCost),
    SetCopiablePT(CardPT),
    AddCopiableCardtype(CardType),
    AddCopiableSubtype(SubType),
    AddCopiableAbility(Vec<Rule>),

    // Layer 1 Effect -- Mutate (Internal)
    MutateOnTop(MutateIndex),
    MutateUnder(MutateIndex),

    // Layer 2 Effect
    SetController(Box<Player>),

    // Layer 3 Effect
    HasAllNamesOfNonlegendaryCreatures,
    HasTextOfGraveyardCardAndTheText(CardInGraveyard, Vec<Rule>),
    SetNameToTheChosenName,
    SetName(NameString),

    // Layer 4 Effect
    AddCardtype(CardType),
    RemoveCardtype(CardType),
    HasAllCreatureTypes,
    AddCreatureType(CreatureType),
    AddArtifactType(ArtifactType),
    AddCreatureTypeVariable(CreatureTypeVariable),
    SetCreatureTypeVariable(CreatureTypeVariable),
    AddLandTypeVariable(LandTypeVariable),
    SetLandTypeVariable(LandTypeVariable),
    AddLandType(LandType),
    AddSupertype(SuperType),
    RemoveSupertype(SuperType),
    SetArtifactType(ArtifactType),
    SetCardtype(CardType),
    SetCardtypes(Vec<CardType>),
    SetCreatureType(CreatureType),
    SetCreatureTypes(Vec<CreatureType>),
    SetLandType(LandType),
    SetLandTypes(Vec<LandType>),
    HasAllLandTypes,
    RemoveAllCreatureTypes,
    RemoveAllLandTypes,

    // Layer 5 Effect
    AddColor(SettableColor),
    SetColor(SettableColor),

    // Layer 6 Effect
    AddAbilityVariable(AbilityVariable),
    AddAbility(Vec<Rule>),
    AddAbilityFromAnExiledHasable_MayOnlyActivateOnecEachTurn(CardsInExile, Vec<CheckHasable>),
    AddAbilityFromAnExiledHasable(CardsInExile, Vec<CheckHasable>),
    AddAbilityFromExiledHasable(CardInExile, Vec<CheckHasable>),
    AddActivatedAbilitiesAndMaySpendManaAsThoughItWasAnyColorToActivate(Box<ActivatedAbilities>),
    AddAbilityFromTopOfLibraryHasable(Vec<CheckHasable>),
    AddAbilityAndLoseAllOtherAbilities(Vec<Rule>),
    AddAbilityFromCardsRemovedFromDraftWithCardsNamedHasable(
        Box<Cards>,
        NameString,
        Vec<CheckHasable>,
    ),
    AddAbilityFromPermanentHasable(Box<Permanent>, Vec<CheckHasable>),
    AddAbilityFromCardsInAPlayersGraveyardHasable(Box<Cards>, Box<Players>, Vec<CheckHasable>),
    AddAbilityFromEachPermanentHasable(Box<Permanents>, Vec<CheckHasable>),
    AddAbilityFromCardsInPlayersGraveyardHasable(Box<Cards>, Box<Player>, Vec<CheckHasable>),
    LosesAbility(CheckHasable),
    LosesAllAbilities,

    // Layer 7 Effect
    SetPower(Box<GameNumber>),
    SetToughness(Box<GameNumber>),
    AdjustPTX(ModX, ModX, Box<GameNumber>),
    AdjustPTXY(ModX, ModY, Box<GameNumber>, Box<GameNumber>),
    SetPowerAndToughnessBoth(Box<GameNumber>),
    AdjustPT(i32, i32),
    SetPT(PT),
    AdjustPTForEach(i32, i32, Box<GameNumber>),
}

// ------------------------------------------------------------------------- //
// --                              Triggers                               -- //
// ------------------------------------------------------------------------- //

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Trigger", content = "args")]
pub enum Trigger {
    // bending
    WhenAPlayerWaterEarthFireOrAirBends(Box<Players>),

    // station
    WhenAPermanentStationsAPermanent(Box<Permanents>, Box<Permanents>),

    // Activate an Ability
    WhenAPlayerActivatesAnAbility(Box<Players>, Box<ActivatedAbilities>),
    WhenAnAbilityIsActivated(Box<ActivatedAbilities>),

    // Add Mana
    WhenAPermanentIsTappedForMana(Box<Permanents>),
    WhenAPermanentIsTappedForManaOfColor(Box<Permanents>, ManaProduce),
    WhenAPlayerTapsAPermanentForMana(Box<Players>, Box<Permanents>),
    WhenAPlayerTapsAPermanentForManaOfColor(Box<Players>, Box<Permanents>, ManaProduce),
    WhenAManaAbilityOfAPermanentResolves(Box<Permanents>),
    WhenAnAbilityCausesAPlayerToAddMana(Abilities, Box<Players>, ManaProduce),

    // Archenemy
    WhenAPlayerSetsASchemeInMotion(Box<Players>, Schemes),

    // Attach
    WhenAPermanentBecomesAttachedToAPermanent(Box<Permanents>, Box<Permanents>),
    WhenAPermanentBecomesUnattachedFromAPermanent(Box<Permanents>, Box<Permanents>),

    // Attractions
    WhenAPlayerClaimsThePrizeOfAnAttraction(Box<Players>),
    WhenAPlayerOpensAnAttraction(Box<Players>),
    WhenAPlayerVisitsAnAttraction(Box<Players>, Box<Permanents>),
    WhenAPlayerRollsToVisitTheirAttractions(Box<Players>),

    // Cast a spell
    WhenASpellBecomesTheTargetOfASpellOrAbility(Box<Spells>, Box<SpellsAndAbilities>),
    WhenAPlayerCastsASpellWithANumberOfTargets(Box<Players>, Box<Spells>, Box<Comparison>),
    WhenAPlayerCastsASpell(Box<Players>, Box<Spells>),
    WhenAPlayerCastsASpellThatTargetsAnyNumberOfPermanents(
        Box<Players>,
        Box<Spells>,
        Box<Permanents>,
    ),
    WhenAPlayerCastsTheirNthSpellInATurn(Box<Players>, Box<Comparison>, Box<Spells>),
    WhenASpellIsCast(Box<Spells>),
    WhenTheNthSpellIsCastInATurn(Box<Spells>, Box<Comparison>),
    WhenAPlayerCastsASpellFromAnywhereOtherThanTheirHand(Box<Players>, Box<Spells>),

    // cast_a_spell_or_activate_an_ability
    WhenAPlayerCastsASpellOrActivatesAnAbility(Box<Players>, Box<Spells>, Box<ActivatedAbilities>),

    // cause_an_ability_to_trigger
    WhenAPermanentEnteringTheBattlefieldCausesAnAbilityToTrigger(Box<Permanents>),
    WhenAPermanentEnteringTheBattlefieldUnderAPlayersControlCausesItsAbilityToTrigger(
        Box<Permanents>,
        Box<Players>,
    ),
    WhenAPermanentAttackingCausesItsAbilityToTrigger(Box<Permanents>),

    // champion
    WhenAPermanentIsChampionedWithAPermanent(Box<Permanents>, Box<Permanents>),

    // clash
    WhenAPlayerClashes(Box<Players>),
    WhenAPlayerClashesAndWins(Box<Players>),

    // class
    WhenAClassBecomesLevel(Box<Permanents>, Box<GameNumber>),

    // commit_a_crime
    WhenAPlayerCommitsACrime(Box<Players>),

    // collect_evidence
    WhenAPlayerCollectsEvidence(Box<Players>),

    // conjure
    WhenAPlayerConjuresAnyNumberOfOtherCards(Box<Players>),
    WhenAPlayerConjuresAnyNumberOfCards(Box<Players>),

    // copy_a_spell
    WhenAPlayerCopiesASpell(Box<Players>, Box<Spells>),

    // counter_a_spell
    WhenASpellIsCountered(Box<Spells>),
    WhenASpellOrAbilityCountersASpell(SpellsAndAbilities, Box<Spells>),

    // craft
    WhenAPermanentIsExiledFromTheBattlefieldWhileAPlayerIsActivatingACraftAbility(
        Box<Permanents>,
        Box<Players>,
    ),

    // create_tokens
    WhenAPlayerCreatesAToken(Box<Players>, Box<Permanents>),
    WhenAPlayerCreatesAnyNumberOfTokensForTheFirstTimeEachTurn(Box<Players>, Box<Permanents>),
    WhenAPlayerCreatesAnyNumberOfTokens(Box<Players>, Box<Permanents>),

    // crew
    WhenAVehicleBecoemsCrewedForTheFirstTimeEachTurn(Box<Permanents>),
    WhenAVehicleBecoemsCrewed(Box<Permanents>),
    WhenACreatureCrewsAVehicle(Box<Permanents>, Box<Permanents>),

    // cumulative_upkeep
    WhenAPlayerDoesntPayAPermanentsCumulativeUpkeepCost(Box<Players>, Box<Permanents>),
    WhenAPlayerPaysAPermanentsCumulativeUpkeepCost(Box<Players>, Box<Permanents>),
    WhenAPermanentsCumulativeUpkeepCostIsPaid(Box<Permanents>),

    // cycle_or_discard
    WhenAPlayerCyclesACard(Box<Players>, Box<CardsInHand>),
    WhenAPlayerCyclesACardForTheFirstTimeEachTurn(Box<Players>, Box<CardsInHand>),
    WhenAPlayerCyclesOrDiscardsACard(Box<Players>, Box<CardsInHand>),
    WhenAPlayerDiscardsACard(Box<Players>, Box<CardsInHand>),
    WhenAPlayerDiscardsAnyNumberOfCards(Box<Players>, Box<CardsInHand>),
    WhenAPlayerDiscardsAnyNumberOfCardsForTheFirstTimeEachTurn(
        Box<Players>,
        CardsInHand,
        Box<Players>,
    ),
    WhenASpellOrAbilityCausesAPlayerToDiscardACard(
        SpellsAndAbilities,
        Box<Players>,
        Box<CardsInHand>,
    ),
    WhenASpellOrAbilityCausesAPlayerToDiscardAnyNumberOfCards(SpellsAndAbilities, Box<Players>),
    WhenAnyNumberOfPlayersDiscardAnyNumberOfCards(Box<Players>, Box<CardsInHand>),

    // day_night
    WhenDayBecomesNightOrNightBecomesDay,

    // deal_damage
    WhenACreatureDealsCombatDamage(Box<Permanents>),
    WhenACreatureDealsCombatDamageToAPermanent(Box<Permanents>, Box<Permanents>),
    WhenACreatureDealsCombatDamageToAPlayer(Box<Permanents>, Box<Players>),
    WhenACreatureDealsCombatDamageToAPlayerForTheFirstTimeEachTurn(Box<Permanents>, Box<Players>),
    WhenACreatureDealsCombatDamageToAnyNumberOfPermanents(Box<Permanents>, Box<Permanents>),
    WhenAPermanentDealsAnAmountDamageToAPlayer(Box<Permanents>, Box<Comparison>, Box<Players>),
    WhenAPermanentDealsDamage(Box<Permanents>),
    WhenAPermanentDealsDamageToAPermanent(Box<Permanents>, Box<Permanents>),
    WhenAPermanentDealsDamageToAPlayer(Box<Permanents>, Box<Players>),
    WhenAPermanentDealsAnAmountDamage(Box<Permanents>, Box<Comparison>),
    WhenAPermanentDealsDamageToAnyNumberOfPermanents(Box<Permanents>, Box<Permanents>),
    WhenAPermanentDealsDamageToAnyNumberOfPlayersForTheFirstTimeEachTurn(
        Box<Permanents>,
        Box<Players>,
    ),
    WhenAPermanentIsDealtAnAmountOfDamage(Box<Permanents>, Box<Comparison>),
    WhenAPermanentIsDealtCombatDamage(Box<Permanents>),
    WhenAPermanentIsDealtDamage(Box<Permanents>),
    WhenAPermanentIsDealtExcessDamage(Box<Permanents>),
    WhenAPermanentIsDealtExcessNoncombatDamage(Box<Permanents>),
    WhenAPlayerIsDealtCombatDamage(Box<Players>),
    WhenAPlayerIsDealtDamage(Box<Players>),
    WhenAPlayerIsDealtNoncombatDamage(Box<Players>),
    WhenASourceDealsAnAmountOfDamageToAPermanent(DamageSources, Box<Comparison>, Box<Permanents>),
    WhenASourceDealsAnAmountOfDamageToAPlayer(DamageSources, Box<Comparison>, Box<Players>),
    WhenASourceDealsDamage(DamageSources),
    WhenASourceDealsDamageToAPermanent(DamageSources, Box<Permanents>),
    WhenASourceDealsNoncombatDamageToAPermanent(DamageSources, Box<Permanents>),
    WhenASourceDealsDamageToAPlayer(DamageSources, Box<Players>),
    WhenASourceDealsDamageToAnyNumberOfPlayersAndOrPermanents(
        DamageSources,
        Box<Players>,
        Box<Permanents>,
    ),
    WhenASourceDealsNoncombatDamageToAPlayer(DamageSources, Box<Players>),
    WhenASpellDealsDamage(Box<Spells>),
    WhenASpellDealsDamageToAPermanent(Box<Spells>, Box<Permanents>),
    WhenASpellDealsDamageToAPlayer(Box<Spells>, Box<Players>),
    WhenAnyNumberOfCreaturesAPlayerControlsDealCombatDamageToAnyNumberOfPlayers(
        Box<Permanents>,
        Box<Players>,
        Box<Players>,
    ),
    WhenAnyNumberOfCreaturesDealCombatDamageToAPermanent(Box<Permanents>, Box<Permanents>),
    WhenAnyNumberOfCreaturesDealCombatDamageToAPlayer(Box<Permanents>, Box<Players>),
    WhenAnyNumberOfCreaturesDealCombatDamageToAnyNumberOfPlayers(Box<Permanents>, Box<Players>),
    WhenAnyNumberOfCreaturesDealDamageToAPlayer(Box<Permanents>, Box<Players>),
    WhenAnyNumberOfPermanentsAreDealtExcessNoncombatDamage(Box<Permanents>),
    WhenAnyNumberOfPermanentsDealDamageToAnyNumberOfPlayers(Box<Permanents>, Box<Players>),
    WhenPlayersAreDealtCombatDamage(Box<Players>),

    // destroy
    WhenASpellOrAbilityDestroysAPermanent(SpellsAndAbilities, Box<Permanents>),
    WhenAPermanentIsDestroyed(Box<Permanents>),

    // devour
    WhenACreatureIsDevoured(Box<Permanents>),

    // discover
    WhenAPlayerDiscovers(Box<Players>),

    // draw_a_card__digital
    WhenAPlayerDrawsASpecificCard(Box<Players>, Box<Cards>),

    // draw_a_card__reveal_this_way
    WhenAPlayerDrawsARevealedCard(Box<Players>, Box<Cards>),
    WhenAPlayerRevealsFirstCardDrawn(Box<Players>, Box<Cards>),

    // draw_a_card
    WhenAPlayerDrawsACardExceptTheFirstCardDuringTheirDrawStep(Box<Players>),
    WhenAPlayerDrawsTheirNthCardEachTurn(Box<Players>, Box<Comparison>),
    WhenAPlayerDrawsACardDuringTheirTurn(Box<Players>),
    WhenAPlayerDrawsTheirNthCardDuringTheirTurn(Box<Players>, Box<Comparison>),
    WhenAPlayerDrawsACard(Box<Players>),
    WhenAPlayerDrawsTheirNthCardDuringTheirDrawStep(Box<Players>, Box<Comparison>),

    // dungeon
    WhenAPlayerCompletesADungeon(Box<Players>),

    // echo
    WhenAnEchoCostOfAPermanentIsPaid(Box<Permanents>),

    // energy
    WhenAPlayerGetsEnergy(Box<Players>),

    // enter_graveyard__from_anywhere_other_than_the_battlefield
    WhenACardIsPutIntoAGraveyardFromAnywhereOtherThanTheBattlefield(Box<Cards>, Box<Players>),

    // enter_graveyard__from_anywhere
    WhenACardIsPutIntoAPlayersGraveyardFromAnywhere(Box<Cards>, Box<Players>),
    WhenAnyNumberOfCardsArePutIntoAPlayersGraveyardFromAnywhere(Box<Cards>, Box<Players>),
    WhenAnyNumberOfCardsArePutIntoAPlayersGraveyardFromAnywhereForTheFirstTimeEachTurn(
        Box<Cards>,
        Box<Players>,
    ),
    WhenASpellOrAbilityCausesAPermanentToBePutIntoAPlayersGraveyard(
        SpellsAndAbilities,
        Box<Permanents>,
        Box<Players>,
    ),

    // enter_graveyard__from_battlefield
    WhenACreatureOrPlaneswalkerDies(Box<Permanents>),
    WhenAnyNumberOfCreaturesOrPlaneswalkersDie(Box<Permanents>),
    WhenAPermanentIsPutIntoAPlayersGraveyard(Box<Permanents>, Box<Players>),
    WhenAnyNumberOfPermanentsArePutIntoAPlayersGraveyards(Box<Permanents>, Box<Players>),

    // enter_graveyard__from_hand
    WhenACardIsPutIntoAPlayersGraveyardFromTheirHand(Box<Cards>, Box<Players>),

    // enter_graveyard__from_library
    WhenACardIsPutIntoAPlayersGraveyardFromTheirLibrary(Box<Cards>, Box<Players>),
    WhenAnyNumberOfCardsArePutIntoAPlayersGraveyardFromTheirLibrary(Box<Cards>, Box<Players>),

    // enter_hand__from_graveyard
    WhenAGraveyardCardIsPutIntoHand(Box<CardsInGraveyard>),

    // enter_hand__from_library
    WhenASpecificCardIsPutIntoAPlayersHandFromTheirLibrary(Box<CardsInHand>, Box<Players>),

    // enter_library__from_anywhere
    WhenAnyNumberOfCardsArePutIntoAPlayersLibraryFromAnywhere(Box<Players>),

    // expend
    WhenAPlayerExpendsAnAmount(Box<Players>, Box<Comparison>),

    // gift
    WhenAPlayerGivesAGift(Box<Players>),

    // plot
    WhenACardBecomesPlotted(Box<CardsInHand>),

    // roll_dice
    WhenAPlayerRollsADie(Box<Players>),
    WhenAPlayerRollsADiesHighestNaturalResult(Box<Players>),
    WhenAPlayerRollsANatural20(Box<Players>),
    WhenAPlayerRollsAValueOnADie(Box<Players>, Box<Comparison>),
    WhenAPlayerRollsAnyNumberOfDice(Box<Players>),
    WhenAPlayerRollsTheirNthDieEachTurn(Box<Players>, Box<GameNumber>),

    // saddle
    WhenACreatureSaddlesAMount(Box<Permanents>, Box<Permanents>),

    // stickers
    WhenAPlayerPlacesASticker(Box<Players>),
    WhenAPlayerPutsAStickerOnAPermanent(Box<Players>, Box<Permanents>),
    WhenAPlayerPutsAnAbilityStickerOnAPermanent(Box<Players>, Box<Permanents>),
    WhenAPlayerPutsAnArtStickerOnAPermanent(Box<Players>, Box<Permanents>),
    WhenAPlayerPutsANameStickerOnAPermanent(Box<Players>, Box<Permanents>),

    // static
    WhenAPlayerHasNumberCardsInHand(Box<Players>, Box<Comparison>),
    WhenAPlayerHasAnAmountOfLife(Box<Players>, Box<Comparison>),
    WhenAPlayerControlsAPermanent(Box<Players>, Box<Permanents>),
    WhenAPlayerControlsNoPermanents(Box<Players>, Box<Permanents>),
    WhenAPlayerControlsNumberPermanents(Box<Players>, Box<Permanents>, Box<Comparison>),
    WhenPlayersControlsNoPermanents(Box<Players>, Box<Permanents>),
    WhenAPermanentHasNumberCountersOfType(Box<Permanents>, Box<Comparison>, CounterType),
    WhenAnyNumberOfPermanentsAreOnTheBattlefield(Box<Permanents>),
    WhenNoPermanentsAreOnTheBattlefield(Box<Permanents>),
    WhenAPermanentHasAbility(Box<Permanents>, CheckHasable),
    WhenAPermanentHasPower(Box<Permanents>, Box<Comparison>),
    WhenAPlayerHasNoCardsInTheirGraveyard(Box<Players>),
    WhenAColorIsntTheMostCommonOrTiedForMostCommonColorAmongPermanents(Color, Box<Permanents>),

    // turn__declare_attackers
    WhenACreatureAttacks(Box<Permanents>),
    WhenACreatureAttacksABattle(Box<Permanents>, Box<Permanents>),
    WhenACreatureAttacksAPlaneswalker(Box<Permanents>, Box<Permanents>),
    WhenACreatureAttacksAPlayer(Box<Permanents>, Box<Players>),
    WhenACreatureAttacksAPlayerOrPlaneswalkerTheyControl(Box<Permanents>, Box<Players>),
    WhenACreatureAttacksAlone(Box<Permanents>),
    WhenACreatureAttacksForTheFirstTimeEachTurn(Box<Permanents>),
    WhenANumberOfCreaturesAttack(Box<Comparison>, Box<Permanents>),
    WhenANumberOfCreaturesAttackAPlayer(Box<Comparison>, Box<Permanents>, Box<Players>),
    WhenAPlayerAttacks(Box<Players>),
    WhenAPlayerAttacksAPlaneswalkerWithAnyNumberOfCreatures(
        Box<Players>,
        Box<Permanents>,
        Box<Permanents>,
    ),
    WhenAPlayerAttacksAPlayer(Box<Players>, Box<Players>),
    WhenAPlayerAttacksAPlayerAndOrPlaneswalkerTheyControl(Box<Players>, Box<Players>),
    WhenAPlayerAttacksAPlayerWithANumberOfCreatures(
        Box<Players>,
        Box<Players>,
        Box<Comparison>,
        Box<Permanents>,
    ),
    WhenAPlayerAttacksAPlayerWithAnyNumberOfCreatures(Box<Players>, Box<Players>, Box<Permanents>),
    WhenAPlayerAttacksAnyNumberOfPlaneswalkers(Box<Players>, Box<Permanents>),
    WhenAPlayerAttacksAnyNumberOfPlayers(Box<Players>, Box<Players>),
    WhenAPlayerAttacksWithACreature(Box<Players>, Box<Permanents>),
    WhenAPlayerAttacksWithANumberOfCreatures(Box<Players>, Box<Comparison>, Box<Permanents>),
    WhenAPlayerAttacksWithASingleCreatureAndANumberOfOtherCreatures(
        Box<Players>,
        Box<Permanents>,
        Box<Comparison>,
        Box<Permanents>,
    ),
    WhenAPlayerAttacksWithAnyNumberOfCreatures(Box<Players>, Box<Permanents>),
    WhenAPlayerAttacksWithAnyNumberOfGroupCreatures(Box<Players>, Box<Permanents>, GroupFilter),
    WhenAPlayerIsAttacked(Box<Players>),
    WhenAnyNumberOfPlayersAreAttacked(Box<Players>),
    WhenASingleCreatureAndANumberOfOtherCreaturesAttack(
        Box<Permanent>,
        Box<Comparison>,
        Box<Permanents>,
    ),
    WhenASingleCreatureAndANumberOfOtherCreaturesAttackDifferentPlayers(
        Box<Permanent>,
        Box<Comparison>,
        Box<Permanents>,
    ),
    WhenASingleCreatureAttacksWithExactlyOneOtherCreature(Box<Permanent>),
    WhenAllCreaturesAttack(Box<Permanents>),
    WhenAnyNumberOfCreaturesAttack(Box<Permanents>),
    WhenAnyNumberOfCreaturesAttackAPlayer(Box<Permanents>, Box<Players>),
    WhenAnyNumberOfCreaturesAttackAPlayerOrPlaneswalkerTheyControl(Box<Permanents>, Box<Players>),
    WhenAnyNumberOfCreaturesAttackAnyNumberOfPlayers(Box<Permanents>, Box<Players>),

    // turn__declare_attackers__enlist
    WhenACreatureEnlistsACreature(Box<Permanents>, Box<Permanents>),

    // turn__declare_blockers
    WhenACreatureAttacksAPlayerAndIsntBlocked(Box<Permanents>, Box<Players>),
    WhenACreatureAttacksAndIsntBlocked(Box<Permanents>),
    WhenACreatureBecomesBlocked(Box<Permanents>),
    WhenACreatureBecomesBlockedByACreature(Box<Permanents>, Box<Permanents>),
    WhenACreatureBecomesBlockedByAnyNumberOfCreatures(Box<Permanents>, Box<Permanents>),
    WhenACreatureBecomesBlockedByANumberOfCreatures(
        Box<Permanents>,
        Box<Comparison>,
        Box<Permanents>,
    ),
    WhenACreatureBlocks(Box<Permanents>),
    WhenACreatureBlocksACreature(Box<Permanents>, Box<Permanents>),
    WhenACreatureBlocksANumberOfCreatures(Box<Permanents>, Box<Comparison>, Box<Permanents>),
    WhenACreatureBlocksAnyNumberOfCreatures(Box<Permanents>, Box<Permanents>),
    WhenANumberOfCreaturesAttacksAPlayerAndArentBlocked(
        Box<Comparison>,
        Box<Permanents>,
        Box<Players>,
    ),
    WhenAnyNumberOfCreaturesBecomeBlocked(Box<Permanents>),
    WhenAnyNumberOfCreaturesBlock(Box<Permanents>),

    // turn__end_of_combat
    AtTheEndOfCombat,
    AtTheEndOfTheFirstCombat,

    // enter_battlefield
    WhenAPermanentEntersTheBattlefieldOrTheCreatureItHauntsDies(Box<Permanents>),
    WhenAnyNumberOfPermanentsEnterTheBattlefieldUnderAPlayersControl(Box<Permanents>, Box<Players>),
    WhenAnyNumberOfPermanentsEnterTheBattlefield(Box<Permanents>),
    WhenAPermanentEntersTheBattlefield(Box<Permanents>),
    WhenAPermanentEntersTheBattlefieldAttachedToAPermanent(Box<Permanents>, Box<Permanents>),
    WhenAPermanentEntersTheBattlefieldAttacking(Box<Permanents>),
    WhenAPermanentEntersTheBattlefieldDuringTheDeclareAttacksStep(Box<Permanents>),
    WhenAPermanentEntersTheBattlefieldFromAPlayersGraveyard(Box<Permanents>, Box<Players>),
    WhenAPermanentEntersTheBattlefieldFromAPlayersHand(Box<Permanents>, Box<Players>),
    WhenAPermanentEntersTheBattlefieldFromAnywhereOtherThanAGraveyardOrExile(Box<Permanents>),
    WhenAPermanentEntersTheBattlefieldFromAnywhereOtherThanTheirHand(Box<Permanents>),
    WhenAPermanentEntersTheBattlefieldFromExile(Box<Permanents>),
    WhenAPermanentEntersTheBattlefieldFromExileOrWasCastFromExile(Box<Permanents>),
    WhenAPermanentEntersTheBattlefieldTapped(Box<Permanents>),
    WhenAPermanentEntersTheBattlefieldTransformed(Box<Permanents>),
    WhenAPermanentEntersTheBattlefieldUnderAPlayersControl(Box<Permanents>, Box<Players>),
    WhenAPermanentEntersTheBattlefieldUnderAPlayersControlWithoutBeingPlayed(
        Box<Permanents>,
        Box<Players>,
    ),
    WhenAPermanentEntersTheBattlefieldUntapped(Box<Permanents>),
    WhenAPermanentEntersTheBattlefieldWithAnyCounters(Box<Permanents>),

    // give_a_player_counters
    WhenAPlayerPutsAnyCountersOnAPlayer(Box<Players>, Box<Players>),

    // prevent_damage
    WhenDamageThatWouldBeDealtToAPlayerIsPrevented(Box<Players>),

    // proliferate
    WhenAPlayerProliferates(Box<Players>),

    // put_a_spell_or_ability_onto_the_stack
    WhenASpellOrAbilityIsPutOntoTheStack(SpellsAndAbilities),

    // enter_command_zone__from_battlefield
    WhenAPermanentIsPutIntoTheCommandZone(Box<Permanents>),

    // enter_command_zone__from_anywhere
    WhenACardIsPutIntoTheCommandZoneFromAnywhere(Box<Cards>),

    // leave_graveyard
    WhenAGraveyardCardLeaves(Box<CardsInGraveyard>),
    WhenAnyNumberOfGraveyardCardsLeave(Box<CardsInGraveyard>),

    // leave_battlefield
    WhenAPermanentLeavesTheBattlefield(Box<Permanents>),
    WhenAPermanentLeavesTheBattlefieldWithoutDying(Box<Permanents>),
    WhenAnyNumberOfPermanentsLeaveTheBattlefield(Box<Permanents>),
    WhenAnyNumberOfPermanentsLeaveTheBattlefieldWithoutDying(Box<Permanents>),

    // gain_control
    WhenAPlayerGainsControlOfAPermanentFromAPlayer(Box<Players>, Box<Permanents>, Box<Players>),

    // lose_control
    WhenAPlayerLosesControlOfAPermanent(Box<Players>, Box<Permanents>),

    // evolve
    WhenAPermanentEvolves(Box<Permanents>),

    // exile
    WhenAnyNumberOfPermanentsAndOrGraveyardCardsArePutIntoExile(
        Box<Permanents>,
        Box<CardsInGraveyard>,
    ),
    WhenAPermanentIsExiled(Box<Permanents>),
    WhenACardIsPutIntoExile(Box<Cards>),
    WhenAnyNumberOfCardsArePutIntoExile(Box<Cards>),
    WhenAnyNumberOfCardsArePutIntoExileFromAPlayersGraveyard(Box<Cards>, Box<Players>),
    WhenAnyNumberOfCardsArePutIntoExileFromAPlayersGraveyardAndOrLibrary(Box<Cards>, Box<Players>),
    WhenAnyNumberOfGenericCardsArePutIntoExileFromAPlayersHand(Box<Players>),
    WhenASpellOrAbilityExilesAnyNumberOfPermanents(SpellsAndAbilities, Box<Permanents>),

    // pays_life
    WhenAPlayerPaysLife(Box<Players>),
    WhenAPlayerPaysLifeToActivateAnAbility(Box<Players>, Box<ActivatedAbilities>),

    // forage
    WhenAPlayerForages(Box<Players>),

    // investigate
    WhenAPlayerInvestigatesForTheFirstTimeEachTurn(Box<Players>),
    WhenAPlayerInvestigates(Box<Players>),

    // kicker
    WhenAPlayerKicksASpell(Box<Players>, Box<Spells>),

    // mentor
    WhenACreatureMentorsACreature(Box<Permanents>, Box<Permanents>),

    // mill
    WhenAPlayerMillsASpecificCard(Box<Players>, Box<Cards>),
    WhenAPlayerMillsAnyNumberOfSpecificCards(Box<Players>, Box<Cards>),
    WhenAPlayerMillsAnyNumberOfCards(Box<Players>),
    WhenAnyNumberOfSpecificCardsAreMilled(Box<Cards>),

    // lose_life
    WhenAPlayerLosesLife(Box<Players>),
    WhenAPlayerLosesLifeDuringTheirTurn(Box<Players>),
    WhenAPlayerLosesLifeForTheFirstTimeEachTurn(Box<Players>),
    WhenAnyNumberOfPlayersEachLoseAnAmountOfLife(Box<Players>, Box<Comparison>),
    WhenAnyNumberOfPlayersLoseLife(Box<Players>),

    // exploit
    WhenAPermanentExploitsAPermanent(Box<Permanents>, Box<Permanents>),

    // explore
    WhenAPermanentExplores(Box<Permanents>),
    WhenAPermanentExploresACardOfType(Box<Permanents>, Box<Cards>),

    // fight
    WhenACreatureFights(Box<Permanents>),
    WhenAnyNumberOfCreaturesFight(Box<Permanents>),

    // flip_coins
    WhenAPlayerWinsACoinFlip(Box<Players>),
    WhenAPlayerLosesACoinFlip(Box<Players>),

    // foretell
    WhenAPlayerForetellsACard(Box<Players>),

    // gain_life
    WhenAPlayerGainsLife(Box<Players>),
    WhenAPlayerGainsLifeDuringTheirTurn(Box<Players>),
    WhenAPlayerGainsLifeForTheFirstTimeEachTurn(Box<Players>),
    WhenASpellCausesAPlayerToGainLife(Box<Spells>, Box<Players>),

    // lose_the_game
    WhenAPlayerLosesTheGame(Box<Players>),

    // manifest_dread
    WhenAPlayerManifestsDread(Box<Players>),

    // monstrosity
    WhenAPermanentBecomesMonstrous(Box<Permanents>),

    // mutate
    WhenACreatureMutates(Box<Permanents>),

    // phasing
    WhenAPermanentPhasesOut(Box<Permanents>),
    WhenAPermanentPhasesIn(Box<Permanents>),
    WhenAnyNumberOfPermanentsPhaseOut(Box<Permanents>),

    // planechase
    WhenAPlaneHasNumberCountersOfType(Planes, Box<Comparison>, CounterType),
    WhenAPlayerEncountersAPhenomenon(Box<Players>, Phenomena),
    WhenAPlayerPlaneswalksAwayFromAPlane(Box<Players>, Planes),
    WhenAPlayerPlaneswalksToAPlane(Box<Players>, Planes),
    WhenAPlayerRollsABlankOnThePlanarDie(Box<Players>),
    WhenAPlayerRollsThePlanarDie(Box<Players>),
    WhenChaosEnsues,

    // play_a_card
    WhenAPlayerPlaysACard(Box<Players>, Box<Cards>),
    WhenAPlayerPlaysACardFromExile(Box<Players>, CardsInExile),

    // play_a_land
    WhenAPlayerPlaysALand(Box<Players>, Box<Permanents>),
    WhenAPlayerPlaysALandFromAmongCardsInExile(Box<Players>, Box<Permanents>, CardsInExile),
    WhenAPlayerPlaysALandFromExile(Box<Players>, Box<Permanents>),
    WhenAPlayerPlaysALandFromAnywhereOtherThanTheirHand(Box<Players>, Box<Permanents>),

    // put_counters
    WhenACounterIsPutOnAPermanent(Box<Permanents>),
    WhenACounterOfTypeIsPutOnAPermanent(CounterType, Box<Permanents>),
    WhenAPlayerPutsACounterOfTypeOnAPermanent(Box<Players>, CounterType, Box<Permanents>),
    WhenAPlayerPutsAnyNumberOfCountersOfTypeOnAPermanent(
        Box<Players>,
        CounterType,
        Box<Permanents>,
    ),
    WhenAPlayerPutsAnyNumberOfGenericCountersOnAPermanent(Box<Players>, Box<Permanents>),
    WhenAnyNumberOfCountersAreRemovedFromAPermanent(Box<Permanents>),
    WhenAnyNumberOfCountersArePutOnAPermanentForTheFirstTimeEachTurn(Box<Permanents>),
    WhenAnyNumberOfCountersArePutOnAPermanent(Box<Permanents>),
    WhenAnyNumberOfCountersOfTypeArePutOnAPermanent(CounterType, Box<Permanents>),
    WhenAnyNumberOfCountersOfTypeArePutOnAPermanentForTheFirstTimeEachTurn(
        CounterType,
        Box<Permanents>,
    ),
    WhenAnyNumberOfCountersOfTypeArePutOnAnyNumberOfPermanents(CounterType, Box<Permanents>),
    WhenTheNthCounterOfTypeIsPutOnAPermanent(CounterType, Box<Permanents>, Box<GameNumber>),
    WhenAPlayerPutsACounterOnAPermanent(Box<Players>, Box<Permanents>),

    // put_permanent_on_battlefield
    WhenAPlayerPutsAPermanentOnTheBattlefield(Box<Players>, Box<Permanents>),

    // remove_counters__exile
    WhenACounterOfTypeIsRemovedFromAnExiledCard(CounterType, CardsInExile),
    WhenAPlayerRemovesACounterOfTypeFromAnExiledCard(Box<Players>, CounterType, CardsInExile),
    WhenTheLastCounterOfTypeIsRemovedFromAnExiledCard(CounterType, CardsInExile),

    // remove_counters
    WhenACounterOfTypeIsRemovedFromAPermanent(CounterType, Box<Permanents>),
    WhenAPlayerRemovesTheLastCounterOfTypeFromAPermanent(
        Box<Players>,
        CounterType,
        Box<Permanents>,
    ),
    WhenAnyNumberOfCountersOfTypeAreRemovedFromAPermanent(CounterType, Box<Permanents>),
    WhenTheLastCounterOfTypeIsRemovedFromAPermanent(CounterType, Box<Permanents>),

    // renown
    WhenACreatureBecomesRenowned(Box<Permanents>),

    // enter_hand__from_battlefield
    WhenAPermanentIsReturnedToAPlayersHand(Box<Permanents>, Box<Players>),
    WhenAnyNumberOfPermanentsAreReturnedToHand(Box<Permanents>),

    // room
    WhenAPlayerFullyUnlocksARoom(Box<Players>, Box<Permanents>),
    WhenAPlayerUnlocksADoor(Box<Players>, Box<Permanents>),

    // ring
    WhenAPlayerChoosesARingBearer(Box<Players>),
    WhenTheRingTemptsAPlayer(Box<Players>),

    // sacrifice
    WhenAPlayerSacrificesAPermanent(Box<Players>, Box<Permanents>),
    WhenAPlayerSacrificesAPermanentForEmerge(Box<Players>, Box<Permanents>),
    WhenAPlayerSacrificesAnyNumberOfPermanentsToActivateAnAbility(
        Box<Players>,
        Box<ActivatedAbilities>,
    ),
    WhenAPlayerSacrificesAnyNumberOfPermanents(Box<Players>),
    WhenAPermanentIsSacrificed(Box<Permanents>),

    // saddle
    WhenAPermanentBecomesSaddledForTheFirstTimeInATurn(Box<Permanents>),

    // saga
    WhenTheFinalChapterOfASagaTriggers(Box<Permanents>),
    WhenTheFinalChapterOfASagaResolves(Box<Permanents>),

    // scry
    WhenAPlayerScrys(Box<Players>),
    WhenAPlayerChoosesToPutAnyCardsOnTheBottomOfTheirLibraryWhileScrying(Box<Players>),

    // search_library
    WhenAPlayerSearchesTheirLibrary(Box<Players>),

    // seek
    WhenAPlayerSeeksAnyNumberOfCards(Box<Players>),

    // shuffle
    WhenASpellOrAbilityCausesAPlayerToShuffleTheirLibrary(SpellsAndAbilities, Box<Players>),
    WhenASpellOrAbilityCausesItsControllerToShuffleTheirLibrary(SpellsAndAbilities),
    WhenAPlayerShufflesTheirLibrary(Box<Players>),

    // solves_a_case
    WhenAPlayerSolvesACase(Box<Players>),

    // specialize
    WhenACreatureSpecializes(Box<Permanents>),
    WhenAGraveyardCardSpecializes(Box<CardsInGraveyard>),
    WhenCardSpecializes(SingleCard),

    // surveil
    WhenAPlayerSurveils(Box<Players>),
    WhenAPlayerSurveilsForTheFirstTimeEachTurn(Box<Players>),

    // tap
    WhenAnyNumberOfPermanentsBecomeTapped(Box<Permanents>),
    WhenAPermanentBecomesTapped(Box<Permanents>),
    WhenAPermanentBecomesTappedForTheFirstTimeEachTurn(Box<Permanents>),
    WhenAPlayerTapsAPermanent(Box<Players>, Box<Permanents>),

    // targets
    WhenAPermanentBecomesTheTargetOfASpell(Box<Permanents>, Box<Spells>),
    WhenAPermanentBecomesTheTargetOfASpellOrAbility(Box<Permanents>, SpellsAndAbilities),
    WhenAPermanentBecomesTheTargetOfASpellOrAbilityForTheFirstTimeEachTurn(
        Box<Permanents>,
        SpellsAndAbilities,
    ),
    WhenAPermanentBecomesTheTargetOfAnAbility(Box<Permanents>, Abilities),
    WhenAPlayerBecomesTheTargetOfASpell(Box<Players>, Box<Spells>),
    WhenAPlayerBecomesTheTargetOfASpellOrAbility(Box<Players>, SpellsAndAbilities),
    WhenAPlayerChoosesTargetsForASpellOrAbility(Box<Players>, SpellsAndAbilities),
    WhenAnyNumberOfPlayersAndOrPermanentsBecomeTheTargetOfASpellOrAbility(
        Box<Players>,
        Box<Permanents>,
        SpellsAndAbilities,
    ),

    // the_monarch
    WhenAPlayerBecomesTheMonarch(Box<Players>),

    // training
    WhenAPermanentTrains(Box<Permanents>),

    // transform
    WhenAPermanentTransformsFromIntoAPermanent(Box<Permanents>, Box<Permanents>),
    WhenAPermanentTransforms(Box<Permanents>),

    // turn__beginning_of_combat
    AtTheBeginningOfCombatDuringAPlayersTurn(Box<Players>),
    AtTheBeginningOfCombat,

    // turn__beginning_of_game
    AtTheBeginningOfTheGame,

    // turn__declare_attackers__exert
    WhenAPlayerExertsACreature(Box<Players>, Box<Permanents>),

    // turn__draw_step
    AtTheBeginningOfAPlayersDrawStep(Box<Players>),

    // turn__end_step
    AtTheBeginningOfAPlayersEndStep(Box<Players>),

    // turn__main_phase
    AtTheBeginningOfAPlayersMainPhases(Box<Players>),
    AtTheBeginningOfAPlayersFirstMainPhase(Box<Players>),
    AtTheBeginningOfAPlayersSecondMainPhase(Box<Players>),
    AtTheBeginningOfAPlayersPostcombatMainPhase(Box<Players>),

    // turn__unkeep
    AtTheBeginningOfTheFirstUpkeepOfTheGame,
    AtTheBeginningOfAPlayersFirstUpkeepOfTheGame(Box<Players>),
    AtTheBeginningOfAPlayersFirstUpkeepEachTurn(Box<Players>),
    AtTheBeginningOfAPlayersUpkeep(Box<Players>),

    // untap
    WhenAPlayerUntapsAnyNumberOfPermanentDuringTheirUntapStep(Box<Players>, Box<Permanents>),
    WhenAPermanentBecomesUntapped(Box<Permanents>),

    // voting
    WhenPlayersFinishVoting,

    // turn_face_up
    WhenAPermanentIsTurnedFaceUp(Box<Permanents>),
    WhenAPlayerTurnsAPermanentFaceUp(Box<Players>, Box<Permanents>),

    // _operators
    Or(Vec<Trigger>),
}

type StickerCost = u32;

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_Sticker", content = "args")]
pub enum Sticker {
    NameSticker(NameString),
    PTSticker(StickerCost, CardPT),
    AbilitySticker(StickerCost, Vec<Rule>),
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
#[serde(tag = "_OracleCard")]
pub struct Card {
    pub name: NameString,

    pub typeline: OracleTypeline,

    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "::serde_with::rust::unwrap_or_skip")]
    pub mana_cost: Option<CardManaCost>,

    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "::serde_with::rust::unwrap_or_skip")]
    pub color_indicator: Option<Vec<ColorIndicatorColor>>,

    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "::serde_with::rust::unwrap_or_skip")]
    pub rules: Option<Vec<Rule>>,

    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "::serde_with::rust::unwrap_or_skip")]
    #[serde(rename = "CardPT")]
    pub card_pt: Option<CardPT>,

    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "::serde_with::rust::unwrap_or_skip")]
    pub loyalty: Option<LoyaltyNumber>,

    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "::serde_with::rust::unwrap_or_skip")]
    pub defense: Option<i32>,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
#[serde(tag = "_OracleCard")]
pub struct MeldPiece {
    pub name: NameString,

    pub typeline: OracleTypeline,

    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "::serde_with::rust::unwrap_or_skip")]
    pub mana_cost: Option<CardManaCost>,

    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "::serde_with::rust::unwrap_or_skip")]
    pub color_indicator: Option<Vec<ColorIndicatorColor>>,

    pub rules: Vec<Rule>,

    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "::serde_with::rust::unwrap_or_skip")]
    #[serde(rename = "CardPT")]
    pub card_pt: Option<CardPT>,

    pub melds_into: NameString,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
#[serde(tag = "_OracleCard")]
pub struct Melded {
    pub name: NameString,

    pub typeline: OracleTypeline,

    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "::serde_with::rust::unwrap_or_skip")]
    pub color_indicator: Option<Vec<ColorIndicatorColor>>,

    pub rules: Vec<Rule>,

    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "::serde_with::rust::unwrap_or_skip")]
    #[serde(rename = "CardPT")]
    pub card_pt: Option<CardPT>,

    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "::serde_with::rust::unwrap_or_skip")]
    pub loyalty: Option<LoyaltyNumber>,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
#[serde(tag = "_OracleCard")]
pub struct Adventurer {
    pub name: NameString,

    pub typeline: OracleTypeline,

    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "::serde_with::rust::unwrap_or_skip")]
    pub mana_cost: Option<CardManaCost>,

    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "::serde_with::rust::unwrap_or_skip")]
    pub color_indicator: Option<Vec<ColorIndicatorColor>>,

    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "::serde_with::rust::unwrap_or_skip")]
    pub rules: Option<Vec<Rule>>,

    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "::serde_with::rust::unwrap_or_skip")]
    #[serde(rename = "CardPT")]
    pub card_pt: Option<CardPT>,

    pub adventure: Card,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
#[serde(tag = "_OracleCard")]
pub struct Preparer {
    pub name: NameString,

    pub typeline: OracleTypeline,

    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "::serde_with::rust::unwrap_or_skip")]
    pub mana_cost: Option<CardManaCost>,

    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "::serde_with::rust::unwrap_or_skip")]
    pub color_indicator: Option<Vec<ColorIndicatorColor>>,

    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "::serde_with::rust::unwrap_or_skip")]
    pub rules: Option<Vec<Rule>>,

    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "::serde_with::rust::unwrap_or_skip")]
    #[serde(rename = "CardPT")]
    pub card_pt: Option<CardPT>,

    pub prepared: Card,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
#[serde(tag = "_OracleCard")]
pub struct Ominous {
    pub name: NameString,

    pub typeline: OracleTypeline,

    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "::serde_with::rust::unwrap_or_skip")]
    pub mana_cost: Option<CardManaCost>,

    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "::serde_with::rust::unwrap_or_skip")]
    pub color_indicator: Option<Vec<ColorIndicatorColor>>,

    pub rules: Vec<Rule>,

    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "::serde_with::rust::unwrap_or_skip")]
    #[serde(rename = "CardPT")]
    pub card_pt: Option<CardPT>,

    pub omen: Card,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
#[serde(tag = "_OracleCard")]
pub struct ModalDFC {
    pub front_face: Card,
    pub back_face: Card,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
#[serde(tag = "_OracleCard")]
pub struct Transforming {
    pub front_face: Card,
    pub back_face: Card,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
#[serde(tag = "_OracleCard")]
pub struct Flip {
    pub mana_cost: CardManaCost,
    pub unflipped: FlipInfo,
    pub flipped: FlipInfo,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
#[serde(tag = "_OracleCard")]
pub struct Room {
    pub typeline: OracleTypeline,
    pub left_door: DoorInfo,
    pub right_door: DoorInfo,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
#[serde(tag = "_OracleCard")]
pub struct Split {
    pub cards: Vec<Card>,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
#[serde(tag = "_OracleCard")]
pub struct Planar {
    pub name: NameString,
    pub typeline: OracleTypeline,
    pub rules: Vec<Rule>,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
#[serde(tag = "_OracleCard")]
pub struct Conspiracy {
    pub name: NameString,
    pub typeline: OracleTypeline,
    pub rules: Vec<Rule>,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
#[serde(tag = "_OracleCard")]
pub struct Scheme {
    pub name: NameString,
    pub typeline: OracleTypeline,
    pub rules: Vec<Rule>,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
#[serde(tag = "_OracleCard")]
pub struct Dungeon {
    pub name: NameString,
    pub typeline: OracleTypeline,
    pub rules: Vec<Rule>,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
#[serde(tag = "_OracleCard")]
pub struct Vanguard {
    pub name: NameString,
    pub typeline: OracleTypeline,
    pub rules: Vec<Rule>,
    pub life_modifier: i32,
    pub hand_modifier: i32,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
#[serde(tag = "_OracleCard")]
pub struct StickerSheet {
    pub stickers: Vec<Sticker>,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
#[serde(tag = "_OracleCard")]
pub struct FlipInfo {
    pub name: NameString,
    pub typeline: OracleTypeline,
    pub rules: Vec<Rule>,

    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "::serde_with::rust::unwrap_or_skip")]
    #[serde(rename = "CardPT")]
    pub card_pt: Option<CardPT>,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
#[serde(tag = "_OracleCard")]
pub struct DoorInfo {
    pub name: NameString,
    pub rules: Vec<Rule>,
    pub mana_cost: CardManaCost,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "_OracleCard")]
// #[serde(untagged)]
pub enum OracleCard {
    #[serde(rename_all = "PascalCase")]
    Card {
        name: NameString,

        typeline: OracleTypeline,

        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(with = "::serde_with::rust::unwrap_or_skip")]
        mana_cost: Option<CardManaCost>,

        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(with = "::serde_with::rust::unwrap_or_skip")]
        color_indicator: Option<Vec<ColorIndicatorColor>>,

        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(with = "::serde_with::rust::unwrap_or_skip")]
        rules: Option<Vec<Rule>>,

        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(with = "::serde_with::rust::unwrap_or_skip")]
        #[serde(rename = "CardPT")]
        card_pt: Option<CardPT>,

        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(with = "::serde_with::rust::unwrap_or_skip")]
        loyalty: Option<LoyaltyNumber>,

        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(with = "::serde_with::rust::unwrap_or_skip")]
        defense: Option<i32>,
    },

    #[serde(rename_all = "PascalCase")]
    MeldPiece {
        name: NameString,

        typeline: OracleTypeline,

        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(with = "::serde_with::rust::unwrap_or_skip")]
        mana_cost: Option<CardManaCost>,

        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(with = "::serde_with::rust::unwrap_or_skip")]
        color_indicator: Option<Vec<ColorIndicatorColor>>,

        rules: Vec<Rule>,

        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(with = "::serde_with::rust::unwrap_or_skip")]
        #[serde(rename = "CardPT")]
        card_pt: Option<CardPT>,

        melds_into: NameString,
    },

    #[serde(rename_all = "PascalCase")]
    Melded {
        name: NameString,

        typeline: OracleTypeline,

        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(with = "::serde_with::rust::unwrap_or_skip")]
        color_indicator: Option<Vec<ColorIndicatorColor>>,

        rules: Vec<Rule>,

        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(with = "::serde_with::rust::unwrap_or_skip")]
        #[serde(rename = "CardPT")]
        card_pt: Option<CardPT>,

        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(with = "::serde_with::rust::unwrap_or_skip")]
        loyalty: Option<LoyaltyNumber>,
    },

    #[serde(rename_all = "PascalCase")]
    Adventurer {
        name: NameString,

        typeline: OracleTypeline,

        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(with = "::serde_with::rust::unwrap_or_skip")]
        mana_cost: Option<CardManaCost>,

        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(with = "::serde_with::rust::unwrap_or_skip")]
        color_indicator: Option<Vec<ColorIndicatorColor>>,

        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(with = "::serde_with::rust::unwrap_or_skip")]
        rules: Option<Vec<Rule>>,

        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(with = "::serde_with::rust::unwrap_or_skip")]
        #[serde(rename = "CardPT")]
        card_pt: Option<CardPT>,

        adventure: Card,
    },

    #[serde(rename_all = "PascalCase")]
    Preparer {
        name: NameString,

        typeline: OracleTypeline,

        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(with = "::serde_with::rust::unwrap_or_skip")]
        mana_cost: Option<CardManaCost>,

        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(with = "::serde_with::rust::unwrap_or_skip")]
        color_indicator: Option<Vec<ColorIndicatorColor>>,

        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(with = "::serde_with::rust::unwrap_or_skip")]
        rules: Option<Vec<Rule>>,

        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(with = "::serde_with::rust::unwrap_or_skip")]
        #[serde(rename = "CardPT")]
        card_pt: Option<CardPT>,

        prepared: Card,
    },

    #[serde(rename_all = "PascalCase")]
    Ominous {
        name: NameString,

        typeline: OracleTypeline,

        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(with = "::serde_with::rust::unwrap_or_skip")]
        mana_cost: Option<CardManaCost>,

        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(with = "::serde_with::rust::unwrap_or_skip")]
        color_indicator: Option<Vec<ColorIndicatorColor>>,

        rules: Vec<Rule>,

        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(with = "::serde_with::rust::unwrap_or_skip")]
        #[serde(rename = "CardPT")]
        card_pt: Option<CardPT>,

        omen: Card,
    },

    #[serde(rename_all = "PascalCase")]
    ModalDFC { front_face: Card, back_face: Card },

    #[serde(rename_all = "PascalCase")]
    Transforming { front_face: Card, back_face: Card },

    #[serde(rename_all = "PascalCase")]
    Flip {
        mana_cost: CardManaCost,
        unflipped: FlipInfo,
        flipped: FlipInfo,
    },

    #[serde(rename_all = "PascalCase")]
    Room {
        typeline: OracleTypeline,
        left_door: DoorInfo,
        right_door: DoorInfo,
    },

    #[serde(rename_all = "PascalCase")]
    Split { cards: Vec<Card> },

    #[serde(rename_all = "PascalCase")]
    Planar {
        name: NameString,
        typeline: OracleTypeline,
        rules: Vec<Rule>,
    },

    #[serde(rename_all = "PascalCase")]
    Conspiracy {
        name: NameString,
        typeline: OracleTypeline,
        rules: Vec<Rule>,
    },

    #[serde(rename_all = "PascalCase")]
    Scheme {
        name: NameString,
        typeline: OracleTypeline,
        rules: Vec<Rule>,
    },

    #[serde(rename_all = "PascalCase")]
    Dungeon {
        name: NameString,
        typeline: OracleTypeline,
        rules: Vec<Rule>,
    },

    #[serde(rename_all = "PascalCase")]
    Vanguard {
        name: NameString,
        typeline: OracleTypeline,
        rules: Vec<Rule>,
        life_modifier: i32,
        hand_modifier: i32,
    },

    #[serde(rename_all = "PascalCase")]
    StickerSheet { stickers: Vec<Sticker> },
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(untagged)]
pub enum RegularCard {
    Card(Card),
    MeldPiece(MeldPiece),
    Melded(Melded),
    Adventurer(Adventurer),
    Ominous(Ominous),
    Preparer(Preparer),
    ModalDFC(ModalDFC),
    Transforming(Transforming),
    Flip(Flip),
    Room(Room),
    Split(Split),
}

// ------------------------------------- //
//  Internal State, not on Oracle Cards  //
// ------------------------------------- //

pub type ManaCost = Vec<ManaSymbol>;
pub type ManaCostX = Vec<ManaSymbolX>;
pub type CardManaCost = Vec<ManaSymbolX>;
pub type CostReduction = Vec<CostReductionSymbol>;
pub type CostReductionX = Vec<CostReductionSymbolX>;

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
#[serde(tag = "_Name")]
pub enum RuleSource {
    Printed,
    Copy {
        effect_source: SourcedRule,
        copied_source: SourcedRule,
    },
    CopyModifier {
        effect_source: SourcedRule,
    },
    AddCopiable {
        effect_source: SourcedRule,
    },
    Mutate {
        effect_source: SourcedRule,
    },
    BattlefieldEffect {
        effect_source: SourcedRule,
        permanent_id: PermanentId,
    },
    Effect {
        effect_source: SourcedRule,
        effect_id: EffectId,
    },
    TestCase,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct SourcedRule {
    rule_source: Box<RuleSource>,
    rule: Rule,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
#[serde(tag = "_Name")]
pub enum ObjectName {
    Name {
        name: NameString,
    },

    FlipName {
        unflipped: NameString,
        flipped: NameString,
    },
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
#[serde(tag = "_Typeline")]
pub enum ObjectTypeline {
    Typeline {
        typeline: OracleTypeline,
    },

    FlipTypline {
        unflipped: OracleTypeline,
        flipped: OracleTypeline,
    },
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
#[serde(tag = "_Name")]
pub enum ObjectPT {
    CardPT {
        #[serde(rename = "CardPT")]
        card_pt: CardPT,
    },

    FlipCardPT {
        unflipped: CardPT,
        flipped: CardPT,
    },
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
#[serde(tag = "_NormalObject")]
pub struct NormalObject {
    pub name: Option<ObjectName>,
    pub typeline: ObjectTypeline,

    #[serde(rename = "CardPT")]
    pub card_pt: Option<ObjectPT>,
    pub mana_cost: Option<CardManaCost>,
    pub rules: Vec<SourcedRule>,

    //  loyalty?:  "X" | number,
    //  defense?:  number,
    //
    //  ColorIndicator?:           color_indicator_color[],
    //  AdditionalColorIndicator?: color_indicator_color[],
    //
    pub melds_into: Option<NameString>,
    //
    //  Flip?: { Unflipped: sourced_rule[],
    //           Flipped:   sourced_rule[] },
    //  Doors?: { Left:  { Name: string, ManaCost: card_manacost, ColorIndicator?: color_indicator_color[], Rules: sourced_rule[] },
    //            Right: { Name: string, ManaCost: card_manacost, ColorIndicator?: color_indicator_color[], Rules: sourced_rule[] }, }
    pub adventure: Option<Box<NormalObject>>,
    pub omen: Option<Box<NormalObject>>,
    pub prepared: Option<Box<NormalObject>>,
}

/*
// Subset of normal_object
type facedown_properties =
| { _OracleCard:          "Card" ,
    Name?:              { _Name:     "Name",     Name:     string},
    Typeline:           { _Typeline: "Typeline", Typeline: typeline},
    CardPT?:            { _CardPT:   "CardPT",   CardPT?:  card_pt},
    Rules:              sourced_rule[],
    ColorIndicator?:    color_indicator_color[],
  }

// Subset of normal_boject
type mutate_stack_object =
| { _OracleCard:          "Card" ,
    Name?:              { _Name:     "Name",     Name:     string   },
    Typeline:           { _Typeline: "Typeline", Typeline: typeline },
    Rules:              sourced_rule[],
    ManaCost?:          card_manacost,
    CardPT?:            { _CardPT:   "CardPT",   CardPT?:   card_pt  },
  }


*/
