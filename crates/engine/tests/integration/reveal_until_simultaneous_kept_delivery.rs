//! Reveal-until "put those cards onto the battlefield" delivers every matched
//! card as ONE simultaneous zone change (CR 608.2f), not one zone change per
//! card.
//!
//! Kindred Summons reveals until X creature cards of the chosen type and puts
//! "those cards" onto the battlefield — one instruction acting on several
//! objects. Two observable consequences pin the batch:
//!
//! * CR 603.6a: when one event puts several permanents onto the battlefield,
//!   every permanent on the battlefield — including the newcomers — is checked
//!   for enters triggers matching that event. An Elvish Warmaster entering
//!   alongside another Elf therefore sees that Elf enter.
//! * CR 616.1 / CR 614.1c: when one matched card's entry pauses on an as-enters
//!   replacement choice (Rhythm of the Wild's granted riot), the remaining
//!   matched cards ride the same parked batch and still enter once the choice
//!   is answered — none is stranded in the library.

use engine::game::combat::{build_declare_attackers_waiting_for, AttackTarget};
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{TargetFilter, TargetRef, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const KINDRED_SUMMONS: &str = "Choose a creature type. Reveal cards from the top of your library until you reveal X creature cards of the chosen type, where X is the number of creatures you control of that type. Put those cards onto the battlefield, then shuffle the rest of the revealed cards into your library.";

const ELVISH_WARMASTER: &str = "Whenever one or more other Elves you control enter, create a 1/1 green Elf Warrior creature token. This ability triggers only once each turn.\n\
{5}{G}{G}: Elves you control get +2/+2 and gain deathtouch until end of turn.";

const ELVISH_VANGUARD: &str = "Whenever another Elf enters, put a +1/+1 counter on this creature.";

const RHYTHM_OF_THE_WILD: &str = "Creature spells you control can't be countered.\n\
Nontoken creatures you control have riot. (They enter with your choice of a +1/+1 counter or haste.)";

const WILD_MAGIC_SURGE: &str = "Destroy target permanent an opponent controls. Its controller reveals cards from the top of their library until they reveal a permanent card that shares a card type with that permanent. They put that card onto the battlefield and the rest on the bottom of their library in a random order.";

/// Move `id` (wherever the scenario staged it) to the top of its owner's
/// library. Mirrors `issue_3252_rhythm_of_the_wild::put_library_top`.
fn put_library_top(runner: &mut GameRunner, id: ObjectId) {
    let owner = runner.state().objects[&id].owner;
    let zone = runner.state().objects[&id].zone;
    let mut events = Vec::new();
    if zone != Zone::Library {
        engine::game::zones::remove_from_zone(runner.state_mut(), id, zone, owner);
        runner.state_mut().objects.get_mut(&id).unwrap().zone = Zone::Library;
        runner.state_mut().players[owner.0 as usize]
            .library
            .push_back(id);
    }
    engine::game::zones::move_to_library_position(runner.state_mut(), id, true, &mut events);
}

/// Arrange `ids` as the top of their owner's library, `ids[0]` on top, and
/// assert the arrangement took (reach guard for every test's reveal order).
fn arrange_library_top_first(runner: &mut GameRunner, ids: &[ObjectId]) {
    for &id in ids.iter().rev() {
        put_library_top(runner, id);
    }
    let owner = runner.state().objects[&ids[0]].owner;
    let top: Vec<ObjectId> = runner.state().players[owner.0 as usize]
        .library
        .iter()
        .take(ids.len())
        .copied()
        .collect();
    assert_eq!(
        top, ids,
        "library top must be staged in the requested order"
    );
}

/// Kindred Summons in P0's hand, free to cast.
fn add_kindred_summons(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Kindred Summons", false, KINDRED_SUMMONS)
        .with_mana_cost(ManaCost::generic(0))
        .id()
}

fn add_vanilla_elf(scenario: &mut GameScenario, name: &str) -> ObjectId {
    scenario
        .add_creature(P0, name, 1, 1)
        .with_subtypes(vec!["Elf"])
        .id()
}

fn add_elvish_warmaster(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, "Elvish Warmaster", 1, 1, ELVISH_WARMASTER)
        .with_subtypes(vec!["Elf", "Warrior"])
        .id()
}

fn elf_warrior_token_count(runner: &GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|obj| obj.zone == Zone::Battlefield && obj.name == "Elf Warrior")
        .count()
}

fn plus_one_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn assert_zone(runner: &GameRunner, id: ObjectId, zone: Zone, what: &str) {
    assert_eq!(
        runner.state().objects[&id].zone,
        zone,
        "{what} must be in {zone:?}"
    );
}

fn assert_stack_empty_priority(runner: &GameRunner) {
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the spell and every trigger must have resolved; waiting_for={}, stack={:?}",
        runner.waiting_for_kind(),
        runner.stack_names()
    );
}

/// CR 608.2f + CR 603.6a: the matched set enters as one event, so Elvish
/// Warmaster — itself the SECOND matched card — sees the first matched Elf
/// enter alongside it and triggers once. Per-card delivery moved the first Elf
/// before Warmaster was on the battlefield, so Warmaster never saw it.
#[test]
fn kindred_summons_co_entering_warmaster_sees_the_other_elf() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // X = 2 Elves on the battlefield.
    add_vanilla_elf(&mut scenario, "Llanowar Elves");
    add_vanilla_elf(&mut scenario, "Elvish Mystic");

    let island = scenario.add_card_to_library_top(P0, "Island");
    let hit_elf = add_vanilla_elf(&mut scenario, "Fyndhorn Elves");
    let bears = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    let warmaster = add_elvish_warmaster(&mut scenario);
    let forest = scenario.add_card_to_library_top(P0, "Forest");
    let spell = add_kindred_summons(&mut scenario);

    let mut runner = scenario.build();
    arrange_library_top_first(&mut runner, &[island, hit_elf, bears, warmaster, forest]);

    runner.cast(spell).choose_option("Elf").resolve();
    assert_stack_empty_priority(&runner);

    // Reach guards: exactly the two Elves were matched and entered.
    assert_zone(&runner, hit_elf, Zone::Battlefield, "the first matched Elf");
    assert_zone(&runner, warmaster, Zone::Battlefield, "Elvish Warmaster");
    assert_zone(&runner, island, Zone::Library, "the revealed Island");
    assert_zone(&runner, bears, Zone::Library, "the revealed Grizzly Bears");
    assert_zone(&runner, forest, Zone::Library, "the unrevealed Forest");

    // Discriminating assertion (CR 603.6a): the co-entering Warmaster saw the
    // other Elf enter in the same event.
    assert_eq!(
        elf_warrior_token_count(&runner),
        1,
        "Elvish Warmaster entering with another Elf must trigger once (CR 603.6a)"
    );
}

/// Preservation: Warmaster as first co-entrant sees the second (derived 1 per
/// CR 603.6a; BASE 1, since Warmaster's entry index precedes the Elf's).
#[test]
fn kindred_summons_warmaster_as_first_hit_fires_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    add_vanilla_elf(&mut scenario, "Llanowar Elves");
    add_vanilla_elf(&mut scenario, "Elvish Mystic");

    let warmaster = add_elvish_warmaster(&mut scenario);
    let island = scenario.add_card_to_library_top(P0, "Island");
    let hit_elf = add_vanilla_elf(&mut scenario, "Fyndhorn Elves");
    let spell = add_kindred_summons(&mut scenario);

    let mut runner = scenario.build();
    arrange_library_top_first(&mut runner, &[warmaster, island, hit_elf]);

    runner.cast(spell).choose_option("Elf").resolve();
    assert_stack_empty_priority(&runner);

    assert_zone(&runner, warmaster, Zone::Battlefield, "Elvish Warmaster");
    assert_zone(
        &runner,
        hit_elf,
        Zone::Battlefield,
        "the second matched Elf",
    );
    assert_zone(&runner, island, Zone::Library, "the revealed Island");
    assert_eq!(
        elf_warrior_token_count(&runner),
        1,
        "Elvish Warmaster entering with another Elf must trigger exactly once"
    );
}

/// CR 616.1 + CR 614.1c + CR 702.136a: Rhythm of the Wild grants riot, so each
/// matched Elf's entry pauses on a replacement choice. The first pause must not
/// strand the second matched Elf: it rides the same parked batch, surfaces its
/// own riot choice once the first is answered, and enters — after which the
/// rest pile, reveal markers, and the Shuffle continuation all settle once.
#[test]
fn kindred_summons_riot_pause_on_first_hit_still_delivers_second_hit() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Rhythm of the Wild", 0, 0, RHYTHM_OF_THE_WILD)
        .as_enchantment();
    add_vanilla_elf(&mut scenario, "Llanowar Elves");
    add_vanilla_elf(&mut scenario, "Elvish Mystic");

    let hit_one = add_vanilla_elf(&mut scenario, "Fyndhorn Elves");
    let island = scenario.add_card_to_library_top(P0, "Island");
    let hit_two = add_vanilla_elf(&mut scenario, "Elvish Visionary");
    let spell = add_kindred_summons(&mut scenario);

    let mut runner = scenario.build();
    arrange_library_top_first(&mut runner, &[hit_one, island, hit_two]);

    runner.cast(spell).choose_option("Elf").resolve();

    // Reach guard: the first matched Elf's entry pauses on its riot choice.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the first matched Elf's entry must pause on its riot choice; waiting_for={}",
        runner.waiting_for_kind()
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("choose riot's +1/+1 counter for the first Elf");
    assert_zone(&runner, hit_one, Zone::Battlefield, "the first matched Elf");

    // Discriminating assertion: the second matched Elf was parked on the same
    // batch, so its own riot choice surfaces now.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the second matched Elf must surface its own riot choice instead of stranding \
         in the library; waiting_for={}, second Elf zone={:?}",
        runner.waiting_for_kind(),
        runner.state().objects[&hit_two].zone
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("choose riot's +1/+1 counter for the second Elf");

    // Drain any remaining priority passes (the spell itself has resolved).
    runner.advance_until_stack_empty();
    assert_stack_empty_priority(&runner);

    assert_zone(
        &runner,
        hit_two,
        Zone::Battlefield,
        "the second matched Elf",
    );
    assert_eq!(plus_one_counters(&runner, hit_one), 1);
    assert_eq!(plus_one_counters(&runner, hit_two), 1);
    assert_zone(&runner, island, Zone::Library, "the revealed Island");
    for id in [hit_one, island, hit_two] {
        assert!(
            !runner.state().revealed_cards.contains(&id),
            "reveal markers must be cleared once the batch settles ({id:?})"
        );
    }
    assert!(
        runner.state().active_batch_delivery().is_none(),
        "the parked batch frame must be fully drained"
    );
}

/// Preservation (CR 603.2c): a pre-existing batched "one or more other Elves"
/// observer triggers once for the whole matched set.
#[test]
fn kindred_summons_preexisting_warmaster_fires_once_for_two_elves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // X = 2: Warmaster + one vanilla Elf.
    add_elvish_warmaster(&mut scenario);
    add_vanilla_elf(&mut scenario, "Llanowar Elves");

    let hit_one = add_vanilla_elf(&mut scenario, "Fyndhorn Elves");
    let island = scenario.add_card_to_library_top(P0, "Island");
    let hit_two = add_vanilla_elf(&mut scenario, "Elvish Visionary");
    let spell = add_kindred_summons(&mut scenario);

    let mut runner = scenario.build();
    arrange_library_top_first(&mut runner, &[hit_one, island, hit_two]);

    runner.cast(spell).choose_option("Elf").resolve();
    assert_stack_empty_priority(&runner);

    assert_zone(&runner, hit_one, Zone::Battlefield, "the first matched Elf");
    assert_zone(
        &runner,
        hit_two,
        Zone::Battlefield,
        "the second matched Elf",
    );
    assert_zone(&runner, island, Zone::Library, "the revealed Island");
    assert_eq!(
        elf_warrior_token_count(&runner),
        1,
        "a batched observer triggers once for the one simultaneous entry"
    );
}

/// No double fire (CR 603.6a + CR 603.2c): a pre-existing per-event observer
/// with no once-per-turn limit ("Whenever another Elf enters") triggers once
/// per entering Elf — two matched Elves, two counters. Re-collecting the
/// batch's events after the per-move collection would give four.
#[test]
fn kindred_summons_preexisting_per_event_observer_counts_each_entry_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // X = 2: Vanguard + one vanilla Elf.
    let vanguard = scenario
        .add_creature_from_oracle(P0, "Elvish Vanguard", 1, 1, ELVISH_VANGUARD)
        .with_subtypes(vec!["Elf", "Warrior"])
        .id();
    add_vanilla_elf(&mut scenario, "Llanowar Elves");

    let elf_a = add_vanilla_elf(&mut scenario, "Fyndhorn Elves");
    let island = scenario.add_card_to_library_top(P0, "Island");
    let elf_b = add_vanilla_elf(&mut scenario, "Elvish Visionary");
    let spell = add_kindred_summons(&mut scenario);

    let mut runner = scenario.build();
    arrange_library_top_first(&mut runner, &[elf_a, island, elf_b]);

    runner.cast(spell).choose_option("Elf").resolve();
    assert_stack_empty_priority(&runner);

    // Reach guards: both matched Elves entered.
    assert_zone(&runner, elf_a, Zone::Battlefield, "Elf A");
    assert_zone(&runner, elf_b, Zone::Battlefield, "Elf B");
    assert_zone(&runner, island, Zone::Library, "the revealed Island");
    assert_eq!(
        plus_one_counters(&runner, vanguard),
        2,
        "Elvish Vanguard must trigger exactly once per entering Elf"
    );
}

/// Single-hit regression: a batch of one behaves exactly like the historical
/// per-card move.
#[test]
fn kindred_summons_single_hit_regression() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // X = 1: only Warmaster.
    add_elvish_warmaster(&mut scenario);

    let island = scenario.add_card_to_library_top(P0, "Island");
    let hit = add_vanilla_elf(&mut scenario, "Fyndhorn Elves");
    let spell = add_kindred_summons(&mut scenario);

    let mut runner = scenario.build();
    arrange_library_top_first(&mut runner, &[island, hit]);

    runner.cast(spell).choose_option("Elf").resolve();
    assert_stack_empty_priority(&runner);

    assert_zone(&runner, hit, Zone::Battlefield, "the matched Elf");
    assert_zone(&runner, island, Zone::Library, "the revealed Island");
    assert_eq!(elf_warrior_token_count(&runner), 1);
}

/// CR 303.4f: a single matched Aura entering by Wild Magic Surge's reveal has
/// two legal hosts, so its entry pauses on an attachment choice inside the
/// kept-set batch. Answering it attaches the Aura, bottoms the miss, clears the
/// reveal markers, and drains the parked batch frame.
#[test]
fn wild_magic_surge_kept_aura_pause_attaches_and_settles_rest_pile() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The destroyed permanent is an enchantment, so the reveal stops at the
    // first enchantment card.
    let doomed = scenario
        .add_creature(P1, "Doomed Enchantment", 0, 0)
        .as_enchantment()
        .id();
    // Two legal hosts for the revealed Aura (CR 303.4f forces a choice).
    let host_a = scenario.add_creature(P1, "Host A", 2, 2).id();
    let host_b = scenario.add_creature(P1, "Host B", 2, 2).id();

    let shock = scenario.add_spell_to_library_top(P1, "Shock", true).id();
    let aura = scenario.add_card_to_library_top(P1, "Revealed Aura");
    let filler = scenario.add_card_to_library_top(P1, "Filler");

    let surge = scenario
        .add_spell_to_hand_from_oracle(P0, "Wild Magic Surge", true, WILD_MAGIC_SURGE)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&aura).unwrap();
        obj.card_types.core_types = vec![CoreType::Enchantment];
        obj.card_types.subtypes = vec!["Aura".to_string()];
        obj.base_card_types = obj.card_types.clone();
        obj.keywords.push(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature(),
        )));
    }
    arrange_library_top_first(&mut runner, &[shock, aura, filler]);

    runner.cast(surge).target_object(doomed).resolve();

    // Reach guards: the target was destroyed and the Aura's entry paused.
    assert_zone(
        &runner,
        doomed,
        Zone::Graveyard,
        "the destroyed enchantment",
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReturnAsAuraTarget { .. }
        ),
        "the revealed Aura's entry must surface a CR 303.4f attachment choice; waiting_for={}",
        runner.waiting_for_kind()
    );

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(host_a)),
        })
        .expect("attach the revealed Aura to Host A");
    runner.advance_until_stack_empty();

    let state = runner.state();
    assert!(
        !matches!(state.waiting_for, WaitingFor::ReturnAsAuraTarget { .. }),
        "the attachment choice must be resolved"
    );
    assert_zone(&runner, aura, Zone::Battlefield, "the revealed Aura");
    assert_eq!(
        state.objects[&aura].attached_to,
        Some(AttachTarget::Object(host_a)),
        "the Aura must be attached to the chosen host"
    );
    let _ = host_b;
    assert_zone(&runner, shock, Zone::Library, "the revealed Shock");
    assert_eq!(
        state.players[P1.0 as usize].library.back().copied(),
        Some(shock),
        "the revealed miss must go to the bottom of its owner's library"
    );
    assert_eq!(
        state.players[P1.0 as usize].library.front().copied(),
        Some(filler),
        "the unrevealed card stays on top"
    );
    assert!(
        state.active_batch_delivery().is_none(),
        "the parked batch frame must be fully drained"
    );
    for id in [shock, aura] {
        assert!(
            !state.revealed_cards.contains(&id),
            "reveal markers must be cleared once the batch settles ({id:?})"
        );
    }
}

const RAPH_AND_MIKEY: &str = "Trample, haste\n\
Whenever Raph & Mikey attack, reveal cards from the top of your library until you reveal a creature card. Put that card onto the battlefield tapped and attacking and the rest on the bottom of your library in a random order.";

const DESTROY_THE_EVIDENCE: &str = "Destroy target land. Its controller reveals cards from the top of their library until they reveal a land card, then puts those cards into their graveyard.";

const DEVOURER_OF_MEMORY: &str = "Whenever one or more cards are put into your graveyard from your library, this creature gets +1/+1 until end of turn and can't be blocked this turn.\n\
{1}{U}{B}: Mill a card.";

/// CR 508.4 + CR 616.1 + CR 614.1c: Raph & Mikey's revealed creature is put
/// onto the battlefield "tapped and attacking". Rhythm of the Wild grants it
/// riot, so its entry pauses on a replacement choice. The attack placement
/// rides the entry itself, so once the choice is answered the creature is an
/// attacking creature — a placement made only after a synchronous batch would
/// be skipped by the pause.
#[test]
fn raph_and_mikey_kept_creature_entering_through_a_riot_pause_is_attacking() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);

    scenario
        .add_creature_from_oracle(P0, "Rhythm of the Wild", 0, 0, RHYTHM_OF_THE_WILD)
        .as_enchantment();
    let raph = scenario
        .add_creature_from_oracle(P0, "Raph & Mikey, Troublemakers", 7, 7, RAPH_AND_MIKEY)
        .id();
    let island = scenario.add_card_to_library_top(P0, "Island");
    let dreadmaw = scenario.add_creature(P0, "Colossal Dreadmaw", 6, 6).id();
    let filler = scenario.add_card_to_library_top(P0, "Filler");

    let mut runner = scenario.build();
    arrange_library_top_first(&mut runner, &[island, dreadmaw, filler]);

    runner.state_mut().waiting_for = build_declare_attackers_waiting_for(runner.state());
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(raph, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("Raph & Mikey attacks P1");

    // Resolve the attack trigger.
    for _ in 0..10 {
        if runner.state().stack.is_empty()
            || !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to resolve the attack trigger");
    }

    // Reach guard: the revealed creature's entry pauses on its riot choice.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the revealed creature's entry must pause on its riot choice; waiting_for={}",
        runner.waiting_for_kind()
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("choose riot's +1/+1 counter for the revealed creature");

    // Reach guards: the revealed creature entered tapped with its riot counter,
    // the miss went to the bottom, and Raph & Mikey is still attacking.
    assert_zone(
        &runner,
        dreadmaw,
        Zone::Battlefield,
        "the revealed creature",
    );
    assert!(
        runner.state().objects[&dreadmaw].tapped,
        "the revealed creature enters tapped"
    );
    assert_eq!(plus_one_counters(&runner, dreadmaw), 1);
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .library
            .back()
            .copied(),
        Some(island),
        "the revealed miss goes to the bottom of the library"
    );
    let combat = runner.state().combat.as_ref().expect("combat in progress");
    assert!(
        combat.attackers.iter().any(|a| a.object_id == raph),
        "Raph & Mikey itself attacks"
    );

    // Discriminating assertion (CR 508.4): the creature entered attacking.
    let entrant = combat
        .attackers
        .iter()
        .find(|a| a.object_id == dreadmaw)
        .unwrap_or_else(|| {
            panic!(
                "the revealed creature must be attacking after its paused entry; attackers={:?}",
                combat
                    .attackers
                    .iter()
                    .map(|a| a.object_id)
                    .collect::<Vec<_>>()
            )
        });
    assert_eq!(entrant.defending_player, P1);
}

const JACE_BELEREN: &str = "[+2]: Each player draws a card.\n\
[\u{2212}1]: Target player draws a card.\n\
[\u{2212}10]: Target player mills twenty cards.";

/// CR 508.4: a creature put onto the battlefield attacking has its
/// controller choose what it attacks. With a defending planeswalker on the
/// battlefield, Raph & Mikey's revealed creature entering inside the
/// reveal-until batch parks that choice; answering it seats the creature
/// against the chosen planeswalker and the parked batch then finishes — the
/// miss reaches the library bottom and the reveal markers are cleared.
#[test]
fn raph_and_mikey_kept_creature_entry_attack_choice_selects_a_planeswalker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);

    let raph = scenario
        .add_creature_from_oracle(P0, "Raph & Mikey, Troublemakers", 7, 7, RAPH_AND_MIKEY)
        .id();
    let jace = scenario
        .add_planeswalker_from_oracle(P1, "Jace Beleren", "Jace", 3, JACE_BELEREN)
        .id();
    let island = scenario.add_card_to_library_top(P0, "Island");
    let dreadmaw = scenario.add_creature(P0, "Colossal Dreadmaw", 6, 6).id();
    let filler = scenario.add_card_to_library_top(P0, "Filler");

    let mut runner = scenario.build();
    arrange_library_top_first(&mut runner, &[island, dreadmaw, filler]);

    runner.state_mut().waiting_for = build_declare_attackers_waiting_for(runner.state());
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(raph, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("Raph & Mikey attacks P1");

    // Resolve the attack trigger.
    for _ in 0..10 {
        if runner.state().stack.is_empty()
            || !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to resolve the attack trigger");
    }

    // Reach guard (CR 508.4): the revealed creature's entry parks its
    // controller's choice between the defending player and the planeswalker.
    match &runner.state().waiting_for {
        WaitingFor::EntryAttackTargetChoice {
            player,
            object_id,
            valid_targets,
        } => {
            assert_eq!(*player, P0, "the entrant's controller chooses");
            assert_eq!(
                *object_id, dreadmaw,
                "the choice is for the revealed creature"
            );
            assert!(
                valid_targets.contains(&AttackTarget::Player(P1))
                    && valid_targets.contains(&AttackTarget::Planeswalker(jace)),
                "both the defending player and the planeswalker are offered; got {valid_targets:?}"
            );
        }
        _ => panic!(
            "the revealed creature's entry must park an entry-attack choice; waiting_for={}",
            runner.waiting_for_kind()
        ),
    }
    runner
        .act(GameAction::ChooseEntryAttackTarget {
            target: AttackTarget::Planeswalker(jace),
        })
        .expect("the revealed creature attacks Jace Beleren");

    let state = runner.state();
    assert!(
        !matches!(
            state.waiting_for,
            WaitingFor::EntryAttackTargetChoice { .. }
        ),
        "the entry-attack choice must be resolved"
    );
    assert_zone(
        &runner,
        dreadmaw,
        Zone::Battlefield,
        "the revealed creature",
    );
    assert!(
        state.objects[&dreadmaw].tapped,
        "the revealed creature enters tapped"
    );
    let combat = state.combat.as_ref().expect("combat in progress");
    assert!(
        combat.attackers.iter().any(|a| a.object_id == raph),
        "Raph & Mikey itself attacks"
    );
    let entrant = combat
        .attackers
        .iter()
        .find(|a| a.object_id == dreadmaw)
        .expect("the revealed creature must be attacking");
    assert_eq!(
        entrant.attack_target,
        AttackTarget::Planeswalker(jace),
        "the revealed creature attacks the chosen planeswalker"
    );
    assert_eq!(entrant.defending_player, P1);

    assert_zone(&runner, island, Zone::Library, "the revealed Island");
    assert_eq!(
        state.players[P0.0 as usize].library.back().copied(),
        Some(island),
        "the revealed miss goes to the bottom of the library"
    );
    assert_eq!(
        state.players[P0.0 as usize].library.front().copied(),
        Some(filler),
        "the unrevealed card stays on top"
    );
    assert!(
        state.active_batch_delivery().is_none(),
        "the parked batch frame must be fully drained"
    );
    for id in [island, dreadmaw] {
        assert!(
            !state.revealed_cards.contains(&id),
            "reveal markers must be cleared once the batch settles ({id:?})"
        );
    }
}

/// CR 608.2f + CR 603.2c: "puts those cards into their graveyard" is one
/// action on every revealed card — the matched land and the misses move in
/// ONE simultaneous zone change, so a batched "one or more cards are put into
/// your graveyard from your library" observer triggers exactly once.
#[test]
fn destroy_the_evidence_mills_the_whole_revealed_pile_as_one_event() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let devourer = scenario
        .add_creature_from_oracle(P0, "Devourer of Memory", 2, 3, DEVOURER_OF_MEMORY)
        .id();
    let doomed_land = scenario
        .add_creature(P0, "Doomed Land", 0, 0)
        .as_land()
        .id();
    let filler = scenario.add_card_to_library_top(P0, "Filler");
    let hit_land = scenario.add_land_to_library_top(P0, "Forest").id();
    let miss_two = scenario.add_card_to_library_top(P0, "Miss Two");
    let miss_one = scenario.add_card_to_library_top(P0, "Miss One");
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Destroy the Evidence", false, DESTROY_THE_EVIDENCE)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();
    arrange_library_top_first(&mut runner, &[miss_one, miss_two, hit_land, filler]);

    runner.cast(spell).target_object(doomed_land).resolve();
    assert_stack_empty_priority(&runner);

    // Reach guards: the target was destroyed and the whole revealed pile —
    // both misses and the matched land — is in the graveyard.
    assert_zone(&runner, doomed_land, Zone::Graveyard, "the destroyed land");
    assert_zone(
        &runner,
        miss_one,
        Zone::Graveyard,
        "the first revealed miss",
    );
    assert_zone(
        &runner,
        miss_two,
        Zone::Graveyard,
        "the second revealed miss",
    );
    assert_zone(&runner, hit_land, Zone::Graveyard, "the revealed land");
    assert_zone(&runner, filler, Zone::Library, "the unrevealed card");
    for id in [miss_one, miss_two, hit_land] {
        assert!(
            !runner.state().revealed_cards.contains(&id),
            "reveal markers must be cleared ({id:?})"
        );
    }

    // Discriminating assertion: one event, one trigger (+1/+1, not +2/+2).
    let devourer_obj = &runner.state().objects[&devourer];
    assert_eq!(
        (devourer_obj.power, devourer_obj.toughness),
        (Some(3), Some(4)),
        "Devourer of Memory must trigger exactly once for the one milled pile"
    );
}

/// No double collection (CR 603.2c): a batched "one or more" observer with no
/// once-per-turn limit triggers exactly once for the simultaneous entry of two
/// matched Elves.
#[test]
fn kindred_summons_batched_observer_without_turn_limit_fires_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_enchantment_from_oracle(
        P0,
        "Elf Entry Observer",
        "Whenever one or more Elves you control enter, you gain 1 life.",
    );
    add_vanilla_elf(&mut scenario, "Llanowar Elves");
    add_vanilla_elf(&mut scenario, "Elvish Mystic");

    let elf_a = add_vanilla_elf(&mut scenario, "Fyndhorn Elves");
    let island = scenario.add_card_to_library_top(P0, "Island");
    let elf_b = add_vanilla_elf(&mut scenario, "Elvish Visionary");
    let spell = add_kindred_summons(&mut scenario);

    let mut runner = scenario.build();
    arrange_library_top_first(&mut runner, &[elf_a, island, elf_b]);
    let life_before = runner.life(P0);

    runner.cast(spell).choose_option("Elf").resolve();
    assert_stack_empty_priority(&runner);

    // Reach guards: both matched Elves entered.
    assert_zone(&runner, elf_a, Zone::Battlefield, "Elf A");
    assert_zone(&runner, elf_b, Zone::Battlefield, "Elf B");
    assert_zone(&runner, island, Zone::Library, "the revealed Island");
    assert_eq!(
        runner.life(P0),
        life_before + 1,
        "the batched observer must trigger exactly once for the one entry event"
    );
}
