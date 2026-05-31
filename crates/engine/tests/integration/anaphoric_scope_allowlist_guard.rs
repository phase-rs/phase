//! Categorical freeze guard for the runtime `ObjectScope::Anaphoric` leak set.
//!
//! ## Background — issue #495 (Rite of Consumption)
//!
//! Issue #495 introduced `ObjectScope::Anaphoric` to disambiguate an anaphoric
//! "its" (a parse-time reference whose antecedent is a trigger source, a bound
//! trigger subject, or a spell's `Target`) from an explicit cost-paid
//! possessive ("the sacrificed creature's power"). Before `Anaphoric` existed,
//! the subject-injection rewrite in the effect parser would clobber a
//! correctly-scoped possessive, which is the root cause of Rite of Consumption
//! dealing no damage.
//!
//! After the #495 fix and the bare-anaphoric-possessive classifier fix (Yuriko,
//! the Tiger's Shadow / Dark Confidant class — `classify_possessive_referent`
//! in `parser/oracle_quantity.rs`), exactly **244** cards in the exported card
//! data retain a runtime `ObjectScope::Anaphoric` in a `DealDamage` /
//! `GainLife` / `LoseLife` (or similar) amount. This test holds that set as a
//! sorted constant and fails if a card leaks in or out of it — a tripwire,
//! not a snapshot.
//!
//! ## The four categories of retained `Anaphoric`
//!
//! 1. **Triggered-ability source anaphora** — e.g. *Conclave Mentor*. The "its"
//!    in the ability text refers to the trigger source `~` (the permanent with
//!    the triggered ability). This is correct: the antecedent genuinely is the
//!    source object, and `Anaphoric` resolves to it identically to how
//!    `CostPaidObject` would, so behavior is unchanged. This category is
//!    correctly parsed.
//!
//! 2. **Trigger-subject anaphora** — e.g. *Warstorm Surge* ("it deals damage
//!    equal to its power"). The "its" refers to the trigger's bound "it" (the
//!    creature that entered / attacked / etc.), not the trigger source. The
//!    parser currently scopes this to `Anaphoric` rather than the bound trigger
//!    subject. This is a *genuine pre-existing misparse* — it happens to
//!    resolve correctly today only because the source and the bound subject
//!    coincide for the common cases, but the scope is semantically wrong.
//!
//! 3. **Target-creature spell anaphora** — e.g. *Chandra's Ignition* ("...
//!    equal to its power", where "its" = the `Target` creature). The "its"
//!    refers to the spell's chosen `Target`, not a source or trigger subject.
//!    This is also a *genuine pre-existing misparse*: the referent should be
//!    the target slot, not an anaphoric source marker.
//!
//! 4. **Bare anaphoric possessive (CR 608.2c reveal/move/effect-sacrifice
//!    class — Yuriko, the Tiger's Shadow / Dark Confidant / Mana Drain /
//!    Calibrated Blast / Reanimate / Vendetta / etc.)** — e.g. "...reveal
//!    the top card of your library... loses life equal to that card's mana
//!    value" or "counter target spell... add an amount of mana equal to
//!    that spell's mana value". The bare "that <type>" / "the <type>"
//!    possessive prefix anchors to the object introduced by an earlier
//!    instruction in the same ability. `classify_possessive_referent`
//!    selects `ObjectScope::Anaphoric` so the runtime consults
//!    `effect_context_object` first (CR 608.2c instruction-order referent)
//!    rather than the cost-paid object or the trigger source. The 88
//!    additions break down by anaphor source:
//!    - **reveal-then-act** (`RevealTop` → instruction reads "that card") —
//!      Yuriko, Dark Confidant (already category 4 by its pronoun form),
//!      Calibrated Blast, Erratic Explosion, Explosive Revelation, Riddle
//!      of Lightning, Sin Prodder, Pain Seer, Ruin Raider, etc.
//!    - **counter-then-act** (`Counter` → instruction reads "that spell") —
//!      Mana Drain (delayed mana refund), Overwhelming Intellect, Refuse,
//!      Scattering Stroke.
//!    - **effect-sacrifice-then-act** (sub-`Sacrifice` → instruction reads
//!      "that creature") — Twisted Justice, Tribute to Hunger, Devour
//!      Flesh, Vendetta, Devour in Shadow, Greven, Predator Captain.
//!    - **reanimate-then-act** (`ChangeZone` graveyard → battlefield, then
//!      reads "that creature") — Reanimate, Daxos of Meletis.
//!    - **mill/discover/explosion chains** with the same "earlier-effect
//!      object" anaphor shape.
//!
//!    This category went from misparsed (`CostPaidObject`, silently reading
//!    the trigger source — Yuriko's bug) to correct (`Anaphoric`, slot-1
//!    `effect_context_object` → revealed/moved/sacrificed object). Each
//!    subclass relies on the corresponding source in
//!    `parent_referent_context_from_events` (`game/effects/mod.rs:602`)
//!    being populated, and on `snapshot_quantity_ref`
//!    (`game/effects/delayed_trigger.rs:331`) including `Anaphoric` in its
//!    snapshot-baking match arm (added in lockstep with this categorization).
//!
//! ## Behavior-neutrality proof (categories 1-3) and intentional behavior
//! change (category 4)
//!
//! The original 156 entries (categories 1-3) parsed as `CostPaidObject`
//! *before* `ObjectScope::Anaphoric` existed — verifiable with
//! `git show <pre-#495>:crates/engine/src/parser/oracle_quantity.rs`. Issue
//! #495's runtime resolution arm (`game/quantity.rs`, `object_for_scope` /
//! `resolve_object_pt` / `resolve_object_mana_value`) resolved `Anaphoric`
//! *identically* to `CostPaidObject` at the time. Therefore #495 was a
//! behavior-preserving relabel for those 156, and a correctness fix for Rite.
//!
//! After Dark Confidant (#511) added the
//! `effect_context_object`-first slot priority to `Anaphoric`'s runtime arm
//! (see `resolve_object_mana_value`), the bare-anaphoric-possessive parser
//! fix (Yuriko, the Tiger's Shadow) routes the category-4 cards (88 entries,
//! including Yuriko itself) onto that already-extended arm. For those
//! cards the change is an *intentional* behavior fix: the runtime now reads
//! the revealed / countered / moved object first, matching CR 608.2c. The
//! previous `CostPaidObject` parse silently fell through to the trigger
//! source (Yuriko's Ninja, the casting spell, etc.) and produced the wrong
//! amount.
//!
//! ## Why this guard exists
//!
//! Categories 2 and 3 are genuine parser misparses. They are pre-existing
//! (not introduced by #495) and are tracked separately:
//!
//! - **#512** — categories 2 & 3: scope trigger-subject / target-creature
//!   anaphora to the correct referent instead of `Anaphoric`.
//! - **#511** — the bare-pronoun reveal-referent variant (*Dark Confidant*
//!   — "its mana value", where "its" = the revealed card).
//!
//! Category 4 is the explicit-possessive sibling of #511 — same antecedent
//! shape, just with an explicit type word ("that card's mana value") instead
//! of the pronoun ("its mana value"). Yuriko, the Tiger's Shadow surfaced the
//! same bug as Dark Confidant once #511 fixed the pronoun branch.
//!
//! This test **freezes** the `Anaphoric` set so it cannot grow silently while
//! #512 does the remaining category-2/3 fixes. A new leak (a new card name,
//! or a count change) fails this test; a human then decides whether it is a
//! legitimate new category-1/2/3/4 case (add it here) or a real regression
//! (fix the parser). The curation lives at the *category* level — the
//! correct granularity — not as 244 per-card annotations.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;

/// Cards whose exported card data retains a runtime `ObjectScope::Anaphoric`.
///
/// Sorted by the export's normalized (lowercase) card key. See the module doc
/// comment for the four categories and the behavior-neutrality proof. Do not
/// edit this list to silence a failure without first classifying the new card:
/// a legitimate category-1/2/3/4 case may be added; a real regression must be
/// fixed in the parser instead.
const ANAPHORIC_SCOPE_CARDS: &[&str] = &[
    "abattoir ghoul",
    "ad nauseam",
    "alchemist's talent",
    "alpha brawl",
    "ambuscade",
    "angelic chorus",
    "archdruid's charm",
    "archon of redemption",
    "assert perfection",
    "augury adept",
    "avatar destiny",
    "backlash",
    "baneful omen",
    "banewasp affliction",
    "bartz and boko",
    "be'lakor, the dark master",
    "beastie beatdown",
    "bite down on crime",
    "blood poet",
    "bottle golems",
    "boulderbranch golem",
    "bounteous kirin",
    "brainstealer dragon",
    "breeches, the blastmaker",
    "brightmare",
    "brokers charm",
    "calibrated blast",
    "captain ripley vance",
    "champion of the path",
    "champion of wits",
    "chastise",
    "circus of the sun",
    "clear shot",
    "cleric class",
    "common black removal",
    "conclave mentor",
    "consume",
    "consuming ferocity",
    "consuming vapors",
    "creature bond",
    "crumble",
    "crush underfoot",
    "dark confidant",
    "dark tutelage",
    "darkstar augur",
    "daxos of meletis",
    "deadshot",
    "death",
    "death watch",
    "death's caress",
    "delif's cone",
    "delirium",
    "devour flesh",
    "devour in shadow",
    "diplomatic relations",
    "dire tactics",
    "divine offering",
    "domri's ambush",
    "doomgape",
    "durkwood tracker",
    "duskmantle seer",
    "efteekay, flame of the kav",
    "electrosiphon",
    "electryte",
    "energy tap",
    "engulfing slagwurm",
    "erratic explosion",
    "exile",
    "explosive revelation",
    "feed the swarm",
    "felling blow",
    "feral encounter",
    "fiendlash",
    "fiery encore",
    "flamethrower sonata",
    "foot chopper",
    "gargantuan gorilla",
    "garruk relentless",
    "garruk, apex predator",
    "gaze of pain",
    "ghastly death tyrant",
    "goblin crash pilot",
    "goblin sleigh ride",
    "goblin tinkerer",
    "golbez, crystal collector",
    "gregor, shrewd magistrate",
    "greven, predator captain",
    "grim contest",
    "grim feast",
    "grisly spectacle",
    "heal the scars",
    "healing technique",
    "hellhole rats",
    "hidetsugu and kairi",
    "hit",
    "horrid shadowspinner",
    "hotel of fears",
    "huatli's final strike",
    "hunter's edge",
    "hunter's mark",
    "ian the reckless",
    "ignite memories",
    "ikra shidiqi, the usurper",
    "immersturm",
    "imp's mischief",
    "infernal reckoning",
    "interpret the signs",
    "jenova, ancient calamity",
    "judge unworthy",
    "judgment of alexander",
    "kaervek the merciless",
    "kamahl's will",
    "karplusan yeti",
    "keeper of secrets",
    "kefka, dancing mad",
    "kindle the carnage",
    "knockout maneuver",
    "laccolith rig",
    "lagonna-band storyteller",
    "lammastide weave",
    "lifeblood hydra",
    "living inferno",
    "lorcan, warlock collector",
    "lothlórien blade",
    "lozhan, dragons' legacy",
    "lukka, coppercoat outcast",
    "lukka, wayward bonder",
    "luminate primordial",
    "madame null, power broker",
    "mage slayer",
    "make yourself useful",
    "mana drain",
    "marshland bloodcaster",
    "master of the wild hunt",
    "mirkwood elk",
    "momentous fall",
    "moonlight hunt",
    "mortis dogs",
    "narset of the ancient way",
    "nature's way",
    "neerdiv, devious diver",
    "niambi, esteemed speaker",
    "nibelheim aflame",
    "nissa's judgment",
    "nissa's revelation",
    "noxious gearhulk",
    "orchard warden",
    "orim's thunder",
    "orzhov charm",
    "osseous sticktwister",
    "overwhelming intellect",
    "packsong pup",
    "pain for all",
    "pain seer",
    "paladin of atonement",
    "pandemonium",
    "parallectric feedback",
    "passionate archaeologist",
    "phthisis",
    "phyrexian delver",
    "planeswalker's fury",
    "planeswalker's mirth",
    "polukranos, world eater",
    "predatory urge",
    "prime speaker zegana",
    "proper burial",
    "protection racket",
    "pyretic rebirth",
    "pyrotechnic performer",
    "queen's bay paladin",
    "rabid gnaw",
    "rage extractor",
    "rapacious guest",
    "rashida scalebane",
    "ravenous gigantotherium",
    "razor hippogriff",
    "reanimate",
    "reanimate [6cb8b8c4-0674-4f14-9d89-010969fbb80e]",
    "refuse",
    "reviving vapors",
    "riddle of lightning",
    "righteous valkyrie",
    "rotfeaster maggot",
    "ruin raider",
    "rupture",
    "sapling of colfenor",
    "sarkhan the mad",
    "scattering stroke",
    "season's beatings",
    "seeds of innocence",
    "seek",
    "selfless exorcist",
    "serene offering",
    "sever soul",
    "sheltering word",
    "sheoldred's restoration",
    "showstopping surprise",
    "shriveling rot",
    "sifter wurm",
    "signature slam",
    "sin prodder",
    "singe-mind ogre",
    "sister hospitaller",
    "solitude",
    "sorin the mirthless",
    "sorin, grim nemesis",
    "south wind avatar",
    "spinal embrace",
    "spirit flare",
    "spoils of the hunt",
    "stalking vengeance",
    "steadfast armasaur",
    "stronghold arena",
    "summon: kujata",
    "sunscourge champion",
    "sylvan smite",
    "syr ginger, the meal ender",
    "tahngarth, talruum hero",
    "tanuki transplanter",
    "terashi's grasp",
    "terminal velocity",
    "teval, arbiter of virtue",
    "teyo, aegis adept",
    "the aesir escape valhalla",
    "the bears of littjara",
    "the creation of avacyn",
    "the great aerie",
    "the lord of pain",
    "the mystery raceway",
    "the provider",
    "the ruinous powers",
    "thorin, mountain-king",
    "thought sponge",
    "thought-string analyst",
    "too greedily, too deep",
    "tracker",
    "traitor's roar",
    "tribute to hunger",
    "trostani, selesnya's voice",
    "twisted justice",
    "undying flames",
    "vanish into memory",
    "vein drinker",
    "vendetta",
    "vengeful rebirth",
    "venom blast",
    "verdant sun's avatar",
    "vial smasher the fierce",
    "viashino heretic",
    "vivien's invocation",
    "volcanic vision",
    "vraska's stoneglare",
    "warstorm surge",
    "weed strangle",
    "willow geist",
    "wolverine riders",
    "yuriko, the tiger's shadow",
];

/// Recursively reports whether a JSON subtree contains an `ObjectScope`
/// `{"type":"Anaphoric"}` node. `Anaphoric` is only ever serialized as an
/// `ObjectScope` variant tag, so a tag match is an exact detector.
fn contains_anaphoric(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            if map.get("type") == Some(&Value::String("Anaphoric".to_string())) {
                return true;
            }
            map.values().any(contains_anaphoric)
        }
        Value::Array(items) => items.iter().any(contains_anaphoric),
        _ => false,
    }
}

#[test]
fn anaphoric_scope_set_is_frozen() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/public/card-data.json");
    if !path.exists() {
        eprintln!("skipping: client/public/card-data.json not generated");
        return;
    }
    let raw = std::fs::read_to_string(&path).expect("export should be readable");
    let cards: Value = serde_json::from_str(&raw).expect("export should be valid JSON");
    let cards = cards.as_object().expect("export root should be an object");

    let observed: BTreeSet<&str> = cards
        .iter()
        .filter(|(_, card)| contains_anaphoric(card))
        .map(|(name, _)| name.as_str())
        .collect();

    let allowed: BTreeSet<&str> = ANAPHORIC_SCOPE_CARDS.iter().copied().collect();

    let leaked: Vec<&str> = observed.difference(&allowed).copied().collect();
    let removed: Vec<&str> = allowed.difference(&observed).copied().collect();

    assert!(
        leaked.is_empty(),
        "New card(s) leaked a runtime ObjectScope::Anaphoric and are not in the \
         frozen allowlist: {leaked:?}. Classify each: a legitimate new \
         category-1/2/3 case (see module doc) should be added to \
         ANAPHORIC_SCOPE_CARDS; a real regression must be fixed in the parser. \
         Categories 2 & 3 are tracked in #512, Dark Confidant's reveal-referent \
         in #511."
    );
    assert!(
        removed.is_empty(),
        "Card(s) in the frozen allowlist no longer retain ObjectScope::Anaphoric: \
         {removed:?}. If #512/#511 fixed the misparse, remove the card(s) from \
         ANAPHORIC_SCOPE_CARDS and update the count assertion."
    );

    // Secondary tripwire: the count itself is pinned. If #512/#511 land,
    // both this and ANAPHORIC_SCOPE_CARDS shrink together.
    assert_eq!(
        observed.len(),
        257,
        "Expected exactly 257 cards retaining ObjectScope::Anaphoric. PR #1451 \
         re-scoped 8 dynamic-quantity 'its power' anaphora off the Anaphoric \
         arm onto typed quantity refs; PR #1522 re-scoped Dead Before Sunrise \
         through the recipient/subject rewrite. Count moved to {}.",
        observed.len()
    );
    assert_eq!(
        ANAPHORIC_SCOPE_CARDS.len(),
        257,
        "ANAPHORIC_SCOPE_CARDS must list exactly 257 cards."
    );
}

/// The allowlist constant must stay sorted so diffs are reviewable and the
/// `BTreeSet` semantics are obvious to a human auditor.
#[test]
fn anaphoric_scope_allowlist_is_sorted_and_unique() {
    let mut sorted = ANAPHORIC_SCOPE_CARDS.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.as_slice(),
        ANAPHORIC_SCOPE_CARDS,
        "ANAPHORIC_SCOPE_CARDS must be sorted and free of duplicates."
    );
}
