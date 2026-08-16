//! Discriminating regression for **Omenpath Journey** (std impulse-exile batch)
//! — the end-step trigger body:
//!
//! > choose a card at random exiled with ~ and put it onto the battlefield
//! > tapped.
//!
//! "choose a card at random exiled with ~" selects ONE card uniformly at random
//! from the source's linked-exile set (`TargetFilter::ExiledBySource`, scanned
//! in Exile), then "put it onto the battlefield tapped" returns the chosen card
//! (`ParentTarget`) tapped. It must lower to `Effect::ChooseFromZone { zone:
//! Exile, filter: ExiledBySource, selection: Random, count: 1 }` →
//! `Effect::ChangeZone { destination: Battlefield, enter_tapped }`. Before the
//! fix the choose fell to `Effect::Unimplemented` and nothing was put onto the
//! battlefield.
//!
//! A random `ChooseFromZone` resolves inline (CR 608.2d override) — no
//! interactive prompt — so the chosen card is deterministic under the seed.
//!
//! DISCRIMINATOR: exactly ONE of the two cards exiled with the source enters the
//! battlefield tapped under P0's control; the other stays in exile. With the
//! choose reverted to `Unimplemented`, ZERO cards enter and the
//! battlefield-count assertion flips from 1 to 0.
//!
//! CR 608.2d: a random selection picks the referent.
//! CR 603.6d: "tapped" — enters the battlefield tapped.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, CardSelectionMode, Effect, TargetFilter};
use engine::types::card_type::CoreType;
use engine::types::game_state::{ExileLink, ExileLinkKind};
use engine::types::identifiers::CardId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const OMENPATH_END_STEP: &str =
    "choose a card at random exiled with ~ and put it onto the battlefield tapped.";

#[test]
fn omenpath_journey_puts_one_random_exiled_with_source_card_onto_battlefield_tapped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let source = scenario.add_creature(P0, "Omenpath Journey", 0, 0).id();

    let mut runner = scenario.build();
    let state = runner.state_mut();

    // Two land cards exiled with the source (Omenpath exiles up to five lands on
    // ETB; model two so the random pick has a real choice).
    let exiled: Vec<_> = ["Exiled Land A", "Exiled Land B"]
        .into_iter()
        .enumerate()
        .map(|(i, name)| {
            let id = create_object(
                state,
                CardId(900 + i as u64),
                P0,
                name.to_string(),
                Zone::Exile,
            );
            state.objects.get_mut(&id).unwrap().card_types.core_types = vec![CoreType::Land];
            state.exile_links.push(ExileLink {
                exiled_id: id,
                source_id: source,
                kind: ExileLinkKind::TrackedBySource,
            });
            id
        })
        .collect();

    // Parser path: random choose-from-exile of the linked set, then put tapped.
    let def = parse_effect_chain(OMENPATH_END_STEP, AbilityKind::Spell);
    assert!(
        matches!(
            &*def.effect,
            Effect::ChooseFromZone {
                count: 1,
                zone: Zone::Exile,
                filter: Some(TargetFilter::ExiledBySource),
                selection: CardSelectionMode::Random,
                ..
            }
        ),
        "must lower to a random ChooseFromZone(Exile, ExiledBySource), got {:?}",
        def.effect
    );

    let ability = build_resolved_from_def(&def, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("random choose-and-put must resolve inline");

    // DISCRIMINATOR: exactly one of the exiled cards is now on the battlefield
    // tapped; the other remains in exile.
    let on_battlefield: Vec<_> = exiled
        .iter()
        .filter(|id| runner.state().objects[id].zone == Zone::Battlefield)
        .copied()
        .collect();
    assert_eq!(
        on_battlefield.len(),
        1,
        "exactly one random exiled-with-source card must enter the battlefield \
         (reverting the choose to Unimplemented yields 0)"
    );
    let entered = on_battlefield[0];
    assert!(
        runner.state().objects[&entered].tapped,
        "the put-onto-battlefield card must enter tapped"
    );
    assert_eq!(
        runner.state().objects[&entered].controller,
        P0,
        "the entered card is controlled by the trigger's controller"
    );

    let still_exiled = exiled
        .iter()
        .filter(|id| runner.state().objects[id].zone == Zone::Exile)
        .count();
    assert_eq!(
        still_exiled, 1,
        "the unchosen exiled card must remain in exile"
    );
}
