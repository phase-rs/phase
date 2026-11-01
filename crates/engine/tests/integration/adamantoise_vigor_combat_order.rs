//! CR 510.2 + CR 616.1: combat damage that two order-material replacements
//! both want must PAUSE for the affected player's choice, not vanish.
//!
//! Field report (2026-10-12): Ancient Adamantoise ("All damage that would be
//! dealt to you and other permanents you control is dealt to this creature
//! instead") + Vigor ("If damage would be dealt to another creature you
//! control, prevent that damage. Put a +1/+1 counter on that creature for each
//! 1 damage prevented this way") against Twinflame Tyrant ("…it deals double
//! that damage instead"). Every unblocked hit had two replacements competing
//! (Tyrant's doubling and the Adamantoise's redirection), and
//! `replace_combat_damage_batch` dropped the `NeedsChoice` — the defending
//! player took nothing, and the Adamantoise got no counters either. Same loss
//! when a blocker other than the Adamantoise is dealt damage (the redirection
//! and Vigor both apply to it).
//!
//! Outside combat the choice was already offered; these tests pin the combat
//! path. The redirected damage must then reach Vigor (CR 614.9 + CR 616.1f:
//! the new event is checked again), so the Adamantoise grows instead of being
//! damaged.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const ADAMANTOISE: &str = "Vigilance, ward {3}\nDamage isn't removed from this creature during cleanup steps.\nAll damage that would be dealt to you and other permanents you control is dealt to this creature instead.\nWhen this creature dies, exile it and create ten tapped Treasure tokens.";
const VIGOR: &str = "Trample\nIf damage would be dealt to another creature you control, prevent that damage. Put a +1/+1 counter on that creature for each 1 damage prevented this way.\nWhen Vigor is put into a graveyard from anywhere, shuffle it into its owner's library.";
const TWINFLAME: &str = "Flying\nIf a source you control would deal damage to an opponent or a permanent an opponent controls, it deals double that damage instead.";

/// Drives P0's attack through combat damage. Every replacement-order prompt is
/// answered with the first candidate; returns how many prompts P1 answered.
fn run_combat(
    runner: &mut GameRunner,
    attackers: &[ObjectId],
    blocks: &[(ObjectId, ObjectId)],
) -> usize {
    let mut attacked = false;
    let mut blocked = false;
    let mut choices = 0;
    for _ in 0..400 {
        if matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ) {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::DeclareAttackers { .. } if !attacked => {
                attacked = true;
                let attacks: Vec<_> = attackers
                    .iter()
                    .map(|&id| (id, AttackTarget::Player(P1)))
                    .collect();
                runner
                    .declare_attackers(&attacks)
                    .expect("declare attackers");
            }
            WaitingFor::DeclareBlockers { .. } if !blocked => {
                blocked = true;
                runner.declare_blockers(blocks).expect("declare blockers");
            }
            WaitingFor::ReplacementChoice { player, .. } => {
                assert_eq!(
                    player, P1,
                    "the defending player orders the replacements on its own damage"
                );
                choices += 1;
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("choose replacement");
            }
            other => panic!("unexpected prompt during combat: {other:?}"),
        }
    }
    assert!(
        matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ),
        "combat must finish, stopped at {:?}",
        runner.state().phase
    );
    choices
}

fn p1p1(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// The reported board: an unblocked attacker under Twinflame Tyrant. Twinflame
/// first (candidate 0) doubles 5 to 10; the Adamantoise moves the 10 onto
/// itself; Vigor turns the 10 into counters.
#[test]
fn twinflame_unblocked_hit_pauses_and_the_adamantoise_grows() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let turtle = scenario
        .add_creature_from_oracle(P1, "Ancient Adamantoise", 8, 20, ADAMANTOISE)
        .id();
    let _vigor = scenario
        .add_creature_from_oracle(P1, "Vigor", 6, 6, VIGOR)
        .id();
    let _tyrant = scenario
        .add_creature_from_oracle(P0, "Twinflame Tyrant", 3, 6, TWINFLAME)
        .id();
    let attacker = scenario.add_creature(P0, "Attacker", 5, 5).id();
    let mut runner = scenario.build();
    let life = runner.life(P1);

    let choices = run_combat(&mut runner, &[attacker], &[]);

    assert!(
        choices >= 1,
        "two competing replacements must reach the defending player as a choice"
    );
    assert_eq!(
        runner.life(P1),
        life,
        "the hit is redirected, never dealt to the player"
    );
    assert_eq!(
        runner.state().objects[&turtle].damage_marked,
        0,
        "Vigor prevents the redirected damage"
    );
    assert_eq!(
        p1p1(&runner, turtle),
        10,
        "doubled to 10, redirected, then turned into 10 counters"
    );
}

/// A creature other than the Adamantoise blocks: the redirection and Vigor both
/// apply to the damage dealt to it. Whichever goes first, the damage ends as
/// counters — never lost.
#[test]
fn blocker_damage_with_redirection_and_vigor_pauses_and_becomes_counters() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let turtle = scenario
        .add_creature_from_oracle(P1, "Ancient Adamantoise", 8, 20, ADAMANTOISE)
        .id();
    let _vigor = scenario
        .add_creature_from_oracle(P1, "Vigor", 6, 6, VIGOR)
        .id();
    let bear = scenario.add_creature(P1, "Bear", 2, 4).id();
    let attacker = scenario.add_creature(P0, "Attacker", 3, 3).id();
    let mut runner = scenario.build();

    let choices = run_combat(&mut runner, &[attacker], &[(bear, attacker)]);

    assert_eq!(
        choices, 1,
        "the Bear's damage is claimed by two replacements"
    );
    assert_eq!(runner.state().objects[&bear].damage_marked, 0);
    assert_eq!(runner.state().objects[&turtle].damage_marked, 0);
    assert_eq!(
        p1p1(&runner, turtle) + p1p1(&runner, bear),
        3,
        "the 3 damage becomes 3 counters on whichever creature it ended on"
    );
}

/// Positive control: with a single replacement per event, no prompt appears and
/// the redirected damage still reaches Vigor.
#[test]
fn unblocked_hits_without_competition_need_no_choice() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let turtle = scenario
        .add_creature_from_oracle(P1, "Ancient Adamantoise", 8, 20, ADAMANTOISE)
        .id();
    let _vigor = scenario
        .add_creature_from_oracle(P1, "Vigor", 6, 6, VIGOR)
        .id();
    let a = scenario.add_creature(P0, "Attacker A", 5, 5).id();
    let b = scenario.add_creature(P0, "Attacker B", 3, 3).id();
    let mut runner = scenario.build();
    let life = runner.life(P1);

    let choices = run_combat(&mut runner, &[a, b], &[]);

    assert_eq!(choices, 0);
    assert_eq!(runner.life(P1), life);
    assert_eq!(p1p1(&runner, turtle), 8);
}
