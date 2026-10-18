//! Spirit-Sister's Call: the returned card, not the enchantment, gains the quoted
//! "exile it instead" replacement.
//!
//! The grant parses with `affected: SelfRef`. That alone does not say which object
//! receives it: the return carries `forward_result`, and
//! `rebind_child_to_forwarded_objects` points the following grant's source at the
//! object the return moved (CR 400.7j) before the SelfRef installer runs. This test
//! drives the real end-step trigger — target choice, the optional sacrifice, the
//! return — and then destroys both permanents through the zone-change hub.
//!
//! CR 603.2 (the beginning-of-end-step trigger), CR 608.2c (instructions in written
//! order; "it" is the returned card), CR 614.1a ("exile it instead" is a replacement
//! effect), CR 611.2a (the grant states no duration).

use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{Effect, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

/// Verbatim Oracle text (MTGJSON `AtomicCards.json`).
const SPIRIT_SISTERS_CALL: &str = "At the beginning of your end step, choose target permanent \
card in your graveyard. You may sacrifice a permanent that shares a card type with the chosen \
card. If you do, return the chosen card from your graveyard to the battlefield and it gains \
\"If this permanent would leave the battlefield, exile it instead of putting it anywhere else.\"";

/// True when `object` hosts the granted "leave the battlefield → exile" replacement.
fn hosts_leave_exile_replacement(runner: &GameRunner, object: ObjectId) -> bool {
    runner.state().objects[&object]
        .replacement_definitions
        .as_slice()
        .iter()
        .any(|replacement| {
            replacement.event == ReplacementEvent::Moved
                && replacement.valid_card == Some(TargetFilter::SelfRef)
        })
}

/// Destroy `object` through the production zone-change hub so a granted
/// Moved→Exile replacement, if live, is consulted. Mirrors
/// `issue_6566_granted_leave_exile::destroy`.
fn destroy(runner: &mut GameRunner, object: ObjectId) {
    let destroy = ResolvedAbility::new(
        Effect::Destroy {
            target: TargetFilter::Any,
            cant_regenerate: false,
        },
        vec![TargetRef::Object(object)],
        object,
        P0,
    );
    let mut events = Vec::<GameEvent>::new();
    resolve_ability_chain(runner.state_mut(), &destroy, &mut events, 0).expect("destroy resolves");
}

#[test]
fn spirit_sisters_call_grants_the_replacement_to_the_returned_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let call = scenario
        .add_enchantment_from_oracle(P0, "Spirit-Sister's Call", SPIRIT_SISTERS_CALL)
        .id();
    let chosen = scenario
        .add_creature_to_graveyard(P0, "Returned Bear", 2, 2)
        .id();
    // Shares the creature type with the chosen card, so it pays the sacrifice.
    let fodder = scenario.add_creature(P0, "Fodder", 1, 1).id();
    let mut runner = scenario.build();

    runner.advance_to_end_step();
    for _ in 0..80 {
        if runner.state().objects[&chosen].zone == Zone::Battlefield
            && runner.state().stack.is_empty()
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("empty attack declaration");
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(chosen)),
                    })
                    .expect("choose the graveyard card");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the optional sacrifice");
            }
            WaitingFor::EffectZoneChoice { .. } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![fodder],
                    })
                    .expect("sacrifice the fodder");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).ok();
            }
            _ => runner.pass_both_players(),
        }
    }

    // Reach guards: the sacrifice was paid and the chosen card came back.
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&chosen].zone, Zone::Battlefield);

    // CR 608.2c: "it gains" names the returned card, not the enchantment.
    assert!(
        hosts_leave_exile_replacement(&runner, chosen),
        "the returned card must host the granted replacement"
    );
    assert!(
        !hosts_leave_exile_replacement(&runner, call),
        "the enchantment must not host the granted replacement"
    );

    // CR 614.1a: the returned card is exiled instead; the enchantment dies normally.
    destroy(&mut runner, chosen);
    assert_eq!(runner.state().objects[&chosen].zone, Zone::Exile);
    destroy(&mut runner, call);
    assert_eq!(runner.state().objects[&call].zone, Zone::Graveyard);
}
