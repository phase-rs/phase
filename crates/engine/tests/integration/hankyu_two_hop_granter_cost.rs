//! Two-hop granter-cost proof (F3): Hankyu equipped to a creature, re-donated
//! through Marvin, Murderous Mimic.
//!
//! CR 201.5a + CR 400.7 + CR 601.2h: "Remove all aim counters from Hankyu"
//! names the granting equipment, so the second-hop activation (from Marvin)
//! removes the counters from Hankyu — the first-bound granter — and never
//! from the intermediate carrier. Damage equals the counters removed from
//! Hankyu (`PreviousEffectAmount`); the carrier's own counters are untouched.
//! A zero-counter Hankyu still pays in full: ALL is a variable
//! remove-counter count (payable with nothing to take), the engine removes
//! zero for the bound payer, and the activation resolves — zero damage is
//! then not dealt at all (CR 120.8).

use engine::game::effects::attach::attach_to;
use engine::game::layers::evaluate_layers;
use engine::game::restrictions::ability_usage_key;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::TargetRef;

// Verbatim Scryfall Oracle text (Hankyu).
const HANKYU: &str =
    "Equipped creature has \"{T}: Put an aim counter on Hankyu\" and \"{T}, Remove all aim counters from Hankyu: This creature deals damage to any target equal to the number of aim counters removed this way.\"\nEquip {4} ({4}: Attach to target creature you control. Equip only as a sorcery.)";

// Verbatim Scryfall Oracle text (Marvin, Murderous Mimic).
const MARVIN: &str = "Marvin has all activated abilities of creatures you control that don't have the same name as this creature.";

fn counter(kind: &str) -> CounterType {
    CounterType::Generic(kind.to_string())
}

fn relayer(state: &mut GameState) {
    state.layers_dirty.mark_full();
    evaluate_layers(state);
}

/// The index of `source`'s activated ability whose text contains `text`.
fn described(runner: &GameRunner, source: ObjectId, text: &str) -> Option<usize> {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|a| a.description.as_deref().is_some_and(|d| d.contains(text)))
}

fn untap(b: &mut Board, id: ObjectId) {
    b.runner.state_mut().objects.get_mut(&id).unwrap().tapped = false;
}

struct Board {
    runner: GameRunner,
    host: ObjectId,
    hankyu: ObjectId,
    marvin: ObjectId,
}

/// Bearer (with one aim counter as the head-read discriminator) equipped with
/// Hankyu, plus Marvin — all under P0's control at precombat main.
fn board() -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Bearer", 2, 2).id();
    scenario.with_counter(host, counter("aim"), 1);
    let hankyu = scenario
        .add_artifact_from_oracle(P0, "Hankyu", HANKYU)
        .with_subtypes(vec!["Equipment"])
        .id();
    let marvin = scenario
        .add_creature_from_oracle(P0, "Marvin, Murderous Mimic", 2, 2, MARVIN)
        .id();
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), hankyu, host);
    relayer(runner.state_mut());
    Board {
        runner,
        host,
        hankyu,
        marvin,
    }
}

/// Activate `source`'s ability at `index`, answering the damage target with
/// P1 when the engine asks.
fn activate(b: &mut Board, source: ObjectId, index: usize) {
    b.runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    if matches!(
        b.runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        b.runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            })
            .unwrap();
    }
    b.runner.advance_until_stack_empty();
}

/// CR 201.5a + CR 601.2h: Marvin's re-donated damage ability removes Hankyu's
/// aim counters (hop 1 puts them there through the granted tap ability), deals
/// damage equal to the counters removed, and leaves the carrier untouched.
#[test]
fn marvin_redonated_hankyu_damage_pays_from_hankyu() {
    let mut b = board();
    assert_ne!(b.hankyu, b.host);

    // Hop 1, positive marker: the granted tap ability puts aim counters on
    // Hankyu itself, not on the equipped host.
    let (host, marvin) = (b.host, b.marvin);
    let put = described(&b.runner, host, "Put an aim counter on Hankyu")
        .expect("reach: the host carries Hankyu's granted put-counter ability");
    activate(&mut b, host, put);
    untap(&mut b, host);
    // Re-resolve the index: activation may reorder the host's ability list.
    let put = described(&b.runner, host, "Put an aim counter on Hankyu")
        .expect("reach: the put-counter ability survives its own activation");
    activate(&mut b, host, put);
    assert_eq!(
        b.runner.state().objects[&b.hankyu]
            .counters
            .get(&counter("aim")),
        Some(&2)
    );
    assert_eq!(
        b.runner.state().objects[&b.host]
            .counters
            .get(&counter("aim")),
        Some(&1)
    );

    // Hop 2, reach-guard: Marvin gained the re-donated damage ability.
    let burn = described(&b.runner, marvin, "Remove all aim counters from Hankyu")
        .expect("reach: Marvin carries the re-donated Hankyu damage ability");
    let p1_life = b.runner.state().players[1].life;
    activate(&mut b, marvin, burn);

    let st = b.runner.state();
    assert!(
        !matches!(st.waiting_for, WaitingFor::PayCost { .. }),
        "{:?}",
        st.waiting_for
    );
    assert!(st.stack.is_empty());
    // The cost came off Hankyu (bound), not the carrier (head).
    assert_eq!(st.objects[&b.hankyu].counters.get(&counter("aim")), None);
    assert_eq!(st.objects[&b.host].counters.get(&counter("aim")), Some(&1));
    // Composite cost paid atomically: Marvin tapped, damage equals the two
    // counters removed from Hankyu.
    assert!(st.objects[&b.marvin].tapped);
    assert_eq!(st.players[1].life, p1_life - 2);
}

/// CR 601.2h + CR 602.2b + CR 120.8: with no aim counters on Hankyu the
/// remove-ALL leg still pays — a variable remove-counter count is payable
/// with nothing to take, the engine removes zero for the bound payer, and the
/// activation resolves. Zero damage is then not dealt at all, so life is
/// unchanged. Every assertion below is resolution evidence an
/// abandon-after-tap defect cannot satisfy: the activation must reach target
/// selection (CR 601.2c — a reversed payment per CR 602.2b + CR 733.1 never
/// gets there), sit on the stack as Marvin's ability (CR 602.2a), and record
/// its turn tally before the stack drains.
#[test]
fn marvin_redonated_hankyu_damage_resolves_with_zero_counters() {
    let mut b = board();

    // Reach-guards: the ability is present and Hankyu is counter-free.
    let burn = described(&b.runner, b.marvin, "Remove all aim counters from Hankyu")
        .expect("reach: Marvin carries the re-donated Hankyu damage ability");
    assert_eq!(
        b.runner.state().objects[&b.hankyu]
            .counters
            .get(&counter("aim")),
        None
    );

    // Announcement + payment complete: the composite cost ({T} plus remove
    // zero) pays, so the engine asks for the damage target. A payment failure
    // would reverse the whole action (CR 602.2b + CR 733.1) and return to
    // Priority with no target prompt.
    let marvin = b.marvin;
    b.runner
        .act(GameAction::ActivateAbility {
            source_id: marvin,
            ability_index: burn,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    assert!(
        matches!(
            b.runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "the zero-counter cost pays; the activation awaits its target: {:?}",
        b.runner.state().waiting_for
    );

    // Placement evidence: answering the target puts Marvin's activation on the
    // stack (CR 602.2a) — an abandoned activation never sits there.
    b.runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Player(P1)],
        })
        .unwrap();
    {
        let st = b.runner.state();
        assert_eq!(st.stack.len(), 1, "one paid activation is on the stack");
        assert!(
            st.stack.back().is_some_and(|entry| matches!(
                &entry.kind,
                StackEntryKind::ActivatedAbility { source_id, ability }
                    if *source_id == marvin && ability.ability_index == Some(burn)
            )),
            "the stacked entry is Marvin's damage activation"
        );
    }
    b.runner.advance_until_stack_empty();

    // Resolution evidence: the announced activation consumed its own usage key
    // (written at placement, never on a reversed action), the stack drained,
    // and the tap leg stayed paid.
    let st = b.runner.state();
    let burn = described(&b.runner, marvin, "Remove all aim counters from Hankyu")
        .expect("reach: the damage ability survives its own resolution");
    let key = ability_usage_key(st, marvin, burn)
        .expect("the tested slot resolves through the activation lookup");
    assert_eq!(
        st.activated_abilities_this_turn.get(&key),
        Some(&1),
        "the resolved activation recorded its turn tally"
    );
    assert!(st.stack.is_empty());
    assert!(
        !matches!(st.waiting_for, WaitingFor::PayCost { .. }),
        "{:?}",
        st.waiting_for
    );
    assert!(st.objects[&marvin].tapped);
    // Zero removed from Hankyu (the bound granter), nothing from the carrier,
    // and no damage: a source that would deal 0 damage deals none (CR 120.8).
    assert_eq!(st.objects[&b.hankyu].counters.get(&counter("aim")), None);
    assert_eq!(st.objects[&b.host].counters.get(&counter("aim")), Some(&1));
    assert_eq!(st.players[1].life, 20);
}
