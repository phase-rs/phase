//! CR 608.2k: "where X is that creature's power / toughness / mana value" in a
//! triggered ability whose trigger condition names the creature reads that
//! creature (as it last existed, CR 608.2h), never an object a cost moved to
//! cast the ability's source and never an empty target slot. A cost or an
//! earlier instruction that introduces the creature keeps its own reading
//! (Disciple of Bolas, Altar of the Wretched, Shadowheart).
//!
//! Every card runs under its printed name and verbatim Oracle text (MTGJSON)
//! through the cast pipeline; vanilla bodies use printed vanilla cards' names.

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::counter::CounterType;
use engine::types::game_state::{CastOfferKind, CastingVariant, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{EmergeCost, Keyword};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const PANTLAZA: &str = "Whenever Pantlaza or another Dinosaur you control enters, you may discover X, where X is that creature's toughness. Do this only once each turn. (Exile cards from the top of your library until you exile a nonland card with that mana value or less. Cast it without paying its mana cost or put it into your hand. Put the rest on the bottom in a random order.)";
const KRESH: &str = "Whenever another creature dies, you may put X +1/+1 counters on Kresh, where X is that creature's power.";
const AMULET: &str = "Starting intensity 0\nEquipped creature gets +X/+X, where X is this Equipment's intensity.\nWhenever a creature you control enters, this Equipment intensifies by X, where X is that creature's power.\nEquip {2}";
const RADAGAST: &str = "Whenever Radagast or another nontoken creature you control enters, look at the top X cards of your library, where X is that creature's mana value. You may reveal a creature card that doesn't share a creature type with a creature you control from among those cards and put it into your hand. Put the rest on the bottom of your library in a random order.";
const CAGE: &str = "When a creature enters, sacrifice this artifact and create X 2/2 green Monkey creature tokens, where X is that creature's mana value.";
const BEIFONG: &str = "Whenever a nonland creature you control dies, earthbend X, where X is that creature's power. (Target land you control becomes a 0/0 creature with haste that's still a land. Put X +1/+1 counters on it. When it dies or is exiled, return it to the battlefield tapped.)";
const HAMZA: &str = "Whenever a creature you control is turned face up, it endures X, where X is that creature's toughness.\nLandfall — Whenever a land you control enters, seek a creature card and manifest it.";
const MURDER: &str = "Destroy target creature.";
const GROWTH: &str = "Target creature gets +3/+3 until end of turn.";
const CLOUDSHIFT: &str = "Exile target creature you control, then return that card to the battlefield under your control.";
const MYSTIC: &str = "{T}: Add {G}, {U}, or {R}.\nMorph {2} (You may cast this card face down as a 2/2 creature for {3}. Turn it face up any time for its morph cost.)\nWhen this creature is turned face up, add {G}{U}{R}.";
const BOLAS: &str = "When this creature enters, sacrifice another creature. You gain X life and draw X cards, where X is that creature's power.";
const FREYALISE: &str = "When this creature enters, you may sacrifice another creature. If you do, you gain X life and draw X cards, where X is that creature's power.";
const ALTAR: &str = "When this artifact enters, you may sacrifice a nontoken creature. If you do, draw X cards, then mill X cards, where X is that creature's power.\nCraft with one or more creatures {2}{B}{B}\n{2}{B}: Return this card from your graveyard to your hand.";
const SHADOWHEART: &str = "{1}{B}, {T}, Sacrifice another creature: Draw X cards, where X is that creature's power.\nChoose a Background (You can have a Background as a second commander.)";

fn main_phase() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, pool([ManaType::Green; 12]));
    scenario
}

fn pool(types: impl IntoIterator<Item = ManaType>) -> Vec<ManaUnit> {
    types
        .into_iter()
        .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
        .collect()
}

fn free() -> ManaCost {
    ManaCost::generic(0)
}

fn murder(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .with_mana_cost(free())
        .id()
}

fn library_spell(scenario: &mut GameScenario, name: &str, mana_value: u32) -> ObjectId {
    scenario
        .add_spell_to_library_top(P0, name, false)
        .with_mana_cost(ManaCost::generic(mana_value))
        .id()
}

fn pass_to_prompt_or_completion(runner: &mut GameRunner) {
    for _ in 0..16 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            || runner.state().stack.is_empty()
        {
            return;
        }
        runner.pass_both_players();
    }
    panic!("stack failed to settle");
}

fn assert_finished(runner: &GameRunner) {
    assert!(runner.state().stack.is_empty());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
}

fn counters(runner: &GameRunner, object: ObjectId) -> u32 {
    runner.state().objects[&object]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn token_count(runner: &GameRunner, name: &str) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|obj| obj.zone == Zone::Battlefield && obj.name == name)
        .count()
}

fn stage_library_creature(
    runner: &mut GameRunner,
    card: ObjectId,
    mut types: engine::types::card_type::CardType,
) {
    types.subtypes = vec!["Bear".into()];
    let object = runner.state_mut().objects.get_mut(&card).unwrap();
    object.card_types = types.clone();
    object.base_card_types = types;
}

fn radagast_look(own_entry: bool, count: usize) {
    let mut scenario = main_phase();
    let entrant = if own_entry {
        scenario
            .add_creature_to_hand_from_oracle(P0, "Radagast the Brown", 2, 5, RADAGAST)
            .with_subtypes(vec!["Avatar", "Wizard"])
            .with_mana_cost(ManaCost::generic(4))
            .id()
    } else {
        scenario
            .add_creature_from_oracle(P0, "Radagast the Brown", 2, 5, RADAGAST)
            .with_subtypes(vec!["Avatar", "Wizard"])
            .with_mana_cost(ManaCost::generic(4));
        scenario
            .add_creature_to_hand(P0, "Trained Armodon", 3, 3)
            .with_subtypes(vec!["Elephant"])
            .with_mana_cost(ManaCost::generic(3))
            .id()
    };
    let mut library: Vec<_> = ["Forest", "Island", "Swamp", "Mountain"]
        .into_iter()
        .map(|name| scenario.add_land_to_library_top(P0, name).id())
        .collect();
    let eligible = scenario.add_card_to_library_top(P0, "Grizzly Bears");
    library.push(eligible);
    library.reverse();
    let mut runner = scenario.build();
    let card_types = runner.state().objects[&entrant].card_types.clone();
    stage_library_creature(&mut runner, eligible, card_types);
    runner.cast(entrant).resolve();
    pass_to_prompt_or_completion(&mut runner);
    let WaitingFor::DigChoice {
        player,
        cards,
        selectable_cards,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected Radagast's look prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(cards, &library[..count]);
    assert!(selectable_cards.contains(&eligible));
}

#[test]
fn radagast_looks_at_cards_equal_to_the_entering_creatures_mana_value() {
    radagast_look(false, 3);
}

#[test]
fn radagast_uses_its_own_mana_value_when_it_enters() {
    radagast_look(true, 4);
}

#[test]
fn monkey_cage_creates_monkeys_for_the_entering_creatures_mana_value() {
    let mut scenario = main_phase();
    let cage = scenario
        .add_artifact_from_oracle(P0, "Monkey Cage", CAGE)
        .with_mana_cost(ManaCost::generic(5))
        .id();
    let entrant = scenario
        .add_creature_to_hand(P0, "Trained Armodon", 3, 3)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let mut runner = scenario.build();
    runner.cast(entrant).resolve();
    pass_to_prompt_or_completion(&mut runner);
    assert_eq!(runner.state().objects[&cage].zone, Zone::Graveyard);
    assert_eq!(token_count(&runner, "Monkey"), 3);
    assert_finished(&runner);
}

fn choose_optional(runner: &mut GameRunner, accept: bool) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::OptionalEffectChoice { player, .. } if player == P0),
        "{:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept })
        .unwrap();
    pass_to_prompt_or_completion(runner);
}

#[test]
fn kresh_cast_with_emerge_counts_the_creature_that_died() {
    let mut scenario = main_phase();
    scenario.with_mana_pool(P0, pool([ManaType::Black, ManaType::Red, ManaType::Green]));
    let fodder = scenario
        .add_creature(P0, "Fodder", 1, 1)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let victim = scenario
        .add_creature(P1, "Grizzly Bears", 2, 2)
        .with_plus_counters(3)
        .id();
    let mana_cost = ManaCost::Cost {
        generic: 2,
        shards: vec![
            ManaCostShard::Black,
            ManaCostShard::Red,
            ManaCostShard::Green,
        ],
    };
    // Herigast, Erupting Nullkite: "Each creature spell you cast has emerge. The emerge
    // cost is equal to its mana cost." The engine does not yet offer the printed grant.
    let kresh = scenario
        .add_creature_to_hand_from_oracle(P0, "Kresh the Bloodbraided", 3, 3, KRESH)
        .with_mana_cost(mana_cost.clone())
        .with_keyword(Keyword::Emerge(EmergeCost::creature(mana_cost)))
        .id();
    let spell = murder(&mut scenario);
    let mut runner = scenario.build();
    runner
        .cast(kresh)
        .casting_variant(CastingVariant::Emerge)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .sacrifice_with(&[fodder])
        .resolve();
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&kresh]
            .cast_cost_paid_object
            .as_ref()
            .unwrap()
            .object_id,
        fodder
    );
    runner
        .cast(spell)
        .target_object(victim)
        .commit()
        .stop_at_optional()
        .resolve();
    choose_optional(&mut runner, true);
    // CR 608.2k: the trigger condition refers to the creature Murder destroyed.
    assert_eq!(counters(&runner, kresh), 5);
    assert_finished(&runner);
}

fn kresh_death(kresh_dies: bool, accept: bool, expected: u32) {
    let mut scenario = main_phase();
    let kresh = scenario
        .add_creature_from_oracle(P0, "Kresh the Bloodbraided", 3, 3, KRESH)
        .id();
    let victim = if kresh_dies {
        kresh
    } else {
        scenario
            .add_creature(P1, "Grizzly Bears", 2, 2)
            .with_plus_counters(3)
            .id()
    };
    let spell = murder(&mut scenario);
    let mut runner = scenario.build();
    runner
        .cast(spell)
        .target_object(victim)
        .commit()
        .stop_at_optional()
        .resolve();
    assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
    if !kresh_dies {
        choose_optional(&mut runner, accept);
        // CR 603.10a + CR 608.2h: the dies trigger uses departure power.
        assert_eq!(counters(&runner, kresh), expected);
    }
    assert_finished(&runner);
}

#[test]
fn kresh_counts_the_dead_creatures_last_known_power() {
    kresh_death(false, true, 5);
}

#[test]
fn kresh_decline_puts_no_counters() {
    kresh_death(false, false, 0);
}

#[test]
fn kresh_does_not_trigger_on_its_own_death() {
    kresh_death(true, false, 0);
}

fn assert_discover(runner: &GameRunner, expected: u32, hit: ObjectId) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::CastOffer {
        player,
        kind: CastOfferKind::Discover { discover_value, hit_card, .. },
        ..
    } if player == P0 && discover_value == expected && hit_card == hit),
        "expected discover {expected} hitting {hit:?}, got {:?}",
        runner.state().waiting_for
    );
}

fn pantlaza_entry(own_entry: bool, dinosaur: bool) {
    let mut scenario = main_phase();
    let name = "Pantlaza, Sun-Favored";
    let entrant = if own_entry {
        scenario
            .add_creature_to_hand_from_oracle(P0, name, 4, 4, PANTLAZA)
            .with_subtypes(vec!["Dinosaur"])
            .with_mana_cost(free())
            .id()
    } else {
        scenario
            .add_creature_from_oracle(P0, name, 4, 4, PANTLAZA)
            .with_subtypes(vec!["Dinosaur"]);
        scenario
            .add_creature_to_hand(
                P0,
                if dinosaur {
                    "Raptor Companion"
                } else {
                    "Grizzly Bears"
                },
                2,
                3,
            )
            .with_subtypes(vec![if dinosaur { "Dinosaur" } else { "Bear" }])
            .with_mana_cost(free())
            .id()
    };
    let value = if own_entry { 4 } else { 3 };
    let hit = library_spell(&mut scenario, "Eligible discover card", value);
    library_spell(&mut scenario, "Over the discover limit", value + 1);
    let mut runner = scenario.build();
    runner.cast(entrant).commit().stop_at_optional().resolve();
    assert_eq!(runner.state().objects[&entrant].zone, Zone::Battlefield);
    if own_entry || dinosaur {
        choose_optional(&mut runner, true);
        assert_discover(&runner, value, hit);
    } else {
        assert_finished(&runner);
    }
}

#[test]
fn pantlaza_discovers_for_the_entering_dinosaurs_toughness() {
    pantlaza_entry(false, true);
}

#[test]
fn pantlaza_discovers_for_its_own_toughness_when_it_enters() {
    pantlaza_entry(true, true);
}

#[test]
fn pantlaza_ignores_a_non_dinosaur() {
    pantlaza_entry(false, false);
}

#[test]
fn pantlaza_uses_last_known_toughness_when_the_dinosaur_has_left() {
    let mut scenario = main_phase();
    scenario
        .add_creature_from_oracle(P0, "Pantlaza, Sun-Favored", 4, 4, PANTLAZA)
        .with_subtypes(vec!["Dinosaur"]);
    let dinosaur = scenario
        .add_creature_to_hand(P0, "Dinosaur", 2, 2)
        .with_subtypes(vec!["Dinosaur"])
        .with_mana_cost(free())
        .id();
    let pump = scenario
        .add_spell_to_hand_from_oracle(P0, "Giant Growth", true, GROWTH)
        .with_mana_cost(free())
        .id();
    let spell = murder(&mut scenario);
    let hit = library_spell(&mut scenario, "Eligible discover card", 5);
    let mut runner = scenario.build();
    runner.cast(dinosaur).commit();
    runner.pass_both_players();
    assert_eq!(runner.state().objects[&dinosaur].zone, Zone::Battlefield);
    assert_eq!(runner.state().stack.len(), 1);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    runner.cast(pump).target_object(dinosaur).commit();
    runner.pass_both_players();
    assert_eq!(runner.state().stack.len(), 1);
    runner.cast(spell).target_object(dinosaur).commit();
    runner.pass_both_players();
    assert_eq!(runner.state().objects[&dinosaur].zone, Zone::Graveyard);
    runner.pass_both_players();
    choose_optional(&mut runner, true);
    // CR 608.2h: use toughness at departure, including Giant Growth.
    assert_discover(&runner, 5, hit);
}

#[test]
fn beifongs_bounty_hunters_earthbends_for_the_dead_creatures_power() {
    let mut scenario = main_phase();
    scenario.add_creature_from_oracle(P0, "Beifong's Bounty Hunters", 4, 4, BEIFONG);
    let victim = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_plus_counters(3)
        .id();
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    let spell = murder(&mut scenario);
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_objects(&[victim, land]).resolve();
    outcome.assert_zone(&[victim], Zone::Graveyard);
    assert_eq!(counters(&runner, land), 5);
    assert_finished(&runner);
}

fn amulet_entry(opponent: bool) {
    let mut scenario = main_phase();
    scenario.with_mana_pool(P1, pool([ManaType::Green; 3]));
    let amulet = scenario
        .add_artifact_from_oracle(P0, "Quickbeast Amulet", "")
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(AMULET)
        .id();
    let wearer = scenario.add_creature(P0, "Wearer", 1, 1).id();
    let entrant = scenario
        .add_creature_to_hand(
            if opponent { P1 } else { P0 },
            "Entrant",
            3,
            if opponent { 3 } else { 5 },
        )
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let mut runner = scenario.build();
    runner.activate(amulet, 0).target_object(wearer).resolve();
    assert_eq!(
        runner.state().objects[&amulet].attached_to,
        Some(AttachTarget::Object(wearer))
    );
    if opponent {
        runner.state_mut().active_player = P1;
        runner.state_mut().priority_player = P1;
        runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    }
    let outcome = runner.cast(entrant).resolve();
    outcome.assert_zone(&[entrant], Zone::Battlefield);
    let expected = if opponent { 0 } else { 3 };
    assert_eq!(outcome.state().objects[&amulet].intensity, expected);
    // The static reads the new intensity at once: 1/1 plus X/X.
    outcome.assert_power_toughness(wearer, 1 + expected as i32, 1 + expected as i32);
    assert_finished(&runner);
}

#[test]
fn quickbeast_amulet_intensifies_by_the_entering_creatures_power() {
    amulet_entry(false);
}

#[test]
fn quickbeast_amulet_ignores_an_opponents_creature() {
    amulet_entry(true);
}

fn stage_mystic(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(P0, "Rattleclaw Mystic", 2, 1, MYSTIC)
        .with_subtypes(vec!["Human", "Shaman"])
        .with_mana_cost(ManaCost::generic(2))
        .with_keyword(Keyword::Morph(engine::types::keywords::MorphCost::Mana(
            ManaCost::generic(2),
        )))
        .id()
}

fn cast_mystic_face_down(runner: &mut GameRunner, mystic: ObjectId) {
    runner
        .cast(mystic)
        .casting_variant(CastingVariant::FaceDown)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .commit();
    runner.pass_both_players();
    let object = &runner.state().objects[&mystic];
    assert_eq!(object.zone, Zone::Battlefield);
    assert!(object.face_down);
}

#[test]
fn hamza_endures_for_the_face_up_creatures_toughness() {
    let mut scenario = main_phase();
    scenario.add_creature_from_oracle(P0, "Hamza, Might of the Yathan", 5, 6, HAMZA);
    let mystic = stage_mystic(&mut scenario);
    let mut runner = scenario.build();
    cast_mystic_face_down(&mut runner, mystic);
    assert_finished(&runner);
    runner
        .act(GameAction::TurnFaceUp {
            object_id: mystic,
            x: 0,
        })
        .unwrap();
    assert!(!runner.state().objects[&mystic].face_down);
    if let WaitingFor::OrderTriggers { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::OrderTriggers { order: vec![0, 1] })
            .unwrap();
    }
    pass_to_prompt_or_completion(&mut runner);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ChooseOneOfBranch { player, .. } if player == P0),
        "{:?}",
        runner.state().waiting_for
    );
    runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
    pass_to_prompt_or_completion(&mut runner);
    // CR 701.63a: choose counters on the enduring creature, whose face-up toughness is 1.
    assert_eq!(counters(&runner, mystic), 1);
    assert_finished(&runner);
}

fn enter_face_down_then_blink(is_radagast: bool) {
    let mut scenario = main_phase();
    let source = if is_radagast {
        scenario
            .add_creature_from_oracle(P0, "Radagast the Brown", 2, 5, RADAGAST)
            .with_subtypes(vec!["Avatar", "Wizard"])
            .id()
    } else {
        scenario
            .add_artifact_from_oracle(P0, "Monkey Cage", CAGE)
            .id()
    };
    let mystic = stage_mystic(&mut scenario);
    let blink = scenario
        .add_spell_to_hand_from_oracle(P0, "Cloudshift", true, CLOUDSHIFT)
        .with_mana_cost(free())
        .id();
    let eligible = scenario.add_card_to_library_top(P0, "Grizzly Bears");
    let mut runner = scenario.build();
    let types = runner.state().objects[&mystic].card_types.clone();
    stage_library_creature(&mut runner, eligible, types);
    cast_mystic_face_down(&mut runner, mystic);
    assert_eq!(runner.state().stack.len(), 1);
    let face_down_incarnation = runner.state().objects[&mystic].incarnation;
    // CR 708.2a + CR 202.3: the face-down creature has no mana cost.
    assert_eq!(runner.state().objects[&mystic].effective_mana_value(), 0);
    runner.cast(blink).target_object(mystic).commit();
    runner.pass_both_players();
    assert_eq!(runner.state().stack.len(), 2);
    let returned = &runner.state().objects[&mystic];
    assert_eq!(returned.zone, Zone::Battlefield);
    assert!(!returned.face_down);
    assert_ne!(returned.incarnation, face_down_incarnation);
    assert_eq!(returned.effective_mana_value(), 2);

    // CR 400.7 + CR 608.2h: resolve the face-up entry first, then the old face-down entry.
    runner.pass_both_players();
    if is_radagast {
        assert!(
            matches!(&runner.state().waiting_for, WaitingFor::DigChoice { selectable_cards, .. }
            if selectable_cards.contains(&eligible))
        );
        runner
            .act(GameAction::SelectCards { cards: vec![] })
            .unwrap();
        assert_eq!(runner.state().stack.len(), 1);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    } else {
        assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
        assert_eq!(token_count(&runner, "Monkey"), 2);
        assert_eq!(runner.state().stack.len(), 1);
    }
    pass_to_prompt_or_completion(&mut runner);
    if is_radagast {
        assert_eq!(runner.state().objects[&eligible].zone, Zone::Library);
    } else {
        assert_eq!(token_count(&runner, "Monkey"), 2);
    }
    assert_finished(&runner);
}

#[test]
fn radagast_reads_the_face_down_incarnation_after_cloudshift() {
    enter_face_down_then_blink(true);
}

#[test]
fn monkey_cage_reads_the_face_down_incarnation_after_cloudshift() {
    enter_face_down_then_blink(false);
}

fn sacrifice_on_entry(name: &str, oracle: &str, expected_life: i32, expected_mill: usize) {
    let mut scenario = main_phase();
    let entrant = if name == "Altar of the Wretched" {
        scenario
            .add_artifact_to_hand_from_oracle(P0, name, oracle)
            .with_mana_cost(free())
            .id()
    } else {
        scenario
            .add_creature_to_hand_from_oracle(P0, name, 2, 1, oracle)
            .with_mana_cost(free())
            .id()
    };
    let fodder = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_plus_counters(3)
        .id();
    scenario.with_library_top(P0, &["Forest"; 12]);
    let mut runner = scenario.build();
    let life = runner.life(P0);
    runner.cast(entrant).accept_optional().resolve();
    pass_to_prompt_or_completion(&mut runner);
    if let WaitingFor::EffectZoneChoice { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::SelectCards {
                cards: vec![fodder],
            })
            .unwrap();
        pass_to_prompt_or_completion(&mut runner);
    }
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
    assert_eq!(runner.life(P0) - life, expected_life);
    assert_eq!(runner.state().players[0].hand.len(), 5);
    assert_eq!(runner.state().players[0].graveyard.len(), 1 + expected_mill);
    assert_finished(&runner);
}

#[test]
fn disciple_of_bolas_reads_the_sacrificed_creature() {
    sacrifice_on_entry("Disciple of Bolas", BOLAS, 5, 0);
}

#[test]
fn disciple_of_freyalise_reads_the_sacrificed_creature() {
    sacrifice_on_entry("Disciple of Freyalise", FREYALISE, 5, 0);
}

#[test]
fn altar_of_the_wretched_reads_the_sacrificed_creature() {
    sacrifice_on_entry("Altar of the Wretched", ALTAR, 0, 5);
}

#[test]
fn shadowheart_reads_the_sacrificed_creature() {
    let mut scenario = main_phase();
    scenario.with_mana_pool(P0, pool([ManaType::Black, ManaType::Colorless]));
    let shadowheart = scenario
        .add_creature_from_oracle(P0, "Shadowheart, Dark Justiciar", 3, 4, SHADOWHEART)
        .id();
    let fodder = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_plus_counters(3)
        .id();
    scenario.with_library_top(P0, &["Forest"; 6]);
    let mut runner = scenario.build();
    runner
        .activate(shadowheart, 0)
        .pay_with(&[fodder])
        .resolve();
    pass_to_prompt_or_completion(&mut runner);
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[0].hand.len(), 5);
    assert_finished(&runner);
}

#[test]
fn arni_gets_power_from_the_entering_creature() {
    let mut scenario = main_phase();
    let arni = scenario.add_creature_from_oracle(P0, "Arni, Renowned Champion", 1, 5, "Trample\nWhenever another creature you control enters, Arni gets +X/+0 until end of turn, where X is that creature's power.").id();
    let entrant = scenario
        .add_creature_to_hand(P0, "Trained Armodon", 3, 3)
        .with_mana_cost(free())
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(entrant).resolve();
    // CR 613.4c: the adjustment adds X power to Arni's printed power.
    outcome.assert_power_toughness(arni, 4, 5);
    assert_finished(&runner);
}

#[test]
fn hamletback_gets_counters_for_the_entering_creatures_power() {
    let mut scenario = main_phase();
    let goliath = scenario.add_creature_from_oracle(P0, "Hamletback Goliath", 6, 6, "Whenever another creature enters, you may put X +1/+1 counters on this creature, where X is that creature's power.").id();
    let entrant = scenario
        .add_creature_to_hand(P0, "Trained Armodon", 3, 3)
        .with_mana_cost(free())
        .id();
    let mut runner = scenario.build();
    runner.cast(entrant).commit().stop_at_optional().resolve();
    choose_optional(&mut runner, true);
    assert_eq!(counters(&runner, goliath), 3);
    assert_finished(&runner);
}

#[test]
fn cream_of_the_crop_looks_at_cards_equal_to_the_entering_creatures_power() {
    let mut scenario = main_phase();
    scenario.add_enchantment_from_oracle(P0, "Cream of the Crop", "Whenever a creature you control enters, you may look at the top X cards of your library, where X is that creature's power. If you do, put one of those cards on top of your library and the rest on the bottom of your library in any order.");
    let entrant = scenario
        .add_creature_to_hand(P0, "Trained Armodon", 3, 3)
        .with_mana_cost(free())
        .id();
    scenario.with_library_top(P0, &["Forest", "Island", "Swamp", "Mountain", "Plains"]);
    let mut runner = scenario.build();
    let top: Vec<_> = runner.state().players[0]
        .library
        .iter()
        .take(3)
        .copied()
        .collect();
    runner.cast(entrant).commit().stop_at_optional().resolve();
    choose_optional(&mut runner, true);
    // Only the look count is under test. The bottom-of-library continuation is a known gap.
    let WaitingFor::EffectZoneChoice { player, cards, .. } = &runner.state().waiting_for else {
        panic!("{:?}", runner.state().waiting_for);
    };
    assert_eq!(*player, P0);
    assert_eq!(*cards, top);
}

#[test]
fn one_with_the_kami_counts_the_enchanted_creatures_power() {
    let mut scenario = main_phase();
    let host = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_plus_counters(2)
        .id();
    let aura = scenario.add_enchantment_to_hand_from_oracle(P0, "One with the Kami", "Flash\nEnchant creature you control\nWhenever enchanted creature or another modified creature you control dies, create X 1/1 colorless Spirit creature tokens, where X is that creature's power. (Equipment, Auras you control, and counters are modifications.)")
        .with_subtypes(vec!["Aura"]).with_mana_cost(free()).id();
    let spell = murder(&mut scenario);
    let mut runner = scenario.build();
    runner.cast(aura).target_object(host).resolve();
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(host))
    );
    runner.cast(spell).target_object(host).resolve();
    assert_eq!(token_count(&runner, "Spirit"), 4);
    assert_finished(&runner);
}
