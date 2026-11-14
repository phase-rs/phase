//! F2: virtual protection candidates are suppressed while prevention is disabled.
//!
//! Board: P0 controls an established Selfless Squire (directly placed, so its
//! ETB shield never exists), a Runed Halo naming "Lightning Bolt", a Jade
//! Monolith, and a Bystander creature. P1 casts Lightning Bolt at the
//! Bystander; P0 answers by activating the Monolith, redirecting the Bolt to
//! P0. The Bolt damage to P0 then meets exactly one applicable protection
//! quality (the Halo) and — in the Skullcrack row — a global
//! `DamagePreventionDisabled` restriction.
//!
//! * Skullcrack row: P1 first resolves Skullcrack ("Damage can't be prevented
//!   this turn"). The Halo's virtual protection candidate must NOT be drawn,
//!   so the redirected 3 damage is dealt (P0 20 → 17) and the Squire never
//!   triggers (0 counters). Pre-fix the candidate applied and prevented (P0
//!   stays 20, Squire fires) — the revert-flip.
//! * Preventable control: no Skullcrack. The lone Halo quality auto-applies,
//!   P0 stays 20, and the Squire consumes exactly 3 — the positive
//!   reach-guard proving the prevention path was live.
//!
//! Verbatim Oracle texts (MTGJSON 5.3.0+20261114):
//! Skullcrack: "Players can't gain life this turn. Damage can't be prevented
//! this turn. Skullcrack deals 3 damage to target player or planeswalker."
//! Lightning Bolt: "Lightning Bolt deals 3 damage to any target."
//! Jade Monolith: "{1}: The next time a source of your choice would deal
//! damage to target creature this turn, that source deals that damage to you
//! instead."
//! Runed Halo: "As this enchantment enters, choose a card name.\nYou have
//! protection from the chosen card name. (You can't be targeted, dealt damage,
//! or enchanted by anything with that name.)"
//! Selfless Squire: "Flash\nWhen this creature enters, prevent all damage
//! that would be dealt to you this turn.\nWhenever damage that would be dealt
//! to you is prevented, put that many +1/+1 counters on this creature."

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityKind, ChosenAttribute, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const RUNED_HALO_ORACLE: &str = "As this enchantment enters, choose a card name.\nYou have protection from the chosen card name. (You can't be targeted, dealt damage, or enchanted by anything with that name.)";
const SELFLESS_SQUIRE_TEXT: &str = "Flash\nWhen this creature enters, prevent all damage that would be dealt to you this turn.\nWhenever damage that would be dealt to you is prevented, put that many +1/+1 counters on this creature.";
const JADE: &str = "{1}: The next time a source of your choice would deal damage to target creature this turn, that source deals that damage to you instead.";
const BOLT: &str = "Lightning Bolt deals 3 damage to any target.";
const SKULLCRACK: &str = "Players can't gain life this turn. Damage can't be prevented this turn. Skullcrack deals 3 damage to target player or planeswalker.";

fn stock_libraries(scenario: &mut GameScenario) {
    scenario.with_library_top(
        P0,
        &["F0a", "F0b", "F0c", "F0d", "F0e", "F0f", "F0g", "F0h"],
    );
    scenario.with_library_top(
        P1,
        &["F1a", "F1b", "F1c", "F1d", "F1e", "F1f", "F1g", "F1h"],
    );
}

fn one_mana(color: ManaType) -> ManaUnit {
    ManaUnit::new(color, ObjectId(0), false, vec![])
}

fn red_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 0,
    }
}

struct Fixture {
    runner: GameRunner,
    squire: ObjectId,
    monolith: ObjectId,
    bystander: ObjectId,
    bolt: ObjectId,
    skullcrack: Option<ObjectId>,
}

fn give_priority(runner: &mut GameRunner, player: PlayerId) {
    runner.state_mut().active_player = player;
    runner.state_mut().priority_player = player;
    runner.state_mut().waiting_for = WaitingFor::Priority { player };
}

/// P0: established Squire + Halo naming "Lightning Bolt" + Monolith +
/// Bystander. P1: Lightning Bolt in hand (+ Skullcrack when asked for).
fn setup(with_skullcrack: bool) -> Fixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    let squire = scenario
        .add_creature_from_oracle(P0, "Selfless Squire", 1, 1, SELFLESS_SQUIRE_TEXT)
        .id();
    scenario
        .add_enchantment_from_oracle(P0, "Runed Halo", RUNED_HALO_ORACLE)
        .id();
    let monolith = scenario
        .add_artifact_from_oracle(P0, "Jade Monolith", JADE)
        .id();
    let bystander = scenario.add_creature(P0, "Bystander", 2, 2).id();
    scenario.with_mana_pool(P0, vec![one_mana(ManaType::Colorless)]);
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P1, "Lightning Bolt", true, BOLT)
        .with_mana_cost(red_cost())
        .id();
    let skullcrack = if with_skullcrack {
        Some(
            scenario
                .add_spell_to_hand_from_oracle(P1, "Skullcrack", true, SKULLCRACK)
                .with_mana_cost(ManaCost::Cost {
                    shards: vec![ManaCostShard::Red],
                    generic: 1,
                })
                .id(),
        )
    } else {
        None
    };
    if with_skullcrack {
        scenario.with_mana_pool(
            P1,
            vec![
                one_mana(ManaType::Colorless),
                one_mana(ManaType::Red),
                one_mana(ManaType::Red),
            ],
        );
    } else {
        scenario.with_mana_pool(P1, vec![one_mana(ManaType::Red)]);
    }
    let mut runner = scenario.build();

    // Name the Halo through the production attribute (direct placement never
    // runs the as-enters choice).
    let halo = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == "Runed Halo")
        .expect("Halo must be on the battlefield");
    runner
        .state_mut()
        .objects
        .get_mut(&halo)
        .unwrap()
        .chosen_attributes
        .push(ChosenAttribute::CardName("Lightning Bolt".to_string()));
    assert_eq!(
        counters_on(&runner, squire),
        0,
        "reach-guard: the established Squire starts with no counters"
    );
    Fixture {
        runner,
        squire,
        monolith,
        bystander,
        bolt,
        skullcrack,
    }
}

fn counters_on(runner: &GameRunner, obj: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&obj)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

fn life_of(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.life(player)
}

fn activated_index(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|a| matches!(a.kind, AbilityKind::Activated))
        .expect("the source must expose its activated ability")
}

/// P1 casts `spell_id` (an instant) at `target`, leaving it committed on the
/// stack so P0 can respond. The cast keeps its ObjectId across the hand →
/// stack zone change, so the caller already knows the stack source id.
fn commit_instant_at(runner: &mut GameRunner, spell_id: ObjectId, target: TargetRef) {
    give_priority(runner, P1);
    match target {
        TargetRef::Player(p) => runner.cast(spell_id).target_player(p).commit(),
        TargetRef::Object(o) => runner.cast(spell_id).target_object(o).commit(),
    };
    assert!(
        !runner.state().stack.is_empty(),
        "the spell must be on the stack"
    );
}

/// P0 activates the Monolith in response: target `protected`, then name
/// `source` at the DamageSourceChoice.
fn install_monolith_redirect(
    runner: &mut GameRunner,
    jade: ObjectId,
    protected: ObjectId,
    source: ObjectId,
) {
    let idx = activated_index(runner, jade);
    // The Bolt commit leaves a priority window with P1; route to P0, then
    // announce the Monolith activation instead of passing.
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { player } if player == P0 => {
                runner
                    .act(GameAction::ActivateAbility {
                        source_id: jade,
                        ability_index: idx,
                        payment_mode: CastPaymentMode::Auto,
                    })
                    .expect("announce the Monolith activation");
                break;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass toward P0's response window");
            }
            other => panic!("expected a priority window, got {other:?}"),
        }
    }
    let WaitingFor::TargetSelection { .. } = runner.state().waiting_for.clone() else {
        panic!(
            "expected Monolith target prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(protected)),
        })
        .expect("target the Bolt's creature with the Monolith");
    for _ in 0..16 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DamageSourceChoice { .. }
        ) {
            break;
        }
        assert!(
            !runner.state().stack.is_empty(),
            "Monolith must reach its source choice"
        );
        runner
            .act(GameAction::PassPriority)
            .expect("resolve Monolith through priority passes");
    }
    let WaitingFor::DamageSourceChoice {
        player, options, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected Monolith source choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "P0 answers its own Monolith choice");
    assert!(
        options.contains(&source),
        "the Bolt source must be choosable, got {options:?}"
    );
    runner
        .act(GameAction::ChooseDamageSource { source })
        .expect("resume Monolith with the Bolt source");
}

/// Drive to quiescence (empty stack, Priority), answering any stray prompts.
/// Returns every event observed along the way.
fn drive_to_quiet(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..200 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return events;
                }
                let result = runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must succeed");
                events.extend(result.events);
            }
            WaitingFor::ReplacementChoice { candidates, .. } => {
                // No crowded prompt is expected on these rows (a lone
                // protection quality auto-applies); answering index 0 keeps the
                // drive total if one ever parks.
                let _ = candidates;
                let result = runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("the ordering choice must be answerable");
                events.extend(result.events);
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                let result = runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("ordering triggers must succeed");
                events.extend(result.events);
            }
            other => panic!("unexpected prompt while draining: {other:?}"),
        }
    }
    panic!("never reached quiescence");
}

fn prevented_amounts(events: &[GameEvent], target: TargetRef) -> Vec<u32> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DamagePrevented {
                target: t, amount, ..
            } if *t == target => Some(*amount),
            _ => None,
        })
        .collect()
}

/// Skullcrack row: prevention disabled ⇒ the Halo candidate is never drawn ⇒
/// the redirected Bolt deals 3 to P0 and the Squire stays quiet.
#[test]
fn skullcrack_disables_halo_protection_for_redirected_bolt() {
    let mut fixture = setup(true);
    let p0_before = life_of(&fixture.runner, P0);
    let p1_before = life_of(&fixture.runner, P1);

    // Skullcrack resolves first (targets P1 itself, keeping P0's life purely
    // Bolt-driven): P1 20 → 17 and prevention turns off for the turn.
    let skullcrack = fixture
        .skullcrack
        .expect("the Skullcrack row needs Skullcrack");
    give_priority(&mut fixture.runner, P1);
    fixture.runner.cast(skullcrack).target_player(P1).resolve();
    assert_eq!(
        life_of(&fixture.runner, P1),
        p1_before - 3,
        "reach-guard: Skullcrack must have resolved, dealing 3 to P1"
    );

    // Bolt at the Bystander, redirected to P0 through the Monolith.
    let bolt = fixture.bolt;
    let monolith = fixture.monolith;
    let bystander = fixture.bystander;
    commit_instant_at(&mut fixture.runner, bolt, TargetRef::Object(bystander));
    install_monolith_redirect(&mut fixture.runner, monolith, bystander, bolt);
    let events = drive_to_quiet(&mut fixture.runner);

    assert_eq!(
        life_of(&fixture.runner, P0),
        p0_before - 3,
        "unpreventable redirected Bolt must deal 3 to P0"
    );
    assert!(
        prevented_amounts(&events, TargetRef::Player(P0)).is_empty(),
        "no prevention may apply under Skullcrack, got {events:?}"
    );
    assert_eq!(
        counters_on(&fixture.runner, fixture.squire),
        0,
        "the Squire must not trigger when nothing is prevented"
    );
    assert!(
        fixture.runner.state().stack.is_empty(),
        "stack is empty at the end"
    );
    assert!(
        matches!(
            fixture.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ),
        "Priority boundary, got {:?}",
        fixture.runner.state().waiting_for
    );
}

/// Preventable control: the same redirect with no Skullcrack. The lone Halo
/// quality prevents all 3 (P0 untouched) and the Squire consumes exactly 3 —
/// proving the prevention path the Skullcrack row suppresses was live.
#[test]
fn redirected_bolt_is_prevented_without_skullcrack() {
    let mut fixture = setup(false);
    let p0_before = life_of(&fixture.runner, P0);

    let bolt = fixture.bolt;
    let monolith = fixture.monolith;
    let bystander = fixture.bystander;
    commit_instant_at(&mut fixture.runner, bolt, TargetRef::Object(bystander));
    install_monolith_redirect(&mut fixture.runner, monolith, bystander, bolt);
    let events = drive_to_quiet(&mut fixture.runner);

    assert_eq!(
        prevented_amounts(&events, TargetRef::Player(P0)),
        vec![3],
        "the Halo must prevent exactly 3 to P0, got {events:?}"
    );
    assert_eq!(
        life_of(&fixture.runner, P0),
        p0_before,
        "P0's life must be unchanged — every point was prevented"
    );
    assert!(
        fixture.runner.state().stack.is_empty(),
        "the Squire trigger must have resolved before the counters are read"
    );
    assert_eq!(
        counters_on(&fixture.runner, fixture.squire),
        3,
        "the Squire must consume exactly the prevented 3"
    );
}
