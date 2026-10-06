//! Tests verifying CR 602.5 + CR 605.3a:
//! When a static effect prohibits activating abilities of a permanent type
//! (e.g. Collector Ouphe: "Activated abilities of artifacts can't be activated."),
//! that prohibition applies to all activated abilities of those permanents,
//! including mana abilities, during spellcasting auto-tap, payment simulation,
//! and direct ability activation.
//!
//! Every prohibited case is paired with the identical source/cast WITHOUT
//! Collector Ouphe, so the rejection is attributable to the prohibition and the
//! producer is proven reachable by automatic payment.

use engine::game::casting::{can_cast_object_now_with_probe, PriorityCastProbe};
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::CastPaymentMode;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const COLLECTOR_OUPHE_ORACLE: &str = "Activated abilities of artifacts can't be activated.";
const ORNITHOPTER_OF_PARADISE_ORACLE: &str = "Flying\n{T}: Add one mana of any color.";
const LLANOWAR_ELVES_ORACLE: &str = "{T}: Add {G}.";
const GREAT_FURNACE_ORACLE: &str = "{T}: Add {R}.";

#[derive(Copy, Clone)]
enum Producer {
    /// Artifact creature with an explicit parsed `{T}: Add one mana of any color`.
    Ornithopter,
    /// Artifact land with an explicit parsed `{T}: Add {R}`.
    GreatFurnace,
    /// Artifact land with only the Forest subtype and NO ability definition:
    /// the CR 305.6 intrinsic mana ability fallback in `land_mana_options`.
    IntrinsicArtifactForest,
    /// Non-artifact creature with a parsed `{T}: Add {G}`.
    LlanowarElves,
    /// Basic Forest.
    Forest,
}

struct Fixture {
    runner: engine::game::scenario::GameRunner,
    source: ObjectId,
    spell: ObjectId,
}

fn setup(producer: Producer, with_ouphe: bool) -> Fixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    if with_ouphe {
        scenario.add_creature_from_oracle(P0, "Collector Ouphe", 2, 2, COLLECTOR_OUPHE_ORACLE);
    }

    let (color, source) = match producer {
        Producer::Ornithopter => {
            let mut b = scenario.add_creature_from_oracle(
                P0,
                "Ornithopter of Paradise",
                0,
                2,
                ORNITHOPTER_OF_PARADISE_ORACLE,
            );
            b.as_artifact_creature();
            (ManaCostShard::Green, Some(b.id()))
        }
        Producer::GreatFurnace => {
            let mut b = scenario.add_land_from_oracle(P0, "Great Furnace", GREAT_FURNACE_ORACLE);
            b.as_artifact_land();
            (ManaCostShard::Red, Some(b.id()))
        }
        Producer::LlanowarElves => (
            ManaCostShard::Green,
            Some(
                scenario
                    .add_creature_from_oracle(P0, "Llanowar Elves", 1, 1, LLANOWAR_ELVES_ORACLE)
                    .id(),
            ),
        ),
        Producer::Forest => (
            ManaCostShard::Green,
            Some(scenario.add_basic_land(P0, ManaColor::Green)),
        ),
        Producer::IntrinsicArtifactForest => (ManaCostShard::Green, None),
    };

    let spell = scenario
        .add_creature_to_hand_from_oracle(P0, "Spell", 1, 1, "")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![color],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();

    let source = source.unwrap_or_else(|| {
        let land = create_object(
            runner.state_mut(),
            CardId(9001),
            P0,
            "Intrinsic Artifact Forest".to_string(),
            Zone::Battlefield,
        );
        let obj = runner.state_mut().objects.get_mut(&land).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Forest".to_string());
        obj.base_card_types = obj.card_types.clone();
        assert!(
            obj.abilities.is_empty(),
            "fixture must rely on the CR 305.6 intrinsic mana ability, not a parsed one"
        );
        land
    });

    Fixture {
        runner,
        source,
        spell,
    }
}

impl Fixture {
    /// Production affordability gate (the legal-action offer), which plans
    /// payment from the auto-tap source list before any `CastSpell` is submitted.
    fn is_castable(&self) -> bool {
        let probe = PriorityCastProbe::new(self.runner.state(), P0);
        can_cast_object_now_with_probe(probe.state(), P0, self.spell, Some(&probe))
    }

    fn cast_auto(&mut self) -> Result<(), String> {
        let card_id = self.runner.state().objects[&self.spell].card_id;
        self.runner
            .act(GameAction::CastSpell {
                object_id: self.spell,
                card_id,
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            })
            .map(|_| ())
            .map_err(|e| format!("{e:?}"))
    }

    fn activate_source(&mut self) -> Result<(), String> {
        self.runner
            .act(GameAction::ActivateAbility {
                source_id: self.source,
                ability_index: 0,
            })
            .map(|_| ())
            .map_err(|e| format!("{e:?}"))
    }

    fn assert_cast_succeeds_tapping_source(&mut self, label: &str) {
        assert!(
            self.is_castable(),
            "{label}: spell must be offered as castable"
        );
        let result = self.cast_auto();
        assert!(result.is_ok(), "{label}: cast must succeed, got {result:?}");
        assert!(
            self.runner.state().objects[&self.source].tapped,
            "{label}: source must be tapped for mana"
        );
        assert_eq!(self.runner.state().objects[&self.spell].zone, Zone::Stack);
    }

    fn assert_cast_rejected_leaving_source_untapped(&mut self, label: &str) {
        assert!(
            !self.is_castable(),
            "{label}: spell must not be offered as castable"
        );
        let result = self.cast_auto();
        assert!(result.is_err(), "{label}: cast must fail, got {result:?}");
        assert!(
            !self.runner.state().objects[&self.source].tapped,
            "{label}: source must remain untapped"
        );
        assert_eq!(self.runner.state().objects[&self.spell].zone, Zone::Hand);
    }
}

fn assert_prohibited_with_positive_control(producer: Producer, label: &str) {
    // Positive control: identical source and cast without Collector Ouphe.
    let mut control = setup(producer, false);
    control.assert_cast_succeeds_tapping_source(&format!("{label} (no Ouphe)"));
    let mut control_act = setup(producer, false);
    assert!(
        control_act.activate_source().is_ok(),
        "{label} (no Ouphe): direct activation must succeed"
    );

    // Prohibited: same source, same cast, with Collector Ouphe.
    let mut blocked = setup(producer, true);
    blocked.assert_cast_rejected_leaving_source_untapped(&format!("{label} (Ouphe)"));
    let mut blocked_act = setup(producer, true);
    assert!(
        blocked_act.activate_source().is_err(),
        "{label} (Ouphe): direct activation must fail"
    );
}

#[test]
fn collector_ouphe_prevents_ornithopter_of_paradise_auto_tap_and_cast() {
    assert_prohibited_with_positive_control(Producer::Ornithopter, "Ornithopter of Paradise");
}

#[test]
fn collector_ouphe_prevents_artifact_land_auto_tap() {
    assert_prohibited_with_positive_control(Producer::GreatFurnace, "Great Furnace");
}

/// CR 305.6 + CR 602.5: the intrinsic basic-land-type mana ability of an
/// artifact land with no explicit ability definition is still an activated
/// ability and is prohibited.
#[test]
fn collector_ouphe_prevents_intrinsic_artifact_land_auto_tap() {
    let mut control = setup(Producer::IntrinsicArtifactForest, false);
    control.assert_cast_succeeds_tapping_source("intrinsic artifact Forest (no Ouphe)");

    let mut blocked = setup(Producer::IntrinsicArtifactForest, true);
    blocked.assert_cast_rejected_leaving_source_untapped("intrinsic artifact Forest (Ouphe)");
}

#[test]
fn collector_ouphe_allows_non_artifact_creature_mana_ability() {
    let mut f = setup(Producer::LlanowarElves, true);
    f.assert_cast_succeeds_tapping_source("Llanowar Elves (Ouphe)");
}

#[test]
fn collector_ouphe_allows_basic_forest_auto_tap() {
    let mut f = setup(Producer::Forest, true);
    f.assert_cast_succeeds_tapping_source("Forest (Ouphe)");
}
