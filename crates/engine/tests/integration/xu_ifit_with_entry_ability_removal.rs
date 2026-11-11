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
//! a later instruction of the same resolution may still apply such an effect (R4, R4d, R8, R8b,
//! R9), including a `ChangeZone` resumed after a CR 616.1 replacement-ordering pause that
//! precedes the entry (R4p); a pause after an entry has been delivered keeps the entry with its
//! owner (known gap).
//!
//! Consequence (b) — CR 113.6h + CR 614.12 + CR 611.2e: an ability of the entrant that modifies
//! how it enters functions as it enters, checked as the permanent would exist on the battlefield,
//! and the same resolution's "has no abilities" effect applies simultaneously with that entry. So
//! none of the entrant's own entry abilities apply: enters tapped (R10), enters with counters
//! (R11), devour (R12, CR 702.82c), an as-enters counter choice (R15, CR 614.12a), intrinsic
//! loyalty (R14, CR 306.5b — with it gone the planeswalker's loyalty is 0, CR 306.5c, and
//! CR 704.5i puts it into the graveyard), and an optional "enter as a copy" (R16, Bronzehide
//! Lion's self-return of a Clone, CR 614.1c). Entry replacements that are not the entrant's own
//! abilities still apply (R11 Deanna Troi, R13 Imposing Sovereign and Archelos). The carrier
//! survives a CR 616.1 ordering pause (R13) and the `EffectZoneChoice` round-trip (R19).
//!
//! REVERT DISCRIMINATORS:
//! - Reverting the collection hand-off (making `change_zone::resolve` call the `Owner` delegate)
//!   halts R4 at `UnlessPayment` and leaves R8 at P0 +1, P1 0.
//! - Removing the `parse_has_no_abilities` arm in
//!   `oracle_static::grammar::parse_loss_enumeration` halts R4 at `UnlessPayment` (the
//!   Dreadnought's ETB triggers) and keeps Haste and Trample in R5.
//! - Dropping handed-off entries instead of handing them on fails R4d and R8.
//! - Reverting only the `resolve_all` call site to the `Owner` delegate leaves R9 at P0 +1.
//! - Reverting the resumed completion in `drain_pending_change_zone_iteration` to the `Owner`
//!   delegates puts the Dreadnought's ETB beside Soul Warden's trigger in R4p.
//! - Claiming handed-off entries in the consumed-before-priority ledger loses Soul Warden's
//!   trigger in R4p.
//! - Offering handed-off entries to settlement's latched batched pass fails R8b: Wayward Servant
//!   does not trigger (P0 +0).
//! - Removing the entrant-abilities candidate gate in `replacement::object_replacement_candidate_applies`
//!   fails R10 (tapped), R11 (2 counters), R12 (devour prompt), R13 (3 candidates), R15
//!   (counter-choice prompt) and R16 (Clone's optional copy prompt).
//! - Removing the seed gate in `zone_pipeline::seed_intrinsic_entry_counters` gives Grist 3
//!   loyalty counters in R14.
//! - Removing the zero-tracking in `zone_pipeline::deliver_replaced_zone_change` leaves Grist on
//!   the battlefield in R14 (not counter-tracked, base loyalty 3).
//! - Dropping the carrier from the event across the pause leaves the Troll 1 counter in R13.
//! - Restoring `Retained` at the `EffectZoneChoice` resume leaves the Ghoul tapped in R19.

use engine::game::game_object::AttachTarget;
use engine::game::layers::flush_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{EffectKind, ExileConcealment};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{ReplacementChoiceKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::proposed_event::{EntrantAbilities, EtbTapState};
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
/// Archelos, Lagoon Mystic {1}{B}{G}{U} Legendary Creature — Turtle Shaman 2/4 — verbatim.
const ARCHELOS_LAGOON_MYSTIC: &str = "As long as Archelos is tapped, other permanents enter tapped.\nAs long as Archelos is untapped, other permanents enter untapped.";
/// Imposing Sovereign {1}{W} Creature — Human Noble 2/1 — verbatim.
const IMPOSING_SOVEREIGN: &str = "Creatures your opponents control enter tapped.";
/// Enduring Innocence {1}{W}{W} Enchantment Creature — Sheep Glimmer 2/1 — verbatim.
const ENDURING_INNOCENCE: &str = "Lifelink\nWhenever one or more other creatures you control with power 2 or less enter, draw a card. This ability triggers only once each turn.\nWhen Enduring Innocence dies, if it was a creature, return it to the battlefield under its owner's control. It's an enchantment. (It's not a creature.)";

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

/// CR 616.1e: answer a two-way entry tap-state ordering offered to P0 with `answer` first,
/// through the public `GameAction`, and assert the offered pair.
fn answer_tap_state_ordering(runner: &mut GameRunner, answer: &str) {
    let index = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice {
            player,
            candidate_count: 2,
            candidates,
            ..
        } => {
            assert_eq!(*player, P0, "the entrant's controller orders (CR 616.1)");
            for source in ["Imposing Sovereign", "Archelos, Lagoon Mystic"] {
                assert!(
                    candidates.iter().any(|c| c.source_name == source),
                    "{source} must be offered: {candidates:?}"
                );
            }
            candidates
                .iter()
                .position(|c| c.source_name == answer)
                .unwrap_or_else(|| panic!("{answer} must be offered"))
        }
        other => panic!("expected the CR 616.1 tap-state ordering, got {other:?}"),
    };
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("answer the tap-state ordering");
}

/// R4p — CR 616.1 + CR 611.2e + CR 603.10: the return pauses on a CR 616.1 ordering of
/// Imposing Sovereign's "enter tapped" and Archelos's "enter untapped". After the answer the
/// resumed ChangeZone completes and the parked "has no abilities" rider applies with the entry,
/// so the Dreadnought's own ETB still never triggers, while Soul Warden's trigger for the same
/// entry is checked once (P0 +1).
#[test]
fn xu_ifit_return_through_replacement_ordering_has_no_enters_trigger() {
    let mut scenario = main_phase();
    let xu = scenario
        .add_creature_from_oracle(P0, "Xu-Ifit, Osteoharmonist", 2, 3, XU_IFIT)
        .id();
    let warden = scenario
        .add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN)
        .id();
    scenario.add_creature_from_oracle(P0, "Archelos, Lagoon Mystic", 2, 4, ARCHELOS_LAGOON_MYSTIC);
    scenario.add_creature_from_oracle(P1, "Imposing Sovereign", 2, 1, IMPOSING_SOVEREIGN);
    let dreadnought = add_dreadnought(&mut scenario);
    let mut runner = scenario.build();
    assert_eq!(trigger_count(&runner, dreadnought), 1, "parsed ETB trigger");
    let life_before = runner.life(P0);

    let _ = runner.activate(xu, 0).target_object(dreadnought).resolve();
    // Reach: the return paused on the ordering before the Dreadnought entered.
    assert_eq!(runner.state().objects[&dreadnought].zone, Zone::Graveyard);
    answer_tap_state_ordering(&mut runner, "Imposing Sovereign");

    // The resumed entry completed and the rider applied.
    assert_eq!(runner.state().objects[&dreadnought].zone, Zone::Battlefield);
    let types = subtypes(&runner, dreadnought);
    for expected in ["Skeleton", "Phyrexian", "Dreadnought"] {
        assert!(types.iter().any(|t| t == expected), "{expected}: {types:?}");
    }
    assert_eq!(trigger_count(&runner, dreadnought), 0, "no live triggers");
    // Only Soul Warden's trigger waits on the stack; none has the Dreadnought as its source.
    let sources: Vec<_> = runner.state().stack.iter().map(|e| e.source_id).collect();
    assert_eq!(sources, vec![warden], "stack sources: {sources:?}");

    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "waiting_for: {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty(), "stack must be empty");
    assert_eq!(
        runner.life(P0) - life_before,
        1,
        "Soul Warden saw the entry"
    );
}

/// R8b — CR 603.10 + CR 603.6a + CR 603.2c, a batched observer at the hand-off: Enduring
/// Innocence ("one or more other creatures you control with power 2 or less") and Wayward
/// Servant ("another Zombie you control") both check the Grizzly Bears' entry with Rise from
/// the Grave's "is a black Zombie" effect applied, each once: P0 draws 1, P0 +1, P1 -1.
#[test]
fn rise_from_the_grave_entry_is_checked_once_by_a_batched_observer() {
    let mut scenario = main_phase();
    scenario
        .add_creature_from_oracle(P0, "Enduring Innocence", 2, 1, ENDURING_INNOCENCE)
        .with_subtypes(vec!["Sheep", "Glimmer"]);
    scenario
        .add_creature_from_oracle(P0, "Wayward Servant", 2, 2, WAYWARD_SERVANT)
        .with_subtypes(vec!["Zombie"]);
    scenario.add_card_to_library_top(P0, "Library Card");
    let bears = scenario
        .add_creature_to_graveyard(P1, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    let rise = scenario
        .add_spell_to_hand_from_oracle(P0, "Rise from the Grave", false, RISE_FROM_THE_GRAVE)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(rise).target_object(bears).resolve();

    // Reach: the Bears entered under P0 as a Zombie.
    let bears_obj = &outcome.state().objects[&bears];
    assert_eq!(bears_obj.zone, Zone::Battlefield);
    assert_eq!(bears_obj.controller, P0);
    assert!(bears_obj.card_types.subtypes.iter().any(|s| s == "Zombie"));

    assert_eq!(outcome.hand_drawn(P0), 1, "Enduring Innocence");
    assert_eq!(outcome.life_delta(P0), 1, "Wayward Servant");
    assert_eq!(outcome.life_delta(P1), -1, "Wayward Servant");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "waiting_for: {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty(), "stack must be empty");
}

// ---------------------------------------------------------------------------
// Consequence (b): the entrant's own entry abilities (CR 113.6h + CR 614.12 + CR 611.2e).
// ---------------------------------------------------------------------------

/// Diregraf Ghoul {B} Creature — Zombie 2/2 — verbatim.
const DIREGRAF_GHOUL: &str = "This creature enters tapped.";
/// Barkhide Troll {G}{G} Creature — Troll 2/2 — verbatim.
const BARKHIDE_TROLL: &str = "This creature enters with a +1/+1 counter on it.\n{1}, Remove a +1/+1 counter from this creature: This creature gains hexproof until end of turn. (It can't be the target of spells or abilities your opponents control.)";
/// Deanna Troi, Psychic Counsel {1}{W} Legendary Creature — Betazoid Human Advisor 2/2 — verbatim.
const DEANNA_TROI: &str =
    "Other creatures you control enter with an additional +1/+1 counter on them.";
/// Famished Worldsire {5}{G}{G}{G} Creature — Leviathan 0/0 — verbatim.
const FAMISHED_WORLDSIRE: &str = "Ward {3}\nDevour land 3 (As this creature enters, you may sacrifice any number of lands. It enters with three times that many +1/+1 counters on it.)\nWhen this creature enters, look at the top X cards of your library, where X is this creature's power. Put any number of land cards from among them onto the battlefield tapped, then shuffle.";
/// Boot Nipper {1}{B} Creature — Beast 2/1 — verbatim.
const BOOT_NIPPER: &str =
    "This creature enters with your choice of a deathtouch counter or a lifelink counter on it.";
/// Grist, the Hunger Tide {1}{B}{G} Legendary Planeswalker — Grist, loyalty 3 — the
/// `grist_zone_functioning.rs` constant text.
const GRIST_ORACLE: &str = "As long as Grist isn't on the battlefield, it's a 1/1 Insect creature in addition to its other types.\n+1: Create a 1/1 black and green Insect creature token, then mill a card. If an Insect card was milled this way, put a loyalty counter on Grist and repeat this process.\n\u{2212}2: You may sacrifice a creature. When you do, destroy target creature or planeswalker.\n\u{2212}5: Each opponent loses life equal to the number of creature cards in your graveyard.";
/// Clone {3}{U} Creature — Shapeshifter 0/0 — verbatim.
const CLONE: &str =
    "You may have this creature enter as a copy of any creature on the battlefield.";
/// Bronzehide Lion {G}{W} Creature — Cat 3/3 — verbatim.
const BRONZEHIDE_LION: &str = "{G}{W}: This creature gains indestructible until end of turn.\nWhen this creature dies, return it to the battlefield. It's an Aura enchantment with enchant creature you control and \"{G}{W}: Enchanted creature gains indestructible until end of turn,\" and it loses all other abilities.";
/// Old-Growth Troll {G}{G}{G} Creature — Troll Warrior 4/4 — verbatim.
const OLD_GROWTH_TROLL: &str = "Trample\nWhen Old-Growth Troll dies, if it was a creature, return it to the battlefield. It's an Aura enchantment with enchant Forest you control and \"Enchanted Forest has '{T}: Add {G}{G}' and '{1}, {T}, Sacrifice this land: Create a tapped 4/4 green Troll Warrior creature token with trample.'\"";
/// Murder {1}{B}{B} Instant — verbatim.
const MURDER: &str = "Destroy target creature.";

/// Which instruction returns the graveyard card: Xu-Ifit's activation (the rider removes all
/// abilities with the entry) or Zombify (no rider; the control).
#[derive(Clone, Copy)]
enum Returner {
    XuIfit,
    Zombify,
}

/// A main-phase board with the returner for P0.
fn returner_board(returner: Returner) -> (GameScenario, ObjectId) {
    let mut scenario = main_phase();
    let source = match returner {
        Returner::XuIfit => scenario
            .add_creature_from_oracle(P0, "Xu-Ifit, Osteoharmonist", 2, 3, XU_IFIT)
            .id(),
        Returner::Zombify => scenario
            .add_spell_to_hand_from_oracle(P0, "Zombify", false, ZOMBIFY)
            .id(),
    };
    (scenario, source)
}

/// Return `card` from P0's graveyard with `returner`, through the production entry point.
fn return_card(
    runner: &mut GameRunner,
    returner: Returner,
    source: ObjectId,
    card: ObjectId,
) -> Vec<GameEvent> {
    let outcome = match returner {
        Returner::XuIfit => runner.activate(source, 0).target_object(card).resolve(),
        Returner::Zombify => runner.cast(source).target_object(card).resolve(),
    };
    outcome.events().to_vec()
}

fn assert_priority_with_empty_stack(runner: &GameRunner) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "waiting_for: {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty(), "stack must be empty");
}

fn plus_one_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn add_diregraf_ghoul(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_graveyard(P0, "Diregraf Ghoul", 2, 2)
        .with_subtypes(vec!["Zombie"])
        .from_oracle_text(DIREGRAF_GHOUL)
        .id()
}

fn add_barkhide_troll(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_graveyard(P0, "Barkhide Troll", 2, 2)
        .with_subtypes(vec!["Troll"])
        .from_oracle_text(BARKHIDE_TROLL)
        .id()
}

/// R10 — CR 614.12 + CR 614.1c + CR 611.2e: Diregraf Ghoul's own "enters tapped" is an ability
/// of the entrant, removed with its entry, so the Ghoul enters untapped.
#[test]
fn xu_ifit_returned_ghoul_enters_untapped() {
    let (mut scenario, xu) = returner_board(Returner::XuIfit);
    let ghoul = add_diregraf_ghoul(&mut scenario);
    let mut runner = scenario.build();

    return_card(&mut runner, Returner::XuIfit, xu, ghoul);

    let ghoul_obj = &runner.state().objects[&ghoul];
    assert_eq!(ghoul_obj.zone, Zone::Battlefield);
    // Reach: the rider applied to the entrant.
    let types = subtypes(&runner, ghoul);
    for expected in ["Skeleton", "Zombie"] {
        assert!(types.iter().any(|t| t == expected), "{expected}: {types:?}");
    }
    assert!(!ghoul_obj.tapped, "its own enters-tapped never applied");
    assert_priority_with_empty_stack(&runner);
}

/// R10c — control: Zombify has no rider, so the Ghoul's own "enters tapped" applies.
#[test]
fn zombify_returned_ghoul_enters_tapped() {
    let (mut scenario, zombify) = returner_board(Returner::Zombify);
    let ghoul = add_diregraf_ghoul(&mut scenario);
    let mut runner = scenario.build();

    return_card(&mut runner, Returner::Zombify, zombify, ghoul);

    let ghoul_obj = &runner.state().objects[&ghoul];
    assert_eq!(ghoul_obj.zone, Zone::Battlefield);
    assert!(ghoul_obj.tapped, "its own enters-tapped applied");
    assert_priority_with_empty_stack(&runner);
}

/// R11 — CR 614.12 + CR 611.2e: Barkhide Troll's own "+1/+1 counter" replacement is withheld,
/// while Deanna Troi's "Other creatures you control enter with an additional +1/+1 counter" is
/// not an ability of the entrant and still applies: exactly one counter.
#[test]
fn xu_ifit_returned_troll_keeps_only_the_external_counter() {
    let (mut scenario, xu) = returner_board(Returner::XuIfit);
    scenario.add_creature_from_oracle(P0, "Deanna Troi, Psychic Counsel", 2, 2, DEANNA_TROI);
    let troll = add_barkhide_troll(&mut scenario);
    let mut runner = scenario.build();

    return_card(&mut runner, Returner::XuIfit, xu, troll);

    assert_eq!(runner.state().objects[&troll].zone, Zone::Battlefield);
    assert!(subtypes(&runner, troll).iter().any(|t| t == "Skeleton"));
    assert_eq!(plus_one_counters(&runner, troll), 1, "Deanna Troi's only");
    assert!(
        runner.state().objects[&troll].abilities.is_empty(),
        "the Troll has no abilities"
    );
    assert_priority_with_empty_stack(&runner);
}

/// R11c — control: with Zombify, the Troll's own counter and Deanna Troi's both apply.
#[test]
fn zombify_returned_troll_gets_both_counters() {
    let (mut scenario, zombify) = returner_board(Returner::Zombify);
    scenario.add_creature_from_oracle(P0, "Deanna Troi, Psychic Counsel", 2, 2, DEANNA_TROI);
    let troll = add_barkhide_troll(&mut scenario);
    let mut runner = scenario.build();

    return_card(&mut runner, Returner::Zombify, zombify, troll);

    assert_eq!(runner.state().objects[&troll].zone, Zone::Battlefield);
    assert_eq!(plus_one_counters(&runner, troll), 2);
    assert_eq!(runner.state().objects[&troll].abilities.len(), 1);
    assert_priority_with_empty_stack(&runner);
}

fn worldsire_board(returner: Returner) -> (GameRunner, ObjectId, ObjectId, [ObjectId; 2]) {
    let (mut scenario, source) = returner_board(returner);
    let forests = [
        scenario.add_basic_land(P0, ManaColor::Green),
        scenario.add_basic_land(P0, ManaColor::Green),
    ];
    scenario.add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN);
    let worldsire = scenario
        .add_creature_to_graveyard(P0, "Famished Worldsire", 0, 0)
        .with_subtypes(vec!["Leviathan"])
        .from_oracle_text_with_keywords(&["Ward {3}", "Devour land 3"], FAMISHED_WORLDSIRE)
        .id();
    (scenario.build(), source, worldsire, forests)
}

/// R12 — CR 702.82a + CR 702.82c + CR 611.2e: devour is the entrant's own "as this enters"
/// replacement, so no sacrifice is offered; the 0/0 Worldsire enters and is put into its
/// owner's graveyard (CR 704.5f), and both Forests stay.
#[test]
fn xu_ifit_returned_worldsire_offers_no_devour() {
    let (mut runner, xu, worldsire, forests) = worldsire_board(Returner::XuIfit);
    let life_before = runner.life(P0);

    return_card(&mut runner, Returner::XuIfit, xu, worldsire);

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { .. }
        ),
        "no devour sacrifice: {:?}",
        runner.state().waiting_for
    );
    assert_priority_with_empty_stack(&runner);
    // Reach: Soul Warden saw the entry.
    assert_eq!(
        runner.life(P0) - life_before,
        1,
        "Soul Warden saw the entry"
    );
    for forest in forests {
        assert_eq!(runner.state().objects[&forest].zone, Zone::Battlefield);
    }
    assert_eq!(runner.state().objects[&worldsire].zone, Zone::Graveyard);
    assert!(runner.state().active_devour_eligible_snapshot().is_none());
}

/// R12c — control: with Zombify, the Worldsire's devour offers P0 the sacrifice of both Forests.
#[test]
fn zombify_returned_worldsire_offers_devour() {
    let (mut runner, zombify, worldsire, forests) = worldsire_board(Returner::Zombify);

    return_card(&mut runner, Returner::Zombify, zombify, worldsire);

    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice {
            player,
            effect_kind: EffectKind::Sacrifice,
            cards,
            ..
        } => {
            assert_eq!(*player, P0);
            for forest in forests {
                assert!(cards.contains(&forest), "{forest:?} offered: {cards:?}");
            }
        }
        other => panic!("expected the devour sacrifice choice, got {other:?}"),
    }
}

/// R13 — CR 616.1 + CR 616.1f + CR 611.2e: the return pauses on the ordering of the two external
/// tap-state replacements only (Barkhide Troll's own counter replacement is not a candidate);
/// after the answer, Archelos's "enter untapped" applies last and the Troll's own counter stays
/// withheld on the repeated pass.
#[test]
fn xu_ifit_returned_troll_through_replacement_ordering_withholds_its_counter() {
    let (mut scenario, xu) = returner_board(Returner::XuIfit);
    scenario.add_creature_from_oracle(P0, "Archelos, Lagoon Mystic", 2, 4, ARCHELOS_LAGOON_MYSTIC);
    scenario.add_creature_from_oracle(P1, "Imposing Sovereign", 2, 1, IMPOSING_SOVEREIGN);
    let troll = add_barkhide_troll(&mut scenario);
    let mut runner = scenario.build();

    return_card(&mut runner, Returner::XuIfit, xu, troll);
    assert_eq!(runner.state().objects[&troll].zone, Zone::Graveyard);
    answer_tap_state_ordering(&mut runner, "Imposing Sovereign");

    let troll_obj = &runner.state().objects[&troll];
    assert_eq!(troll_obj.zone, Zone::Battlefield);
    assert_eq!(
        plus_one_counters(&runner, troll),
        0,
        "its own counter withheld"
    );
    assert!(!troll_obj.tapped, "Archelos applied last");
    assert_priority_with_empty_stack(&runner);
}

fn add_grist(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_graveyard(P0, "Grist, the Hunger Tide", false)
        .from_oracle_text(GRIST_ORACLE)
        .as_planeswalker_with_loyalty("Grist", 3)
        .as_legendary()
        .id()
}

fn grist_board(returner: Returner) -> (GameRunner, ObjectId, ObjectId) {
    let (mut scenario, source) = returner_board(returner);
    let grist = add_grist(&mut scenario);
    let mut runner = scenario.build();
    // Grist is a creature card in the graveyard once its own static is applied.
    runner.state_mut().layers_dirty.mark_full();
    flush_layers(runner.state_mut());
    (runner, source, grist)
}

/// R14 — CR 306.5b + CR 306.5c + CR 611.2e + CR 704.5i: Grist's intrinsic loyalty comes from an
/// ability removed with its entry, so it enters with no loyalty counters, its loyalty is 0, and
/// it is put into its owner's graveyard.
#[test]
fn xu_ifit_returned_grist_enters_without_loyalty_and_dies() {
    let (mut runner, xu, grist) = grist_board(Returner::XuIfit);

    let events = return_card(&mut runner, Returner::XuIfit, xu, grist);

    let entered = events
        .iter()
        .position(|event| {
            matches!(
                event,
                GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. }
                    if *object_id == grist
            )
        })
        .expect("reach: Grist entered the battlefield");
    assert!(
        events[entered..].iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged { object_id, to: Zone::Graveyard, .. } if *object_id == grist
        )),
        "Grist is put into the graveyard after entering"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            GameEvent::CounterAdded {
                object_id,
                counter_type: CounterType::Loyalty,
                ..
            } if *object_id == grist
        )),
        "no loyalty counter was put on Grist"
    );
    assert_eq!(runner.state().objects[&grist].zone, Zone::Graveyard);
    assert_priority_with_empty_stack(&runner);
}

/// R14c — control: with Zombify, Grist enters with three loyalty counters and stays.
#[test]
fn zombify_returned_grist_enters_with_loyalty() {
    let (mut runner, zombify, grist) = grist_board(Returner::Zombify);

    return_card(&mut runner, Returner::Zombify, zombify, grist);

    let grist_obj = &runner.state().objects[&grist];
    assert_eq!(grist_obj.zone, Zone::Battlefield);
    assert_eq!(
        grist_obj.counters.get(&CounterType::Loyalty).copied(),
        Some(3)
    );
}

fn add_boot_nipper(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_graveyard(P0, "Boot Nipper", 2, 1)
        .with_subtypes(vec!["Beast"])
        .from_oracle_text(BOOT_NIPPER)
        .id()
}

/// R15 — CR 614.12a + CR 611.2e: Boot Nipper's "enters with your choice of a deathtouch counter
/// or a lifelink counter" is its own ability, so no choice is offered and it has no counters.
#[test]
fn xu_ifit_returned_boot_nipper_offers_no_counter_choice() {
    let (mut scenario, xu) = returner_board(Returner::XuIfit);
    let nipper = add_boot_nipper(&mut scenario);
    let mut runner = scenario.build();

    return_card(&mut runner, Returner::XuIfit, xu, nipper);

    assert_priority_with_empty_stack(&runner);
    let nipper_obj = &runner.state().objects[&nipper];
    assert_eq!(nipper_obj.zone, Zone::Battlefield);
    assert!(subtypes(&runner, nipper).iter().any(|t| t == "Skeleton"));
    assert!(nipper_obj.counters.is_empty(), "{:?}", nipper_obj.counters);
}

/// R15c — control: with Zombify, P0 is offered Boot Nipper's counter choice.
#[test]
fn zombify_returned_boot_nipper_offers_counter_choice() {
    let (mut scenario, zombify) = returner_board(Returner::Zombify);
    let nipper = add_boot_nipper(&mut scenario);
    let mut runner = scenario.build();

    return_card(&mut runner, Returner::Zombify, zombify, nipper);

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseOneOfBranch { player: P0, source_id, .. } if source_id == nipper
        ),
        "waiting_for: {:?}",
        runner.state().waiting_for
    );
}

/// A Clone cast by P0 as a copy of P1's `copied_name` creature, then destroyed by P0's Murder.
fn clone_copy_then_murder(
    copied_name: &str,
    copied_text: &str,
    with_forest: bool,
) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = main_phase();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    if with_forest {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let copied = scenario
        .add_creature_from_oracle(P1, copied_name, 3, 3, copied_text)
        .id();
    let clone = scenario
        .add_creature_to_hand_from_oracle(P0, "Clone", 0, 0, CLONE)
        .with_subtypes(vec!["Shapeshifter"])
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = scenario.build();

    let _ = runner
        .cast(clone)
        .replacement_choice(0)
        .copy_target(copied)
        .resolve();
    // Reach: the Clone entered as a copy.
    assert_eq!(runner.state().objects[&clone].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&clone].name, copied_name);

    let _ = runner.cast(murder).target_object(clone).resolve();
    (runner, clone, bears)
}

/// R16 — CR 614.1c + CR 611.2e, the `ReturnAsAura` producer: a Clone copying Bronzehide Lion
/// dies, and the copied trigger returns it as an Aura that "loses all other abilities". Clone's
/// own optional "enter as a copy" is an ability removed with that entry, so it is not offered;
/// the Clone returns as an Aura named Clone attached to P0's Grizzly Bears.
#[test]
fn bronzehide_lion_copy_returns_without_clones_copy_choice() {
    let (runner, clone, bears) = clone_copy_then_murder("Bronzehide Lion", BRONZEHIDE_LION, false);

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "Clone's copy choice must not be offered: {:?}",
        runner.state().waiting_for
    );
    assert_priority_with_empty_stack(&runner);
    let clone_obj = &runner.state().objects[&clone];
    assert_eq!(clone_obj.zone, Zone::Battlefield);
    assert_eq!(clone_obj.name, "Clone");
    assert_eq!(clone_obj.card_types.core_types, vec![CoreType::Enchantment]);
    assert!(clone_obj.card_types.subtypes.iter().any(|s| s == "Aura"));
    assert_eq!(clone_obj.attached_to, Some(AttachTarget::Object(bears)));
}

/// R16c — control: Old-Growth Troll's return grants abilities but removes none, so the Clone's
/// own optional "enter as a copy" is offered to P0.
#[test]
fn old_growth_troll_copy_return_offers_clones_copy_choice() {
    let (runner, clone, _bears) =
        clone_copy_then_murder("Old-Growth Troll", OLD_GROWTH_TROLL, true);

    match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice {
            player,
            kind: ReplacementChoiceKind::OptionalBranch,
            candidates,
            ..
        } => {
            assert_eq!(*player, P0);
            assert!(
                candidates.iter().any(|c| c.source_id == clone),
                "Clone's own copy replacement: {candidates:?}"
            );
        }
        other => panic!("expected Clone's optional copy choice, got {other:?}"),
    }
}

/// A constructed `EffectZoneChoice` for a `ChangeZone` from P0's graveyard to the battlefield,
/// carrying `entrant`.
fn change_zone_choice(source: ObjectId, card: ObjectId, entrant: EntrantAbilities) -> WaitingFor {
    WaitingFor::EffectZoneChoice {
        player: P0,
        cards: vec![card],
        count: 1,
        min_count: 0,
        up_to: false,
        source_id: source,
        effect_kind: EffectKind::ChangeZone,
        zone: Zone::Graveyard,
        destination: Some(Zone::Battlefield),
        enter_tapped: EtbTapState::Unspecified,
        enter_transformed: false,
        enters_under_player: None,
        enters_attacking: false,
        owner_library: false,
        track_exiled_by_source: false,
        face_down_in_exile: ExileConcealment::Public,
        face_down_profile: None,
        entrant_abilities: entrant,
        enter_with_counters: vec![],
        conditional_enter_with_counters: vec![],
        count_param: 0,
        library_position: None,
        mass_library_order: None,
        is_cost_payment: false,
        enters_modified_if: None,
        duration: None,
    }
}

/// R19 — CR 611.2e + CR 614.12: the `EffectZoneChoice` resume restores the carried value and
/// never re-derives it. Constructed-state seam test: no printed card parks this choice with an
/// ability-removing rider (Xu-Ifit is targeted; every `ReturnAsAura` is a self-return). The
/// nearest printed card is Path of the Schemer ("Each player mills two cards. Then you put a
/// creature card from a graveyard onto the battlefield under your control. It's an artifact in
/// addition to its other types."), whose non-targeted graveyard-to-battlefield `ChangeZone`
/// carries a `WithEntry` "is" rider without removal. The paired `Retained` choice is the
/// positive control.
#[test]
fn effect_zone_choice_resume_restores_the_carried_entrant_abilities() {
    for (entrant, tapped) in [
        (EntrantAbilities::RemovedWithEntry, false),
        (EntrantAbilities::Retained, true),
    ] {
        let mut scenario = main_phase();
        let source = scenario.add_creature(P0, "Choice Source", 1, 1).id();
        let ghoul = add_diregraf_ghoul(&mut scenario);
        let mut runner = scenario.build();
        runner.state_mut().waiting_for = change_zone_choice(source, ghoul, entrant);

        runner
            .act(GameAction::SelectCards { cards: vec![ghoul] })
            .expect("the selected card is a legal choice");

        let ghoul_obj = &runner.state().objects[&ghoul];
        assert_eq!(ghoul_obj.zone, Zone::Battlefield, "{entrant:?}");
        assert_eq!(ghoul_obj.tapped, tapped, "{entrant:?}");
    }
}
