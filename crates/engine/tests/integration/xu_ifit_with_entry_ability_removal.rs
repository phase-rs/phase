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
//! owner, and a rider that pauses before it applies (a ReturnAsAura host choice among two or
//! more candidates) lets the post-action collector check the entry first (known gaps). A
//! `ChangeZone` completed from an `EffectZoneChoice` resume (Ashiok, Nightmare Weaver's -X) has
//! its entry read with the carried Nightmare rider (R-A), but the owner still checks its enter
//! triggers before the "is/has" effect exists (known gap). The hand-off is made only when no
//! later instruction that starts after the
//! entry acts on a moved object (CR 608.2c): Call a Surprise Witness's "Put a flying counter on it." comes after the
//! entry, so the owner checks the entry before it and Watcher of the Spheres does not trigger
//! (R8c). Such an entry is then also checked before the "is/has" effect exists (known gap). R8c
//! keeps Soul Warden on the board: when no observer matches at the owner's check, the owner
//! queues no collection witness and the post-action collector checks the entry again after the
//! counter (an older gap, outside this hand-off). The hand-off covers a whole logical group or
//! none of it (CR 603.2c): when a member ends outside the battlefield, the owner keeps every
//! occurrence, so a batched observer triggers once for the event (R8m, R9m). A handed-off entry
//! from a graveyard keeps its CR 603.10a leaves-the-graveyard half (R4t, R9t).
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
//! Consequence (c) — CR 614.12 + CR 611.2e: external entry replacements and
//! `EntersWithAdditionalCounters` statics read the entrant with the resolution's simultaneous
//! is/has effect applied: R20–R29, R-L2, R-A, R9m, R8m.
//!
//! Consequence (d) — CR 603.6a: "Each time an event puts one or more permanents onto the
//! battlefield, all permanents on the battlefield (including the newcomers) are checked". The
//! entrants of one completed logical group (a handed-off Storm of Souls return or an
//! owner-collected Living Death return) see each other's entries whichever was delivered first,
//! and Impact Tremors still triggers once per entrant (CR 603.2c): R30a, R30b, R30o-a, R30o-b. A
//! returned entrant still misses an event before its own entry (issue #8160, CR 603.10a), and its
//! sacrifice trigger is live on the battlefield: R-8160m, R-8160p. A save taken while such a
//! stamped entry is in a live carrier restores, because the entry's per-turn ledger row and every
//! parked copy carry the stamp (CR 400.7 + CR 603.2c): R-P1, R-P2, R-P3.
//!
//! REVERT DISCRIMINATORS:
//! - Reverting the collection hand-off (making `change_zone::resolve` call the `Owner` delegate)
//!   halts R4 at `UnlessPayment` and leaves R8 at P0 +1, P1 0.
//! - Removing the `parse_has_no_abilities` arm in
//!   `oracle_static::grammar::parse_loss_enumeration` halts R4 at `UnlessPayment` (the
//!   Dreadnought's ETB triggers) and keeps Haste and Trample in R5.
//! - Dropping handed-off entries instead of handing them on fails R4d and R8.
//! - Reverting only the `resolve_all` call site to the `Owner` delegate leaves R9 at P0 +1.
//! - Dropping the destination check from `triggers::EntryOccurrenceCollector::for_group` gives
//!   two Zombie tokens in R8m and R9m; dropping only the `for_group` call at the
//!   `change_zone::resolve` / `resolve_all` site gives two in R8m / R9m.
//! - Dropping the moved-object check from `effects::entry_occurrence_collector` makes Watcher of
//!   the Spheres trigger in R8c (Watcher 3/3).
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
//! - Restoring no with-entry effect (`None`) at the `EffectZoneChoice` resume leaves the Ghoul
//!   tapped in R19.
//! - Removing the `for_proposed_entry` read in `filter::matches_target_filter_on_battlefield_entry`
//!   gives R20, R21, R22, R23, R-L2, R-A and R19' 0 counters, R25 1, R27 and R29 tapped, and
//!   Grist exiled in R9m and R8m.
//! - Removing the `entered` read in `zone_pipeline::enters_with_additional_counters_for_entry`
//!   gives R28 1 counter.
//! - Skipping the controller override on the projection's scratch gives R21h 1 counter and R21,
//!   R-L2 and R-A 0.
//! - Skipping `stamp_battlefield_entries`, or dropping the co-entry spans in
//!   `battlefield_entry_observation_indices`, gives R30a and R30o-a P0 +0.
//! - Stamping after the final segment's owner collection gives R30o-a P1 −4.
//! - Dropping the `deferred_entry_events` sync is not covered by a row. No printed board observes
//!   this sync. Metallic Mimic's "As this creature enters, choose a creature type." is a choice
//!   made before it enters (CR 614.12a; `replacement::park_as_enters_named_choice`): `resolve_all`
//!   pauses before Mimic moves (`ZoneMoveTerminalResult::NeedsChoice`), so nothing is captured
//!   into `state.deferred_entry_events`. A mid-entry choice that does capture an entry (enter as a
//!   copy, an enters-with-your-choice-of-counter branch, an exact-object as-enters `NamedChoice`)
//!   also makes its delivery return `NeedsChoice` (`zone_pipeline::replacement_pause_delivery_result`),
//!   and every multi-member owner pauses on it, so the group completes in a later action, whose
//!   buffer never holds the captured occurrence. The sync keeps a still-parked deferred copy
//!   stamped like the group's retained occurrence.
//! - Deleting the ledger half of `mirror_logical_zone_change_entry_stamps` makes R-P1 and R-P2 fail
//!   to restore with 'current-turn ZoneChanged record has no ledger occurrence'.
//! - Deleting its parked-trigger half makes R-P3 fail to restore with the same error.
//!
//! R-L1: Lorcan, Warlock Collector's trigger return carries its Warlock rider through a CR 616.1
//! ordering pause; removing the triggering-object reading of `MovedObjectsSource::of` leaves the
//! parked proposal without it.

use engine::game::coverage::unimplemented_mechanics;
use engine::game::game_object::AttachTarget;
use engine::game::layers::{flush_layers, mark_layers_full};
use engine::game::scenario::{CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    ChosenAttribute, ContinuousModification, EffectKind, ExileConcealment,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, ExileLink, ExileLinkKind, GameState, PersistedGameState,
    PersistedRestoreFinalization, ReplacementChoiceKind, StackEntryKind, WaitingFor,
    ZoneChangeRecord,
};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::keywords::{Keyword, KeywordKind};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::proposed_event::{EtbTapState, ProposedEvent, WithEntryEffect};
use engine::types::triggers::TriggerMode;
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

/// Call a Surprise Witness {1}{W} Sorcery — verbatim.
const CALL_A_SURPRISE_WITNESS: &str = "Return target creature card with mana value 3 or less from your graveyard to the battlefield. Put a flying counter on it. It's a Spirit in addition to its other types.";
/// Watcher of the Spheres {W}{U} Creature — Bird Wizard 2/2 — verbatim.
const WATCHER_OF_THE_SPHERES: &str = "Flying\nCreature spells with flying you cast cost {1} less to cast.\nWhenever another creature you control with flying enters, this creature gets +1/+1 until end of turn.";
/// Suntail Hawk {W} Creature — Bird 1/1 — verbatim.
const SUNTAIL_HAWK: &str = "Flying";

/// A board with Watcher of the Spheres and Soul Warden for P0, `returned` in P0's graveyard, and
/// Call a Surprise Witness in P0's hand. Soul Warden ("another creature") matches the entrant at
/// the owner's check, so that check leaves a collection witness for the entry.
fn surprise_witness_board(
    add_returned: impl FnOnce(&mut GameScenario) -> ObjectId,
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = main_phase();
    let watcher = scenario
        .add_creature_from_oracle(P0, "Watcher of the Spheres", 2, 2, WATCHER_OF_THE_SPHERES)
        .with_subtypes(vec!["Bird", "Wizard"])
        .id();
    scenario.add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN);
    let returned = add_returned(&mut scenario);
    let witness = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Call a Surprise Witness",
            false,
            CALL_A_SURPRISE_WITNESS,
        )
        .id();
    (scenario.build(), watcher, returned, witness)
}

/// R8c — CR 608.2c + CR 603.10 + CR 611.2e: Call a Surprise Witness puts its flying counter on
/// the returned Grizzly Bears after the entry; only "It's a Spirit" applies with the entry. The
/// entry is checked immediately after it, before the counter: Soul Warden ("another creature")
/// gives P0 +1, and Watcher of the Spheres ("another creature you control with flying") does
/// not trigger (Watcher stays 2/2).
#[test]
fn call_a_surprise_witness_counter_comes_after_watcher_checks_the_entry() {
    let (mut runner, watcher, bears, witness) = surprise_witness_board(|scenario| {
        scenario
            .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
            .with_subtypes(vec!["Bear"])
            .with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Green],
            })
            .id()
    });
    assert_eq!(trigger_count(&runner, watcher), 1, "parsed Watcher trigger");

    let outcome = runner.cast(witness).target_object(bears).resolve();

    // Reach: the Bears entered, got the flying counter (so it has flying now), and is a Spirit;
    // Soul Warden saw the entry.
    assert_eq!(outcome.life_delta(P0), 1, "Soul Warden saw the entry");
    assert_eq!(runner.state().objects[&bears].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&bears]
            .counters
            .get(&CounterType::Keyword(KeywordKind::Flying))
            .copied(),
        Some(1),
        "flying counter"
    );
    assert!(has_keyword(&runner, bears, &Keyword::Flying));
    assert!(subtypes(&runner, bears).iter().any(|t| t == "Spirit"));

    // The entry was checked without the later counter: Watcher did not trigger.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "waiting_for: {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty(), "stack must be empty");
    assert_eq!(
        runner.state().objects[&watcher].power,
        Some(2),
        "Watcher of the Spheres must not trigger"
    );
}

/// R8c positive control — CR 603.10: a creature card with printed flying (Suntail Hawk)
/// returned by the same spell has flying as it enters, so Watcher of the Spheres triggers
/// (Watcher 3/3) beside Soul Warden (P0 +1).
#[test]
fn call_a_surprise_witness_printed_flier_triggers_watcher() {
    let (mut runner, watcher, hawk, witness) = surprise_witness_board(|scenario| {
        scenario
            .add_creature_to_graveyard(P0, "Suntail Hawk", 1, 1)
            .with_subtypes(vec!["Bird"])
            .with_mana_cost(ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::White],
            })
            .from_oracle_text_with_keywords(&["Flying"], SUNTAIL_HAWK)
            .id()
    });

    let outcome = runner.cast(witness).target_object(hawk).resolve();

    assert_eq!(outcome.life_delta(P0), 1, "Soul Warden saw the entry");
    assert_eq!(runner.state().objects[&hawk].zone, Zone::Battlefield);
    assert!(subtypes(&runner, hawk).iter().any(|t| t == "Spirit"));
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "waiting_for: {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty(), "stack must be empty");
    assert_eq!(
        runner.state().objects[&watcher].power,
        Some(3),
        "Watcher of the Spheres saw the flier enter"
    );
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
/// Grist, the Hunger Tide {1}{B}{G} Legendary Planeswalker — Grist, loyalty 3 — verbatim
/// (Scryfall form of the loyalty costs; shared with grist_zone_functioning.rs).
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

/// Tormod, the Desecrator {3}{B} Legendary Creature — Zombie Wizard 4/2 — verbatim.
const TORMOD_THE_DESECRATOR: &str = "Whenever one or more cards leave your graveyard, create a tapped 2/2 black Zombie creature token.\nPartner (You can have two commanders if both have partner.)";
/// Containment Priest {1}{W} Creature — Human Cleric 2/2 — verbatim.
const CONTAINMENT_PRIEST: &str =
    "Flash\nIf a nontoken creature would enter and it wasn't cast, exile it instead.";

/// Ever After {4}{B}{B} Sorcery — verbatim.
const EVER_AFTER: &str = "Return up to two target creature cards from your graveyard to the battlefield. Each of those creatures is a black Zombie in addition to its other colors and types. Put Ever After on the bottom of its owner's library.";

/// Grizzly Bears {1}{G} Creature — Bear 2/2 (no Oracle text) in `player`'s graveyard.
fn add_grizzly_bears_to_graveyard(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_creature_to_graveyard(player, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .with_subtypes(vec!["Bear"])
        .id()
}

/// A board for one return of Grist, the Hunger Tide and Grizzly Bears from P0's graveyard with
/// Containment Priest ("If a nontoken creature would enter and it wasn't cast, exile it
/// instead.") on the battlefield, observed by Tormod, the Desecrator. CR 614.12 + CR 611.2e: the
/// Priest checks each card as it would exist on the battlefield with the return's rider applied.
/// Grist is a creature only while it isn't on the battlefield, so it is not exiled (Containment
/// Priest ruling, 2021-03-19: a creature card that wasn't cast and "is entering the battlefield as
/// a noncreature permanent … won't be exiled"); the Bears is still a creature with the rider, so
/// it is exiled instead (CR 614.6).
fn mixed_destination_board() -> (GameScenario, ObjectId, ObjectId) {
    let mut scenario = main_phase();
    add_tormod(&mut scenario);
    scenario
        .add_creature_from_oracle(P0, "Containment Priest", 2, 2, CONTAINMENT_PRIEST)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 1,
        })
        .with_subtypes(vec!["Human", "Cleric"]);
    let grist = add_grist(&mut scenario);
    let bears = add_grizzly_bears_to_graveyard(&mut scenario, P0);
    (scenario, grist, bears)
}

/// Build the mixed-destination board's runner with its layers applied.
fn build_mixed_destination(scenario: GameScenario) -> GameRunner {
    let mut runner = scenario.build();
    // Grist is a creature card in the graveyard once its own static is applied.
    runner.state_mut().layers_dirty.mark_full();
    flush_layers(runner.state_mut());
    runner
}

/// CR 603.2c + CR 603.10a + CR 614.6 + CR 205.3d: Grist entered as a planeswalker (the rider's
/// creature subtype can't apply to it), the Bears was exiled instead, and both left the graveyard
/// in one event, so Tormod ("Whenever one or more cards leave your graveyard") triggered once:
/// exactly one Zombie token.
fn assert_one_batched_trigger(runner: &GameRunner, grist: ObjectId, bears: ObjectId) {
    // Reach: one member entered and the other was exiled instead.
    let grist_obj = &runner.state().objects[&grist];
    assert_eq!(grist_obj.zone, Zone::Battlefield, "Grist is not exiled");
    assert!(
        grist_obj
            .card_types
            .core_types
            .contains(&CoreType::Planeswalker),
        "{:?}",
        grist_obj.card_types
    );
    assert!(
        !grist_obj
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "{:?}",
        grist_obj.card_types
    );
    assert_eq!(
        grist_obj.counters.get(&CounterType::Loyalty).copied(),
        Some(3)
    );
    assert_eq!(runner.state().objects[&bears].zone, Zone::Exile);
    assert_eq!(
        zombie_tokens(runner),
        1,
        "Tormod triggers once for the one event"
    );
    assert_priority_with_empty_stack(runner);
}

/// R9m — a mixed-destination group at the mass (`ChangeZoneAll`) owner: Storm of Souls returns
/// both cards; Grist enters as a planeswalker and the Bears is exiled instead.
#[test]
fn storm_of_souls_mixed_destination_return_triggers_a_batched_observer_once() {
    let (mut scenario, grist, bears) = mixed_destination_board();
    let storm = scenario
        .add_spell_to_hand_from_oracle(P0, "Storm of Souls", false, STORM_OF_SOULS)
        .id();
    let mut runner = build_mixed_destination(scenario);

    let _ = runner.cast(storm).resolve();
    runner.advance_until_stack_empty();

    assert_one_batched_trigger(&runner, grist, bears);
}

/// R8m — a mixed-destination group at the targeted (`ChangeZone`) owner: Ever After targets both
/// cards; Grist enters as a planeswalker and the Bears is exiled instead.
#[test]
fn ever_after_mixed_destination_return_triggers_a_batched_observer_once() {
    let (mut scenario, grist, bears) = mixed_destination_board();
    let ever_after = scenario
        .add_spell_to_hand_from_oracle(P0, "Ever After", false, EVER_AFTER)
        .id();
    let mut runner = build_mixed_destination(scenario);

    let _ = runner
        .cast(ever_after)
        .target_object(grist)
        .target_object(bears)
        .resolve();
    runner.advance_until_stack_empty();

    assert_one_batched_trigger(&runner, grist, bears);
}

fn zombie_tokens(runner: &GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|object| {
            object.is_token
                && object.zone == Zone::Battlefield
                && object.card_types.subtypes.iter().any(|s| s == "Zombie")
        })
        .count()
}

fn add_tormod(scenario: &mut GameScenario) {
    scenario
        .add_creature_from_oracle(P0, "Tormod, the Desecrator", 4, 2, TORMOD_THE_DESECRATOR)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 3,
        })
        .with_subtypes(vec!["Zombie", "Wizard"])
        .as_legendary();
}

/// R4t — CR 603.10a + CR 611.2e: Xu-Ifit's single entry is handed off to the post-action
/// collector whole, so its leaves-the-graveyard half still triggers Tormod ("Whenever one or more
/// cards leave your graveyard") exactly once, and the Dreadnought's own ETB does not trigger.
#[test]
fn xu_ifit_handed_off_entry_triggers_a_leaves_graveyard_observer_once() {
    let mut scenario = main_phase();
    let xu = scenario
        .add_creature_from_oracle(P0, "Xu-Ifit, Osteoharmonist", 2, 3, XU_IFIT)
        .id();
    add_tormod(&mut scenario);
    let dreadnought = add_dreadnought(&mut scenario);
    let mut runner = scenario.build();

    let _ = runner.activate(xu, 0).target_object(dreadnought).resolve();
    runner.advance_until_stack_empty();

    // Reach: the entry happened with the rider applied.
    assert_eq!(runner.state().objects[&dreadnought].zone, Zone::Battlefield);
    assert!(subtypes(&runner, dreadnought)
        .iter()
        .any(|t| t == "Skeleton"));
    assert_eq!(zombie_tokens(&runner), 1, "Tormod triggers once");
    assert_priority_with_empty_stack(&runner);
}

/// R9t — CR 603.2c + CR 603.10a + CR 611.2e: Storm of Souls hands its single-destination group off
/// whole, so both cards leaving the graveyard in one event trigger Tormod exactly once.
#[test]
fn storm_of_souls_handed_off_group_triggers_a_leaves_graveyard_observer_once() {
    let mut scenario = main_phase();
    add_tormod(&mut scenario);
    let giant = scenario
        .add_creature_to_graveyard(P0, "Hill Giant", 3, 3)
        .with_subtypes(vec!["Giant"])
        .id();
    let bears = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    let storm = scenario
        .add_spell_to_hand_from_oracle(P0, "Storm of Souls", false, STORM_OF_SOULS)
        .id();
    let mut runner = scenario.build();

    let _ = runner.cast(storm).resolve();
    runner.advance_until_stack_empty();

    // Reach: both members entered with the rider applied.
    for id in [giant, bears] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
        assert!(subtypes(&runner, id).iter().any(|t| t == "Spirit"));
    }
    assert_eq!(zombie_tokens(&runner), 1, "Tormod triggers once");
    assert_priority_with_empty_stack(&runner);
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
/// carrying `with_entry_effect`.
fn change_zone_choice(
    source: ObjectId,
    card: ObjectId,
    with_entry_effect: Option<Box<WithEntryEffect>>,
) -> WaitingFor {
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
        with_entry_effect,
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

/// R19 — CR 611.2e + CR 614.12: the `EffectZoneChoice` resume restores the carried with-entry
/// effect and never re-derives it; the carrier installs nothing itself (the rider instruction
/// does when it is followed, and the constructed frame has none), so the Ghoul gets no Skeleton
/// subtype. Constructed-state seam test: no printed card parks this choice with an
/// ability-removing rider (Xu-Ifit is targeted; every `ReturnAsAura` is a self-return). The
/// nearest printed card is Path of the Schemer ("Each player mills two cards. Then you put a
/// creature card from a graveyard onto the battlefield under your control. It's an artifact in
/// addition to its other types."), whose non-targeted graveyard-to-battlefield `ChangeZone`
/// carries a `WithEntry` "is" rider without removal; Ashiok, Nightmare Weaver's −X parks this
/// choice with a non-removing rider. The paired choice without a carried effect is the
/// positive control.
#[test]
fn effect_zone_choice_resume_restores_the_carried_with_entry_effect() {
    let skeleton_with_no_abilities = |source| {
        Some(Box::new(WithEntryEffect {
            source_id: source,
            controller: P0,
            modifications: vec![
                ContinuousModification::AddSubtype {
                    subtype: "Skeleton".to_string(),
                },
                ContinuousModification::RemoveAllAbilities,
            ],
        }))
    };
    for (removes_abilities, tapped) in [(true, false), (false, true)] {
        let mut scenario = main_phase();
        let source = scenario.add_creature(P0, "Choice Source", 1, 1).id();
        let ghoul = add_diregraf_ghoul(&mut scenario);
        let mut runner = scenario.build();
        let carried = removes_abilities
            .then(|| skeleton_with_no_abilities(source))
            .flatten();
        runner.state_mut().waiting_for = change_zone_choice(source, ghoul, carried);

        runner
            .act(GameAction::SelectCards { cards: vec![ghoul] })
            .expect("the selected card is a legal choice");

        let ghoul_obj = &runner.state().objects[&ghoul];
        assert_eq!(ghoul_obj.zone, Zone::Battlefield, "{removes_abilities}");
        assert_eq!(ghoul_obj.tapped, tapped, "{removes_abilities}");
        let types = subtypes(&runner, ghoul);
        assert!(
            !types.iter().any(|t| t == "Skeleton"),
            "the carrier installs nothing: {types:?}"
        );
    }
}

/// R19' — CR 611.2e + CR 614.12: the with-entry effect restored at the `EffectZoneChoice` resume
/// feeds the CR 614.12 read. With a carried `[AddSubtype Skeleton]` (no removal), Metallic Mimic
/// naming Skeleton ("Each other creature you control of the chosen type enters with an additional
/// +1/+1 counter on it.") sees the Diregraf Ghoul as a Skeleton: one counter, and its own "enters
/// tapped" still applies. The paired choice without a carried effect gets no counter. The carrier
/// still installs nothing (no Skeleton subtype after the entry). Synthetic frame: a printed frame
/// re-derives the same list, so only a constructed one can discriminate restoring the carried
/// effect from re-deriving it; the printed route is R-A (Ashiok, Nightmare Weaver: "That creature
/// is a Nightmare in addition to its other types.").
#[test]
fn effect_zone_choice_resume_feeds_the_carried_effect_to_the_entry_read() {
    for (carries_skeleton, counters) in [(true, 1), (false, 0)] {
        let mut scenario = main_phase();
        let source = scenario.add_creature(P0, "Choice Source", 1, 1).id();
        let mimic = add_metallic_mimic(&mut scenario, P0);
        let ghoul = add_diregraf_ghoul(&mut scenario);
        let mut runner = scenario.build();
        name_creature_type(&mut runner, mimic, "Skeleton");
        let carried = carries_skeleton.then(|| {
            Box::new(WithEntryEffect {
                source_id: source,
                controller: P0,
                modifications: vec![ContinuousModification::AddSubtype {
                    subtype: "Skeleton".to_string(),
                }],
            })
        });
        runner.state_mut().waiting_for = change_zone_choice(source, ghoul, carried);

        runner
            .act(GameAction::SelectCards { cards: vec![ghoul] })
            .expect("the selected card is a legal choice");

        let ghoul_obj = &runner.state().objects[&ghoul];
        assert_eq!(ghoul_obj.zone, Zone::Battlefield, "{carries_skeleton}");
        assert!(
            ghoul_obj.tapped,
            "its own enters-tapped: {carries_skeleton}"
        );
        assert_eq!(
            plus_one_counters(&runner, ghoul),
            counters,
            "Mimic's counter: {carries_skeleton}"
        );
        let types = subtypes(&runner, ghoul);
        assert!(
            !types.iter().any(|t| t == "Skeleton"),
            "the carrier installs nothing: {types:?}"
        );
    }
}

/// Lorcan, Warlock Collector {5}{B}{B} Legendary Creature — Devil 6/6 — verbatim.
const LORCAN_WARLOCK_COLLECTOR: &str = "Flying\nWhenever a creature card is put into an opponent's graveyard from anywhere, you may pay life equal to its mana value. If you do, put it onto the battlefield under your control. It's a Warlock in addition to its other types.\nIf a Warlock you control would die, exile it instead.";
/// Orb of Dreams {3} Artifact — verbatim.
const ORB_OF_DREAMS: &str = "Permanents enter tapped.";

/// A board where every battlefield entry pauses on a CR 616.1 ordering of Orb of Dreams's
/// "enter tapped" and Archelos's "enter untapped" (both P0's), with Murder and Zombify in P0's
/// hand. Grizzly Bears ({1}{G} Creature — Bear 2/2, no Oracle text) is on P1's battlefield or in
/// P0's graveyard.
fn ordering_pause_board(bears_in_p0_graveyard: bool) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = main_phase();
    scenario.add_creature_from_oracle(
        P0,
        "Lorcan, Warlock Collector",
        6,
        6,
        LORCAN_WARLOCK_COLLECTOR,
    );
    scenario.add_creature_from_oracle(P0, "Archelos, Lagoon Mystic", 2, 4, ARCHELOS_LAGOON_MYSTIC);
    scenario.add_artifact_from_oracle(P0, "Orb of Dreams", ORB_OF_DREAMS);
    let bears_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 1,
    };
    let bears = if bears_in_p0_graveyard {
        scenario
            .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
            .with_mana_cost(bears_cost)
            .with_subtypes(vec!["Bear"])
            .id()
    } else {
        scenario
            .add_creature(P1, "Grizzly Bears", 2, 2)
            .with_mana_cost(bears_cost)
            .with_subtypes(vec!["Bear"])
            .id()
    };
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let zombify = scenario
        .add_spell_to_hand_from_oracle(P0, "Zombify", false, ZOMBIFY)
        .id();
    (scenario.build(), bears, murder, zombify)
}

/// CR 616.1: the Bears' entry is paused on the ordering of Orb of Dreams and Archelos. Returns
/// the parked proposal's carried with-entry effect and answers the ordering with Orb of Dreams
/// first, as the prompted player, through the public `GameAction`.
fn read_pause_and_answer_orb_first(
    runner: &mut GameRunner,
    bears: ObjectId,
) -> Option<WithEntryEffect> {
    // Reach: the entry has not happened yet.
    assert_eq!(runner.state().objects[&bears].zone, Zone::Graveyard);
    let index = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => {
            for source in ["Orb of Dreams", "Archelos, Lagoon Mystic"] {
                assert!(
                    candidates.iter().any(|c| c.source_name == source),
                    "{source} must be offered: {candidates:?}"
                );
            }
            candidates
                .iter()
                .position(|c| c.source_name == "Orb of Dreams")
                .expect("Orb of Dreams must be offered")
        }
        other => panic!("expected the CR 616.1 tap-state ordering, got {other:?}"),
    };
    let pending = runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("the paused entry is parked");
    let carried = match &pending.proposed {
        ProposedEvent::ZoneChange {
            object_id,
            to: Zone::Battlefield,
            ..
        } if *object_id == bears => pending.proposed.with_entry_effect().cloned(),
        other => panic!("expected the Bears' battlefield entry, got {other:?}"),
    };
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("answer the tap-state ordering");
    runner.advance_until_stack_empty();
    assert_priority_with_empty_stack(runner);
    carried
}

/// R-L1 — CR 611.2e + CR 614.12 + CR 616.1 + CR 608.2c: P0's Murder destroys P1's Grizzly
/// Bears; Lorcan, Warlock Collector's trigger pays 2 life (its mana value) and puts the card onto
/// the battlefield under P0's control, a `ParentTarget` return bound to the trigger event. Its
/// "It's a Warlock in addition to its other types" names that same card, so the parked proposal
/// carries the Warlock rider through the CR 616.1 ordering pause, and after the answer the
/// Bears is a Warlock Bear under P0's control.
#[test]
fn lorcan_trigger_return_carries_its_warlock_rider_through_an_ordering_pause() {
    let (mut runner, bears, murder, _zombify) = ordering_pause_board(false);
    let lorcan = runner
        .state()
        .objects
        .values()
        .find(|o| o.name == "Lorcan, Warlock Collector")
        .map(|o| o.id)
        .expect("Lorcan is on the battlefield");
    let life_before = runner.life(P0);

    let _ = runner
        .cast(murder)
        .target_object(bears)
        .accept_optional()
        .resolve();
    // Reach: Lorcan's payment was made and the Bears is in P1's graveyard at the pause.
    assert_eq!(
        runner.life(P0),
        life_before - 2,
        "paid life equal to its mana value"
    );
    assert_eq!(runner.state().objects[&bears].owner, P1);

    let carried = read_pause_and_answer_orb_first(&mut runner, bears);
    assert_eq!(
        carried,
        Some(WithEntryEffect {
            source_id: lorcan,
            controller: P0,
            modifications: vec![ContinuousModification::AddSubtype {
                subtype: "Warlock".to_string(),
            }],
        }),
        "the parked proposal carries Lorcan's rider"
    );

    let bears_obj = &runner.state().objects[&bears];
    assert_eq!(bears_obj.zone, Zone::Battlefield);
    assert_eq!(bears_obj.controller, P0);
    let types = subtypes(&runner, bears);
    for expected in ["Warlock", "Bear"] {
        assert!(types.iter().any(|t| t == expected), "{expected}: {types:?}");
    }
}

/// R-L1n — control: Zombify has no rider, so the same pause carries no with-entry effect and
/// the Bears returns without becoming a Warlock.
#[test]
fn zombify_return_through_the_same_pause_carries_no_rider() {
    let (mut runner, bears, _murder, zombify) = ordering_pause_board(true);

    let _ = runner.cast(zombify).target_object(bears).resolve();

    let carried = read_pause_and_answer_orb_first(&mut runner, bears);
    assert_eq!(carried, None, "no rider, nothing carried");

    assert_eq!(runner.state().objects[&bears].zone, Zone::Battlefield);
    let types = subtypes(&runner, bears);
    assert!(!types.iter().any(|t| t == "Warlock"), "{types:?}");
    assert!(types.iter().any(|t| t == "Bear"), "{types:?}");
}

// ---------------------------------------------------------------------------
// Consequence (c): external entry replacements and `EntersWithAdditionalCounters` statics read
// the entrant with the resolution's simultaneous is/has effect applied (CR 614.12 + CR 611.2e).
// ---------------------------------------------------------------------------

/// Metallic Mimic {2} Artifact Creature — Shapeshifter 2/1 — verbatim.
const METALLIC_MIMIC: &str = "As this creature enters, choose a creature type.\nThis creature is the chosen type in addition to its other types.\nEach other creature you control of the chosen type enters with an additional +1/+1 counter on it.";
/// Valkyrie's Call {3}{W}{W} Enchantment — verbatim.
const VALKYRIES_CALL: &str = "Whenever a nontoken, non-Angel creature you control dies, return that card to the battlefield under its owner's control with a +1/+1 counter on it. It has flying and is an Angel in addition to its other types.";
/// Ashiok, Nightmare Weaver {1}{U}{B} Legendary Planeswalker — Ashiok, loyalty 3 — verbatim
/// (card-data form of the loyalty costs).
const ASHIOK_NIGHTMARE_WEAVER: &str = "[+2]: Exile the top three cards of target opponent's library.\n[\u{2212}X]: Put a creature card with mana value X exiled with Ashiok onto the battlefield under your control. That creature is a Nightmare in addition to its other types.\n[\u{2212}10]: Exile all cards from all opponents' hands and graveyards.";

/// Metallic Mimic on `player`'s battlefield, with no creature type chosen yet.
fn add_metallic_mimic(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_creature_from_oracle(player, "Metallic Mimic", 2, 1, METALLIC_MIMIC)
        .as_artifact_creature()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 2,
        })
        .with_subtypes(vec!["Shapeshifter"])
        .id()
}

/// The Mimic already on the battlefield chose `chosen` as it entered (board state).
fn name_creature_type(runner: &mut GameRunner, mimic: ObjectId, chosen: &str) {
    runner
        .state_mut()
        .objects
        .get_mut(&mimic)
        .expect("the Mimic is on the board")
        .chosen_attributes
        .push(ChosenAttribute::CreatureType(chosen.to_string()));
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());
}

/// CR 607.2a: `exiled` is a card exiled with `source`.
fn link_exiled_with(runner: &mut GameRunner, exiled: ObjectId, source: ObjectId) {
    runner.state_mut().exile_links.push(ExileLink {
        exiled_id: exiled,
        source_id: source,
        kind: ExileLinkKind::TrackedBySource,
    });
}

/// CR 606.4 + CR 107.3: activate `pw`'s loyalty ability `index`, announcing X = `x` for its
/// `[−X]` cost, and stop at the next prompt.
fn activate_loyalty_x(runner: &mut GameRunner, pw: ObjectId, index: usize, x: u32) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: pw,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("loyalty activation announced");
    if matches!(runner.state().waiting_for, WaitingFor::ChooseXValue { .. }) {
        runner
            .act(GameAction::ChooseX { value: x })
            .expect("announce X for the loyalty cost");
    }
}

fn hill_giant_in_graveyard(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_creature_to_graveyard(player, "Hill Giant", 3, 3)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 3,
        })
        .with_subtypes(vec!["Giant"])
        .id()
}

fn grizzly_bears_on_battlefield(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_creature(player, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .with_subtypes(vec!["Bear"])
        .id()
}

/// R20 (B1) — CR 614.12 + CR 611.2e: Xu-Ifit's "It's a Skeleton in addition to its other types and
/// has no abilities" applies with the entry, so Metallic Mimic naming Skeleton sees the returned
/// Barkhide Troll as a Skeleton and gives it a counter, while the Troll's own "enters with a +1/+1
/// counter" is an ability removed with the entry: exactly one counter.
#[test]
fn xu_ifit_returned_troll_gets_mimics_counter_for_its_skeleton_type() {
    let (mut scenario, xu) = returner_board(Returner::XuIfit);
    let mimic = add_metallic_mimic(&mut scenario, P0);
    let troll = add_barkhide_troll(&mut scenario);
    let mut runner = scenario.build();
    name_creature_type(&mut runner, mimic, "Skeleton");

    return_card(&mut runner, Returner::XuIfit, xu, troll);

    let troll_obj = &runner.state().objects[&troll];
    assert_eq!(troll_obj.zone, Zone::Battlefield);
    assert!(subtypes(&runner, troll).iter().any(|t| t == "Skeleton"));
    assert!(troll_obj.abilities.is_empty(), "the Troll has no abilities");
    assert_eq!(
        plus_one_counters(&runner, troll),
        1,
        "Metallic Mimic's only"
    );
    assert_priority_with_empty_stack(&runner);
}

/// R20n — control: without Metallic Mimic the Troll's own counter stays withheld (0).
#[test]
fn xu_ifit_returned_troll_without_mimic_gets_no_counter() {
    let (mut scenario, xu) = returner_board(Returner::XuIfit);
    let troll = add_barkhide_troll(&mut scenario);
    let mut runner = scenario.build();

    return_card(&mut runner, Returner::XuIfit, xu, troll);

    assert_eq!(runner.state().objects[&troll].zone, Zone::Battlefield);
    assert!(subtypes(&runner, troll).iter().any(|t| t == "Skeleton"));
    assert_eq!(plus_one_counters(&runner, troll), 0);
    assert_priority_with_empty_stack(&runner);
}

/// R20c — control: Zombify has no rider, so the Troll is not a Skeleton and Mimic naming Skeleton
/// does not apply; only the Troll's own counter (CR 614.1c), with no CR 616.1 ordering offered.
#[test]
fn zombify_returned_troll_gets_only_its_own_counter_beside_a_skeleton_mimic() {
    let (mut scenario, zombify) = returner_board(Returner::Zombify);
    let mimic = add_metallic_mimic(&mut scenario, P0);
    let troll = add_barkhide_troll(&mut scenario);
    let mut runner = scenario.build();
    name_creature_type(&mut runner, mimic, "Skeleton");

    return_card(&mut runner, Returner::Zombify, zombify, troll);

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "no ordering: {:?}",
        runner.state().waiting_for
    );
    let troll_obj = &runner.state().objects[&troll];
    assert_eq!(troll_obj.zone, Zone::Battlefield);
    assert_eq!(troll_obj.abilities.len(), 1);
    assert_eq!(plus_one_counters(&runner, troll), 1, "its own counter only");
    assert_priority_with_empty_stack(&runner);
}

/// A Rise from the Grave board: Hill Giant in P1's graveyard, and Metallic Mimic naming Zombie
/// controlled by `mimic_controller`.
fn rise_from_the_grave_mimic_board(mimic_controller: PlayerId) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = main_phase();
    let mimic = add_metallic_mimic(&mut scenario, mimic_controller);
    let giant = hill_giant_in_graveyard(&mut scenario, P1);
    let rise = scenario
        .add_spell_to_hand_from_oracle(P0, "Rise from the Grave", false, RISE_FROM_THE_GRAVE)
        .id();
    let mut runner = scenario.build();
    name_creature_type(&mut runner, mimic, "Zombie");
    (runner, rise, giant)
}

/// The Giant is on the battlefield under P0, owned by P1, a black Zombie.
fn assert_risen_black_zombie(runner: &GameRunner, giant: ObjectId) {
    let giant_obj = &runner.state().objects[&giant];
    assert_eq!(giant_obj.zone, Zone::Battlefield);
    assert_eq!(giant_obj.controller, P0);
    assert_eq!(giant_obj.owner, P1);
    assert!(giant_obj.card_types.subtypes.iter().any(|s| s == "Zombie"));
    assert!(giant_obj.color.contains(&ManaColor::Black));
}

/// R21 — CR 614.12 + CR 611.2e + CR 110.2a: Rise from the Grave puts P1's Hill Giant onto the
/// battlefield under P0's control as "a black Zombie in addition to its other colors and types",
/// so P0's Metallic Mimic naming Zombie gives it a counter.
#[test]
fn rise_from_the_grave_returned_giant_gets_mimics_counter() {
    let (mut runner, rise, giant) = rise_from_the_grave_mimic_board(P0);

    let _ = runner.cast(rise).target_object(giant).resolve();

    assert_risen_black_zombie(&runner, giant);
    assert_eq!(plus_one_counters(&runner, giant), 1);
    assert_priority_with_empty_stack(&runner);
}

/// R21h — hostile (multi-authority), CR 110.2a: the Giant is checked under the player it enters
/// under (P0), not its owner (P1), so P1's Metallic Mimic naming Zombie ("Each other creature you
/// control") gives it no counter.
#[test]
fn rise_from_the_grave_returned_giant_is_checked_under_its_new_controller() {
    let (mut runner, rise, giant) = rise_from_the_grave_mimic_board(P1);

    let _ = runner.cast(rise).target_object(giant).resolve();

    assert_risen_black_zombie(&runner, giant);
    assert_eq!(plus_one_counters(&runner, giant), 0);
    assert_priority_with_empty_stack(&runner);
}

/// R22 — CR 614.12 + CR 611.2e, the mass route: Storm of Souls returns Hill Giant and Grizzly
/// Bears, "Each of them is a 1/1 Spirit with flying", so Metallic Mimic naming Spirit gives each
/// a counter: each is a 2/2.
#[test]
fn storm_of_souls_returned_creatures_each_get_mimics_counter() {
    let mut scenario = main_phase();
    let mimic = add_metallic_mimic(&mut scenario, P0);
    let giant = hill_giant_in_graveyard(&mut scenario, P0);
    let bears = add_grizzly_bears_to_graveyard(&mut scenario, P0);
    let storm = scenario
        .add_spell_to_hand_from_oracle(P0, "Storm of Souls", false, STORM_OF_SOULS)
        .id();
    let mut runner = scenario.build();
    name_creature_type(&mut runner, mimic, "Spirit");

    let _ = runner.cast(storm).resolve();

    assert_eq!(runner.state().objects[&storm].zone, Zone::Exile);
    for id in [giant, bears] {
        let obj = &runner.state().objects[&id];
        assert_eq!(obj.zone, Zone::Battlefield);
        assert!(subtypes(&runner, id).iter().any(|t| t == "Spirit"));
        assert_eq!(plus_one_counters(&runner, id), 1, "{}", obj.name);
        assert_eq!(
            (obj.power, obj.toughness),
            (Some(2), Some(2)),
            "{}",
            obj.name
        );
    }
    assert_priority_with_empty_stack(&runner);
}

/// R23 — CR 614.12 + CR 611.2e: Ever After's "Each of those creatures is a black Zombie" names
/// the chain's tracked set, and Metallic Mimic naming Zombie gives the Giant a counter.
#[test]
fn ever_after_returned_giant_gets_mimics_counter() {
    let mut scenario = main_phase();
    let mimic = add_metallic_mimic(&mut scenario, P0);
    let giant = hill_giant_in_graveyard(&mut scenario, P0);
    let ever_after = scenario
        .add_spell_to_hand_from_oracle(P0, "Ever After", false, EVER_AFTER)
        .id();
    let mut runner = scenario.build();
    name_creature_type(&mut runner, mimic, "Zombie");

    let _ = runner.cast(ever_after).target_object(giant).resolve();

    let giant_obj = &runner.state().objects[&giant];
    assert_eq!(giant_obj.zone, Zone::Battlefield);
    assert!(giant_obj.card_types.subtypes.iter().any(|s| s == "Zombie"));
    assert!(giant_obj.color.contains(&ManaColor::Black));
    assert_eq!(
        runner.state().players[0].library.back(),
        Some(&ever_after),
        "Ever After is on the bottom of its owner's library"
    );
    assert_eq!(plus_one_counters(&runner, giant), 1);
    assert_priority_with_empty_stack(&runner);
}

/// A Valkyrie's Call board: Grizzly Bears and Metallic Mimic naming `chosen` on P0's
/// battlefield, Murder in P0's hand.
fn valkyries_call_board(chosen: &str) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = main_phase();
    scenario.add_enchantment_from_oracle(P0, "Valkyrie's Call", VALKYRIES_CALL);
    let mimic = add_metallic_mimic(&mut scenario, P0);
    let bears = grizzly_bears_on_battlefield(&mut scenario, P0);
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = scenario.build();
    name_creature_type(&mut runner, mimic, chosen);
    (runner, bears, murder)
}

/// Murder `victim`, then let every trigger resolve.
fn murder_and_settle(runner: &mut GameRunner, murder: ObjectId, victim: ObjectId) {
    let _ = runner.cast(murder).target_object(victim).resolve();
    runner.advance_until_stack_empty();
}

/// R25 — CR 614.12 + CR 611.2e, a trigger return: Valkyrie's Call returns the dead Bears "with a
/// +1/+1 counter on it. It has flying and is an Angel in addition to its other types." (its rider
/// names the triggering object), so Metallic Mimic naming Angel adds a second counter. The Angel
/// subtype is asserted, not flying (the rider's flying is a pre-existing parse defect).
#[test]
fn valkyries_call_returned_bears_gets_mimics_counter_as_an_angel() {
    let (mut runner, bears, murder) = valkyries_call_board("Angel");

    murder_and_settle(&mut runner, murder, bears);

    assert_eq!(runner.state().objects[&bears].zone, Zone::Battlefield);
    assert!(subtypes(&runner, bears).iter().any(|t| t == "Angel"));
    assert_eq!(plus_one_counters(&runner, bears), 2);
    assert_priority_with_empty_stack(&runner);
}

/// R25c — control: Metallic Mimic naming Elf does not apply to a Bear Angel; Valkyrie's Call's own
/// counter is the only one.
#[test]
fn valkyries_call_returned_bears_beside_an_elf_mimic_gets_one_counter() {
    let (mut runner, bears, murder) = valkyries_call_board("Elf");

    murder_and_settle(&mut runner, murder, bears);

    assert_eq!(runner.state().objects[&bears].zone, Zone::Battlefield);
    assert!(subtypes(&runner, bears).iter().any(|t| t == "Angel"));
    assert_eq!(plus_one_counters(&runner, bears), 1);
    assert_priority_with_empty_stack(&runner);
}

/// A Lorcan board: Lorcan, Warlock Collector and Metallic Mimic naming `chosen` on P0's
/// battlefield with Murder in P0's hand, and P1's Grizzly Bears on the battlefield.
fn lorcan_mimic_board(chosen: &str) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = main_phase();
    scenario.add_creature_from_oracle(
        P0,
        "Lorcan, Warlock Collector",
        6,
        6,
        LORCAN_WARLOCK_COLLECTOR,
    );
    let mimic = add_metallic_mimic(&mut scenario, P0);
    let bears = grizzly_bears_on_battlefield(&mut scenario, P1);
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = scenario.build();
    name_creature_type(&mut runner, mimic, chosen);
    (runner, bears, murder)
}

/// Murder P1's Bears and pay Lorcan's life; the Bears returns under P0 as a Warlock Bear.
fn murder_for_lorcan(runner: &mut GameRunner, murder: ObjectId, bears: ObjectId) {
    let life_before = runner.life(P0);
    let _ = runner
        .cast(murder)
        .target_object(bears)
        .accept_optional()
        .resolve();
    runner.advance_until_stack_empty();
    // Reach: Lorcan's payment was made and the Bears returned with its rider.
    assert_eq!(
        runner.life(P0),
        life_before - 2,
        "paid life equal to its mana value"
    );
    let bears_obj = &runner.state().objects[&bears];
    assert_eq!(bears_obj.zone, Zone::Battlefield);
    assert_eq!(bears_obj.controller, P0);
    assert_eq!(bears_obj.owner, P1);
    let types = subtypes(runner, bears);
    for expected in ["Warlock", "Bear"] {
        assert!(types.iter().any(|t| t == expected), "{expected}: {types:?}");
    }
}

/// R-L2 — CR 614.12 + CR 611.2e + CR 110.2a: Lorcan, Warlock Collector puts P1's dead Grizzly
/// Bears onto the battlefield under P0's control; "It's a Warlock in addition to its other types."
/// applies with the entry, so P0's Metallic Mimic naming Warlock gives it a counter.
#[test]
fn lorcan_returned_bears_gets_mimics_counter_as_a_warlock() {
    let (mut runner, bears, murder) = lorcan_mimic_board("Warlock");

    murder_for_lorcan(&mut runner, murder, bears);

    assert_eq!(plus_one_counters(&runner, bears), 1);
    assert_priority_with_empty_stack(&runner);
}

/// R-L2c — control: Metallic Mimic naming Elf does not apply to a Warlock Bear.
#[test]
fn lorcan_returned_bears_beside_an_elf_mimic_gets_no_counter() {
    let (mut runner, bears, murder) = lorcan_mimic_board("Elf");

    murder_for_lorcan(&mut runner, murder, bears);

    assert_eq!(plus_one_counters(&runner, bears), 0);
    assert_priority_with_empty_stack(&runner);
}

/// An Ashiok board: Ashiok, Nightmare Weaver (loyalty 3) and Metallic Mimic naming `chosen` on
/// P0's battlefield, and P1's Grizzly Bears and Runeclaw Bear ({1}{G} Creature — Bear 2/2, no
/// Oracle text) exiled with Ashiok.
fn ashiok_board(chosen: &str) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = main_phase();
    let ashiok = scenario
        .add_planeswalker_from_oracle(
            P0,
            "Ashiok, Nightmare Weaver",
            "Ashiok",
            3,
            ASHIOK_NIGHTMARE_WEAVER,
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Black],
            generic: 1,
        })
        .as_legendary()
        .id();
    let mimic = add_metallic_mimic(&mut scenario, P0);
    let mut exiled_bear = |name: &str| {
        scenario
            .add_creature_to_exile(P1, name, 2, 2)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 1,
            })
            .with_subtypes(vec!["Bear"])
            .id()
    };
    let bears = exiled_bear("Grizzly Bears");
    let runeclaw = exiled_bear("Runeclaw Bear");
    let mut runner = scenario.build();
    name_creature_type(&mut runner, mimic, chosen);
    for exiled in [bears, runeclaw] {
        link_exiled_with(&mut runner, exiled, ashiok);
    }
    (runner, ashiok, bears, runeclaw)
}

/// Activate Ashiok's −X with X = 2, resolve it to the zone choice, and put the Bears onto the
/// battlefield. Reach: the choice is the `EffectZoneChoice` offering both exiled cards and
/// carrying Ashiok's Nightmare rider; afterwards the Bears is a Nightmare under P0's control, the
/// Runeclaw Bear is still exiled, and Ashiok has 1 loyalty.
fn ashiok_returns_the_bears(runner: &mut GameRunner, ashiok: ObjectId, bears: ObjectId) {
    activate_loyalty_x(runner, ashiok, 1, 2);
    runner.resolve_top();
    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice {
            cards,
            with_entry_effect,
            ..
        } => {
            assert_eq!(cards.len(), 2, "both exiled Bears are eligible: {cards:?}");
            assert!(cards.contains(&bears), "{cards:?}");
            assert_eq!(
                with_entry_effect.as_deref(),
                Some(&WithEntryEffect {
                    source_id: ashiok,
                    controller: P0,
                    modifications: vec![ContinuousModification::AddSubtype {
                        subtype: "Nightmare".to_string(),
                    }],
                }),
                "the parked choice carries Ashiok's rider"
            );
        }
        other => panic!("expected Ashiok's zone choice, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards { cards: vec![bears] })
        .expect("the Bears is a legal choice");
    runner.advance_until_stack_empty();

    let bears_obj = &runner.state().objects[&bears];
    assert_eq!(bears_obj.zone, Zone::Battlefield);
    assert_eq!(bears_obj.controller, P0);
    assert!(subtypes(runner, bears).iter().any(|t| t == "Nightmare"));
    assert_eq!(
        runner.state().objects[&ashiok]
            .counters
            .get(&CounterType::Loyalty)
            .copied(),
        Some(1)
    );
}

/// R-A — CR 614.12 + CR 611.2e: Ashiok, Nightmare Weaver's "[−X]: Put a creature card with mana
/// value X exiled with Ashiok onto the battlefield under your control. That creature is a
/// Nightmare in addition to its other types." parks an `EffectZoneChoice` between two exiled
/// Bears; the resume restores the carried Nightmare rider, so Metallic Mimic naming Nightmare
/// gives the chosen Bears a counter.
#[test]
fn ashiok_returned_bears_gets_mimics_counter_as_a_nightmare() {
    let (mut runner, ashiok, bears, runeclaw) = ashiok_board("Nightmare");

    ashiok_returns_the_bears(&mut runner, ashiok, bears);

    assert_eq!(runner.state().objects[&runeclaw].zone, Zone::Exile);
    assert_eq!(plus_one_counters(&runner, bears), 1);
    assert_priority_with_empty_stack(&runner);
}

/// R-Ac — control: Metallic Mimic naming Elf does not apply to a Nightmare Bear.
#[test]
fn ashiok_returned_bears_beside_an_elf_mimic_gets_no_counter() {
    let (mut runner, ashiok, bears, runeclaw) = ashiok_board("Elf");

    ashiok_returns_the_bears(&mut runner, ashiok, bears);

    assert_eq!(runner.state().objects[&runeclaw].zone, Zone::Exile);
    assert_eq!(plus_one_counters(&runner, bears), 0);
    assert_priority_with_empty_stack(&runner);
}

/// Enduring Innocence {1}{W}{W} Enchantment Creature — Sheep Glimmer 2/1 on P0's battlefield.
fn add_enduring_innocence(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, "Enduring Innocence", 2, 1, ENDURING_INNOCENCE)
        .as_enchantment()
        .as_creature()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::White],
            generic: 1,
        })
        .with_subtypes(vec!["Sheep", "Glimmer"])
        .id()
}

/// Innocence is back on the battlefield as an enchantment that is not a creature.
fn assert_returned_as_noncreature_enchantment(runner: &GameRunner, innocence: ObjectId) {
    let obj = &runner.state().objects[&innocence];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(
        obj.card_types.core_types.contains(&CoreType::Enchantment),
        "{:?}",
        obj.card_types
    );
    assert!(
        !obj.card_types.core_types.contains(&CoreType::Creature),
        "{:?}",
        obj.card_types
    );
}

/// R27 — CR 614.12 + CR 611.2e + CR 205.1a: Enduring Innocence returns "It's an enchantment.
/// (It's not a creature.)", so P1's Imposing Sovereign ("Creatures your opponents control enter
/// tapped.") does not apply: it enters untapped.
#[test]
fn enduring_innocence_returns_untapped_past_imposing_sovereign() {
    let mut scenario = main_phase();
    let innocence = add_enduring_innocence(&mut scenario);
    scenario.add_creature_from_oracle(P1, "Imposing Sovereign", 2, 1, IMPOSING_SOVEREIGN);
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = scenario.build();

    murder_and_settle(&mut runner, murder, innocence);

    assert_returned_as_noncreature_enchantment(&runner, innocence);
    assert!(!runner.state().objects[&innocence].tapped);
    assert_priority_with_empty_stack(&runner);
}

/// R28 — CR 614.12 + CR 611.2e + CR 614.1c, the delivered read: Enduring Innocence returns as a
/// noncreature enchantment, so Deanna Troi's "Other creatures you control enter with an
/// additional +1/+1 counter on them." does not apply: no counter.
#[test]
fn enduring_innocence_returns_without_deanna_trois_counter() {
    let mut scenario = main_phase();
    let innocence = add_enduring_innocence(&mut scenario);
    scenario.add_creature_from_oracle(P0, "Deanna Troi, Psychic Counsel", 2, 2, DEANNA_TROI);
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = scenario.build();

    murder_and_settle(&mut runner, murder, innocence);

    assert_returned_as_noncreature_enchantment(&runner, innocence);
    assert_eq!(plus_one_counters(&runner, innocence), 0);
    assert_priority_with_empty_stack(&runner);
}

/// R29 — CR 614.12 + CR 611.2e + CR 303.4: Bronzehide Lion returns as "an Aura enchantment with
/// enchant creature you control", attached to P0's Grizzly Bears (its only candidate), so P1's
/// Imposing Sovereign does not apply: it enters untapped.
#[test]
fn bronzehide_lion_returns_untapped_past_imposing_sovereign() {
    let mut scenario = main_phase();
    let lion = scenario
        .add_creature_from_oracle(P0, "Bronzehide Lion", 3, 3, BRONZEHIDE_LION)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::White],
            generic: 0,
        })
        .with_subtypes(vec!["Cat"])
        .id();
    let bears = grizzly_bears_on_battlefield(&mut scenario, P0);
    scenario.add_creature_from_oracle(P1, "Imposing Sovereign", 2, 1, IMPOSING_SOVEREIGN);
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = scenario.build();

    murder_and_settle(&mut runner, murder, lion);

    let lion_obj = &runner.state().objects[&lion];
    assert_eq!(lion_obj.zone, Zone::Battlefield);
    assert!(lion_obj.card_types.subtypes.iter().any(|s| s == "Aura"));
    assert!(
        !lion_obj.card_types.core_types.contains(&CoreType::Creature),
        "{:?}",
        lion_obj.card_types
    );
    assert_eq!(lion_obj.attached_to, Some(AttachTarget::Object(bears)));
    assert!(!lion_obj.tapped);
    assert_priority_with_empty_stack(&runner);
}

/// Living Death {3}{B}{B} Sorcery — verbatim.
const LIVING_DEATH: &str = "Each player exiles all creature cards from their graveyard, then sacrifices all creatures they control, then puts all cards they exiled this way onto the battlefield.";
/// Carmen, Cruel Skymarcher {3}{W}{B} Legendary Creature — Vampire Soldier 2/2 — verbatim.
const CARMEN_CRUEL_SKYMARCHER: &str = "Flying\nWhenever a player sacrifices a permanent, put a +1/+1 counter on Carmen and you gain 1 life.\nWhenever Carmen attacks, return up to one target permanent card with mana value less than or equal to Carmen's power from your graveyard to the battlefield.";
/// Impact Tremors {1}{R} Enchantment — verbatim.
const IMPACT_TREMORS: &str =
    "Whenever a creature you control enters, this enchantment deals 1 damage to each opponent.";
/// Village Rites {B} Instant — verbatim.
const VILLAGE_RITES: &str =
    "As an additional cost to cast this spell, sacrifice a creature.\nDraw two cards.";

/// A card a mass-return board stages in a graveyard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GraveyardCard {
    HillGiant,
    SoulWarden,
    CarmenCruelSkymarcher,
    FamishedWorldsire,
    MetallicMimic,
    ContainmentPriest,
}

impl GraveyardCard {
    /// Put this card into `player`'s graveyard and return its id.
    fn add_to_graveyard(self, scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
        match self {
            GraveyardCard::HillGiant => hill_giant_in_graveyard(scenario, player),
            GraveyardCard::SoulWarden => scenario
                .add_creature_to_graveyard(player, "Soul Warden", 1, 1)
                .with_subtypes(vec!["Human", "Cleric"])
                .from_oracle_text(SOUL_WARDEN)
                .id(),
            GraveyardCard::CarmenCruelSkymarcher => scenario
                .add_creature_to_graveyard(player, "Carmen, Cruel Skymarcher", 2, 2)
                .with_subtypes(vec!["Vampire", "Soldier"])
                .from_oracle_text(CARMEN_CRUEL_SKYMARCHER)
                .id(),
            GraveyardCard::FamishedWorldsire => scenario
                .add_creature_to_graveyard(player, "Famished Worldsire", 0, 0)
                .with_subtypes(vec!["Leviathan"])
                .from_oracle_text_with_keywords(&["Ward {3}", "Devour land 3"], FAMISHED_WORLDSIRE)
                .id(),
            GraveyardCard::MetallicMimic => metallic_mimic_in_graveyard(scenario, player),
            GraveyardCard::ContainmentPriest => containment_priest_in_graveyard(scenario, player),
        }
    }
}

/// The permanent a Living Death board puts on P0's battlefield before the cast.
#[derive(Clone, Copy, Debug)]
enum Resident {
    ImpactTremors,
    GrizzlyBears,
}

/// Position of `id`'s battlefield `ZoneChanged` in `events`.
fn battlefield_entry_index(events: &[GameEvent], id: ObjectId) -> usize {
    events
        .iter()
        .position(|event| {
            matches!(
                event,
                GameEvent::ZoneChanged {
                    object_id,
                    to: Zone::Battlefield,
                    ..
                } if *object_id == id
            )
        })
        .unwrap_or_else(|| panic!("{id:?} has no battlefield entry in {events:?}"))
}

/// Every ordering of `0..n`.
fn index_permutations(n: usize) -> Vec<Vec<usize>> {
    if n == 0 {
        return vec![Vec::new()];
    }
    index_permutations(n - 1)
        .into_iter()
        .flat_map(|rest| {
            (0..n).map(move |slot| {
                let mut ordering = rest.clone();
                ordering.insert(slot, n - 1);
                ordering
            })
        })
        .collect()
}

/// Where a driven board observed each card's battlefield entry.
trait ObservedEntryOrder {
    /// Position of `id`'s battlefield entry among the run's observed zone changes.
    fn battlefield_entry_position(&self, id: ObjectId) -> usize;
}

impl ObservedEntryOrder for CastOutcome {
    fn battlefield_entry_position(&self, id: ObjectId) -> usize {
        battlefield_entry_index(self.events(), id)
    }
}

/// A run stopped before its cast outcome exists reads the per-turn zone-change ledger.
impl ObservedEntryOrder for GameRunner {
    fn battlefield_entry_position(&self, id: ObjectId) -> usize {
        ledger_battlefield_entry_position(self.state(), id)
    }
}

/// Position of `id`'s first battlefield row in the per-turn zone-change ledger.
fn ledger_battlefield_entry_position(state: &GameState, id: ObjectId) -> usize {
    state
        .zone_changes_this_turn
        .iter()
        .position(|record| record.object_id == id && record.to_zone == Zone::Battlefield)
        .unwrap_or_else(|| panic!("{id:?} has no battlefield row in the zone-change ledger"))
}

/// `id`'s first battlefield row in the per-turn zone-change ledger.
fn ledger_battlefield_entry(state: &GameState, id: ObjectId) -> &ZoneChangeRecord {
    &state.zone_changes_this_turn[ledger_battlefield_entry_position(state, id)]
}

/// Run a mass-return board so its cards enter the battlefield in `order`.
///
/// The mass return delivers its cards in the object store's iteration order, which is fixed for
/// a set of ids, so the creation order chooses which card holds which id. `run` stages the cards
/// in the creation order it is given and returns their ids in that order. Every creation order is
/// tried, and the first outcome whose observed battlefield entries follow `order` is returned,
/// with the ids aligned to `order`.
fn with_entry_order<const N: usize, R: ObservedEntryOrder>(
    order: [GraveyardCard; N],
    run: impl Fn([GraveyardCard; N]) -> (R, [ObjectId; N]),
) -> (R, [ObjectId; N]) {
    for ordering in index_permutations(N) {
        let creation: [GraveyardCard; N] = std::array::from_fn(|k| order[ordering[k]]);
        let (outcome, created) = run(creation);
        let mut ids = created;
        for (k, &slot) in ordering.iter().enumerate() {
            ids[slot] = created[k];
        }
        let entries: Vec<usize> = ids
            .iter()
            .map(|&id| outcome.battlefield_entry_position(id))
            .collect();
        if entries.windows(2).all(|pair| pair[0] < pair[1]) {
            return (outcome, ids);
        }
    }
    panic!("no creation order delivered {order:?}");
}

/// Stage `creation` in P0's graveyard, in that order, and return their ids.
fn stage_in_graveyard<const N: usize>(
    scenario: &mut GameScenario,
    creation: [GraveyardCard; N],
) -> [ObjectId; N] {
    creation.map(|card| card.add_to_graveyard(scenario, P0))
}

/// Storm of Souls in P0's hand and `creation` in P0's graveyard, on an empty battlefield. Returns
/// the scenario, Storm of Souls and the staged ids.
fn storm_scenario(creation: [GraveyardCard; 2]) -> (GameScenario, ObjectId, [ObjectId; 2]) {
    let mut scenario = main_phase();
    let ids = stage_in_graveyard(&mut scenario, creation);
    let storm = scenario
        .add_spell_to_hand_from_oracle(P0, "Storm of Souls", false, STORM_OF_SOULS)
        .id();
    (scenario, storm, ids)
}

/// Storm of Souls in P0's hand and `creation` in P0's graveyard, on an empty battlefield; cast
/// and resolve it.
fn storm_board(creation: [GraveyardCard; 2]) -> (CastOutcome, [ObjectId; 2]) {
    let (scenario, storm, ids) = storm_scenario(creation);
    let mut runner = scenario.build();
    (runner.cast(storm).resolve(), ids)
}

/// Living Death in P0's hand, `creation` in P0's graveyard and `resident` on P0's battlefield.
/// Returns the scenario, Living Death and the staged ids.
fn living_death_scenario(
    creation: [GraveyardCard; 2],
    resident: Resident,
) -> (GameScenario, ObjectId, [ObjectId; 2]) {
    let mut scenario = main_phase();
    let ids = stage_in_graveyard(&mut scenario, creation);
    match resident {
        Resident::ImpactTremors => {
            scenario.add_enchantment_from_oracle(P0, "Impact Tremors", IMPACT_TREMORS);
        }
        Resident::GrizzlyBears => {
            grizzly_bears_on_battlefield(&mut scenario, P0);
        }
    }
    let living_death = scenario
        .add_spell_to_hand_from_oracle(P0, "Living Death", false, LIVING_DEATH)
        .id();
    (scenario, living_death, ids)
}

/// Living Death in P0's hand, `creation` in P0's graveyard and `resident` on P0's battlefield;
/// cast and resolve it.
fn living_death_board(
    creation: [GraveyardCard; 2],
    resident: Resident,
) -> (CastOutcome, [ObjectId; 2]) {
    let (scenario, living_death, ids) = living_death_scenario(creation, resident);
    let mut runner = scenario.build();
    (runner.cast(living_death).resolve(), ids)
}

/// Pass priority one action at a time while `spell` is on the stack.
fn pass_until_off_the_stack(runner: &mut GameRunner, spell: ObjectId) {
    let spell_on_stack = |runner: &GameRunner| {
        runner.state().stack.iter().any(|entry| {
            matches!(entry.kind, StackEntryKind::Spell { .. }) && entry.source_id == spell
        })
    };
    for _ in 0..4 {
        if !spell_on_stack(runner) {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority passes while the spell is on the stack");
    }
    assert!(
        !spell_on_stack(runner),
        "{spell:?} is still on the stack: {:?}",
        runner.state().waiting_for
    );
}

/// Pass priority one action at a time until P0 is asked for a devour sacrifice.
fn pass_until_devour_choice(runner: &mut GameRunner) {
    let at_devour_choice = |runner: &GameRunner| {
        matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice {
                player: P0,
                effect_kind: EffectKind::Sacrifice,
                ..
            }
        )
    };
    for _ in 0..4 {
        if at_devour_choice(runner) {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority passes before the devour choice");
    }
    assert!(
        at_devour_choice(runner),
        "no devour choice: {:?}",
        runner.state().waiting_for
    );
}

/// Serialize `state` as a raw or a trusted save and restore it.
fn restore_round_trip(state: &GameState, trusted: bool) -> Result<GameState, String> {
    let persisted = if trusted {
        PersistedGameState::capture(state.clone())
    } else {
        PersistedGameState::Raw(Box::new(state.clone()))
    };
    let json = serde_json::to_string(&persisted).map_err(|error| error.to_string())?;
    serde_json::from_str::<PersistedGameState>(&json)
        .map_err(|error| error.to_string())?
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .map_err(|error| error.to_string())?
        .finalize_after_rehydration(|_| Ok(()))
        .map_err(|error| error.to_string())
}

/// The record of the battlefield entry that caused the stack's only entry, a triggered ability
/// of `source`.
fn sole_trigger_event_record(state: &GameState, source: ObjectId) -> &ZoneChangeRecord {
    assert_eq!(state.stack.len(), 1, "stack: {:?}", state.stack);
    match &state.stack[0].kind {
        StackEntryKind::TriggeredAbility {
            source_id,
            trigger_event:
                Some(GameEvent::ZoneChanged {
                    to: Zone::Battlefield,
                    record,
                    ..
                }),
            ..
        } if *source_id == source => record,
        other => panic!("expected {source:?}'s entry trigger, got {other:?}"),
    }
}

fn assert_outcome_priority_with_empty_stack(outcome: &CastOutcome) {
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "waiting_for: {:?}",
        outcome.final_waiting_for()
    );
    assert!(outcome.state().stack.is_empty(), "stack must be empty");
}

/// Storm of Souls' returned cards are on the battlefield as Spirits (its rider applied), and
/// they entered in the listed order.
fn assert_storm_returned_in_order(outcome: &CastOutcome, ids: [ObjectId; 2]) {
    for id in ids {
        let obj = &outcome.state().objects[&id];
        assert_eq!(obj.zone, Zone::Battlefield, "{}", obj.name);
        assert!(
            obj.card_types.subtypes.iter().any(|s| s == "Spirit"),
            "{}: {:?}",
            obj.name,
            obj.card_types.subtypes
        );
    }
    assert!(
        battlefield_entry_index(outcome.events(), ids[0])
            < battlefield_entry_index(outcome.events(), ids[1])
    );
}

/// R30a — CR 603.6a: Storm of Souls returns Hill Giant and Soul Warden in one event. Soul Warden
/// is among the newcomers checked for that event, so it sees the Giant's entry although the
/// Giant was delivered first: P0 +1.
#[test]
fn storm_of_souls_soul_warden_sees_hill_giant_entering_first() {
    let (outcome, ids) = with_entry_order(
        [GraveyardCard::HillGiant, GraveyardCard::SoulWarden],
        storm_board,
    );

    assert_storm_returned_in_order(&outcome, ids);
    assert_eq!(
        outcome.life_delta(P0),
        1,
        "Soul Warden saw Hill Giant enter"
    );
    assert_outcome_priority_with_empty_stack(&outcome);
}

/// R30b — CR 603.6a, control order: Soul Warden delivered first sees Hill Giant: P0 +1.
#[test]
fn storm_of_souls_soul_warden_sees_hill_giant_entering_second() {
    let (outcome, ids) = with_entry_order(
        [GraveyardCard::SoulWarden, GraveyardCard::HillGiant],
        storm_board,
    );

    assert_storm_returned_in_order(&outcome, ids);
    assert_eq!(
        outcome.life_delta(P0),
        1,
        "Soul Warden saw Hill Giant enter"
    );
    assert_outcome_priority_with_empty_stack(&outcome);
}

/// Living Death's returned cards and Impact Tremors are on the battlefield, and the cards
/// entered in the listed order.
fn assert_living_death_returned_in_order(outcome: &CastOutcome, ids: [ObjectId; 2]) {
    for id in ids {
        let obj = &outcome.state().objects[&id];
        assert_eq!(obj.zone, Zone::Battlefield, "{}", obj.name);
    }
    let tremors = outcome
        .find_object(|obj| obj.name == "Impact Tremors")
        .expect("Impact Tremors is staged");
    assert_eq!(outcome.state().objects[&tremors].zone, Zone::Battlefield);
    assert!(
        battlefield_entry_index(outcome.events(), ids[0])
            < battlefield_entry_index(outcome.events(), ids[1])
    );
}

/// R30o-a — CR 603.6a + CR 603.2c, owner-collected route: Living Death returns Hill Giant and
/// Soul Warden in one event, the Giant delivered first. Soul Warden sees the Giant (P0 +1), and
/// Impact Tremors triggers once for each creature that entered (P1 −2).
#[test]
fn living_death_soul_warden_sees_hill_giant_entering_first() {
    let (outcome, ids) = with_entry_order(
        [GraveyardCard::HillGiant, GraveyardCard::SoulWarden],
        |creation| living_death_board(creation, Resident::ImpactTremors),
    );

    assert_living_death_returned_in_order(&outcome, ids);
    assert_eq!(
        outcome.life_delta(P0),
        1,
        "Soul Warden saw Hill Giant enter"
    );
    assert_eq!(
        outcome.life_delta(P1),
        -2,
        "Impact Tremors once per entrant"
    );
    assert_outcome_priority_with_empty_stack(&outcome);
}

/// R30o-b — CR 603.6a + CR 603.2c, owner control order: Soul Warden delivered first: P0 +1 and
/// P1 −2.
#[test]
fn living_death_soul_warden_sees_hill_giant_entering_second() {
    let (outcome, ids) = with_entry_order(
        [GraveyardCard::SoulWarden, GraveyardCard::HillGiant],
        |creation| living_death_board(creation, Resident::ImpactTremors),
    );

    assert_living_death_returned_in_order(&outcome, ids);
    assert_eq!(
        outcome.life_delta(P0),
        1,
        "Soul Warden saw Hill Giant enter"
    );
    assert_eq!(
        outcome.life_delta(P1),
        -2,
        "Impact Tremors once per entrant"
    );
    assert_outcome_priority_with_empty_stack(&outcome);
}

/// R-8160m — CR 608.2c + CR 113.6 + CR 603.10a, issue #8160 with a two-entrant group: Living
/// Death sacrifices Grizzly Bears before it returns Carmen and Hill Giant together, so Carmen was
/// not on the battlefield for the sacrifice and gains no counter, in either delivery order.
#[test]
fn living_death_returned_carmen_misses_the_earlier_sacrifice_in_either_order() {
    for order in [
        [
            GraveyardCard::CarmenCruelSkymarcher,
            GraveyardCard::HillGiant,
        ],
        [
            GraveyardCard::HillGiant,
            GraveyardCard::CarmenCruelSkymarcher,
        ],
    ] {
        let (outcome, ids) = with_entry_order(order, |creation| {
            living_death_board(creation, Resident::GrizzlyBears)
        });
        let carmen = ids[order
            .iter()
            .position(|&card| card == GraveyardCard::CarmenCruelSkymarcher)
            .expect("Carmen is staged")];

        // Reach: the sacrifice happened, and both cards returned in the requested order.
        let bears = outcome
            .find_object(|obj| obj.name == "Grizzly Bears")
            .expect("Grizzly Bears is staged");
        assert_eq!(outcome.state().objects[&bears].zone, Zone::Graveyard);
        for id in ids {
            assert_eq!(outcome.state().objects[&id].zone, Zone::Battlefield);
        }
        assert!(
            battlefield_entry_index(outcome.events(), ids[0])
                < battlefield_entry_index(outcome.events(), ids[1])
        );
        // Reach: the returned Carmen's sacrifice trigger is live in this construction.
        let carmen_obj = &outcome.state().objects[&carmen];
        assert_eq!(
            carmen_obj
                .trigger_definitions
                .iter_unchecked()
                .filter(|trigger| trigger.definition.mode == TriggerMode::Sacrificed)
                .count(),
            1
        );
        assert!(
            !carmen_obj.has_unimplemented_mechanics(),
            "{:?}",
            unimplemented_mechanics(carmen_obj)
        );

        assert_eq!(
            outcome.counters(carmen, CounterType::Plus1Plus1),
            0,
            "{order:?}"
        );
        assert_eq!(outcome.life_delta(P0), 0, "{order:?}");
        assert_outcome_priority_with_empty_stack(&outcome);
    }
}

/// R-8160p — CR 113.6 + CR 603.10a, positive control for R-8160m: Carmen is on the battlefield
/// when Village Rites' additional cost sacrifices Grizzly Bears, so her trigger gives her one
/// counter and P0 1 life.
#[test]
fn carmen_on_the_battlefield_gains_a_counter_for_a_sacrifice() {
    let mut scenario = main_phase();
    let carmen = scenario
        .add_creature_from_oracle(
            P0,
            "Carmen, Cruel Skymarcher",
            2,
            2,
            CARMEN_CRUEL_SKYMARCHER,
        )
        .id();
    let bears = grizzly_bears_on_battlefield(&mut scenario, P0);
    let rites = scenario
        .add_spell_to_hand_from_oracle(P0, "Village Rites", true, VILLAGE_RITES)
        .id();
    scenario.with_mana_pool(P0, mana(ManaType::Black, 1));
    // The draw must not empty P0's library.
    scenario.add_card_to_library_top(P0, "Plains");
    scenario.add_card_to_library_top(P0, "Plains");
    let mut runner = scenario.build();

    let outcome = runner.cast(rites).sacrifice_with(&[bears]).resolve();

    // Reach: the cost was paid, with Carmen on the battlefield.
    assert_eq!(outcome.state().objects[&bears].zone, Zone::Graveyard);
    assert_eq!(outcome.state().objects[&carmen].zone, Zone::Battlefield);
    assert_eq!(outcome.counters(carmen, CounterType::Plus1Plus1), 1);
    assert_eq!(outcome.life_delta(P0), 1);
    assert_outcome_priority_with_empty_stack(&outcome);
}

/// Cast and commit `spell` from the staged board, then pass priority until it has resolved.
fn resolve_off_the_stack(scenario: GameScenario, spell: ObjectId) -> GameRunner {
    let mut runner = scenario.build();
    runner.cast(spell).commit();
    pass_until_off_the_stack(&mut runner, spell);
    runner
}

/// The ids of Hill Giant and Soul Warden among `ids`, staged in `order`.
fn giant_and_warden(order: [GraveyardCard; 2], ids: [ObjectId; 2]) -> (ObjectId, ObjectId) {
    let id_of = |card: GraveyardCard| {
        ids[order
            .iter()
            .position(|&staged| staged == card)
            .expect("the card is staged")]
    };
    (
        id_of(GraveyardCard::HillGiant),
        id_of(GraveyardCard::SoulWarden),
    )
}

/// CR 400.7 + CR 603.2c + CR 603.6a: Soul Warden's trigger for Hill Giant is on the stack with a
/// copy of the Giant's entry stamped with its co-entrant. A raw and a trusted save of that state
/// restore, the restored ledger row and stack copy keep the stamp, and the trigger then resolves
/// as it would have: P0 +1.
fn assert_save_with_soul_wardens_trigger_restores(
    runner: &GameRunner,
    giant: ObjectId,
    warden: ObjectId,
) {
    // Reach: a stamped copy of the Giant's entry is in a live carrier, the stack.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "waiting_for: {:?}",
        runner.state().waiting_for
    );
    let trigger_record = sole_trigger_event_record(runner.state(), warden);
    assert_eq!(trigger_record.object_id, giant);
    assert_eq!(trigger_record.co_entered, vec![warden]);
    let life_at_save = runner.life(P0);

    for trusted in [false, true] {
        let restored = restore_round_trip(runner.state(), trusted)
            .unwrap_or_else(|error| panic!("trusted {trusted}: {error}"));
        assert_eq!(
            ledger_battlefield_entry(&restored, giant).co_entered,
            vec![warden],
            "trusted {trusted}"
        );
        assert_eq!(
            sole_trigger_event_record(&restored, warden).co_entered,
            vec![warden],
            "trusted {trusted}"
        );
        let mut restored = GameRunner::from_state(restored);
        restored.advance_until_stack_empty();
        assert_eq!(
            restored.life(P0) - life_at_save,
            1,
            "Soul Warden's trigger resolves after the restore (trusted {trusted})"
        );
        assert_priority_with_empty_stack(&restored);
    }
}

/// R-P1 — CR 400.7 + CR 603.2c + CR 603.6a, handed-off route: Storm of Souls returns Hill Giant
/// and Soul Warden in one event and the game is saved with Soul Warden's trigger for the Giant on
/// the stack. The save restores in either delivery order, because the Giant's per-turn ledger row
/// carries the same `co_entered` stamp as the trigger's copy of its entry.
#[test]
fn storm_of_souls_save_with_soul_wardens_trigger_on_the_stack_restores() {
    for order in [
        [GraveyardCard::HillGiant, GraveyardCard::SoulWarden],
        [GraveyardCard::SoulWarden, GraveyardCard::HillGiant],
    ] {
        let (runner, ids) = with_entry_order(order, |creation| {
            let (scenario, storm, ids) = storm_scenario(creation);
            (resolve_off_the_stack(scenario, storm), ids)
        });
        let (giant, warden) = giant_and_warden(order, ids);
        assert_save_with_soul_wardens_trigger_restores(&runner, giant, warden);
    }
}

/// R-P2 — CR 400.7 + CR 603.2c + CR 603.6a, owner route: Living Death sacrifices Grizzly Bears,
/// then returns Hill Giant and Soul Warden in one event. The owner's final segment collects Soul
/// Warden's trigger after the entries are stamped and parks it, and the game is saved with that
/// stamped trigger on the stack. The save restores in either delivery order.
#[test]
fn living_death_save_with_soul_wardens_trigger_on_the_stack_restores() {
    for order in [
        [GraveyardCard::HillGiant, GraveyardCard::SoulWarden],
        [GraveyardCard::SoulWarden, GraveyardCard::HillGiant],
    ] {
        let (runner, ids) = with_entry_order(order, |creation| {
            let (scenario, living_death, ids) =
                living_death_scenario(creation, Resident::GrizzlyBears);
            (resolve_off_the_stack(scenario, living_death), ids)
        });
        let (giant, warden) = giant_and_warden(order, ids);

        // Reach: Living Death's own step sacrificed the Bears.
        let bears = runner
            .state()
            .objects
            .values()
            .find(|obj| obj.name == "Grizzly Bears")
            .expect("Grizzly Bears is staged");
        assert_eq!(bears.zone, Zone::Graveyard);
        assert_save_with_soul_wardens_trigger_restores(&runner, giant, warden);
    }
}

/// A Living Death run stopped at the next decision after P0 answered the Worldsire's devour
/// choice, with what it recorded at the devour pause.
struct DevourPauseRun {
    runner: GameRunner,
    /// Hill Giant's zone at the devour pause.
    giant_zone_at_pause: Zone,
    /// Each parked context whose event is Hill Giant's battlefield entry, as its source and the
    /// record of that entry, at the devour pause.
    giant_entry_parked_at_pause: Vec<(ObjectId, ZoneChangeRecord)>,
}

impl ObservedEntryOrder for DevourPauseRun {
    fn battlefield_entry_position(&self, id: ObjectId) -> usize {
        self.runner.battlefield_entry_position(id)
    }
}

/// Living Death in P0's hand; Impact Tremors and two Forests on P0's battlefield; `creation` in
/// P0's graveyard. Cast it, pass to the Worldsire's devour choice, record the pause, sacrifice
/// both Forests and stop at the next decision.
fn devour_pause_run(creation: [GraveyardCard; 2]) -> (DevourPauseRun, [ObjectId; 2]) {
    let (mut scenario, living_death, ids) =
        living_death_scenario(creation, Resident::ImpactTremors);
    let forests = vec![
        scenario.add_basic_land(P0, ManaColor::Green),
        scenario.add_basic_land(P0, ManaColor::Green),
    ];
    let giant = ids[creation
        .iter()
        .position(|&staged| staged == GraveyardCard::HillGiant)
        .expect("Hill Giant is staged")];
    let mut runner = scenario.build();
    runner.cast(living_death).commit();
    pass_until_devour_choice(&mut runner);

    let state = runner.state();
    let giant_zone_at_pause = state.objects[&giant].zone;
    let giant_entry_parked_at_pause = state
        .deferred_triggers
        .iter()
        .filter_map(|context| match &context.pending.trigger_event {
            Some(GameEvent::ZoneChanged {
                object_id,
                to: Zone::Battlefield,
                record,
                ..
            }) if *object_id == giant => Some((context.pending.source_id, record.as_ref().clone())),
            _ => None,
        })
        .collect();

    runner
        .act(GameAction::SelectCards { cards: forests })
        .expect("P0 devours both Forests");
    (
        DevourPauseRun {
            runner,
            giant_zone_at_pause,
            giant_entry_parked_at_pause,
        },
        ids,
    )
}

/// R-P3 — CR 400.7 + CR 603.2c + CR 603.6a: Living Death returns Hill Giant, then pauses on
/// Famished Worldsire's devour choice (CR 614.12a), which it makes before the Worldsire enters.
/// Impact Tremors' trigger for the Giant is parked in the first action with an unstamped copy of
/// the Giant's entry; the answer completes the group, and the parked copy gets the same stamp as
/// the Giant's ledger row. A raw and a trusted save after the answer restore, and the game then
/// finishes resolving: the Worldsire has 6 counters.
///
/// Pre-existing defect outside this row's claim: the devour route delivers the Worldsire before
/// its devour choice and collects its entry twice (Impact Tremors triggers three times and the
/// Worldsire's own enters trigger twice, at base too), so this row asserts neither the
/// Worldsire's zone at the pause nor P1's life total.
#[test]
fn living_death_save_after_the_devour_pause_restores_the_parked_tremors_trigger() {
    let (run, [giant, worldsire]) = with_entry_order(
        [GraveyardCard::HillGiant, GraveyardCard::FamishedWorldsire],
        devour_pause_run,
    );

    // Reach, at the pause: the Giant has entered, and Impact Tremors' trigger for the Giant is
    // parked on an unstamped copy.
    let state = run.runner.state();
    let tremors = state
        .objects
        .values()
        .find(|obj| obj.name == "Impact Tremors")
        .expect("Impact Tremors is staged")
        .id;
    assert_eq!(run.giant_zone_at_pause, Zone::Battlefield);
    assert_eq!(
        run.giant_entry_parked_at_pause.len(),
        1,
        "{:?}",
        run.giant_entry_parked_at_pause
    );
    let (parked_source, parked_record) = &run.giant_entry_parked_at_pause[0];
    assert_eq!(*parked_source, tremors);
    assert!(parked_record.co_entered.is_empty(), "{parked_record:?}");
    // Reach, after the answer: completion stamped the Giant's ledger row.
    assert_eq!(
        ledger_battlefield_entry(state, giant).co_entered,
        vec![worldsire]
    );
    for trusted in [false, true] {
        let restored = restore_round_trip(state, trusted)
            .unwrap_or_else(|error| panic!("trusted {trusted}: {error}"));
        let mut restored = GameRunner::from_state(restored);
        restored.advance_until_stack_empty();

        assert_eq!(plus_one_counters(&restored, worldsire), 6);
        let forest_zones: Vec<Zone> = restored
            .state()
            .objects
            .values()
            .filter(|obj| obj.name == "Forest")
            .map(|obj| obj.zone)
            .collect();
        assert_eq!(
            forest_zones,
            vec![Zone::Graveyard; 2],
            "both Forests were devoured (trusted {trusted})"
        );
        for id in [giant, worldsire] {
            assert_eq!(restored.state().objects[&id].zone, Zone::Battlefield);
        }
        assert_priority_with_empty_stack(&restored);
    }
}

// ---------------------------------------------------------------------------
// CR 614.4 + CR 614.12: a member of one battlefield-entry event delivered earlier is not a
// source of replacement effects for a later member. The engine moves an event's members one at
// a time, but a replacement effect must exist before the event it modifies (CR 614.4), and an
// entry is checked "taking into account … continuous effects that already exist" (CR 614.12).
// Metallic Mimic ruling (2017-02-09): "Creatures of the chosen type that enter the battlefield
// at the same time as Metallic Mimic won't enter with an additional +1/+1 counter."
// ---------------------------------------------------------------------------

/// Eerie Ultimatum {W}{W}{B}{B}{B}{G}{G} Sorcery — verbatim.
const EERIE_ULTIMATUM: &str =
    "Return any number of permanent cards with different names from your graveyard to the battlefield.";
/// Collected Company {3}{G} Instant — verbatim.
const COLLECTED_COMPANY: &str = "Look at the top six cards of your library. Put up to two creature cards with mana value 3 or less from among them onto the battlefield. Put the rest on the bottom of your library in any order.";
/// Elvish Mystic {G} Creature — Elf Druid 1/1 — verbatim.
const ELVISH_MYSTIC: &str = "{T}: Add {G}.";
/// Graaz, Unstoppable Juggernaut {8} Legendary Artifact Creature — Juggernaut 7/5 — verbatim.
const GRAAZ_UNSTOPPABLE_JUGGERNAUT: &str = "Juggernauts you control attack each combat if able.\nJuggernauts you control can't be blocked by Walls.\nOther creatures you control have base power and toughness 5/3 and are Juggernauts in addition to their other creature types.";

/// Metallic Mimic in `player`'s graveyard.
fn metallic_mimic_in_graveyard(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_creature_to_graveyard(player, "Metallic Mimic", 2, 1)
        .as_artifact_creature()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 2,
        })
        .with_subtypes(vec!["Shapeshifter"])
        .from_oracle_text(METALLIC_MIMIC)
        .id()
}

/// Containment Priest in `player`'s graveyard.
fn containment_priest_in_graveyard(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_creature_to_graveyard(player, "Containment Priest", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 1,
        })
        .with_subtypes(vec!["Human", "Cleric"])
        .from_oracle_text_with_keywords(&["Flash"], CONTAINMENT_PRIEST)
        .id()
}

/// Deanna Troi, Psychic Counsel in P0's graveyard.
fn deanna_troi_in_graveyard(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_graveyard(P0, "Deanna Troi, Psychic Counsel", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 1,
        })
        .with_subtypes(vec!["Betazoid", "Human", "Advisor"])
        .from_oracle_text(DEANNA_TROI)
        .id()
}

/// Elvish Mystic in P0's graveyard.
fn elvish_mystic_in_graveyard(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_graveyard(P0, "Elvish Mystic", 1, 1)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .with_subtypes(vec!["Elf", "Druid"])
        .from_oracle_text(ELVISH_MYSTIC)
        .id()
}

/// Graaz, Unstoppable Juggernaut on P0's battlefield or in P0's graveyard. The scenario builder
/// has no supertype setter, and no row reads Legendary.
fn add_graaz(scenario: &mut GameScenario, zone: Zone) -> ObjectId {
    let name = "Graaz, Unstoppable Juggernaut";
    let cost = ManaCost::Cost {
        shards: vec![],
        generic: 8,
    };
    match zone {
        Zone::Battlefield => scenario
            .add_creature_from_oracle(P0, name, 7, 5, GRAAZ_UNSTOPPABLE_JUGGERNAUT)
            .as_artifact_creature()
            .with_mana_cost(cost)
            .with_subtypes(vec!["Juggernaut"])
            .id(),
        Zone::Graveyard => scenario
            .add_creature_to_graveyard(P0, name, 7, 5)
            .as_artifact_creature()
            .with_mana_cost(cost)
            .with_subtypes(vec!["Juggernaut"])
            .from_oracle_text(GRAAZ_UNSTOPPABLE_JUGGERNAUT)
            .id(),
        other => panic!("Graaz is staged on the battlefield or in the graveyard, not {other:?}"),
    }
}

/// Move `cards` from `player`'s graveyard to the top of their library, in that order (the last
/// one ends on top).
fn put_on_library_top(runner: &mut GameRunner, player: PlayerId, cards: &[ObjectId]) {
    let state = runner.state_mut();
    for &card in cards {
        let player_state = state
            .players
            .iter_mut()
            .find(|staged| staged.id == player)
            .expect("the player exists");
        player_state.graveyard.retain(|&staged| staged != card);
        player_state.library.push_front(card);
        state
            .objects
            .get_mut(&card)
            .expect("the card is staged")
            .zone = Zone::Library;
    }
}

/// A prompt a driven resolution answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Prompt {
    /// Metallic Mimic's "As this creature enters, choose a creature type." (CR 614.12a).
    NamedType,
    /// A choice among applicable replacement effects (CR 616.1).
    Replacement,
    /// Eerie Ultimatum's choice of cards to return.
    EffectZone,
    /// Collected Company's choice of cards to put onto the battlefield.
    Dig,
}

/// Answer every prompt of a committed spell's resolution until P0 has priority with an empty
/// stack: name `chosen` at each creature-type choice, take the first applicable replacement
/// (CR 616.1), select `selection` in that order, and pass priority while the stack is not empty.
/// Returns the prompts answered, in order.
fn answer_until_priority(
    runner: &mut GameRunner,
    chosen: &str,
    selection: &[ObjectId],
) -> Vec<Prompt> {
    let mut prompts = Vec::new();
    for _ in 0..24 {
        match runner.state().waiting_for.clone() {
            WaitingFor::NamedChoice { .. } => {
                prompts.push(Prompt::NamedType);
                runner
                    .act(GameAction::ChooseOption {
                        choice: chosen.to_string(),
                    })
                    .expect("name the creature type");
            }
            WaitingFor::ReplacementChoice { .. } => {
                prompts.push(Prompt::Replacement);
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("order the replacements");
            }
            WaitingFor::EffectZoneChoice { .. } => {
                prompts.push(Prompt::EffectZone);
                runner
                    .act(GameAction::SelectCards {
                        cards: selection.to_vec(),
                    })
                    .expect("choose the cards to return");
            }
            WaitingFor::DigChoice { .. } => {
                prompts.push(Prompt::Dig);
                runner
                    .act(GameAction::SelectCards {
                        cards: selection.to_vec(),
                    })
                    .expect("choose the cards to put onto the battlefield");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority passes while the stack is not empty");
            }
            _ => break,
        }
    }
    assert_priority_with_empty_stack(runner);
    prompts
}

/// Pass priority one action at a time until P0 is asked to name a creature type.
fn pass_until_named_choice(runner: &mut GameRunner) {
    for _ in 0..4 {
        if matches!(runner.state().waiting_for, WaitingFor::NamedChoice { .. }) {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority passes before the creature-type choice");
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::NamedChoice { .. }),
        "no creature-type choice: {:?}",
        runner.state().waiting_for
    );
}

/// Cast Ever After from P0's hand targeting `targets` in that order, then answer every prompt.
fn ever_after_returns(
    runner: &mut GameRunner,
    ever_after: ObjectId,
    targets: &[ObjectId],
    chosen: &str,
) -> Vec<Prompt> {
    runner.cast(ever_after).target_objects(targets).commit();
    answer_until_priority(runner, chosen, &[])
}

/// The id of `card` among `ids`, staged in `order`.
fn staged_id<const N: usize>(
    order: [GraveyardCard; N],
    ids: [ObjectId; N],
    card: GraveyardCard,
) -> ObjectId {
    ids[order
        .iter()
        .position(|&staged| staged == card)
        .expect("the card is staged")]
}

/// Number of `prompt`s among `prompts`.
fn prompt_count(prompts: &[Prompt], prompt: Prompt) -> usize {
    prompts
        .iter()
        .filter(|&&answered| answered == prompt)
        .count()
}

/// Reach: `first`'s battlefield entry precedes `second`'s in the per-turn ledger.
fn assert_entered_in_order(runner: &GameRunner, first: ObjectId, second: ObjectId) {
    let state = runner.state();
    assert!(
        ledger_battlefield_entry_position(state, first)
            < ledger_battlefield_entry_position(state, second),
        "{} must enter before {}",
        state.objects[&first].name,
        state.objects[&second].name
    );
}

/// Reach: the Mimic chose `chosen` as it entered.
fn assert_mimic_chose(runner: &GameRunner, mimic: ObjectId, chosen: &str) {
    let mimic_obj = &runner.state().objects[&mimic];
    assert!(
        mimic_obj
            .chosen_attributes
            .contains(&ChosenAttribute::CreatureType(chosen.to_string())),
        "{:?}",
        mimic_obj.chosen_attributes
    );
}

/// `id` is on the battlefield as a black Zombie (Ever After's rider applied).
fn assert_black_zombie_on_battlefield(runner: &GameRunner, id: ObjectId) {
    let obj = &runner.state().objects[&id];
    assert_eq!(obj.zone, Zone::Battlefield, "{}", obj.name);
    assert!(
        subtypes(runner, id).iter().any(|t| t == "Zombie"),
        "{}",
        obj.name
    );
    assert!(obj.color.contains(&ManaColor::Black), "{}", obj.name);
}

/// Ever After in P0's hand, and Metallic Mimic and Hill Giant in P0's graveyard.
fn ever_after_mimic_board() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = main_phase();
    let mimic = metallic_mimic_in_graveyard(&mut scenario, P0);
    let giant = hill_giant_in_graveyard(&mut scenario, P0);
    let ever_after = scenario
        .add_spell_to_hand_from_oracle(P0, "Ever After", false, EVER_AFTER)
        .id();
    (scenario.build(), ever_after, mimic, giant)
}

/// R-CE1a — CR 614.4 + CR 614.12: Ever After returns Metallic Mimic, then Hill Giant, in one
/// event. The Mimic's "enters with an additional +1/+1 counter" did not exist before the event,
/// so the Giant, checked as a black Zombie with the rider, gets no counter although the Mimic
/// named Zombie.
/// The Mimic's own "As this creature enters, choose a creature type." still applies (CR 614.12a),
/// and the group pauses on it before the Giant moves.
#[test]
fn ever_after_returned_mimic_gives_no_counter_to_a_giant_returned_with_it() {
    let (mut runner, ever_after, mimic, giant) = ever_after_mimic_board();

    let prompts = ever_after_returns(&mut runner, ever_after, &[mimic, giant], "Zombie");

    // Reach: the Mimic was delivered first and named Zombie at its own choice.
    assert_eq!(prompt_count(&prompts, Prompt::NamedType), 1, "{prompts:?}");
    assert_entered_in_order(&runner, mimic, giant);
    assert_mimic_chose(&runner, mimic, "Zombie");
    // Pre-existing rider-after-pause defect (on main, filed separately): a member whose
    // as-enters choice pauses the event, and every later member, never gets the resolution's
    // "is/has" rider (CR 611.2e), so this row asserts no rider type, color or P/T on them.
    for id in [mimic, giant] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
    }
    assert_eq!(plus_one_counters(&runner, giant), 0);
    assert_eq!(plus_one_counters(&runner, mimic), 0);
}

/// R-CE1b — order guard for R-CE1a: Hill Giant is delivered before Metallic Mimic and gets no
/// counter.
#[test]
fn ever_after_giant_returned_before_the_mimic_gets_no_counter() {
    let (mut runner, ever_after, mimic, giant) = ever_after_mimic_board();

    ever_after_returns(&mut runner, ever_after, &[giant, mimic], "Zombie");

    assert_entered_in_order(&runner, giant, mimic);
    assert_mimic_chose(&runner, mimic, "Zombie");
    assert_black_zombie_on_battlefield(&runner, giant);
    assert_eq!(plus_one_counters(&runner, giant), 0);
}

/// Storm of Souls returns `creation` from P0's graveyard; Metallic Mimic names Spirit.
fn storm_mimic_run(creation: [GraveyardCard; 2]) -> (GameRunner, [ObjectId; 2]) {
    let (scenario, storm, ids) = storm_scenario(creation);
    let mut runner = scenario.build();
    runner.cast(storm).commit();
    answer_until_priority(&mut runner, "Spirit", &[]);
    (runner, ids)
}

/// Both `ids` are on the battlefield, and the Giant has no +1/+1 counter.
fn assert_storm_giant_without_counter(runner: &GameRunner, ids: [ObjectId; 2], giant: ObjectId) {
    for id in ids {
        assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
    }
    assert_eq!(plus_one_counters(runner, giant), 0);
}

/// R-CE2a — CR 614.4 + CR 614.12: Storm of Souls delivers Metallic Mimic (naming Spirit) before
/// Hill Giant. The Mimic did not exist before the event, so the Giant gets no counter.
#[test]
fn storm_of_souls_returned_mimic_gives_no_counter_to_a_giant_returned_after_it() {
    let (runner, [mimic, giant]) = with_entry_order(
        [GraveyardCard::MetallicMimic, GraveyardCard::HillGiant],
        storm_mimic_run,
    );

    assert_mimic_chose(&runner, mimic, "Spirit");
    // Pre-existing rider-after-pause defect (on main, filed separately): a member whose
    // as-enters choice pauses the event, and every later member, never gets the resolution's
    // "is/has" rider (CR 611.2e), so this row asserts no rider type, color or P/T on them.
    assert_storm_giant_without_counter(&runner, [mimic, giant], giant);
}

/// R-CE2b — order guard for R-CE2a: Hill Giant delivered before the Mimic gets no counter.
#[test]
fn storm_of_souls_giant_returned_before_the_mimic_gets_no_counter() {
    let (runner, [giant, mimic]) = with_entry_order(
        [GraveyardCard::HillGiant, GraveyardCard::MetallicMimic],
        storm_mimic_run,
    );

    assert_mimic_chose(&runner, mimic, "Spirit");
    assert_storm_giant_without_counter(&runner, [giant, mimic], giant);
}

/// R-CE3 — CR 614.4 + CR 614.12: Living Death returns Metallic Mimic (naming Giant) and Hill
/// Giant in one event. The Giant gets no counter in either delivery order.
#[test]
fn living_death_returned_mimic_gives_no_counter_to_a_co_entering_giant_in_either_order() {
    for order in [
        [GraveyardCard::MetallicMimic, GraveyardCard::HillGiant],
        [GraveyardCard::HillGiant, GraveyardCard::MetallicMimic],
    ] {
        let (runner, ids) = with_entry_order(order, |creation| {
            let (scenario, living_death, ids) =
                living_death_scenario(creation, Resident::GrizzlyBears);
            let mut runner = scenario.build();
            runner.cast(living_death).commit();
            answer_until_priority(&mut runner, "Giant", &[]);
            (runner, ids)
        });
        let mimic = staged_id(order, ids, GraveyardCard::MetallicMimic);
        let giant = staged_id(order, ids, GraveyardCard::HillGiant);

        // Reach: both returned in the requested order, and the Mimic named Giant.
        for id in ids {
            assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
        }
        assert_entered_in_order(&runner, ids[0], ids[1]);
        assert_mimic_chose(&runner, mimic, "Giant");
        assert_eq!(plus_one_counters(&runner, giant), 0, "{order:?}");
    }
}

/// R-CE4 — CR 614.4 + CR 614.12, paired positive: Metallic Mimic A (naming Zombie) is on the
/// battlefield before Ever After returns Metallic Mimic B, then Hill Giant. A's replacement
/// existed before the event and applies to both; B's did not. The Giant and B get one counter
/// each, A's.
#[test]
fn ever_after_giant_gets_only_the_preexisting_mimics_counter() {
    let mut scenario = main_phase();
    let mimic_a = add_metallic_mimic(&mut scenario, P0);
    let mimic_b = metallic_mimic_in_graveyard(&mut scenario, P0);
    let giant = hill_giant_in_graveyard(&mut scenario, P0);
    let ever_after = scenario
        .add_spell_to_hand_from_oracle(P0, "Ever After", false, EVER_AFTER)
        .id();
    let mut runner = scenario.build();
    name_creature_type(&mut runner, mimic_a, "Zombie");

    ever_after_returns(&mut runner, ever_after, &[mimic_b, giant], "Zombie");

    assert_entered_in_order(&runner, mimic_b, giant);
    assert_mimic_chose(&runner, mimic_b, "Zombie");
    assert_eq!(runner.state().objects[&giant].zone, Zone::Battlefield);
    assert_eq!(plus_one_counters(&runner, giant), 1, "Mimic A's only");
    assert_eq!(plus_one_counters(&runner, mimic_b), 1, "Mimic A's");
    assert_eq!(plus_one_counters(&runner, mimic_a), 0);
}

/// Eerie Ultimatum in P0's hand, and `first` then Hill Giant in P0's graveyard. Cast it, return
/// `[first, giant]` in that order, and answer every prompt (a Mimic names Giant).
fn eerie_ultimatum_returns(
    stage_first: impl FnOnce(&mut GameScenario) -> ObjectId,
) -> (GameRunner, Vec<Prompt>, ObjectId, ObjectId) {
    let mut scenario = main_phase();
    let first = stage_first(&mut scenario);
    let giant = hill_giant_in_graveyard(&mut scenario, P0);
    let eerie = scenario
        .add_spell_to_hand_from_oracle(P0, "Eerie Ultimatum", false, EERIE_ULTIMATUM)
        .id();
    let mut runner = scenario.build();
    runner.cast(eerie).commit();
    let prompts = answer_until_priority(&mut runner, "Giant", &[first, giant]);
    (runner, prompts, first, giant)
}

/// R-CE5a — CR 614.4 + CR 614.12: Eerie Ultimatum returns Metallic Mimic (naming Giant), then
/// Hill Giant. The group pauses on the Mimic's choice; the Giant, moved on the resume, gets no
/// counter.
#[test]
fn eerie_ultimatum_returned_mimic_gives_no_counter_to_a_giant_returned_with_it() {
    let (runner, prompts, mimic, giant) =
        eerie_ultimatum_returns(|scenario| metallic_mimic_in_graveyard(scenario, P0));

    // Reach: the cards were chosen, and the Mimic was delivered first and named Giant.
    assert_eq!(prompt_count(&prompts, Prompt::EffectZone), 1, "{prompts:?}");
    assert_eq!(prompt_count(&prompts, Prompt::NamedType), 1, "{prompts:?}");
    assert_entered_in_order(&runner, mimic, giant);
    assert_mimic_chose(&runner, mimic, "Giant");
    assert_eq!(runner.state().objects[&giant].zone, Zone::Battlefield);
    assert_eq!(plus_one_counters(&runner, giant), 0);
}

/// R-CE5b — CR 614.4 + CR 614.12: Eerie Ultimatum returns Containment Priest ("If a nontoken
/// creature would enter and it wasn't cast, exile it instead."), then Hill Giant. The Priest did
/// not exist before the event, so it does not exile the Giant: both are on the battlefield.
#[test]
fn eerie_ultimatum_returned_priest_does_not_exile_a_giant_returned_with_it() {
    let (runner, prompts, priest, giant) =
        eerie_ultimatum_returns(|scenario| containment_priest_in_graveyard(scenario, P0));

    assert_eq!(prompt_count(&prompts, Prompt::EffectZone), 1, "{prompts:?}");
    assert_eq!(runner.state().objects[&priest].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&giant].zone, Zone::Battlefield);
    assert_entered_in_order(&runner, priest, giant);
}

/// R-CE6a — CR 614.1d + CR 614.4: Ever After returns Deanna Troi, Psychic Counsel ("Other
/// creatures you control enter with an additional +1/+1 counter on them.") and Hill Giant in one
/// event. Deanna's static did not exist before the event, so the Giant gets no counter in either
/// delivery order.
#[test]
fn ever_after_returned_deanna_troi_gives_no_counter_to_a_co_entering_giant() {
    for deanna_first in [true, false] {
        let mut scenario = main_phase();
        let deanna = deanna_troi_in_graveyard(&mut scenario);
        let giant = hill_giant_in_graveyard(&mut scenario, P0);
        let ever_after = scenario
            .add_spell_to_hand_from_oracle(P0, "Ever After", false, EVER_AFTER)
            .id();
        let mut runner = scenario.build();
        let targets = if deanna_first {
            [deanna, giant]
        } else {
            [giant, deanna]
        };

        ever_after_returns(&mut runner, ever_after, &targets, "Zombie");

        assert_entered_in_order(&runner, targets[0], targets[1]);
        assert_black_zombie_on_battlefield(&runner, deanna);
        assert_black_zombie_on_battlefield(&runner, giant);
        assert_eq!(
            plus_one_counters(&runner, giant),
            0,
            "deanna_first {deanna_first}"
        );
        assert_eq!(plus_one_counters(&runner, deanna), 0);
    }
}

/// R-CE7a — CR 614.4 + CR 614.12: Ever After returns Containment Priest and Hill Giant in one
/// event. The Priest did not exist before the event, so both are on the battlefield in either
/// delivery order.
#[test]
fn ever_after_returned_priest_does_not_exile_a_co_entering_giant() {
    for priest_first in [true, false] {
        let mut scenario = main_phase();
        let priest = containment_priest_in_graveyard(&mut scenario, P0);
        let giant = hill_giant_in_graveyard(&mut scenario, P0);
        let ever_after = scenario
            .add_spell_to_hand_from_oracle(P0, "Ever After", false, EVER_AFTER)
            .id();
        let mut runner = scenario.build();
        let targets = if priest_first {
            [priest, giant]
        } else {
            [giant, priest]
        };

        ever_after_returns(&mut runner, ever_after, &targets, "Zombie");

        assert_black_zombie_on_battlefield(&runner, priest);
        assert_black_zombie_on_battlefield(&runner, giant);
        assert_entered_in_order(&runner, targets[0], targets[1]);
    }
}

/// R-CE7c — CR 614.4 + CR 614.12: Storm of Souls delivers Containment Priest before Hill Giant
/// (created first, so delivered first). The Priest did not exist before the event, so the Giant
/// is on the battlefield as a Spirit.
#[test]
fn storm_of_souls_returned_priest_does_not_exile_a_giant_returned_after_it() {
    let (scenario, storm, [priest, giant]) =
        storm_scenario([GraveyardCard::ContainmentPriest, GraveyardCard::HillGiant]);
    let mut runner = scenario.build();
    runner.cast(storm).commit();

    answer_until_priority(&mut runner, "Spirit", &[]);

    for id in [priest, giant] {
        let obj = &runner.state().objects[&id];
        assert_eq!(obj.zone, Zone::Battlefield, "{}", obj.name);
        assert!(
            subtypes(&runner, id).iter().any(|t| t == "Spirit"),
            "{}",
            obj.name
        );
    }
    assert_entered_in_order(&runner, priest, giant);
}

/// Living Death in P0's hand and `creation` staged in that order, each in the named player's
/// graveyard; cast and resolve it.
fn cross_player_living_death(
    creation: [(GraveyardCard, PlayerId); 2],
) -> (GameRunner, [ObjectId; 2]) {
    let mut scenario = main_phase();
    let ids = creation.map(|(card, owner)| card.add_to_graveyard(&mut scenario, owner));
    let living_death = scenario
        .add_spell_to_hand_from_oracle(P0, "Living Death", false, LIVING_DEATH)
        .id();
    let mut runner = scenario.build();
    let _ = runner.cast(living_death).resolve();
    (runner, ids)
}

/// Both `ids` are on the battlefield under their owners' control.
fn assert_on_battlefield_under_owners(runner: &GameRunner, ids: [ObjectId; 2]) {
    for id in ids {
        let obj = &runner.state().objects[&id];
        assert_eq!(obj.zone, Zone::Battlefield, "{}", obj.name);
        assert_eq!(obj.controller, obj.owner, "{}", obj.name);
    }
}

/// R-CE7d — CR 614.4 + CR 614.12, members owned by different players in one event: Living Death
/// returns Containment Priest from one player's graveyard and Hill Giant from the other's, the
/// Priest delivered first. The Priest did not exist before the event, so both are on the
/// battlefield under their owners, whichever player owns which card.
#[test]
fn living_death_returned_priest_does_not_exile_another_players_giant() {
    for (priest_owner, giant_owner) in [(P0, P1), (P1, P0)] {
        let (runner, [priest, giant]) = cross_player_living_death([
            (GraveyardCard::ContainmentPriest, priest_owner),
            (GraveyardCard::HillGiant, giant_owner),
        ]);

        assert_on_battlefield_under_owners(&runner, [priest, giant]);
        assert_entered_in_order(&runner, priest, giant);
        assert_priority_with_empty_stack(&runner);
    }
}

/// R-CE7e — order guard for R-CE7d: Hill Giant delivered before Containment Priest; both are on
/// the battlefield under their owners.
#[test]
fn living_death_giant_returned_before_another_players_priest_stays() {
    for (priest_owner, giant_owner) in [(P0, P1), (P1, P0)] {
        let (runner, [giant, priest]) = cross_player_living_death([
            (GraveyardCard::HillGiant, giant_owner),
            (GraveyardCard::ContainmentPriest, priest_owner),
        ]);

        assert_on_battlefield_under_owners(&runner, [giant, priest]);
        assert_entered_in_order(&runner, giant, priest);
        assert_priority_with_empty_stack(&runner);
    }
}

/// R-CE8 — CR 614.4 + CR 614.12, batch route: Collected Company puts Metallic Mimic (naming Elf)
/// and Elvish Mystic onto the battlefield in one event. The Mystic gets no counter in either
/// delivery order; Mimic-first pauses on the Mimic's choice and resumes the batch.
#[test]
fn collected_company_mimic_gives_no_counter_to_a_co_entering_elf() {
    for mimic_first in [true, false] {
        let mut scenario = main_phase();
        let mimic = metallic_mimic_in_graveyard(&mut scenario, P0);
        let mystic = elvish_mystic_in_graveyard(&mut scenario);
        let company = scenario
            .add_spell_to_hand_from_oracle(P0, "Collected Company", true, COLLECTED_COMPANY)
            .id();
        let mut runner = scenario.build();
        put_on_library_top(&mut runner, P0, &[mimic, mystic]);
        let selection = if mimic_first {
            [mimic, mystic]
        } else {
            [mystic, mimic]
        };
        runner.cast(company).commit();

        let prompts = answer_until_priority(&mut runner, "Elf", &selection);

        // Reach: both cards were chosen and delivered in the selected order.
        assert_eq!(prompt_count(&prompts, Prompt::Dig), 1, "{prompts:?}");
        for id in [mimic, mystic] {
            assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
        }
        assert_entered_in_order(&runner, selection[0], selection[1]);
        assert_mimic_chose(&runner, mimic, "Elf");
        assert_eq!(
            plus_one_counters(&runner, mystic),
            0,
            "mimic_first {mimic_first}"
        );
    }
}

/// Metallic Mimic on P0's battlefield naming Juggernaut, Graaz, Unstoppable Juggernaut in
/// `graaz_zone`, Hill Giant in P0's graveyard and Ever After in P0's hand.
fn graaz_board(graaz_zone: Zone) -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut scenario = main_phase();
    let mimic = add_metallic_mimic(&mut scenario, P0);
    let graaz = add_graaz(&mut scenario, graaz_zone);
    let giant = hill_giant_in_graveyard(&mut scenario, P0);
    let ever_after = scenario
        .add_spell_to_hand_from_oracle(P0, "Ever After", false, EVER_AFTER)
        .id();
    let mut runner = scenario.build();
    name_creature_type(&mut runner, mimic, "Juggernaut");
    (runner, ever_after, mimic, graaz, giant)
}

/// The Giant is on the battlefield as a black Giant Zombie Juggernaut with power and toughness
/// `pt`.
fn assert_giant_zombie_juggernaut(runner: &GameRunner, giant: ObjectId, pt: (i32, i32)) {
    assert_black_zombie_on_battlefield(runner, giant);
    let types = subtypes(runner, giant);
    for subtype in ["Giant", "Juggernaut"] {
        assert!(types.iter().any(|t| t == subtype), "{types:?}");
    }
    let giant_obj = &runner.state().objects[&giant];
    assert_eq!(
        (giant_obj.power, giant_obj.toughness),
        (Some(pt.0), Some(pt.1))
    );
}

/// R-CE10a — CR 614.12 + CR 614.4: Metallic Mimic named Juggernaut before Ever After returns
/// Graaz, Unstoppable Juggernaut, then Hill Giant. The Mimic's replacement existed before the
/// event, but Graaz's "Other creatures you control … are Juggernauts" did not, so the Giant is
/// checked as a black Zombie Giant, not a Juggernaut: no counter. It ends a 5/3 Giant Zombie
/// Juggernaut once Graaz's static applies.
#[test]
fn ever_after_giant_is_not_a_juggernaut_for_the_mimic_when_graaz_returns_with_it() {
    let (mut runner, ever_after, mimic, graaz, giant) = graaz_board(Zone::Graveyard);

    ever_after_returns(&mut runner, ever_after, &[graaz, giant], "Zombie");

    // Reach: Graaz was delivered first, and the pre-existing Mimic gave it a counter in the
    // same event.
    assert_mimic_chose(&runner, mimic, "Juggernaut");
    assert_entered_in_order(&runner, graaz, giant);
    assert_eq!(runner.state().objects[&graaz].zone, Zone::Battlefield);
    assert_eq!(plus_one_counters(&runner, graaz), 1);
    assert_eq!(plus_one_counters(&runner, giant), 0);
    assert_giant_zombie_juggernaut(&runner, giant, (5, 3));
}

/// R-CE10b — order guard for R-CE10a: Hill Giant delivered before Graaz gets no counter.
#[test]
fn ever_after_giant_returned_before_graaz_gets_no_counter() {
    let (mut runner, ever_after, mimic, graaz, giant) = graaz_board(Zone::Graveyard);

    ever_after_returns(&mut runner, ever_after, &[giant, graaz], "Zombie");

    assert_mimic_chose(&runner, mimic, "Juggernaut");
    assert_entered_in_order(&runner, giant, graaz);
    assert_eq!(plus_one_counters(&runner, giant), 0);
    assert_giant_zombie_juggernaut(&runner, giant, (5, 3));
}

/// R-CE10c — CR 614.12, positive reach guard for R-CE10a: Graaz is on the battlefield before Ever
/// After returns Hill Giant, so its effect already exists, the Giant is checked as a Juggernaut,
/// and the Mimic gives it a counter: a 6/4 Giant Zombie Juggernaut.
#[test]
fn ever_after_giant_is_a_juggernaut_for_the_mimic_beside_a_preexisting_graaz() {
    let (mut runner, ever_after, mimic, graaz, giant) = graaz_board(Zone::Battlefield);

    ever_after_returns(&mut runner, ever_after, &[giant], "Zombie");

    assert_mimic_chose(&runner, mimic, "Juggernaut");
    assert_eq!(runner.state().objects[&graaz].zone, Zone::Battlefield);
    assert_eq!(plus_one_counters(&runner, giant), 1);
    assert_giant_zombie_juggernaut(&runner, giant, (6, 4));
}

/// R-CE9 — CR 614.4 + CR 614.12, self vs external in one entry: Ever After returns Metallic Mimic
/// (naming Zombie), then Barkhide Troll. The Mimic did not exist before the event, while the
/// Troll's own "This creature enters with a +1/+1 counter on it." is its own ability as it enters:
/// exactly one counter.
#[test]
fn ever_after_returned_troll_keeps_only_its_own_counter_beside_a_co_entering_mimic() {
    let mut scenario = main_phase();
    let mimic = metallic_mimic_in_graveyard(&mut scenario, P0);
    let troll = add_barkhide_troll(&mut scenario);
    let ever_after = scenario
        .add_spell_to_hand_from_oracle(P0, "Ever After", false, EVER_AFTER)
        .id();
    let mut runner = scenario.build();

    ever_after_returns(&mut runner, ever_after, &[mimic, troll], "Zombie");

    assert_entered_in_order(&runner, mimic, troll);
    assert_mimic_chose(&runner, mimic, "Zombie");
    assert_eq!(runner.state().objects[&troll].zone, Zone::Battlefield);
    assert_eq!(plus_one_counters(&runner, troll), 1, "the Troll's own only");
}

/// R-P-group — CR 614.4 + CR 603.6a: Ever After returns Metallic Mimic, then Hill Giant, and the
/// game is saved at the Mimic's creature-type choice, before either card moves. A raw and a
/// trusted save restore with the paused group's announced members, and after the answer the
/// Giant gets no counter.
#[test]
fn ever_after_save_at_the_mimics_choice_keeps_the_announced_members() {
    let (mut runner, ever_after, mimic, giant) = ever_after_mimic_board();
    let announced = [mimic, giant]
        .map(|id| ObjectIncarnationRef::of(id, runner.state().objects[&id].incarnation));
    runner
        .cast(ever_after)
        .target_objects(&[mimic, giant])
        .commit();
    pass_until_named_choice(&mut runner);

    // Reach: the group paused before the Mimic moved.
    for id in [mimic, giant] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard);
    }
    for trusted in [false, true] {
        let restored = restore_round_trip(runner.state(), trusted)
            .unwrap_or_else(|error| panic!("trusted {trusted}: {error}"));
        let group = &restored
            .active_change_zone_frame()
            .and_then(|frame| frame.pending.as_ref())
            .expect("the paused ChangeZone frame survives the restore")
            .logical_zone_change_group;
        assert_eq!(
            group.announced_members(),
            &announced[..],
            "trusted {trusted}"
        );
        let mut restored = GameRunner::from_state(restored);
        answer_until_priority(&mut restored, "Zombie", &[]);

        assert_entered_in_order(&restored, mimic, giant);
        assert_mimic_chose(&restored, mimic, "Zombie");
        assert_eq!(restored.state().objects[&giant].zone, Zone::Battlefield);
        assert_eq!(plus_one_counters(&restored, giant), 0, "trusted {trusted}");
    }
}

/// R-P-proposal — CR 614.4 + CR 614.12a + CR 616.1f: Ever After returns Metallic Mimic A, then
/// Metallic Mimic B, both naming Zombie. After A's choice, B's own choice is its only applicable
/// replacement (A did not exist before the event), so B's creature-type choice comes next, with
/// B's proposal parked. A raw and a trusted save there restore, and neither Mimic gets a counter.
#[test]
fn ever_after_second_mimic_choice_is_not_preceded_by_a_replacement_choice() {
    let mut scenario = main_phase();
    let mimic_a = metallic_mimic_in_graveyard(&mut scenario, P0);
    let mimic_b = metallic_mimic_in_graveyard(&mut scenario, P0);
    let ever_after = scenario
        .add_spell_to_hand_from_oracle(P0, "Ever After", false, EVER_AFTER)
        .id();
    let mut runner = scenario.build();
    let mimic_a_announced =
        ObjectIncarnationRef::of(mimic_a, runner.state().objects[&mimic_a].incarnation);
    runner
        .cast(ever_after)
        .target_objects(&[mimic_a, mimic_b])
        .commit();
    pass_until_named_choice(&mut runner);
    assert_eq!(runner.state().objects[&mimic_a].zone, Zone::Graveyard);
    runner
        .act(GameAction::ChooseOption {
            choice: "Zombie".to_string(),
        })
        .expect("Mimic A names Zombie");

    // Reach: A is on the battlefield and B is still in the graveyard; B's own choice is next.
    assert_eq!(runner.state().objects[&mimic_a].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&mimic_b].zone, Zone::Graveyard);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::NamedChoice { .. }),
        "waiting_for: {:?}",
        runner.state().waiting_for
    );
    for trusted in [false, true] {
        let restored = restore_round_trip(runner.state(), trusted)
            .unwrap_or_else(|error| panic!("trusted {trusted}: {error}"));
        let parked = restored
            .pending_replacement
            .as_ref()
            .expect("B's entry proposal is parked at its choice");
        assert!(
            parked
                .proposed
                .co_entrants()
                .expect("B's proposal carries its co-entrants")
                .as_slice()
                .contains(&mimic_a_announced),
            "B's parked proposal names Mimic A as a co-entrant (trusted {trusted})"
        );
        let mut restored = GameRunner::from_state(restored);
        let prompts = answer_until_priority(&mut restored, "Zombie", &[]);

        assert_eq!(prompts, vec![Prompt::NamedType], "trusted {trusted}");
        for mimic in [mimic_a, mimic_b] {
            assert_mimic_chose(&restored, mimic, "Zombie");
            assert_eq!(restored.state().objects[&mimic].zone, Zone::Battlefield);
            assert!(
                subtypes(&restored, mimic).iter().any(|t| t == "Zombie"),
                "a Zombie by its own choice (trusted {trusted})"
            );
            assert_eq!(plus_one_counters(&restored, mimic), 0, "trusted {trusted}");
        }
    }
}
