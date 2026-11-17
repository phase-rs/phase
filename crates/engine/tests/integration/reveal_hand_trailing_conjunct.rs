//! CR 701.20a + CR 608.2c: the instruction joined to an own-hand reveal by a bare
//! "and" ("that player reveals their hand and exiles all cards …") is a real second
//! instruction performed by the revealing player, and must not be swallowed by the
//! reveal.
//!
//! Driving cards: **Legion's End** (same-name exile from hand and graveyard, whose
//! player is the target's controller), **Hour of Glory** (same-name exile from the
//! revealed hand, gated on the target having been a God) and **Rag Man** (a random
//! discard performed by a declared player).
//!
//! CR 400.7 + CR 608.2h: "that name" is read after the target has left and after
//! other instructions of the same spell have run. The target's own exit last-known
//! information answers, so a Clone's copied name and a face-down creature's lack of
//! a name both survive an intervening instruction that yields one other referent.

use engine::game::morph::apply_face_down_creature_characteristics;
use engine::game::printed_cards::snapshot_object_base_face;
use engine::game::scenario::{CardBuilder, GameRunner, GameScenario, P0, P1};
use engine::types::ability::FaceDownProfile;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const LEGIONS_END: &str = "Exile target creature an opponent controls with mana value 2 or less \
and all other creatures that player controls with the same name as that creature. Then that \
player reveals their hand and exiles all cards with that name from their hand and graveyard.";

const HOUR_OF_GLORY: &str = "Exile target creature. If that creature was a God, its controller \
reveals their hand and exiles all cards from it with the same name as that creature.";

const RAG_MAN: &str = "{B}{B}{B}, {T}: Target opponent reveals their hand and discards a creature \
card at random. Activate only during your turn.";

const CLONE: &str =
    "You may have this creature enter as a copy of any creature on the battlefield.";

const UNSUMMON: &str = "Return target creature to its owner's hand.";

const RHONAS: &str = "Rhonas the Indomitable";

const RHONAS_ORACLE: &str = "Deathtouch, indestructible\nRhonas can't attack or block unless you \
control another creature with power 4 or greater.\n{2}{G}: Another target creature gets +2/+0 and \
gains trample until end of turn.";

/// Grizzly Bears and Hill Giant are printed vanilla creatures, so they have no
/// Oracle text to reproduce.
const BEARS: &str = "Grizzly Bears";
const HILL_GIANT: &str = "Hill Giant";

fn free() -> ManaCost {
    ManaCost::Cost {
        generic: 0,
        shards: vec![],
    }
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn assert_zones(runner: &GameRunner, expected: &[(&str, ObjectId, Zone)]) {
    for (label, id, zone_expected) in expected {
        assert_eq!(zone(runner, *id), *zone_expected, "{label}");
    }
}

fn assert_resolved(runner: &GameRunner) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the spell must finish resolving, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty(), "the stack must be empty");
}

fn add_legions_end(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Legion's End", false, LEGIONS_END)
        .with_mana_cost(free())
        .id()
}

fn add_hour_of_glory(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Hour of Glory", true, HOUR_OF_GLORY)
        .with_mana_cost(free())
        .id()
}

/// Rhonas the Indomitable's printed rules on a builder of any zone.
fn as_rhonas(builder: &mut CardBuilder<'_>) -> ObjectId {
    builder
        .with_subtypes(vec!["God"])
        .from_oracle_text_with_keywords(&["Deathtouch", "Indestructible"], RHONAS_ORACLE)
        .id()
}

/// T1. CR 608.2c + CR 400.1: the exile reaches that player's hand and graveyard and
/// nothing else: not their library, not differently named cards, not another
/// player's copies. Fails with the reveal split reverted (the hand and graveyard copies
/// stay) and with the parser arm reverted (the generic single-origin path leaves the
/// graveyard copy).
#[test]
fn legions_end_exiles_same_name_cards_from_that_players_hand_and_graveyard() {
    let mut scenario = GameScenario::new_n_player(3, 71);
    scenario.at_phase(Phase::PreCombatMain);

    let victim = scenario.add_creature(P1, BEARS, 2, 2).id();
    let twin = scenario.add_creature(P1, BEARS, 2, 2).id();
    let other_battlefield = scenario.add_creature(P1, HILL_GIANT, 3, 3).id();
    let hand_copy = scenario.add_creature_to_hand(P1, BEARS, 2, 2).id();
    let hand_other = scenario.add_creature_to_hand(P1, HILL_GIANT, 3, 3).id();
    let graveyard_copy = scenario.add_creature_to_graveyard(P1, BEARS, 2, 2).id();
    let graveyard_other = scenario
        .add_creature_to_graveyard(P1, HILL_GIANT, 3, 3)
        .id();
    let library_copy = scenario.add_card_to_library_top(P1, BEARS);

    let caster_battlefield = scenario.add_creature(P0, BEARS, 2, 2).id();
    let caster_hand = scenario.add_creature_to_hand(P0, BEARS, 2, 2).id();
    let caster_graveyard = scenario.add_creature_to_graveyard(P0, BEARS, 2, 2).id();
    let caster_library = scenario.add_card_to_library_top(P0, BEARS);

    let third_battlefield = scenario.add_creature(P2, BEARS, 2, 2).id();
    let third_hand = scenario.add_creature_to_hand(P2, BEARS, 2, 2).id();
    let third_graveyard = scenario.add_creature_to_graveyard(P2, BEARS, 2, 2).id();
    let third_library = scenario.add_card_to_library_top(P2, BEARS);

    let spell = add_legions_end(&mut scenario);
    let mut runner = scenario.build();
    runner.cast(spell).target_objects(&[victim]).resolve();

    assert_resolved(&runner);
    assert_zones(
        &runner,
        &[
            ("target", victim, Zone::Exile),
            ("twin", twin, Zone::Exile),
            ("P1 hand copy", hand_copy, Zone::Exile),
            ("P1 graveyard copy", graveyard_copy, Zone::Exile),
            ("P1 other creature", other_battlefield, Zone::Battlefield),
            ("P1 other hand card", hand_other, Zone::Hand),
            ("P1 other graveyard card", graveyard_other, Zone::Graveyard),
            ("P1 library copy", library_copy, Zone::Library),
            ("caster creature", caster_battlefield, Zone::Battlefield),
            ("caster hand copy", caster_hand, Zone::Hand),
            ("caster graveyard copy", caster_graveyard, Zone::Graveyard),
            ("caster library copy", caster_library, Zone::Library),
            ("P2 creature", third_battlefield, Zone::Battlefield),
            ("P2 hand copy", third_hand, Zone::Hand),
            ("P2 graveyard copy", third_graveyard, Zone::Graveyard),
            ("P2 library copy", third_library, Zone::Library),
        ],
    );
}

/// T2. CR 108.3 + CR 109.4 + CR 608.2c + CR 608.2h: "that player" is the target's controller as it last
/// existed on the battlefield, not its owner. The target is owned by P2 and
/// controlled by P1.
#[test]
fn legions_end_binds_that_player_to_the_controller_not_the_owner() {
    let mut scenario = GameScenario::new_n_player(3, 71);
    scenario.at_phase(Phase::PreCombatMain);

    let victim = scenario
        .add_creature(P2, BEARS, 2, 2)
        .controlled_by(P1)
        .id();
    let controller_hand = scenario.add_creature_to_hand(P1, BEARS, 2, 2).id();
    let controller_graveyard = scenario.add_creature_to_graveyard(P1, BEARS, 2, 2).id();
    let owner_hand = scenario.add_creature_to_hand(P2, BEARS, 2, 2).id();
    let owner_graveyard = scenario.add_creature_to_graveyard(P2, BEARS, 2, 2).id();

    let spell = add_legions_end(&mut scenario);
    let mut runner = scenario.build();
    assert_eq!(
        runner.state().objects[&victim].controller,
        P1,
        "reach guard: P1 controls the target as the spell is cast"
    );
    runner.cast(spell).target_objects(&[victim]).resolve();

    assert_resolved(&runner);
    assert_eq!(
        runner.state().objects[&victim].controller,
        P2,
        "reach guard: once exiled the target reverts to its owner, so a live read would pick P2"
    );
    assert_zones(
        &runner,
        &[
            ("target", victim, Zone::Exile),
            ("controller hand copy", controller_hand, Zone::Exile),
            (
                "controller graveyard copy",
                controller_graveyard,
                Zone::Exile,
            ),
            ("owner hand copy", owner_hand, Zone::Hand),
            ("owner graveyard copy", owner_graveyard, Zone::Graveyard),
        ],
    );
}

/// T3. CR 608.2h + CR 707.2: a Clone's copied name is the name that counts, even
/// though it reverts when Clone leaves. The mass exile moves exactly one other
/// object (the Bears Clone copied), which overwrites the target's snapshot, and two
/// cards in hand keep the reveal from yielding a single referent. So only the
/// target's own exit last-known information can answer. Fails with the
/// `parent_target_name` read reverted: the Bears card stays and the Clone cards go.
#[test]
fn legions_end_uses_the_copied_name_after_the_mass_exile_moves_one_other_object() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let model = scenario
        .add_creature(P1, BEARS, 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    let clone = scenario
        .add_creature_to_hand_from_oracle(P1, "Clone", 0, 0, CLONE)
        .with_subtypes(vec!["Shapeshifter"])
        .with_mana_cost(free())
        .id();
    let hand_bears = scenario.add_creature_to_hand(P1, BEARS, 2, 2).id();
    let hand_clone = scenario
        .add_creature_to_hand_from_oracle(P1, "Clone", 0, 0, CLONE)
        .with_subtypes(vec!["Shapeshifter"])
        .id();
    let graveyard_clone = scenario
        .add_creature_to_graveyard(P1, "Clone", 0, 0)
        .with_subtypes(vec!["Shapeshifter"])
        .from_oracle_text(CLONE)
        .id();
    let spell = add_legions_end(&mut scenario);
    let mut runner = scenario.build();

    // P1's main phase for the Clone cast.
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    runner
        .cast(clone)
        .replacement_choice(0)
        .copy_target(model)
        .resolve();
    assert_eq!(runner.state().objects[&clone].name, BEARS);
    assert_eq!(runner.state().objects[&clone].base_name, "Clone");

    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    runner.cast(spell).target_object(clone).resolve();

    assert_resolved(&runner);
    assert_zones(
        &runner,
        &[
            ("target", clone, Zone::Exile),
            ("the creature Clone copied", model, Zone::Exile),
            ("Bears card in hand", hand_bears, Zone::Exile),
            ("Clone card in hand", hand_clone, Zone::Hand),
            ("Clone card in graveyard", graveyard_clone, Zone::Graveyard),
        ],
    );
}

/// T4. CR 708.2a + CR 201.2a: a face-down creature has no name, so it has the same
/// name as nothing. Exactly one card in hand makes the reveal yield a single
/// referent, which overwrites the target's snapshot. The target is face up again in
/// exile, so a live read would report its printed name. Fails with the
/// `parent_target_name` read reverted: the hand and graveyard copies get exiled.
#[test]
fn legions_end_exiles_nothing_more_for_a_nameless_target_after_a_single_card_reveal() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let victim = scenario.add_creature(P1, BEARS, 2, 2).id();
    let twin = scenario.add_creature(P1, BEARS, 2, 2).id();
    let hand_copy = scenario.add_creature_to_hand(P1, BEARS, 2, 2).id();
    let graveyard_copy = scenario.add_creature_to_graveyard(P1, BEARS, 2, 2).id();
    let spell = add_legions_end(&mut scenario);
    let mut runner = scenario.build();

    // The production face-down shape: the real face is stashed in `back_face`
    // before the CR 708.2a characteristics apply.
    {
        let state = runner.state_mut();
        let stash = snapshot_object_base_face(&state.objects[&victim]);
        apply_face_down_creature_characteristics(
            state.objects.get_mut(&victim).unwrap(),
            &FaceDownProfile::vanilla_2_2(),
        );
        state.objects.get_mut(&victim).unwrap().back_face = Some(stash);
    }
    assert_eq!(
        runner.state().objects[&victim].name,
        "",
        "reach guard: the target is nameless on the battlefield"
    );

    runner.cast(spell).target_objects(&[victim]).resolve();

    assert_resolved(&runner);
    assert_eq!(
        runner.state().objects[&victim].name,
        BEARS,
        "reach guard: the exiled target has its printed name, so a live read would match"
    );
    assert_zones(
        &runner,
        &[
            ("target", victim, Zone::Exile),
            ("same-real-name creature", twin, Zone::Battlefield),
            ("same-real-name hand card", hand_copy, Zone::Hand),
            (
                "same-real-name graveyard card",
                graveyard_copy,
                Zone::Graveyard,
            ),
        ],
    );
}

/// T5. CR 608.2c: the instructions are followed in order whatever the reveal showed,
/// so an empty hand still leaves the graveyard leg of the exile to run.
#[test]
fn legions_end_empty_hand_still_exiles_the_graveyard_copy() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let victim = scenario.add_creature(P1, BEARS, 2, 2).id();
    let graveyard_copy = scenario.add_creature_to_graveyard(P1, BEARS, 2, 2).id();
    let spell = add_legions_end(&mut scenario);
    let mut runner = scenario.build();
    assert_eq!(
        runner.state().players[P1.0 as usize].hand.len(),
        0,
        "reach guard: P1's hand is empty"
    );

    runner.cast(spell).target_objects(&[victim]).resolve();

    assert_resolved(&runner);
    assert_zones(
        &runner,
        &[
            ("target", victim, Zone::Exile),
            ("graveyard copy", graveyard_copy, Zone::Exile),
        ],
    );
}

/// T6. CR 608.2b: with its only target illegal the spell does not resolve, so nothing
/// is revealed or exiled and it goes to the graveyard.
#[test]
fn legions_end_with_an_illegal_target_moves_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let victim = scenario.add_creature(P1, BEARS, 2, 2).id();
    let twin = scenario.add_creature(P1, BEARS, 2, 2).id();
    let hand_copy = scenario.add_creature_to_hand(P1, BEARS, 2, 2).id();
    let graveyard_copy = scenario.add_creature_to_graveyard(P1, BEARS, 2, 2).id();
    let spell = add_legions_end(&mut scenario);
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
        .with_mana_cost(free())
        .id();
    let mut runner = scenario.build();

    let _ = runner.cast(spell).target_object(victim).commit();
    let _ = runner.cast(unsummon).target_object(victim).commit();
    runner.advance_until_stack_empty();

    assert_resolved(&runner);
    assert_zones(
        &runner,
        &[
            ("bounced target", victim, Zone::Hand),
            ("Legion's End", spell, Zone::Graveyard),
            ("twin", twin, Zone::Battlefield),
            ("hand copy", hand_copy, Zone::Hand),
            ("graveyard copy", graveyard_copy, Zone::Graveyard),
        ],
    );
}

/// T7. CR 608.2c + CR 402.1: Hour of Glory's "from it" is the revealed hand only.
/// Fails with the parser arm reverted: the hand copy stays.
#[test]
fn hour_of_glory_exiles_same_name_cards_from_the_revealed_hand_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mut target = scenario.add_creature(P1, RHONAS, 5, 5);
    let target = as_rhonas(&mut target);
    let mut hand_copy = scenario.add_creature_to_hand(P1, RHONAS, 5, 5);
    let hand_copy = as_rhonas(&mut hand_copy);
    let hand_other = scenario.add_creature_to_hand(P1, HILL_GIANT, 3, 3).id();
    let mut graveyard_copy = scenario.add_creature_to_graveyard(P1, RHONAS, 5, 5);
    let graveyard_copy = as_rhonas(&mut graveyard_copy);
    // Only the name of a library card is read here.
    let library_copy = scenario.add_card_to_library_top(P1, RHONAS);
    let mut caster_hand = scenario.add_creature_to_hand(P0, RHONAS, 5, 5);
    let caster_hand = as_rhonas(&mut caster_hand);

    let spell = add_hour_of_glory(&mut scenario);
    let mut runner = scenario.build();
    runner.cast(spell).target_objects(&[target]).resolve();

    assert_resolved(&runner);
    assert_zones(
        &runner,
        &[
            ("target", target, Zone::Exile),
            ("hand copy", hand_copy, Zone::Exile),
            ("different hand card", hand_other, Zone::Hand),
            ("graveyard copy", graveyard_copy, Zone::Graveyard),
            ("library copy", library_copy, Zone::Library),
            ("caster's hand copy", caster_hand, Zone::Hand),
        ],
    );
}

/// T8. CR 608.2c: the God condition gates the reveal and the exile together, so a
/// non-God target leaves the same-name hand card alone.
#[test]
fn hour_of_glory_non_god_target_keeps_the_hand_copy() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let target = scenario.add_creature(P1, BEARS, 2, 2).id();
    let hand_copy = scenario.add_creature_to_hand(P1, BEARS, 2, 2).id();
    let spell = add_hour_of_glory(&mut scenario);
    let mut runner = scenario.build();
    runner.cast(spell).target_objects(&[target]).resolve();

    assert_resolved(&runner);
    assert_zones(
        &runner,
        &[
            ("target", target, Zone::Exile),
            ("hand copy", hand_copy, Zone::Hand),
        ],
    );
}

/// T9. CR 701.9a + CR 701.9b: Rag Man's declared opponent discards a creature card at
/// random. The only creature card in hand makes the outcome deterministic. Fails with
/// the split reverted: the creature card stays in hand.
#[test]
fn rag_man_opponent_discards_a_creature_card_at_random() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let rag_man = scenario
        .add_creature_from_oracle(P0, "Rag Man", 2, 1, RAG_MAN)
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Black);
    }
    let creature_card = scenario.add_creature_to_hand(P1, BEARS, 2, 2).id();
    let forest = scenario.add_land_to_hand(P1, "Forest").id();
    let mountain = scenario.add_land_to_hand(P1, "Mountain").id();
    let activators_creature = scenario.add_creature_to_hand(P0, BEARS, 2, 2).id();
    let mut runner = scenario.build();

    runner.activate(rag_man, 0).target_player(P1).resolve();

    assert_resolved(&runner);
    assert_zones(
        &runner,
        &[
            ("P1 creature card", creature_card, Zone::Graveyard),
            ("P1 Forest", forest, Zone::Hand),
            ("P1 Mountain", mountain, Zone::Hand),
            ("activator's creature card", activators_creature, Zone::Hand),
        ],
    );
}
