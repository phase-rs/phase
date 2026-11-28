//! `assimilate` across its operand forms, driven through the real pipeline.
//!
//! The keyword action has two reminder-text definitions, split on where the
//! object is:
//!   * a card outside the battlefield: "Put it onto the battlefield under your
//!     control with a +1/+1 counter. It's a Borg artifact creature and loses all
//!     other creature types."
//!   * a permanent: "Gain control of it and put a +1/+1 counter on it. It's a
//!     Borg artifact creature and loses all other creature types."
//!
//! Cards under test (Oracle text verbatim, Scryfall-verified):
//!   * Assimilation Assessor — "When this creature enters, exile target
//!     opponent's graveyard. Whenever this creature attacks, assimilate a
//!     creature card exiled with it." (CR 607.2a linked exile.)
//!   * Borg Cube — "Whenever this Spacecraft enters or attacks, assimilate a
//!     creature card exiled with it. If you can't, exile up to one target
//!     creature." (The fallback's exile is linked to the Cube, so a later
//!     trigger assimilates what an earlier one exiled.)
//!   * Gift of Perfection — "Each opponent reveals cards from the top of their
//!     library until they reveal a creature card. Assimilate one of the creature
//!     cards revealed this way. Then each opponent puts the other cards they
//!     revealed this way on the bottom of their library in a random order."
//!   * The Best of Both Worlds — "I — Assimilate target nonartifact creature an
//!     opponent controls. … III — Exile the Borg assimilated with this Saga, then
//!     return it to the battlefield under its owner's control." (CR 607.1.)
//!
//! `RemoveAllSubtypes { Creature }` strips only subtypes listed in
//! `all_creature_types`, which `GameScenario` leaves empty; every test seeds it
//! so "loses all other creature types" is never vacuous.

use engine::game::combat::AttackTarget;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const ASSIMILATION_ASSESSOR: &str =
    "When this creature enters, exile target opponent's graveyard.\n\
     Whenever this creature attacks, assimilate a creature card exiled with it. (Put it onto the \
     battlefield under your control with a +1/+1 counter. It's a Borg artifact creature and loses \
     all other creature types.)";

const BORG_CUBE: &str = "Whenever this Spacecraft enters or attacks, assimilate a creature card \
     exiled with it. If you can't, exile up to one target creature. (To assimilate a card, put it \
     onto the battlefield under your control with a +1/+1 counter. It's a Borg artifact creature \
     and loses all other creature types.)";

const GIFT_OF_PERFECTION: &str = "Each opponent reveals cards from the top of their library until \
     they reveal a creature card. Assimilate one of the creature cards revealed this way. Then each \
     opponent puts the other cards they revealed this way on the bottom of their library in a \
     random order. (To assimilate a card, put it onto the battlefield under your control with a \
     +1/+1 counter. It's a Borg artifact creature and loses all other creature types.)";

const THE_BEST_OF_BOTH_WORLDS: &str = "(As this Saga enters and after your draw step, add a lore \
     counter. Sacrifice after III.)\n\
     I \u{2014} Assimilate target nonartifact creature an opponent controls. (Gain control of it and \
     put a +1/+1 counter on it. It's a Borg artifact creature and loses all other creature types.)\n\
     II \u{2014} Artifact creatures you control get +2/+2 until end of turn.\n\
     III \u{2014} Exile the Borg assimilated with this Saga, then return it to the battlefield under \
     its owner's control.";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn seed_creature_types(runner: &mut GameRunner) {
    runner.state_mut().all_creature_types = ["Human", "Soldier", "Borg", "Elf"]
        .iter()
        .map(|s| s.to_string())
        .collect();
}

fn relayer(runner: &mut GameRunner) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
}

fn plus_one_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn has_subtype(runner: &GameRunner, id: ObjectId, subtype: &str) -> bool {
    runner.state().objects[&id]
        .card_types
        .subtypes
        .iter()
        .any(|s| s == subtype)
}

/// The full "assimilated" read-back: on the battlefield under P0's control,
/// one +1/+1 counter, a Borg artifact creature, no prior creature type.
fn assert_assimilated(runner: &mut GameRunner, id: ObjectId, prior_types: &[&str]) {
    relayer(runner);
    let obj = &runner.state().objects[&id];
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "the assimilated object is on the battlefield"
    );
    assert_eq!(
        obj.controller, P0,
        "CR 110.2a / CR 613.1b: under our control"
    );
    assert_eq!(
        plus_one_counters(runner, id),
        1,
        "CR 122.1: one +1/+1 counter"
    );
    assert!(has_subtype(runner, id, "Borg"), "CR 205.1a: Borg");
    for prior in prior_types {
        assert!(
            !has_subtype(runner, id, prior),
            "CR 205.1b: prior creature type {prior} is replaced"
        );
    }
    let types = &runner.state().objects[&id].card_types.core_types;
    assert!(types.contains(&CoreType::Artifact), "CR 205.1b: artifact");
    assert!(types.contains(&CoreType::Creature), "CR 205.1b: creature");
}

/// Resolve the stack, answering each prompt: the first legal target for target
/// prompts and the first offered card for a one-card choice. Panics on any
/// other prompt.
fn resolve_all(runner: &mut GameRunner) {
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .choose_first_legal_target()
                    .expect("choose the first legal target");
            }
            WaitingFor::ChooseFromZoneChoice { cards, .. } => {
                let pick = cards[0];
                runner
                    .act(GameAction::SelectCards { cards: vec![pick] })
                    .expect("pick the first offered card");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return;
                }
                runner.pass_holding_team().expect("pass priority");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    panic!("the stack never emptied");
}

fn clear_sickness(runner: &mut GameRunner, id: ObjectId) {
    let turn = runner.state().turn_number;
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.summoning_sick = false;
    obj.entered_battlefield_turn = Some(turn.saturating_sub(1));
}

fn attack_with(runner: &mut GameRunner, attacker: ObjectId) {
    clear_sickness(runner, attacker);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare the attacker");
    resolve_all(runner);
}

// ---------------------------------------------------------------------------
// "a creature card exiled with it" — Assimilation Assessor
// ---------------------------------------------------------------------------

/// CR 607.2a: the attack trigger assimilates a creature card the enters
/// trigger exiled — and only a creature card (the exiled land stays put).
#[test]
fn assimilation_assessor_assimilates_a_creature_card_it_exiled() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_land_to_graveyard(P1, "Grave Plains").id();
    let creature = scenario
        .add_creature_to_graveyard(P1, "Grave Soldier", 2, 2)
        .with_subtypes(vec!["Human", "Soldier"])
        .id();
    let assessor = scenario
        .add_creature_to_hand_from_oracle(P0, "Assimilation Assessor", 3, 3, ASSIMILATION_ASSESSOR)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    seed_creature_types(&mut runner);

    runner.cast(assessor).target_players(&[P1]).resolve();
    resolve_all(&mut runner);
    // REACH-GUARD: the enters trigger exiled the whole graveyard.
    assert_eq!(runner.state().objects[&creature].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&land].zone, Zone::Exile);

    attack_with(&mut runner, assessor);

    assert_assimilated(&mut runner, creature, &["Human", "Soldier"]);
    assert_eq!(
        runner.state().objects[&land].zone,
        Zone::Exile,
        "the operand is a CREATURE card: the exiled land is not assimilated"
    );
}

// ---------------------------------------------------------------------------
// "If you can't, exile up to one target creature" — Borg Cube
// ---------------------------------------------------------------------------

/// With nothing exiled with it, the enters trigger cannot assimilate, so it
/// exiles the target creature instead; that card is now exiled with the Cube,
/// so the next trigger assimilates it.
#[test]
fn borg_cube_exiles_when_it_cannot_assimilate_then_assimilates_that_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario
        .add_creature(P1, "Elvish Defender", 2, 3)
        .with_subtypes(vec!["Elf"])
        .id();
    let cube = scenario
        .add_artifact_to_hand_from_oracle(P0, "Borg Cube", BORG_CUBE)
        .with_subtypes(vec!["Spacecraft"])
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    seed_creature_types(&mut runner);

    runner.cast(cube).target_objects(&[victim]).resolve();
    resolve_all(&mut runner);
    // "If you can't": nothing was exiled with the Cube, so the target is exiled.
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Exile,
        "the fallback exiles the target creature when nothing can be assimilated"
    );

    // The Cube is a Spacecraft, not a creature; make it one so it can attack and
    // fire its "attacks" trigger (Station is not under test here).
    {
        let obj = runner.state_mut().objects.get_mut(&cube).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(6);
        obj.toughness = Some(6);
        obj.base_power = Some(6);
        obj.base_toughness = Some(6);
    }
    attack_with(&mut runner, cube);

    assert_assimilated(&mut runner, victim, &["Elf"]);
}

// ---------------------------------------------------------------------------
// "one of the creature cards revealed this way" — Gift of Perfection
// ---------------------------------------------------------------------------

/// CR 608.2d: the caster chooses ONE creature card across every opponent's
/// reveal; every other revealed card — the unchosen creature too — goes to the
/// bottom of its owner's library.
#[test]
fn gift_of_perfection_assimilates_one_revealed_creature_and_bottoms_the_rest() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    // Built bottom-first. P1: [p1_land, p1_creature, p1_deep]; P2: [p2_creature, p2_deep].
    let p1_deep = scenario.add_land_to_library_top(P1, "P1 Deep").id();
    let p1_creature = scenario
        .add_creature_to_library_top(P1, "P1 Soldier", 2, 2)
        .with_subtypes(vec!["Human", "Soldier"])
        .id();
    let p1_land = scenario.add_land_to_library_top(P1, "P1 Plains").id();
    let p2_deep = scenario.add_land_to_library_top(P2, "P2 Deep").id();
    let p2_creature = scenario
        .add_creature_to_library_top(P2, "P2 Elf", 1, 1)
        .with_subtypes(vec!["Elf"])
        .id();
    let gift = scenario
        .add_spell_to_hand_from_oracle(P0, "Gift of Perfection", false, GIFT_OF_PERFECTION)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    seed_creature_types(&mut runner);

    runner.cast(gift).resolve();
    let mut offered = Vec::new();
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ChooseFromZoneChoice { player, cards, .. } => {
                assert_eq!(player, P0, "the caster chooses");
                offered = cards.clone();
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![p2_creature],
                    })
                    .expect("choose P2's creature");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.pass_holding_team().expect("pass");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    offered.sort();
    let mut expected = vec![p1_creature, p2_creature];
    expected.sort();
    assert_eq!(
        offered, expected,
        "CR 608.2d: the pool is exactly the revealed creature cards"
    );

    assert_assimilated(&mut runner, p2_creature, &["Elf"]);
    assert_eq!(runner.state().objects[&p2_creature].owner, P2, "CR 108.3");

    // The other revealed cards are under each library's unrevealed card.
    let p1_library: Vec<ObjectId> = runner.state().library_of(P1).iter().copied().collect();
    assert_eq!(
        p1_library.len(),
        3,
        "P1's revealed cards stay in P1's library"
    );
    assert_eq!(
        p1_library[0], p1_deep,
        "the unrevealed card is now on top: {p1_library:?}"
    );
    assert!(
        p1_library[1..].contains(&p1_creature) && p1_library[1..].contains(&p1_land),
        "the unchosen creature and the land are on the bottom: {p1_library:?}"
    );
    let p2_library: Vec<ObjectId> = runner.state().library_of(P2).iter().copied().collect();
    assert_eq!(
        p2_library,
        vec![p2_deep],
        "P2's only other card is untouched"
    );
}

// ---------------------------------------------------------------------------
// The battlefield form + "the Borg assimilated with this Saga"
// ---------------------------------------------------------------------------

fn lore_count(runner: &GameRunner, saga: ObjectId) -> u32 {
    runner.state().objects[&saga]
        .counters
        .get(&CounterType::Lore)
        .copied()
        .unwrap_or(0)
}

/// CR 714.3c: start P0's next turn at upkeep and advance into precombat main,
/// where the lore counter is added and the next chapter triggers; resolve it.
fn fire_next_chapter(runner: &mut GameRunner, saga: ObjectId) {
    start_next_chapter(runner, saga);
    resolve_all(runner);
}

/// CR 714.3c: start P0's next turn at upkeep and advance into precombat main,
/// where the lore counter is added and the next chapter triggers. The chapter
/// ability is left for the caller to handle.
fn start_next_chapter(runner: &mut GameRunner, saga: ObjectId) {
    let expected = lore_count(runner, saga) + 1;
    {
        let state = runner.state_mut();
        state.turn_number += 2;
        state.active_player = P0;
        state.phase = Phase::Upkeep;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(
        lore_count(runner, saga),
        expected,
        "a lore counter was added; phase={:?} turn={} wf={:?} types={:?}",
        runner.state().phase,
        runner.state().turn_number,
        runner.state().waiting_for,
        runner.state().objects[&saga].card_types,
    );
}

/// Chapter I gains control of the creature and assimilates it; chapter III
/// exiles exactly that creature and returns it under its owner's control as a
/// new object (CR 400.7), so the control change, counter, and retype are gone.
#[test]
fn the_best_of_both_worlds_assimilates_a_permanent_and_releases_it() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // An artifact creature is not a legal chapter I target ("nonartifact").
    let artifact_creature = scenario
        .add_creature(P1, "Opposing Golem", 3, 3)
        .as_artifact_creature()
        .id();
    let victim = scenario
        .add_creature(P1, "Opposing Soldier", 2, 2)
        .with_subtypes(vec!["Human", "Soldier"])
        .id();
    // Each chapter starts a turn whose draw step needs a card to draw.
    for i in 0..5 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
    }
    let saga = scenario
        .add_creature(P0, "The Best of Both Worlds", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(THE_BEST_OF_BOTH_WORLDS)
        .id();
    let mut runner = scenario.build();
    seed_creature_types(&mut runner);

    fire_next_chapter(&mut runner, saga);
    // CR 613.1b + CR 122.1 + CR 205.1b: the battlefield form.
    assert_assimilated(&mut runner, victim, &["Human", "Soldier"]);
    assert_eq!(runner.state().objects[&victim].owner, P1, "CR 108.3");
    assert_eq!(
        runner.state().objects[&artifact_creature].controller,
        P1,
        "the artifact creature was never a legal target"
    );

    fire_next_chapter(&mut runner, saga); // II
    fire_next_chapter(&mut runner, saga); // III

    // CR 607.1 + CR 400.7: the recorded Borg was exiled and returned; it is a
    // new object under its owner's control with none of chapter I's effects.
    let returned = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == "Opposing Soldier")
        .expect("the Borg returned to the battlefield");
    relayer(&mut runner);
    let obj = &runner.state().objects[&returned];
    assert_eq!(obj.controller, P1, "returned under its owner's control");
    assert_eq!(
        plus_one_counters(&runner, returned),
        0,
        "a new object has no counter"
    );
    assert!(
        has_subtype(&runner, returned, "Human"),
        "its own creature types are back"
    );
    assert!(
        !has_subtype(&runner, returned, "Borg"),
        "the retype ended with the old object"
    );
    assert_eq!(
        runner.state().objects[&artifact_creature].zone,
        Zone::Battlefield,
        "chapter III exiles only the Borg assimilated with this Saga"
    );
}

const STRIONIC_RESONATOR: &str = "{2}, {T}: Copy target triggered ability you control. You may \
     choose new targets for the copy. (A triggered ability uses the words \"when,\" \"whenever,\" \
     or \"at.\")";

/// The Best of Both Worlds ruling (2026-10-07): "If The Best of Both Worlds's
/// first chapter ability assimilated more than one creature (usually because
/// the ability was copied), its third chapter ability will exile all of the
/// assimilated Borg and then return them all to the battlefield under their
/// owners' control." CR 607.1: the linked reader names every object the
/// paired ability affected. Strionic Resonator copies chapter I, the copy is
/// retargeted (CR 707.10c), and chapter III must release both creatures.
#[test]
fn the_best_of_both_worlds_releases_every_borg_a_copied_chapter_one_assimilated() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let soldier = scenario
        .add_creature(P1, "Opposing Soldier", 2, 2)
        .with_subtypes(vec!["Human", "Soldier"])
        .id();
    let elf = scenario
        .add_creature(P1, "Opposing Elf", 1, 1)
        .with_subtypes(vec!["Elf"])
        .id();
    for i in 0..5 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
    }
    let resonator = scenario
        .add_artifact_from_oracle(P0, "Strionic Resonator", STRIONIC_RESONATOR)
        .id();
    for i in 0..2 {
        scenario.add_land_from_oracle(P0, &format!("P0 Wastes {i}"), "{T}: Add {C}.");
    }
    let saga = scenario
        .add_creature(P0, "The Best of Both Worlds", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(THE_BEST_OF_BOTH_WORLDS)
        .id();
    let mut runner = scenario.build();
    seed_creature_types(&mut runner);

    // Chapter I triggers and targets the Soldier.
    start_next_chapter(&mut runner, saga);
    let chapter_one = loop {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(soldier)),
                    })
                    .expect("chapter I targets the Soldier");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                break runner
                    .state()
                    .stack
                    .iter()
                    .find(|entry| {
                        entry.source_id == saga
                            && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
                    })
                    .map(|entry| entry.id)
                    .expect("reach guard: chapter I is on the stack");
            }
            other => panic!("unexpected chapter I prompt: {other:?}"),
        }
    };

    // Strionic Resonator copies chapter I.
    if let WaitingFor::Priority { player } = runner.state().waiting_for {
        assert_eq!(player, P0, "P0 holds priority with chapter I on the stack");
    }
    runner
        .act(GameAction::ActivateAbility {
            source_id: resonator,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Strionic Resonator");
    let mut retargeted = false;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } if !retargeted => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(chapter_one)],
                    })
                    .expect("Resonator targets chapter I");
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay {2}");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("choose new targets for the copy");
            }
            // CR 707.10c: the copy's new-target choice.
            WaitingFor::CopyRetarget { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(elf)),
                    })
                    .expect("the copy targets the Elf");
                retargeted = true;
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.pass_holding_team().expect("pass priority");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert!(
        retargeted,
        "reach guard: the copy was retargeted to the Elf"
    );
    assert!(runner.state().stack.is_empty(), "the stack resolved");

    // Both chapter I resolutions assimilated their creature.
    assert_assimilated(&mut runner, soldier, &["Human", "Soldier"]);
    assert_assimilated(&mut runner, elf, &["Elf"]);

    fire_next_chapter(&mut runner, saga); // II
    fire_next_chapter(&mut runner, saga); // III

    // CR 607.1 + CR 400.7: chapter III exiled and returned BOTH Borg.
    relayer(&mut runner);
    for name in ["Opposing Soldier", "Opposing Elf"] {
        let returned = runner
            .state()
            .battlefield
            .iter()
            .copied()
            .find(|id| runner.state().objects[id].name == name)
            .unwrap_or_else(|| panic!("{name} is on the battlefield"));
        let obj = &runner.state().objects[&returned];
        assert_eq!(
            obj.controller, P1,
            "{name} returned under its owner's control"
        );
        assert_eq!(
            plus_one_counters(&runner, returned),
            0,
            "{name} is a new object with no counter"
        );
        assert!(
            !has_subtype(&runner, returned, "Borg"),
            "{name}'s retype ended with the old object"
        );
    }
}
