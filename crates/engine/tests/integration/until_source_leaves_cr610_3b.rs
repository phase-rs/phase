//! CR 610.3b regression for the generic linked-exile duration path.
//!
//! White Auracite is an O-Ring-class source: its ETB exile carries
//! `Duration::UntilHostLeavesPlay`. If the source leaves after that trigger is
//! on the stack but before it resolves, the initial exile does not happen and
//! no source-linked exile record is installed.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::Duration;
use engine::types::actions::GameAction;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const WHITE_AURACITE: &str = "When this artifact enters, exile target nonland permanent an opponent controls until this artifact leaves the battlefield.\n{T}: Add {W}.";
const DESTROY_TARGET_ARTIFACT: &str = "Destroy target artifact.";

/// CR 610.3b: If the specified event occurs after the linked-exile trigger is
/// put on the stack but before its initial zone change resolves, the target
/// stays on the battlefield and no exile link is created.
#[test]
fn source_leaving_before_linked_exile_trigger_resolves_prevents_initial_exile() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = {
        let mut source = scenario.add_creature_to_hand(P0, "White Auracite", 0, 0);
        source.as_artifact().from_oracle_text(WHITE_AURACITE);
        source.id()
    };
    let target = scenario
        .add_creature(P1, "Opponent Nonland Permanent", 2, 2)
        .as_enchantment()
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P1, "Destroy White Auracite", true, DESTROY_TARGET_ARTIFACT)
        .id();

    let mut runner = scenario.build();
    let mut committed = runner.cast(source).target_object(target).commit();
    committed
        .act(GameAction::PassPriority)
        .expect("P0 can pass priority for White Auracite");
    committed
        .act(GameAction::PassPriority)
        .expect("P1 can pass priority for White Auracite");
    assert_eq!(committed.state().objects[&source].zone, Zone::Battlefield);
    assert!(
        matches!(
            committed.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "reach-guard: the single ETB trigger must be on the stack and return priority to P0"
    );

    let StackEntryKind::TriggeredAbility { ability, .. } = &committed
        .state()
        .stack
        .back()
        .expect("reach-guard: White Auracite's ETB trigger is on the stack")
        .kind
    else {
        panic!("reach-guard: White Auracite's stack entry must be a triggered ability");
    };
    assert_eq!(
        ability.duration,
        Some(Duration::UntilHostLeavesPlay),
        "reach-guard: parser synthesis must route this through the generic duration event"
    );

    committed
        .act(GameAction::PassPriority)
        .expect("P0 can pass priority to P1's response");
    committed.cast(destroy).target_object(source).resolve();

    assert_eq!(
        committed.state().objects[&source].zone,
        Zone::Graveyard,
        "reach-guard: the duration-ending source must leave while its ETB trigger waits"
    );
    assert_eq!(
        committed.state().objects[&target].zone,
        Zone::Battlefield,
        "the response must not move the ETB target"
    );
    assert!(
        committed.state().exile_links.is_empty(),
        "no exile link can exist before the suppressed ETB trigger resolves"
    );

    for _ in 0..8 {
        if committed.state().stack.is_empty() {
            break;
        }
        committed
            .act(GameAction::PassPriority)
            .expect("priority must advance the pending ETB trigger");
    }

    assert!(committed.state().stack.is_empty());
    assert_eq!(
        committed.state().objects[&target].zone,
        Zone::Battlefield,
        "CR 610.3b suppresses the initial exile after the source-left event"
    );
    assert!(
        committed.state().exile_links.is_empty(),
        "the suppressed initial one-shot effect must not install an exile link"
    );
}
