//! "… discards a card with the greatest mana value among cards in their hand."
//!
//! CR 608.2c: "their hand" is the DISCARDING player's hand, not the caster's.
//! The aggregate inside the discard's card filter must therefore compare
//! against the recipient's cards, whether the recipient is a targeted player
//! (Draconic Fealty) or an untargeted player reached through another target
//! ("its controller"). Each fixture gives the caster a low-mana-value card that
//! matches a low-value card in the recipient's hand, so a filter evaluated
//! against the caster's hand would discard the wrong card.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{Effect, TargetFilter};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DRACONIC_FEALTY: &str = "As an additional cost to cast this spell, you may behold a \
Dragon.\nTarget player discards a card with the greatest mana value among cards in their hand. \
If a Dragon was beheld, exile that player's graveyard.";

/// The "its controller discards …" clause shape Intruder's Inquisition shares,
/// behind a single object target so the anaphor's referent is unambiguous.
const DESTROY_THEN_CONTROLLER_DISCARDS: &str = "Destroy target creature. Its controller \
discards a card with the greatest mana value among cards in their hand.";

/// CR 608.2c + CR 115.1: a targeted player discards the greatest-mana-value
/// card from THEIR hand, measured against their own cards.
#[test]
fn targeted_player_discards_their_greatest_mana_value_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_spell_to_hand(P0, "Shock", true)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Red],
        });
    let opt = scenario
        .add_spell_to_hand(P1, "Opt", true)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    let elemental = scenario
        .add_creature_to_hand(P1, "Air Elemental", 4, 4)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
        })
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Draconic Fealty", false, DRACONIC_FEALTY)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Black],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Black, spell, false, vec![])],
    );
    let mut runner = scenario.build();

    let outcome = runner
        .cast(spell)
        .target_player(P1)
        .decline_optional()
        .resolve();

    outcome.assert_zone(&[elemental], Zone::Graveyard);
    outcome.assert_zone(&[opt], Zone::Hand);
    outcome.assert_zone(&[spell], Zone::Graveyard);
    assert_eq!(
        outcome.state().players[0].mana_pool.total(),
        0,
        "Draconic Fealty's {{B}} must be paid"
    );
}

/// CR 608.2c + CR 115.10: "its controller" is affected without being targeted,
/// and still discards from their own hand. The destroyed creature's controller
/// is read from last known information (CR 608.2h).
#[test]
fn untargeted_controller_discards_their_greatest_mana_value_card() {
    let parsed = engine::parser::oracle::parse_oracle_text(
        DESTROY_THEN_CONTROLLER_DISCARDS,
        "Recipient Probe",
        &[],
        &["Sorcery".to_owned()],
        &[],
    );
    let discard = parsed.abilities[0]
        .sub_ability
        .as_deref()
        .expect("discard sub-ability");
    assert!(
        matches!(
            &*discard.effect,
            Effect::Discard {
                target: TargetFilter::ParentTargetController,
                filter: Some(_),
                ..
            }
        ),
        "expected an untargeted ParentTargetController discard, got {:?}",
        discard.effect
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_vanilla(P1, 1, 1);
    scenario
        .add_spell_to_hand(P0, "Shock", true)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Red],
        });
    let opt = scenario
        .add_spell_to_hand(P1, "Opt", true)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    let elemental = scenario
        .add_creature_to_hand(P1, "Air Elemental", 4, 4)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
        })
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Recipient Probe",
            false,
            DESTROY_THEN_CONTROLLER_DISCARDS,
        )
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Black],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Black, spell, false, vec![])],
    );
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_object(victim).resolve();

    outcome.assert_zone(&[victim, elemental], Zone::Graveyard);
    outcome.assert_zone(&[opt], Zone::Hand);
    assert_eq!(
        outcome.state().players[0].mana_pool.total(),
        0,
        "the probe's {{B}} must be paid"
    );
}
