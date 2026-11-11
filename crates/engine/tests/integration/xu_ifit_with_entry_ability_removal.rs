//! CR 611.2e + CR 613.1f: a continuous effect created by the same resolution that puts a
//! permanent onto the battlefield, worded in "is/has" form and removing all of its abilities
//! (Xu-Ifit, Osteoharmonist: "It's a Skeleton in addition to its other types and has no
//! abilities."), applies simultaneously with the entry. The entrant therefore never has its own
//! "when this enters" trigger (CR 603.2 + CR 603.10: the trigger is collected from the entrant
//! with the effect already applied), and its later-granted abilities are kept while earlier
//! static grants are removed (CR 613.7a + CR 613.7b, layer 6).
//!
//! Rulings (2025-07-25): the returned creature "will lose that ability before it can trigger";
//! a creature that "gains an ability after being returned ... will keep that ability".
//!
//! The class also includes every enter trigger of an entry being checked after the resolution's
//! simultaneous "is/has" effect exists (CR 603.10 + CR 603.6a + CR 611.2e): the owner of a
//! completed `ChangeZone` / `ChangeZoneAll` hands its entries to the post-action collector while
//! a later instruction of the same resolution may still apply such an effect (R4, R4d, R8, R9).
//!
//! Consequence (b) rows (entry-modifying abilities such as replacement effects) land in phase 2.
//!
//! REVERT DISCRIMINATORS:
//! - Reverting the collection hand-off (making `change_zone::resolve` call the `Owner` delegate)
//!   halts R4 at `UnlessPayment` and leaves R8 at P0 +1, P1 0.
//! - Removing the `parse_has_no_abilities` arm in
//!   `oracle_static::grammar::parse_loss_enumeration` halts R4 at `UnlessPayment` (the
//!   Dreadnought's ETB triggers) and keeps Haste and Trample in R5.
//! - Dropping handed-off entries instead of handing them on fails R4d and R8.
//! - Reverting only the `resolve_all` call site to the `Owner` delegate leaves R9 at P0 +1.

use engine::game::layers::flush_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Xu-Ifit, Osteoharmonist {1}{B}{B} Legendary Creature — Human Wizard 2/3 — verbatim.
const XU_IFIT: &str = "{T}: Return target creature card from your graveyard to the battlefield. It's a Skeleton in addition to its other types and has no abilities. Activate only as a sorcery.";
/// Phyrexian Dreadnought {1} Artifact Creature — Phyrexian Dreadnought 12/12 — verbatim.
const DREADNOUGHT: &str = "Trample\nWhen this creature enters, sacrifice it unless you sacrifice any number of creatures with total power 12 or greater.";
/// Soul Warden {W} Creature — Human Cleric 1/1 — verbatim.
const SOUL_WARDEN: &str = "Whenever another creature enters, you gain 1 life.";
/// Zombify {3}{B} Sorcery — verbatim.
const ZOMBIFY: &str = "Return target creature card from your graveyard to the battlefield.";
/// Colossal Dreadmaw {4}{G}{G} Creature — Dinosaur 6/6 — verbatim.
const DREADMAW: &str = "Trample (This creature can deal excess combat damage to the player or planeswalker it's attacking.)";
/// Fervor {2}{R} Enchantment — verbatim.
const FERVOR: &str = "Creatures you control have haste. (They can attack and {T} as soon as they come under your control.)";
/// Jump {U} Instant — verbatim.
const JUMP: &str = "Target creature gains flying until end of turn.";
/// Rise from the Grave {4}{B} Sorcery — verbatim.
const RISE_FROM_THE_GRAVE: &str = "Put target creature card from a graveyard onto the battlefield under your control. That creature is a black Zombie in addition to its other colors and types.";
/// Wayward Servant {W}{B} Creature — Zombie 2/2 — verbatim.
const WAYWARD_SERVANT: &str =
    "Whenever another Zombie you control enters, each opponent loses 1 life and you gain 1 life.";
/// Storm of Souls {4}{W}{W} Sorcery — verbatim.
const STORM_OF_SOULS: &str = "Return all creature cards from your graveyard to the battlefield. Each of them is a 1/1 Spirit with flying in addition to its other types. Exile Storm of Souls.";
/// Drogskol Cavalry {5}{W}{W} Creature — Spirit Knight 4/4 — verbatim.
const DROGSKOL_CAVALRY: &str = "Flying\nWhenever another Spirit you control enters, you gain 2 life.\n{3}{W}: Create a 1/1 white Spirit creature token with flying.";

fn main_phase() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn mana(kind: ManaType, n: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(kind, ObjectId(0), false, vec![]); n]
}

fn add_dreadnought(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_graveyard(P0, "Phyrexian Dreadnought", 12, 12)
        .as_artifact_creature()
        .with_subtypes(vec!["Phyrexian", "Dreadnought"])
        .from_oracle_text_with_keywords(&["Trample"], DREADNOUGHT)
        .id()
}

fn subtypes(runner: &GameRunner, id: ObjectId) -> Vec<String> {
    runner.state().objects[&id].card_types.subtypes.clone()
}

fn has_keyword(runner: &GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    runner.state().objects[&id].has_keyword(keyword)
}

fn trigger_count(runner: &GameRunner, id: ObjectId) -> usize {
    runner.state().objects[&id]
        .trigger_definitions
        .iter_unchecked()
        .count()
}

/// R4 — CR 611.2e + CR 613.1f + CR 603.2: the returned Dreadnought has no abilities as it
/// enters, so its "unless you sacrifice" ETB never triggers.
#[test]
fn xu_ifit_returned_creature_has_no_enters_trigger() {
    let mut scenario = main_phase();
    let xu = scenario
        .add_creature_from_oracle(P0, "Xu-Ifit, Osteoharmonist", 2, 3, XU_IFIT)
        .id();
    scenario.add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN);
    let dreadnought = add_dreadnought(&mut scenario);
    let mut runner = scenario.build();

    // Pre-activation parse reach guard: the graveyard Dreadnought has its ETB trigger.
    assert_eq!(trigger_count(&runner, dreadnought), 1, "parsed ETB trigger");

    let outcome = runner.activate(xu, 0).target_object(dreadnought).resolve();

    // Reach: Soul Warden saw the entry, and the rider applied to the moved object.
    assert_eq!(outcome.life_delta(P0), 1, "Soul Warden saw the entry");
    assert_eq!(runner.state().objects[&dreadnought].zone, Zone::Battlefield);
    let types = subtypes(&runner, dreadnought);
    for expected in ["Skeleton", "Phyrexian", "Dreadnought"] {
        assert!(types.iter().any(|t| t == expected), "{expected}: {types:?}");
    }

    // The entrant's own ETB did not trigger and the run completed.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "waiting_for: {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty(), "stack must be empty");
    assert_eq!(trigger_count(&runner, dreadnought), 0, "no live triggers");
    assert!(!has_keyword(&runner, dreadnought, &Keyword::Trample));

    // Binding hostile (CR 608.2c): the source is not the subject of the rider.
    assert!(!subtypes(&runner, xu).iter().any(|t| t == "Skeleton"));
    assert_eq!(
        runner.state().objects[&xu].abilities.len(),
        1,
        "Xu-Ifit keeps its activated ability"
    );
}

/// R4c — control: without the rider, the Dreadnought's ETB triggers and its unless-payment is
/// offered to its controller (CR 603.6a).
#[test]
fn zombify_returned_dreadnought_offers_unless_payment() {
    let mut scenario = main_phase();
    let zombify = scenario
        .add_spell_to_hand_from_oracle(P0, "Zombify", false, ZOMBIFY)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Black],
        })
        .id();
    let dreadnought = add_dreadnought(&mut scenario);
    let mut pool = mana(ManaType::Black, 1);
    pool.extend(mana(ManaType::Colorless, 3));
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();

    let _ = runner.cast(zombify).target_object(dreadnought).resolve();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::UnlessPayment { player: P0, .. }
        ),
        "waiting_for: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().objects[&dreadnought].zone, Zone::Battlefield);
    assert!(has_keyword(&runner, dreadnought, &Keyword::Trample));
}

/// R5 — CR 613.7a + CR 613.7b, layer 6: Fervor's earlier grant and the printed Trample are
/// removed; Jump's later grant is kept.
#[test]
fn xu_ifit_removes_earlier_grants_and_keeps_later_ones() {
    let mut scenario = main_phase();
    let xu = scenario
        .add_creature_from_oracle(P0, "Xu-Ifit, Osteoharmonist", 2, 3, XU_IFIT)
        .id();
    scenario.add_enchantment_from_oracle(P0, "Fervor", FERVOR);
    let dreadmaw = scenario
        .add_creature_to_graveyard(P0, "Colossal Dreadmaw", 6, 6)
        .with_subtypes(vec!["Dinosaur"])
        .from_oracle_text_with_keywords(&["Trample"], DREADMAW)
        .id();
    let jump = scenario
        .add_spell_to_hand_from_oracle(P0, "Jump", true, JUMP)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    scenario.with_mana_pool(P0, mana(ManaType::Blue, 1));
    let mut runner = scenario.build();

    flush_layers(runner.state_mut());
    // Reach: Fervor's grant functions on Xu-Ifit, and the printed Trample parsed.
    assert!(
        has_keyword(&runner, xu, &Keyword::Haste),
        "Fervor grants Haste"
    );
    assert!(
        runner.state().objects[&dreadmaw]
            .base_keywords
            .contains(&Keyword::Trample),
        "printed Trample parsed"
    );

    let _ = runner.activate(xu, 0).target_object(dreadmaw).resolve();
    assert_eq!(runner.state().objects[&dreadmaw].zone, Zone::Battlefield);
    let types = subtypes(&runner, dreadmaw);
    for expected in ["Skeleton", "Dinosaur"] {
        assert!(types.iter().any(|t| t == expected), "{expected}: {types:?}");
    }
    assert!(!has_keyword(&runner, dreadmaw, &Keyword::Haste));
    assert!(!has_keyword(&runner, dreadmaw, &Keyword::Trample));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());

    let _ = runner.cast(jump).target_object(dreadmaw).resolve();
    assert!(
        has_keyword(&runner, dreadmaw, &Keyword::Flying),
        "later grant kept"
    );
    assert!(!has_keyword(&runner, dreadmaw, &Keyword::Haste));
    assert!(!has_keyword(&runner, dreadmaw, &Keyword::Trample));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
}

/// R4d — CR 603.6a + CR 603.10 + CR 611.2e: a handed-off entry still checks the entrant's own
/// ETB. Rise from the Grave's "is a black Zombie" rider does not remove abilities, so the
/// returned Dreadnought's "unless you sacrifice" trigger is collected from the post-rider entrant
/// and its unless-payment is offered to its controller, P0.
#[test]
fn rise_from_the_grave_returned_dreadnought_keeps_its_enters_trigger() {
    let mut scenario = main_phase();
    let warden = scenario
        .add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN)
        .id();
    let dreadnought = add_dreadnought(&mut scenario);
    let rise = scenario
        .add_spell_to_hand_from_oracle(P0, "Rise from the Grave", false, RISE_FROM_THE_GRAVE)
        .id();
    let mut runner = scenario.build();

    let _ = runner.cast(rise).target_object(dreadnought).resolve();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::UnlessPayment { player: P0, .. }
        ),
        "waiting_for: {:?}",
        runner.state().waiting_for
    );
    // Reach: the rider applied to the entrant.
    assert_eq!(runner.state().objects[&dreadnought].zone, Zone::Battlefield);
    let types = subtypes(&runner, dreadnought);
    for expected in ["Zombie", "Phyrexian", "Dreadnought"] {
        assert!(types.iter().any(|t| t == expected), "{expected}: {types:?}");
    }
    // Multi-authority: the same entry also carries Soul Warden's observer trigger.
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == warden),
        "Soul Warden's trigger is waiting on the stack: {:?}",
        runner.state().stack
    );
}

/// R8 — CR 603.10 + CR 603.6a + CR 611.2e: Rise from the Grave's "is a black Zombie" effect
/// exists as the Giant enters, so every enter trigger is checked with it applied: Soul Warden
/// ("another creature") gives P0 +1, and Wayward Servant ("another Zombie you control") gives
/// P0 +1 and P1 -1 — even though Soul Warden would have matched the pre-rider entrant.
#[test]
fn rise_from_the_grave_entry_is_checked_after_its_zombie_effect() {
    let mut scenario = main_phase();
    scenario
        .add_creature_from_oracle(P0, "Wayward Servant", 2, 2, WAYWARD_SERVANT)
        .with_subtypes(vec!["Zombie"]);
    scenario.add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN);
    let giant = scenario
        .add_creature_to_graveyard(P1, "Hill Giant", 3, 3)
        .with_subtypes(vec!["Giant"])
        .id();
    let rise = scenario
        .add_spell_to_hand_from_oracle(P0, "Rise from the Grave", false, RISE_FROM_THE_GRAVE)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(rise).target_object(giant).resolve();

    // Reach: the Giant entered under P0 as a Zombie.
    let giant_obj = &outcome.state().objects[&giant];
    assert_eq!(giant_obj.zone, Zone::Battlefield);
    assert_eq!(giant_obj.controller, P0);
    assert!(giant_obj.card_types.subtypes.iter().any(|s| s == "Zombie"));

    assert_eq!(outcome.life_delta(P0), 2, "Soul Warden + Wayward Servant");
    assert_eq!(outcome.life_delta(P1), -1, "Wayward Servant");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "waiting_for: {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty(), "stack must be empty");
}

fn storm_of_souls_board(with_soul_warden: bool) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = main_phase();
    scenario
        .add_creature_from_oracle(P0, "Drogskol Cavalry", 4, 4, DROGSKOL_CAVALRY)
        .with_subtypes(vec!["Spirit", "Knight"]);
    if with_soul_warden {
        scenario.add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN);
    }
    let giant = scenario
        .add_creature_to_graveyard(P0, "Hill Giant", 3, 3)
        .with_subtypes(vec!["Giant"])
        .id();
    let storm = scenario
        .add_spell_to_hand_from_oracle(P0, "Storm of Souls", false, STORM_OF_SOULS)
        .id();
    (scenario.build(), storm, giant)
}

/// R9 — CR 603.10 + CR 603.6a + CR 611.2e, at the mass (`ChangeZoneAll`) owner: Storm of Souls'
/// "Each of them is a 1/1 Spirit" effect exists as the Giant enters, so Soul Warden ("another
/// creature") gives P0 +1 and Drogskol Cavalry ("another Spirit you control") gives P0 +2.
#[test]
fn storm_of_souls_mass_entry_is_checked_after_its_spirit_effect() {
    let (mut runner, storm, giant) = storm_of_souls_board(true);

    let outcome = runner.cast(storm).resolve();

    // Reach: the Giant entered as a Spirit.
    let giant_obj = &outcome.state().objects[&giant];
    assert_eq!(giant_obj.zone, Zone::Battlefield);
    assert!(giant_obj.card_types.subtypes.iter().any(|s| s == "Spirit"));

    assert_eq!(outcome.life_delta(P0), 3, "Soul Warden + Drogskol Cavalry");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "waiting_for: {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty(), "stack must be empty");
}

/// R9 positive control — CR 603.10 + CR 611.2e: without Soul Warden, Drogskol Cavalry alone
/// sees the Spirit entry (P0 +2).
#[test]
fn storm_of_souls_mass_entry_without_soul_warden_triggers_cavalry() {
    let (mut runner, storm, giant) = storm_of_souls_board(false);

    let outcome = runner.cast(storm).resolve();

    let giant_obj = &outcome.state().objects[&giant];
    assert_eq!(giant_obj.zone, Zone::Battlefield);
    assert!(giant_obj.card_types.subtypes.iter().any(|s| s == "Spirit"));
    assert_eq!(outcome.life_delta(P0), 2, "Drogskol Cavalry");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "waiting_for: {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty(), "stack must be empty");
}
