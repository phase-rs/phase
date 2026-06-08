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
//! After the #495 fix and the bare-possessive classifier fix (Yuriko, the
//! Tiger's Shadow / Dark Confidant class — `classify_possessive_referent` in
//! `parser/oracle_quantity.rs`), the retained anaphora referents split across
//! two `ObjectScope` variants:
//!
//! - **153** cards retain `ObjectScope::Anaphoric` — the *pronoun* "its"
//!   (categories 1-3 below), which the subject-injection rewrite may rebind.
//! - **111** cards retain `ObjectScope::Demonstrative` — the bare *demonstrative
//!   / definite* possessive ("that creature's toughness", "that card's mana
//!   value"; category 4 below), whose antecedent is fixed by the Oracle text and
//!   must never be rebound.
//!
//! The split is what fixed the Steadfast Armasaur LKI-toughness bug: "its
//! toughness" (pronoun → `Anaphoric` → rebound to `Source`, so it LKI-falls-back
//! to 3 when the source is destroyed in response) and "that creature's toughness"
//! (demonstrative → `Demonstrative`, never rebound) parse to the same
//! `QuantityRef::Toughness` property and previously collapsed to one scope, so
//! generalizing the Power-only rebind to toughness regressed Creature Bond.
//! Splitting the variant let the rebind cover every per-object characteristic
//! while touching only the genuine pronoun. This test holds each set as a sorted
//! constant and fails if a card leaks in or out of either — a tripwire, not a
//! snapshot.
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
//! 4. **Bare demonstrative possessive (CR 608.2c reveal/move/effect-sacrifice
//!    class — Yuriko, the Tiger's Shadow / Dark Confidant / Mana Drain /
//!    Calibrated Blast / Reanimate / Vendetta / etc.)** — e.g. "...reveal
//!    the top card of your library... loses life equal to that card's mana
//!    value" or "counter target spell... add an amount of mana equal to
//!    that spell's mana value". The bare "that <type>" / "the <type>"
//!    possessive prefix anchors to the object introduced by an earlier
//!    instruction in the same ability. `classify_possessive_referent`
//!    selects `ObjectScope::Demonstrative` (tracked by
//!    `DEMONSTRATIVE_SCOPE_CARDS`, NOT this `Anaphoric` set) so the runtime
//!    consults `effect_context_object` first (CR 608.2c instruction-order
//!    referent) rather than the cost-paid object or the trigger source — while
//!    the dedicated variant keeps the subject-injection rewrite from rebinding
//!    the fixed antecedent. The 88
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
//! This test **freezes** both the `Anaphoric` (pronoun) and `Demonstrative`
//! (noun-phrase) sets so neither can grow silently while #512 does the
//! remaining category-2/3 fixes. A new leak (a new card name, or a count
//! change) fails this test; a human then decides whether it is a legitimate
//! new case (add it to the matching list) or a real regression (fix the
//! parser). The curation lives at the *category* level — the correct
//! granularity — not as per-card annotations.

use std::collections::BTreeSet;

use serde_json::Value;

/// Cards whose exported card data retains a runtime `ObjectScope::Anaphoric`.
///
/// Sorted by the export's normalized (lowercase) card key. See the module doc
/// comment for the four categories and the behavior-neutrality proof. Do not
/// edit this list to silence a failure without first classifying the new card:
/// a legitimate category-1/2/3/4 case may be added; a real regression must be
/// fixed in the parser instead.
const ANAPHORIC_SCOPE_CARDS: &[&str] = &[
    "ad nauseam",
    "alpha brawl",
    "ambuscade",
    "angelic chorus",
    "aradesh, the founder",
    "archdruid's charm",
    "argivian cavalier",
    "assert perfection",
    "augury adept",
    "avatar destiny",
    "backlash",
    "balduvian berserker",
    "banewasp affliction",
    "barkweave crusher",
    "bartz and boko",
    "beastie beatdown",
    "benalish faithbonder",
    "benalish knight-counselor",
    "bite down on crime",
    "blood poet",
    "bottle golems",
    "boulderbranch golem",
    "brainstealer dragon",
    "brokers charm",
    "champion of the path",
    "champion of wits",
    "chastise",
    "circus of the sun",
    "clear shot",
    "coalition skyknight",
    "coalition warbrute",
    "common black removal",
    "conclave mentor",
    "consume",
    "consuming ferocity",
    "crumble",
    "crush underfoot",
    "dark confidant",
    "dark tutelage",
    "darkstar augur",
    "deadshot",
    "death",
    "death watch",
    "death's caress",
    "delif's cone",
    "delirium",
    "diplomatic relations",
    "divine offering",
    "domri's ambush",
    "durkwood tracker",
    "effie, fast learner",
    "electrosiphon",
    "electryte",
    "exile",
    "felling blow",
    "feral encounter",
    "foot chopper",
    "gargantuan gorilla",
    "garruk relentless",
    "garruk, apex predator",
    "gaze of pain",
    "ghastly death tyrant",
    "goblin crash pilot",
    "goblin morale sergeant",
    "goblin sleigh ride",
    "goblin tinkerer",
    "gregor, shrewd magistrate",
    "grim contest",
    "grim feast",
    "guardian of new benalia",
    "hexbane tortoise",
    "hidetsugu and kairi",
    "horrid shadowspinner",
    "hotel of fears",
    "huatli's final strike",
    "hunter's edge",
    "hunter's mark",
    "infernal reckoning",
    "jenova, ancient calamity",
    "judgment of alexander",
    "kamahl's will",
    "karplusan yeti",
    "kefka, dancing mad",
    "keldon flamesage",
    "knockout maneuver",
    "lagonna-band storyteller",
    "lammastide weave",
    "lifeblood hydra",
    "linebreaker baloth",
    "living inferno",
    "lorcan, warlock collector",
    "lukka, coppercoat outcast",
    "luminate primordial",
    "madame null, power broker",
    "make yourself useful",
    "master of the wild hunt",
    "momentous fall",
    "moonlight hunt",
    "mortis dogs",
    "nature's way",
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
    "phthisis",
    "polukranos, world eater",
    "predatory urge",
    "prime speaker zegana",
    "pyrotechnic performer",
    "queen's bay paladin",
    "rabid gnaw",
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
    "spirit flare",
    "spoils of the hunt",
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
    "traitor's roar",
    "vein drinker",
    "venom blast",
    "vivien's invocation",
    "vraska's stoneglare",
    "willow geist",
    "wolverine riders",
    "yavimaya steelcrusher",
];

/// Cards whose exported card data retains a runtime `ObjectScope::Demonstrative`
/// (the CR 608.2c bare demonstrative / definite possessive back-reference —
/// "that creature's toughness", "that card's mana value"). Split out of the
/// former `Anaphoric` set so the subject-injection rewrite can rebind the
/// pronoun "its" without clobbering these fixed antecedents. Frozen for the
/// same anti-silent-drift reason as [`ANAPHORIC_SCOPE_CARDS`]; sorted by the
/// export's normalized (lowercase) card key.
const DEMONSTRATIVE_SCOPE_CARDS: &[&str] = &[
    "abattoir ghoul",
    "agonizing demise",
    "alchemist's talent",
    "archon of redemption",
    "artifact mutation",
    "aura mutation",
    "baneful omen",
    "boros fury-shield",
    "bounteous kirin",
    "breeches, the blastmaker",
    "brightmare",
    "calibrated blast",
    "cinder cloud",
    "cleric class",
    "consuming vapors",
    "cragganwick cremator",
    "creature bond",
    "daxos of meletis",
    "dead reckoning",
    "devour flesh",
    "devour in shadow",
    "dire tactics",
    "doomgape",
    "dovescape",
    "duskmantle seer",
    "energy tap",
    "engulfing slagwurm",
    "erratic explosion",
    "essence backlash",
    "explosive revelation",
    "feed the swarm",
    "fiery encore",
    "flamethrower sonata",
    "golbez, crystal collector",
    "grab the reins",
    "greven, predator captain",
    "grisly spectacle",
    "heal the scars",
    "healing technique",
    "heart-piercer manticore",
    "hellhole rats",
    "hit",
    "hoard-smelter dragon",
    "ignite memories",
    "ikra shidiqi, the usurper",
    "imp's mischief",
    "induce paranoia",
    "interpret the signs",
    "judge unworthy",
    "kaervek the merciless",
    "kaervek's purge",
    "keeper of secrets",
    "kindle the carnage",
    "lie in wait",
    "lozhan, dragons' legacy",
    "mana drain",
    "marshland bloodcaster",
    "mirkwood elk",
    "narset of the ancient way",
    "niambi, esteemed speaker",
    "orchard warden",
    "orim's thunder",
    "overwhelming intellect",
    "pain seer",
    "parallectric feedback",
    "passionate archaeologist",
    "phyrexian delver",
    "planeswalker's fury",
    "planeswalker's mirth",
    "proper burial",
    "protection racket",
    "pyretic rebirth",
    "rage extractor",
    "rakdos joins up",
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
    "sheltering word",
    "sheoldred's restoration",
    "sifter wurm",
    "sin prodder",
    "singe-mind ogre",
    "summon: kujata",
    "terror of the peaks",
    "the lord of pain",
    "the provider",
    "tribute to hunger",
    "trostani, selesnya's voice",
    "twisted justice",
    "undying flames",
    "unnatural hunger",
    "vanish into memory",
    "vendetta",
    "vengeful rebirth",
    "verdant sun's avatar",
    "vial smasher the fierce",
    "viashino heretic",
    "volcanic vision",
    "weed strangle",
    "yuriko, the tiger's shadow",
    "ziatora, the incinerator",
];

/// Recursively reports whether a JSON subtree contains an `ObjectScope`
/// `{"type":<tag>}` node. `Anaphoric` / `Demonstrative` are only ever
/// serialized as `ObjectScope` variant tags, so a tag match is an exact
/// detector.
fn contains_scope_tag(value: &Value, tag: &str) -> bool {
    match value {
        Value::Object(map) => {
            if map.get("type") == Some(&Value::String(tag.to_string())) {
                return true;
            }
            map.values().any(|v| contains_scope_tag(v, tag))
        }
        Value::Array(items) => items.iter().any(|v| contains_scope_tag(v, tag)),
        _ => false,
    }
}

/// Collect the set of card keys whose exported data retains the given
/// `ObjectScope` tag. Returns `None` when the export has not been generated
/// (CI without card data), so callers can self-skip.
fn observed_scope_set<'a>(
    cards: &'a serde_json::Map<String, Value>,
    tag: &str,
) -> BTreeSet<&'a str> {
    cards
        .iter()
        .filter(|(_, card)| contains_scope_tag(card, tag))
        .map(|(name, _)| name.as_str())
        .collect()
}

#[test]
fn anaphoric_scope_set_is_frozen() {
    let Some(cards) = crate::support::shared_card_export_json() else {
        eprintln!("skipping: client/public/card-data.json not generated");
        return;
    };

    let observed = observed_scope_set(cards, "Anaphoric");
    let allowed: BTreeSet<&str> = ANAPHORIC_SCOPE_CARDS.iter().copied().collect();

    let leaked: Vec<&str> = observed.difference(&allowed).copied().collect();
    let removed: Vec<&str> = allowed.difference(&observed).copied().collect();

    assert!(
        leaked.is_empty(),
        "New card(s) leaked a runtime ObjectScope::Anaphoric and are not in the \
         frozen allowlist: {leaked:?}. Classify each: a legitimate new \
         category-1/2/3 pronoun case (see module doc) should be added to \
         ANAPHORIC_SCOPE_CARDS; a bare demonstrative ('that creature's …') \
         belongs in DEMONSTRATIVE_SCOPE_CARDS; a real regression must be fixed \
         in the parser. Categories 2 & 3 are tracked in #512, Dark Confidant's \
         reveal-referent in #511."
    );
    assert!(
        removed.is_empty(),
        "Card(s) in the frozen allowlist no longer retain ObjectScope::Anaphoric: \
         {removed:?}. If #512/#511 fixed the misparse — or the card was a \
         demonstrative that moved to DEMONSTRATIVE_SCOPE_CARDS — remove the \
         card(s) from ANAPHORIC_SCOPE_CARDS and update the count assertion."
    );

    // Secondary tripwire: the count itself is pinned. Splitting the bare
    // demonstrative possessives onto `ObjectScope::Demonstrative` (so the
    // subject-injection rewrite can rebind the pronoun "its" without clobbering
    // them) moved 95 cards from this set into DEMONSTRATIVE_SCOPE_CARDS, and
    // Steadfast Armasaur's "its toughness" rebound to `Source` (the LKI-toughness
    // fix), taking the count 252 -> 156; the Optional_YouMay capture fix
    // (#2277) then dropped "ian the reckless" to 155. The "may have" causative
    // optional fix (#2313) restructured the optional sub-effect of Pandemonium /
    // Immersturm ("...may have it deal damage equal to its power..."), letting the
    // anaphoric rebind resolve "its power" to `EventSource` (the entering
    // creature, CR 603.2) — the category-2 trigger-subject fix #512 anticipated —
    // dropping both to 153. Enlist keyword synthesis then surfaced the tapped
    // creature's power anaphor for 15 Enlist cards, taking the count to 168. If
    // #512/#511 land, this shrinks further.
    assert_eq!(
        observed.len(),
        168,
        "Expected exactly 168 cards retaining ObjectScope::Anaphoric (pronoun \
         'its' antecedents). Count moved to {}.",
        observed.len()
    );
    assert_eq!(
        ANAPHORIC_SCOPE_CARDS.len(),
        168,
        "ANAPHORIC_SCOPE_CARDS must list exactly 168 cards."
    );
}

/// Companion tripwire for the bare demonstrative possessive class
/// (`ObjectScope::Demonstrative`, CR 608.2c). Split out of the former
/// `Anaphoric` set so the subject-injection rewrite never rebinds these fixed
/// antecedents (Creature Bond, Erratic Explosion, Mana Drain, Yuriko, …).
/// Freezes the set for the same anti-silent-drift reason as the `Anaphoric`
/// guard: a new leak or count change forces a human classification.
#[test]
fn demonstrative_scope_set_is_frozen() {
    let Some(cards) = crate::support::shared_card_export_json() else {
        eprintln!("skipping: client/public/card-data.json not generated");
        return;
    };

    let observed = observed_scope_set(cards, "Demonstrative");
    let allowed: BTreeSet<&str> = DEMONSTRATIVE_SCOPE_CARDS.iter().copied().collect();

    let leaked: Vec<&str> = observed.difference(&allowed).copied().collect();
    let removed: Vec<&str> = allowed.difference(&observed).copied().collect();

    assert!(
        leaked.is_empty(),
        "New card(s) leaked a runtime ObjectScope::Demonstrative and are not in \
         DEMONSTRATIVE_SCOPE_CARDS: {leaked:?}. A bare 'that <type>'s …' / \
         'the <type>'s …' possessive is the expected source; add it, or fix a \
         real parser regression."
    );
    assert!(
        removed.is_empty(),
        "Card(s) in DEMONSTRATIVE_SCOPE_CARDS no longer retain \
         ObjectScope::Demonstrative: {removed:?}. Remove the card(s) and update \
         the count assertion."
    );
    assert_eq!(
        observed.len(),
        111,
        "Expected exactly 111 cards retaining ObjectScope::Demonstrative. Count \
         moved to {}.",
        observed.len()
    );
    assert_eq!(
        DEMONSTRATIVE_SCOPE_CARDS.len(),
        111,
        "DEMONSTRATIVE_SCOPE_CARDS must list exactly 111 cards."
    );
}

/// The allowlist constants must stay sorted so diffs are reviewable and the
/// `BTreeSet` semantics are obvious to a human auditor.
#[test]
fn anaphoric_scope_allowlist_is_sorted_and_unique() {
    for (label, list) in [
        ("ANAPHORIC_SCOPE_CARDS", ANAPHORIC_SCOPE_CARDS),
        ("DEMONSTRATIVE_SCOPE_CARDS", DEMONSTRATIVE_SCOPE_CARDS),
    ] {
        let mut sorted = list.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.as_slice(),
            list,
            "{label} must be sorted and free of duplicates."
        );
    }
}
