//! CR 702.19b: a trampling attacker's lethal-damage minimum takes into account "damage already
//! marked on the creature and damage from other creatures that's being assigned during the same
//! combat damage step".
//!
//! Two consequences pinned here through the production combat pipeline (declare attackers,
//! declare blockers, the `AssignCombatDamage` prompt):
//! - damage an earlier attacker already committed to a shared blocker lowers a later trampler's
//!   minimum, and committed deathtouch damage (CR 702.2c) leaves it owing nothing;
//! - a blocker that already has lethal damage marked owes nothing, even to a deathtouch trampler.
//!
//! CR 510.1e is only partly modelled: attackers are still assigned and validated one at a time,
//! so an earlier attacker cannot assign excess based on damage a later attacker will assign
//! (Command Zone #285). `shared_blocker_committed_damage_lowers_the_next_minimum` pins that.
//!
//! Oracle text verified verbatim against `data/mtgjson/AtomicCards.json`: Foriysian Brigade,
//! Darksteel Myr, Lava Dart, War Mammoth (3/3), Fangren Hunter (4/4), Giant Warthog (5/5), Sedge
//! Scorpion (1/1) and Archetype of Finality. Every attacker is one of these printed cards.
//!
//! No printed creature has both deathtouch and trample of its own, so the deathtouch trampler
//! is War Mammoth ("Trample") under Archetype of Finality ("Creatures you control have
//! deathtouch.").

use engine::game::combat::{AttackTarget, DamageAssignment, DamageTarget};
use engine::game::engine::EngineError;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{CombatDamageAssignmentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const FORIYSIAN_BRIGADE: &str = "This creature can block an additional creature each combat.";
const DARKSTEEL_MYR: &str = "Indestructible (Damage and effects that say \"destroy\" don't \
     destroy this creature. If its toughness is 0 or less, it still dies.)";
const WAR_MAMMOTH: &str = "Trample";
const FANGREN_HUNTER: &str = "Trample";
const GIANT_WARTHOG: &str = "Trample";
const SEDGE_SCORPION: &str =
    "Deathtouch (Any amount of damage this deals to a creature is enough to destroy it.)";
const ARCHETYPE_OF_FINALITY: &str = "Creatures you control have deathtouch.\nCreatures your \
     opponents control lose deathtouch and can't have or gain deathtouch.";
const LAVA_DART: &str = "Lava Dart deals 1 damage to any target.\n\
     Flashback\u{2014}Sacrifice a Mountain. (You may cast this card from your graveyard for its \
     flashback cost. Then exile it.)";

/// A printed P0 creature on the battlefield with its verbatim Oracle text and MTGJSON keywords.
fn printed(
    scenario: &mut GameScenario,
    name: &str,
    (power, toughness): (i32, i32),
    keywords: &[&str],
    text: &str,
) -> ObjectId {
    scenario
        .add_creature(P0, name, power, toughness)
        .from_oracle_text_with_keywords(keywords, text)
        .id()
}

/// Attack P1 with `attackers` (declared in this order), declare `blocks` (`(blocker, attacker)`)
/// for P1, then pass priority until combat damage is assigned: either an `AssignCombatDamage`
/// prompt is up, or the combat damage step has dealt its damage.
fn attack_and_block(
    runner: &mut GameRunner,
    attackers: &[ObjectId],
    blocks: &[(ObjectId, ObjectId)],
) {
    let mut declared_attackers = false;
    let mut declared_blockers = false;
    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::AssignCombatDamage { .. } => break,
            WaitingFor::Priority { .. } if runner.state().phase == Phase::CombatDamage => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::DeclareAttackers { player, .. } => {
                assert_eq!(*player, P0);
                assert!(!declared_attackers, "attackers are declared once");
                declared_attackers = true;
                let attacks: Vec<_> = attackers
                    .iter()
                    .map(|&id| (id, AttackTarget::Player(P1)))
                    .collect();
                runner
                    .declare_attackers(&attacks)
                    .expect("declare attackers");
            }
            WaitingFor::DeclareBlockers { player, .. } => {
                assert_eq!(*player, P1);
                assert!(!declared_blockers, "blockers are declared once");
                declared_blockers = true;
                runner.declare_blockers(blocks).expect("declare blockers");
            }
            other => panic!("unexpected prompt before combat damage: {other:?}"),
        }
    }
    assert!(
        declared_attackers && declared_blockers,
        "combat must reach the declared block"
    );
}

/// The open `AssignCombatDamage` prompt's attacker and the lethal minimum it publishes for
/// `blocker`. Panics if no prompt is up.
fn prompt_minimum(runner: &GameRunner, blocker: ObjectId) -> (ObjectId, u32) {
    let WaitingFor::AssignCombatDamage {
        player,
        attacker_id,
        blockers,
        trample,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected a combat damage assignment prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert!(trample.is_some(), "the prompted attacker tramples");
    let slot = blockers
        .iter()
        .find(|slot| slot.blocker_id == blocker)
        .expect("the blocker is a slot of the prompt");
    (*attacker_id, slot.lethal_minimum)
}

fn assign(
    runner: &mut GameRunner,
    blocker: ObjectId,
    to_blocker: u32,
    trample_damage: u32,
) -> Result<(), EngineError> {
    runner
        .act(GameAction::AssignCombatDamage {
            mode: CombatDamageAssignmentMode::Normal,
            assignments: vec![(blocker, to_blocker)],
            trample_damage,
            controller_damage: 0,
        })
        .map(|_| ())
}

/// The combat damage `attacker` has committed to `blocker` so far this step.
fn committed(runner: &GameRunner, attacker: ObjectId, blocker: ObjectId) -> Vec<u32> {
    runner
        .state()
        .combat
        .as_ref()
        .expect("combat is in progress")
        .pending_damage
        .iter()
        .filter(|(source, _)| *source == attacker)
        .filter_map(|(_, DamageAssignment { target, amount })| {
            (*target == DamageTarget::Object(blocker)).then_some(*amount)
        })
        .collect()
}

/// Finish combat after the last assignment and return to a settled priority window.
fn finish_combat(runner: &mut GameRunner) {
    runner.combat_damage();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "combat finishes at a priority window, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

/// T1: Foriysian Brigade (2/4) blocks two 3/3 tramplers. The first is auto-assigned all 3 to the
/// Brigade (3 does not exceed its lethal 4), so the second owes only 1 more (CR 702.19b).
///
/// CR 510.1e fence (Command Zone #285): the total assignment in which each trampler assigns 2 to
/// the Brigade and 1 to P1 is legal by the rules but not offered — the first attacker is never
/// prompted.
#[test]
fn shared_blocker_committed_damage_lowers_the_next_minimum() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let first = printed(
        &mut scenario,
        "War Mammoth",
        (3, 3),
        &["Trample"],
        WAR_MAMMOTH,
    );
    let second = printed(
        &mut scenario,
        "War Mammoth",
        (3, 3),
        &["Trample"],
        WAR_MAMMOTH,
    );
    let brigade = scenario
        .add_creature_from_oracle(P1, "Foriysian Brigade", 2, 4, FORIYSIAN_BRIGADE)
        .id();
    let mut runner = scenario.build();
    let p1_life = runner.life(P1);

    attack_and_block(
        &mut runner,
        &[first, second],
        &[(brigade, first), (brigade, second)],
    );

    let (prompted, minimum) = prompt_minimum(&runner, brigade);
    // (a) CR 510.1e fence: the first trampler was assigned all 3 without a prompt.
    assert_eq!(prompted, second, "only the second trampler is prompted");
    assert_eq!(committed(&runner, first, brigade), vec![3]);
    // (b) CR 702.19b: 4 toughness less 3 committed.
    assert_eq!(minimum, 1);
    // (c)
    assign(&mut runner, brigade, 1, 2).expect("1 is lethal after the committed 3");
    finish_combat(&mut runner);
    // (d), (e)
    assert_eq!(runner.life(P1), p1_life - 2);
    assert_eq!(
        zone(&runner, brigade),
        Zone::Graveyard,
        "CR 704.5g: 3 + 1 damage"
    );
}

/// T1b: committed damage that is already lethal leaves the next trampler owing 0.
#[test]
fn shared_blocker_with_lethal_committed_owes_nothing() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let first = printed(
        &mut scenario,
        "Fangren Hunter",
        (4, 4),
        &["Trample"],
        FANGREN_HUNTER,
    );
    let second = printed(
        &mut scenario,
        "Fangren Hunter",
        (4, 4),
        &["Trample"],
        FANGREN_HUNTER,
    );
    let brigade = scenario
        .add_creature_from_oracle(P1, "Foriysian Brigade", 2, 4, FORIYSIAN_BRIGADE)
        .id();
    let mut runner = scenario.build();
    let p1_life = runner.life(P1);

    attack_and_block(
        &mut runner,
        &[first, second],
        &[(brigade, first), (brigade, second)],
    );

    let (prompted, minimum) = prompt_minimum(&runner, brigade);
    assert_eq!(prompted, second);
    assert_eq!(committed(&runner, first, brigade), vec![4]);
    // (a) CR 702.19b: the committed 4 is lethal.
    assert_eq!(minimum, 0);
    // (b)
    assign(&mut runner, brigade, 0, 4).expect("nothing more is owed to the Brigade");
    finish_combat(&mut runner);
    // (c)
    assert_eq!(runner.life(P1), p1_life - 4);
    assert_eq!(zone(&runner, brigade), Zone::Graveyard);
}

/// Board for T2/T3: P1's Darksteel Myr (0/1, indestructible) blocks P0's 3/3 deathtouch trampler
/// (War Mammoth under Archetype of Finality).
/// With `dart`, P0 first casts Lava Dart at the Myr, which survives with 1 damage marked.
fn myr_blocks_deathtouch_trampler(dart: bool) -> (GameRunner, ObjectId, i32) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    printed(
        &mut scenario,
        "Archetype of Finality",
        (2, 3),
        &[],
        ARCHETYPE_OF_FINALITY,
    );
    let attacker = printed(
        &mut scenario,
        "War Mammoth",
        (3, 3),
        &["Trample"],
        WAR_MAMMOTH,
    );
    let myr = scenario
        .add_creature(P1, "Darksteel Myr", 0, 1)
        .as_artifact_creature()
        .from_oracle_text_with_keywords(&["Indestructible"], DARKSTEEL_MYR)
        .id();
    let lava_dart = scenario
        .add_spell_to_hand_from_oracle(P0, "Lava Dart", true, LAVA_DART)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    if dart {
        let outcome = runner.cast(lava_dart).target_object(myr).resolve();
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
        assert!(outcome.state().stack.is_empty());
        // CR 702.12b: lethal damage does not destroy an indestructible creature.
        assert_eq!(outcome.zone_of(myr), Zone::Battlefield);
        assert_eq!(outcome.state().objects[&myr].damage_marked, 1);
    }

    let p1_life = runner.life(P1);
    attack_and_block(&mut runner, &[attacker], &[(myr, attacker)]);
    (runner, myr, p1_life)
}

/// T2: CR 702.19b "take into account damage already marked": the Myr already has lethal damage
/// (1 against toughness 1), so even a deathtouch trampler owes it nothing.
#[test]
fn already_lethal_blocker_owes_a_deathtouch_trampler_nothing() {
    let (mut runner, myr, p1_life) = myr_blocks_deathtouch_trampler(true);

    let (_, minimum) = prompt_minimum(&runner, myr);
    assert_eq!(minimum, 0);
    assign(&mut runner, myr, 0, 3).expect("the already-lethal Myr is owed nothing");
    finish_combat(&mut runner);
    assert_eq!(runner.life(P1), p1_life - 3);
    assert_eq!(zone(&runner, myr), Zone::Battlefield, "CR 702.12b");
}

/// T3: an undamaged Myr is not yet lethal, so the deathtouch trampler still owes it 1
/// (CR 702.2c).
#[test]
fn undamaged_blocker_still_owes_a_deathtouch_trampler_one() {
    let (mut runner, myr, p1_life) = myr_blocks_deathtouch_trampler(false);

    let (_, minimum) = prompt_minimum(&runner, myr);
    assert_eq!(minimum, 1);
    assert!(
        assign(&mut runner, myr, 0, 3).is_err(),
        "CR 702.19b: excess before lethal is rejected"
    );
    // Reach guard: the rejection left the prompt open, and lethal-then-excess completes.
    assert_eq!(prompt_minimum(&runner, myr).1, 1);
    assign(&mut runner, myr, 1, 2).expect("1 deathtouch damage is lethal");
    finish_combat(&mut runner);
    assert_eq!(runner.life(P1), p1_life - 2);
    assert_eq!(zone(&runner, myr), Zone::Battlefield, "CR 702.12b");
}

/// T4: an earlier non-trampling deathtouch attacker's committed 1 damage is lethal (CR 702.2c),
/// so a later non-deathtouch trampler owes the shared Brigade nothing.
#[test]
fn committed_deathtouch_damage_is_lethal_for_a_later_trampler() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let deathtouch = printed(
        &mut scenario,
        "Sedge Scorpion",
        (1, 1),
        &["Deathtouch"],
        SEDGE_SCORPION,
    );
    let trampler = printed(
        &mut scenario,
        "War Mammoth",
        (3, 3),
        &["Trample"],
        WAR_MAMMOTH,
    );
    let brigade = scenario
        .add_creature_from_oracle(P1, "Foriysian Brigade", 2, 4, FORIYSIAN_BRIGADE)
        .id();
    let mut runner = scenario.build();
    let p1_life = runner.life(P1);

    attack_and_block(
        &mut runner,
        &[deathtouch, trampler],
        &[(brigade, deathtouch), (brigade, trampler)],
    );

    let (prompted, minimum) = prompt_minimum(&runner, brigade);
    assert_eq!(prompted, trampler);
    // (a)
    assert_eq!(committed(&runner, deathtouch, brigade), vec![1]);
    // (b)
    assert_eq!(minimum, 0);
    // (c)
    assign(&mut runner, brigade, 0, 3).expect("the committed deathtouch damage is lethal");
    finish_combat(&mut runner);
    // (d), (e) CR 704.5h
    assert_eq!(runner.life(P1), p1_life - 3);
    assert_eq!(zone(&runner, brigade), Zone::Graveyard);
}

/// T5: damage committed by answering an earlier trampler's prompt counts for the next trampler.
/// The first 5/5 is prompted (5 exceeds the Brigade's lethal 4) and assigns 4 to it; once that
/// answer is committed, the second 5/5 owes the Brigade nothing (CR 702.19b).
#[test]
fn answered_prompt_damage_counts_for_the_next_trampler() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let first = printed(
        &mut scenario,
        "Giant Warthog",
        (5, 5),
        &["Trample"],
        GIANT_WARTHOG,
    );
    let second = printed(
        &mut scenario,
        "Giant Warthog",
        (5, 5),
        &["Trample"],
        GIANT_WARTHOG,
    );
    let brigade = scenario
        .add_creature_from_oracle(P1, "Foriysian Brigade", 2, 4, FORIYSIAN_BRIGADE)
        .id();
    let mut runner = scenario.build();
    let p1_life = runner.life(P1);

    attack_and_block(
        &mut runner,
        &[first, second],
        &[(brigade, first), (brigade, second)],
    );

    let (prompted, minimum) = prompt_minimum(&runner, brigade);
    assert_eq!(prompted, first);
    assert_eq!(minimum, 4, "nothing is committed yet");
    assign(&mut runner, brigade, 4, 1).expect("lethal then excess");

    let (prompted, minimum) = prompt_minimum(&runner, brigade);
    assert_eq!(prompted, second);
    assert_eq!(committed(&runner, first, brigade), vec![4]);
    assert_eq!(minimum, 0, "the answered 4 is lethal");
    assign(&mut runner, brigade, 0, 5).expect("nothing more is owed to the Brigade");
    finish_combat(&mut runner);
    assert_eq!(runner.life(P1), p1_life - 6);
    assert_eq!(zone(&runner, brigade), Zone::Graveyard);
}
