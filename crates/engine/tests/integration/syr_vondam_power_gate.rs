//! Syr Vondam, Sunstar Exemplar: "When Syr Vondam dies or is put into exile
//! while its power is 4 or greater, destroy up to one target nonland permanent."
//!
//! CR 603.2: the power gate modifies both events of the disjunction. The
//! trigger-line splitter used to give the "dies" leg no gate at all, so a 2/2
//! Syr Vondam dying destroyed a permanent. CR 603.10a: the leaves-the-battlefield
//! leg looks back, so the gate reads the power Syr Vondam had on the battlefield
//! (a lord's bonus and its +1/+1 counters), not its 2/2 card in the graveyard.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{TargetRef, TriggerCondition};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SYR_VONDAM: &str = "Vigilance, menace\nWhenever another creature you control dies or is put into exile, put a +1/+1 counter on Syr Vondam and you gain 1 life.\nWhen Syr Vondam dies or is put into exile while its power is 4 or greater, destroy up to one target nonland permanent.";
const BENALISH_MARSHAL: &str = "Other creatures you control get +1/+1.";
const TRAVEL_PREPARATIONS: &str = "Put a +1/+1 counter on each of up to two target creatures.\nFlashback {1}{W} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
const WRATH_OF_GOD: &str = "Destroy all creatures. They can't be regenerated.";
const SOL_RING: &str = "{T}: Add {C}{C}.";
const DESTROY_TEXT: &str = "destroy up to one target nonland permanent";

struct Board {
    runner: GameRunner,
    syr: ObjectId,
    sol_ring: ObjectId,
    wrath: ObjectId,
    travel: ObjectId,
}

/// P0: Syr Vondam (2/2), optionally Benalish Marshal, Travel Preparations and
/// Wrath of God in hand, and lands. P1: Sol Ring, the surviving nonland target.
fn board(with_marshal: bool) -> Board {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let syr = s
        .add_creature_from_oracle(P0, "Syr Vondam, Sunstar Exemplar", 2, 2, SYR_VONDAM)
        .id();
    if with_marshal {
        s.add_creature_from_oracle(P0, "Benalish Marshal", 3, 3, BENALISH_MARSHAL);
    }
    let travel = s
        .add_spell_to_hand_from_oracle(P0, "Travel Preparations", false, TRAVEL_PREPARATIONS)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();
    let wrath = s
        .add_spell_to_hand_from_oracle(P0, "Wrath of God", false, WRATH_OF_GOD)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::White],
            generic: 2,
        })
        .id();
    s.add_basic_land(P0, ManaColor::Green);
    for _ in 0..5 {
        s.add_basic_land(P0, ManaColor::White);
    }
    let sol_ring = s.add_artifact_from_oracle(P1, "Sol Ring", SOL_RING).id();
    Board {
        runner: s.build(),
        syr,
        sol_ring,
        wrath,
        travel,
    }
}

/// What the wipe's trigger window showed for Syr Vondam's destruction ability.
#[derive(Default)]
struct DestroyTrigger {
    prompts: usize,
    offered_sol_ring: bool,
}

/// Cast Wrath of God and drive the game until the stack is empty, answering
/// trigger ordering in printed order. The only targeting prompt on this board
/// is the destruction ability's (asserted by its description); it is answered
/// with Sol Ring.
fn wrath(b: &mut Board) -> DestroyTrigger {
    b.runner.cast(b.wrath).commit();
    let mut seen = DestroyTrigger::default();
    for _ in 0..40 {
        match b.runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                b.runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order triggers");
            }
            WaitingFor::TriggerTargetSelection {
                description,
                target_slots,
                selection,
                ..
            } => {
                assert!(
                    description
                        .as_deref()
                        .is_some_and(|d| d.to_lowercase().contains(DESTROY_TEXT)),
                    "only the destruction ability targets: {description:?}"
                );
                seen.prompts += 1;
                seen.offered_sol_ring = target_slots[selection.current_slot]
                    .legal_targets
                    .contains(&TargetRef::Object(b.sol_ring));
                b.runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(b.sol_ring)),
                    })
                    .expect("choose Sol Ring for the destruction trigger");
            }
            WaitingFor::Priority { .. } if !b.runner.state().stack.is_empty() => {
                b.runner.act(GameAction::PassPriority).expect("pass");
            }
            _ => break,
        }
    }
    assert!(
        b.runner.state().stack.is_empty()
            && matches!(b.runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the wipe and every trigger must finish at an empty-stack Priority, got {}",
        b.runner.waiting_for_kind()
    );
    assert_eq!(
        b.runner.state().objects[&b.syr].zone,
        Zone::Graveyard,
        "Wrath of God destroyed Syr Vondam"
    );
    seen
}

/// Reach guard: both legs of the printed line carry the power gate.
#[test]
fn syr_vondam_power_gate_parses_on_both_legs() {
    let parsed = parse_oracle_text(
        SYR_VONDAM,
        "Syr Vondam, Sunstar Exemplar",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let gated: Vec<_> = parsed
        .triggers
        .iter()
        .filter(|t| {
            t.description
                .as_deref()
                .is_some_and(|d| d.to_lowercase().contains(DESTROY_TEXT))
        })
        .collect();
    assert_eq!(gated.len(), 2, "dies leg and exile leg");
    for t in gated {
        assert!(
            matches!(t.condition, Some(TriggerCondition::EventTime { .. })),
            "CR 603.2: each leg keeps the power gate: {:?}",
            t.condition
        );
    }
}

/// CR 603.2: a 2/2 Syr Vondam dying is below the gate; the destruction ability
/// does not trigger and Sol Ring survives.
#[test]
fn syr_vondam_dies_below_four_power_does_not_destroy() {
    let mut b = board(false);
    let seen = wrath(&mut b);
    assert_eq!(seen.prompts, 0, "no destruction trigger at 2 power");
    assert_eq!(
        b.runner.state().objects[&b.sol_ring].zone,
        Zone::Battlefield
    );
}

/// CR 603.10a hostile board: Syr Vondam is 4/4 only through Benalish Marshal's
/// +1/+1 and one +1/+1 counter from Travel Preparations, and Wrath of God
/// destroys the Marshal with it. In the graveyard it is a 2/2 with no counters;
/// the look-back reads 4. (The Marshal's death also triggers Syr Vondam's
/// untargeted first ability; the targeting prompt is asserted to be the
/// destruction ability's own.)
#[test]
fn syr_vondam_dies_at_four_power_through_lord_and_counter_destroys() {
    let mut b = board(true);
    let (travel, syr) = (b.travel, b.syr);
    b.runner.cast(travel).target_object(syr).resolve();
    assert_eq!(
        b.runner.state().objects[&syr].power,
        Some(4),
        "2 base + Marshal + one counter"
    );

    let seen = wrath(&mut b);
    assert_eq!(seen.prompts, 1, "exactly one destruction trigger");
    assert!(seen.offered_sol_ring, "Sol Ring is a legal target");
    assert_eq!(
        b.runner.state().objects[&b.sol_ring].zone,
        Zone::Graveyard,
        "the destruction trigger resolved against Sol Ring"
    );
}
