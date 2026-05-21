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
//! After the #495 fix, exactly **160** cards in the exported card data retain a
//! runtime `ObjectScope::Anaphoric` in a `DealDamage` / `GainLife` / `LoseLife`
//! (or similar) amount. This test holds that set as a sorted constant and
//! fails if a card leaks in or out of it — a tripwire, not a snapshot.
//!
//! ## The three categories of retained `Anaphoric`
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
//! ## Behavior-neutrality proof
//!
//! Every one of the 160 cards below parsed as `CostPaidObject` *before*
//! `ObjectScope::Anaphoric` existed — verifiable with
//! `git show HEAD:crates/engine/src/parser/oracle_quantity.rs` against the
//! pre-#495 commit. Issue #495's runtime resolution arm (`game/quantity.rs`,
//! `object_for_scope` / `resolve_object_pt` / `resolve_object_mana_value`)
//! resolves `Anaphoric` *identically* to `CostPaidObject`. Therefore #495
//! changes the runtime behavior of **zero** cards except Rite of Consumption
//! itself — `Anaphoric` is a behavior-preserving relabel for these 160, and a
//! correctness fix for Rite.
//!
//! ## Why this guard exists
//!
//! Categories 2 and 3 are genuine parser misparses. They are pre-existing
//! (not introduced by #495) and are tracked separately:
//!
//! - **#512** — categories 2 & 3: scope trigger-subject / target-creature
//!   anaphora to the correct referent instead of `Anaphoric`.
//! - **#511** — the reveal-referent variant (*Dark Confidant* — "its mana
//!   value", where "its" = the revealed card).
//!
//! This test **freezes** the `Anaphoric` set so it cannot grow silently while
//! #512 / #511 do the real fixes. A new leak (a new card name, or a count
//! change) fails this test; a human then decides whether it is a legitimate
//! new category-1/2/3 case (add it here) or a real regression (fix the parser).
//! The curation lives at the *category* level — the correct granularity — not
//! as 160 per-card annotations.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;

/// Cards whose exported card data retains a runtime `ObjectScope::Anaphoric`.
///
/// Sorted by the export's normalized (lowercase) card key. See the module doc
/// comment for the three categories and the behavior-neutrality proof. Do not
/// edit this list to silence a failure without first classifying the new card:
/// a legitimate category-1/2/3 case may be added; a real regression must be
/// fixed in the parser instead.
const ANAPHORIC_SCOPE_CARDS: &[&str] = &[
    "a-heartfire hero",
    "ad nauseam",
    "alpha brawl",
    "angelic chorus",
    "aspiring champion",
    "augury adept",
    "avatar destiny",
    "backlash",
    "banewasp affliction",
    "bartz and boko",
    "be'lakor, the dark master",
    "beastie beatdown",
    "blood poet",
    "bottle golems",
    "boulderbranch golem",
    "brainstealer dragon",
    "champion of the path",
    "champion of wits",
    "chastise",
    "circus of the sun",
    "common black removal",
    "conclave mentor",
    "consume",
    "consuming ferocity",
    "crumble",
    "dark confidant",
    "dark tutelage",
    "darkstar augur",
    "dead before sunrise",
    "death",
    "death watch",
    "death's caress",
    "delif's cone",
    "delirium",
    "divine offering",
    "domri's ambush",
    "durkwood tracker",
    "efteekay, flame of the kav",
    "electrosiphon",
    "electryte",
    "evereth, viceroy of plunder",
    "exile",
    "felling blow",
    "feral encounter",
    "fiendlash",
    "flaming tyrannosaurus",
    "foot chopper",
    "gargantuan gorilla",
    "garruk relentless",
    "garruk, apex predator",
    "gau, feral youth",
    "gaze of pain",
    "ghastly death tyrant",
    "giggling skitterspike",
    "goblin crash pilot",
    "goblin sleigh ride",
    "goblin tinkerer",
    "gregor, shrewd magistrate",
    "grim contest",
    "grim feast",
    "heartfire hero",
    "hidetsugu and kairi",
    "horrid shadowspinner",
    "hotel of fears",
    "hunter's edge",
    "ian the reckless",
    "immersturm",
    "infernal reckoning",
    "jenova, ancient calamity",
    "judgment of alexander",
    "kamahl's will",
    "karplusan yeti",
    "kefka, dancing mad",
    "laccolith rig",
    "lagonna-band storyteller",
    "lammastide weave",
    "lifeblood hydra",
    "living inferno",
    "lorcan, warlock collector",
    "lothlórien blade",
    "lukka, coppercoat outcast",
    "lukka, wayward bonder",
    "luminate primordial",
    "madame null, power broker",
    "mage slayer",
    "make yourself useful",
    "master of the wild hunt",
    "momentous fall",
    "moonlight hunt",
    "mortis dogs",
    "neerdiv, devious diver",
    "nibelheim aflame",
    "nissa's judgment",
    "nissa's revelation",
    "noxious gearhulk",
    "orzhov charm",
    "osseous sticktwister",
    "packsong pup",
    "pain for all",
    "paladin of atonement",
    "pandemonium",
    "polukranos, world eater",
    "predatory urge",
    "prime speaker zegana",
    "pyrotechnic performer",
    "queen's bay paladin",
    "rapacious guest",
    "rashida scalebane",
    "ravenous gigantotherium",
    "sapling of colfenor",
    "sarkhan the mad",
    "season's beatings",
    "seeds of innocence",
    "seek",
    "selfless exorcist",
    "serene offering",
    "sever soul",
    "showstopping surprise",
    "shriveling rot",
    "signature slam",
    "sister hospitaller",
    "solitude",
    "sorin the mirthless",
    "sorin, grim nemesis",
    "south wind avatar",
    "spinal embrace",
    "spoils of the hunt",
    "stalking vengeance",
    "steadfast armasaur",
    "stronghold arena",
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
    "the mystery raceway",
    "the ruinous powers",
    "thorin, mountain-king",
    "thought sponge",
    "thought-string analyst",
    "too greedily, too deep",
    "tracker",
    "vein drinker",
    "vivien's invocation",
    "vraska's stoneglare",
    "warstorm surge",
    "willow geist",
    "wolverine riders",
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
        156,
        "Expected exactly 156 cards retaining ObjectScope::Anaphoric (the #495 \
         behavior-neutral floor, minus four cards unlocked by #607's target-subject \
         DamageAll source wrapper: Betrayal at the Vault, Chandra's Ignition, \
         Spinning Wheel Kick, Waltz of Rage); count moved to {}.",
        observed.len()
    );
    assert_eq!(
        ANAPHORIC_SCOPE_CARDS.len(),
        156,
        "ANAPHORIC_SCOPE_CARDS must list exactly 156 cards."
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
