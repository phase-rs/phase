//! Issue #1019 — Heraldic Banner pumps creatures of the chosen color.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const HERALDIC_BANNER_ORACLE: &str = "As this artifact enters, choose a color.\n\
Creatures you control of the chosen color get +1/+0.\n\
{T}: Add one mana of the chosen color.";

const GIFTED_AETHERBORN_ORACLE: &str = "Deathtouch, lifelink";

#[test]
fn heraldic_banner_pumps_creature_of_chosen_color() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let aetherborn = scenario
        .add_creature_from_oracle(P0, "Gifted Aetherborn", 2, 3, GIFTED_AETHERBORN_ORACLE)
        .with_subtypes(vec!["Aetherborn", "Vampire"])
        .with_mana_cost(engine::types::mana::ManaCost::Cost {
            generic: 1,
            shards: vec![
                engine::types::mana::ManaCostShard::Black,
                engine::types::mana::ManaCostShard::Black,
            ],
        })
        .id();
    let banner = scenario
        .add_spell_to_hand(P0, "Heraldic Banner", false)
        .as_artifact()
        .from_oracle_text(HERALDIC_BANNER_ORACLE)
        .with_mana_cost(engine::types::mana::ManaCost::generic(3))
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(9_997), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_998), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_999), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    let outcome = runner.cast(banner).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::NamedChoice { .. }),
        "Heraldic Banner must pause on the as-enters color choice, got {:?}",
        outcome.final_waiting_for()
    );

    let WaitingFor::NamedChoice { options, .. } = runner.state().waiting_for.clone() else {
        panic!("expected NamedChoice for color");
    };
    let black = options
        .iter()
        .find(|opt| opt.eq_ignore_ascii_case("black"))
        .cloned()
        .expect("color options must include Black");
    runner
        .act(GameAction::ChooseOption { choice: black })
        .expect("choose black");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&banner].chosen_color(),
        Some(engine::types::mana::ManaColor::Black),
        "banner must remember the chosen color"
    );
    assert!(
        runner.state().objects[&aetherborn]
            .color
            .contains(&engine::types::mana::ManaColor::Black),
        "Gifted Aetherborn must be black, colors={:?}",
        runner.state().objects[&aetherborn].color
    );
    let statics = runner.state().objects[&banner]
        .static_definitions
        .as_slice();
    assert!(
        statics.iter().any(|sd| {
            sd.modifications.iter().any(|m| {
                matches!(
                    m,
                    engine::types::ability::ContinuousModification::AddPower { value: 1 }
                )
            }) && matches!(
                &sd.affected,
                Some(engine::types::ability::TargetFilter::Typed(tf))
                    if tf.properties.iter().any(|p| {
                        matches!(p, engine::types::ability::FilterProp::IsChosenColor)
                    })
            )
        }),
        "banner must carry a chosen-color +1/+0 static, statics={statics:?}"
    );

    assert_eq!(
        runner.state().objects[&banner].zone,
        Zone::Battlefield,
        "Heraldic Banner must be on the battlefield after the color choice"
    );
    let (power, toughness) = runner
        .state()
        .objects
        .get(&aetherborn)
        .map(|o| (o.power.unwrap_or(0), o.toughness.unwrap_or(0)))
        .unwrap_or((0, 0));
    assert_eq!(
        (power, toughness),
        (3, 3),
        "Gifted Aetherborn must get +1/+0 from Heraldic Banner when Black is chosen"
    );
}
