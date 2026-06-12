//! Issue #924: Iridescent Vinelasher — Offspring creates the token but also
//! incorrectly labels the original creature as a copy/offspring.

use engine::game::game_object::DisplaySource;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::TriggerCondition;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;

const VINELASHER_ORACLE: &str = "\
Offspring {2} (You may pay an additional {2} as you cast this spell. If you do, when this creature enters, create a 1/1 token copy of it.)\n\
Landfall — Whenever a land you control enters, this creature deals 1 damage to target opponent.";

fn fund_mana(runner: &mut GameRunner, count: usize) {
    let p0 = runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap();
    for _ in 0..count {
        p0.mana_pool.add(ManaUnit {
            color: ManaType::Colorless,
            source_id: engine::types::identifiers::ObjectId(0),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        });
    }
}

fn pass_until_priority(runner: &mut GameRunner) {
    for _ in 0..40 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty()
        {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
}

#[test]
fn offspring_cast_creates_token_without_labeling_original_as_copy() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Black);

    let spell_id = scenario
        .add_creature_to_hand_from_oracle(P0, "Iridescent Vinelasher", 1, 2, VINELASHER_ORACLE)
        .with_subtypes(vec!["Lizard", "Assassin"])
        .id();

    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&spell_id).unwrap();
        obj.color = vec![ManaColor::Black];
        obj.base_color = vec![ManaColor::Black];
    }
    let card_id = runner.state().objects[&spell_id].card_id;
    fund_mana(&mut runner, 5);

    runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast should start");

    // Pay offspring optional cost during casting (CR 601.2f).
    loop {
        match &runner.state().waiting_for {
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { pay: true })
                    .expect("pay offspring");
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay mana");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected waiting_for during cast: {other:?}"),
        }
    }

    // Resolve the spell and offspring ETB trigger.
    pass_until_priority(&mut runner);

    let battlefield: Vec<_> = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id).map(|o| (*id, o)))
        .filter(|(_, o)| o.card_types.core_types.contains(&CoreType::Creature))
        .collect();

    assert_eq!(
        battlefield.len(),
        2,
        "expected parent + offspring token creatures on battlefield, got {battlefield:?}"
    );

    let (parent_id, parent) = battlefield
        .iter()
        .find(|(_, o)| !o.is_token)
        .map(|(id, o)| (*id, o))
        .expect("parent creature");
    let (token_id, token) = battlefield
        .iter()
        .find(|(_, o)| o.is_token)
        .map(|(id, o)| (*id, o))
        .expect("offspring token");

    assert_ne!(parent_id, token_id);

    // Original must remain a real card, not a token/copy.
    assert!(!parent.is_token, "parent must not be marked is_token");
    assert_eq!(parent.display_source, DisplaySource::Card);
    assert_eq!(parent.power, Some(1), "parent keeps printed power");
    assert_eq!(parent.toughness, Some(2), "parent keeps printed toughness");
    assert!(
        parent
            .keywords
            .iter()
            .any(|kw| matches!(kw, Keyword::Offspring(_))),
        "parent retains printed Offspring keyword"
    );

    // Offspring token is 1/1 per CR 702.175a and uses predefined token art.
    assert!(token.is_token);
    assert_eq!(token.power, Some(1));
    assert_eq!(token.toughness, Some(1));
    assert_eq!(
        token.display_source,
        DisplaySource::Token,
        "offspring token must use token art, not card-copy display"
    );
    assert!(
        token.token_image_ref.is_some(),
        "offspring token must carry a resolved token_image_ref"
    );
    assert!(
        !token
            .keywords
            .iter()
            .any(|kw| matches!(kw, Keyword::Offspring(_))),
        "offspring token must not display cast-only Offspring keyword"
    );
    assert!(
        !token
            .trigger_definitions
            .iter_unchecked()
            .any(|trig| matches!(
                trig.condition,
                Some(TriggerCondition::AdditionalCostPaid { .. })
            )),
        "offspring token must not retain offspring ETB trigger"
    );
    assert!(
        token.trigger_definitions.iter_unchecked().any(|trig| trig
            .description
            .as_deref()
            .is_some_and(|d| d.contains("land you control enters"))),
        "offspring token keeps copied landfall trigger"
    );

    // Sanity invariant (not new behavior in this fix): `reset_for_battlefield_entry`
    // already zeroes cast-time payment metadata at token creation; the
    // discriminating assertion for this fix is the trigger strip above.
    assert_eq!(token.additional_cost_payment_count, 0);
}
