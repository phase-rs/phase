//! Cryptex (#4140 deferred type 1): `{T}, Collect evidence 3: Add one mana of
//! any color. Put an unlock counter on this artifact.`
//!
//! This is a mana ability whose activation cost is
//! `Composite[Tap, CollectEvidence{amount: 3}]` (CR 701.59). The interactive
//! collect-evidence exile must be paid as a mana-ability activation cost
//! (CR 605.2) before the mana is produced, and the `SequentialSibling`
//! PutCounter sub-ability resolves inline once afterward.
//!
//! Drives the real activation pipeline:
//!   ActivateAbility → CollectEvidenceChoice (the new mana-ability cost gate)
//!     → SelectCards → ChooseManaColor → mana produced + unlock counter placed.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 701.59a: collect evidence N exiles graveyard cards with total MV >= N.
//!   - CR 605.2: a mana ability's cost is paid before it produces mana.
//!   - CR 605.3b: mana abilities resolve immediately, not on the stack.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::AbilityCost;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{ManaChoice, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaType;
use engine::types::phase::Phase;

const CRYPTEX_ORACLE: &str =
    "{T}, Collect evidence 3: Add one mana of any color. Put an unlock counter on this artifact.";

/// Find the index of the mana ability whose cost contains a CollectEvidence
/// component (robust to ability ordering).
fn collect_evidence_ability_index(
    state: &engine::types::game_state::GameState,
    id: ObjectId,
) -> usize {
    let obj = state.objects.get(&id).expect("Cryptex exists");
    obj.abilities
        .iter()
        .position(|a| matches!(&a.cost, Some(cost) if cost_has_collect_evidence(cost)))
        .expect("Cryptex has a collect-evidence mana ability")
}

fn cost_has_collect_evidence(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::CollectEvidence { .. } => true,
        AbilityCost::Composite { costs } => costs.iter().any(cost_has_collect_evidence),
        _ => false,
    }
}

/// Cryptex is an artifact, not a creature. The scenario creature helper is used
/// to parse Oracle text onto a battlefield permanent; convert it to a pure
/// artifact and clear P/T so the 0/0 stub is not destroyed as an SBA (CR 704.5f)
/// before the mana ability can be activated.
fn make_artifact(runner: &mut GameRunner, id: ObjectId) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![engine::types::card_type::CoreType::Artifact];
    obj.base_card_types = obj.card_types.clone();
    obj.power = None;
    obj.toughness = None;
    obj.base_power = None;
    obj.base_toughness = None;
}

#[test]
fn cryptex_collect_evidence_mana_ability_produces_mana_and_counter() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    // Three cards in graveyard with total mana value >= 3 (2 + 2 = 4 >= 3).
    scenario
        .add_spell_to_graveyard(P0, "Opt", true)
        .with_mana_cost(engine::types::mana::ManaCost::Cost {
            generic: 2,
            shards: vec![],
        });
    scenario
        .add_spell_to_graveyard(P0, "Negate", true)
        .with_mana_cost(engine::types::mana::ManaCost::Cost {
            generic: 2,
            shards: vec![],
        });
    // Cryptex is an artifact on the battlefield; place via add_creature then make
    // it a non-summoning-sick permanent (mana ability with {T}).
    let cryptex = scenario
        .add_creature_from_oracle(P0, "Cryptex", 0, 0, CRYPTEX_ORACLE)
        .id();
    let mut runner = scenario.build();
    make_artifact(&mut runner, cryptex);

    let idx = collect_evidence_ability_index(runner.state(), cryptex);

    // CR 602.2a: announce activation. A mana ability resolves immediately and
    // surfaces the new collect-evidence cost prompt (CR 605.2 / CR 701.59).
    runner
        .act(GameAction::ActivateAbility {
            source_id: cryptex,
            ability_index: idx,
        })
        .expect("activation accepted");

    let (legal_cards, min) = match runner.state().waiting_for.clone() {
        WaitingFor::CollectEvidenceChoice {
            cards,
            minimum_mana_value,
            ..
        } => (cards, minimum_mana_value),
        other => panic!("Expected CollectEvidenceChoice, got {other:?}"),
    };
    assert_eq!(min, 3, "collect evidence 3 threshold");
    assert!(legal_cards.len() >= 2, "graveyard cards offered");

    // Exile two graveyard cards (total MV 4 >= 3) to pay the cost.
    let chosen: Vec<ObjectId> = legal_cards.iter().copied().take(2).collect();
    runner
        .act(GameAction::SelectCards {
            cards: chosen.clone(),
        })
        .expect("collect-evidence selection accepted");

    // CR 605.3b: production prompts for the "any color" choice.
    match runner.state().waiting_for.clone() {
        WaitingFor::ChooseManaColor { .. } => {}
        other => panic!("Expected ChooseManaColor, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Blue),
            count: 1,
        })
        .expect("color choice accepted");

    // Cards exiled (no longer in graveyard).
    for id in &chosen {
        assert_eq!(
            runner.state().objects.get(id).unwrap().zone,
            engine::types::zones::Zone::Exile,
            "evidence card exiled"
        );
    }
    // +1 blue mana produced.
    let pool = &runner.state().players[0].mana_pool;
    assert_eq!(
        pool.mana
            .iter()
            .filter(|m| m.color == ManaType::Blue)
            .count(),
        1,
        "exactly one blue mana produced"
    );
    // Exactly one unlock counter on Cryptex (the SequentialSibling PutCounter
    // sub-ability resolved once).
    let unlock = runner
        .state()
        .objects
        .get(&cryptex)
        .unwrap()
        .counters
        .get(&CounterType::Generic("unlock".to_string()))
        .copied()
        .unwrap_or(0);
    assert_eq!(unlock, 1, "exactly one unlock counter placed");
}

#[test]
fn cryptex_collect_evidence_not_payable_below_threshold() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    // Only one MV-1 card in graveyard → total MV 1 < 3.
    scenario
        .add_spell_to_graveyard(P0, "Opt", true)
        .with_mana_cost(engine::types::mana::ManaCost::Cost {
            generic: 1,
            shards: vec![],
        });
    let cryptex = scenario
        .add_creature_from_oracle(P0, "Cryptex", 0, 0, CRYPTEX_ORACLE)
        .id();
    let mut runner = scenario.build();
    make_artifact(&mut runner, cryptex);
    let idx = collect_evidence_ability_index(runner.state(), cryptex);

    let result = runner.act(GameAction::ActivateAbility {
        source_id: cryptex,
        ability_index: idx,
    });
    // CR 701.59b: cannot collect evidence 3 with only MV 1 in graveyard — the
    // mana ability is not payable, so activation is rejected and no mana is
    // produced / no counter placed.
    assert!(
        result.is_err()
            || !matches!(
                runner.state().waiting_for,
                WaitingFor::CollectEvidenceChoice { .. } | WaitingFor::ChooseManaColor { .. }
            ),
        "below-threshold activation must not enter the collect-evidence flow"
    );
}
