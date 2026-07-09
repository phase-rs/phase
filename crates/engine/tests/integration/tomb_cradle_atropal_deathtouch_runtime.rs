//! Runtime (engine-path) regression for Tomb of Annihilation "Cradle of the
//! Death God".
//!
//! The room creates "The Atropal, a legendary 4/4 black God Horror creature
//! token with deathtouch" (CR 702.2a). The token was built with an empty
//! keyword list, silently dropping the deathtouch. This drives the real venture
//! pipeline: with the marker in Oubliette (room 2 → [4] Cradle of the Death
//! God), a venture advances into Cradle, its room ability goes on the stack, and
//! resolving it creates the Atropal on the battlefield. On the old code the
//! token has no deathtouch and the assertion below fails — so this discriminates
//! the real game behavior, not just the AST shape.

use engine::game::dungeon::DungeonId;
use engine::game::effects::venture;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{Effect, ResolvedAbility};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;

#[test]
fn tomb_cradle_atropal_enters_with_deathtouch_at_runtime() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();

    // Marker already in Tomb of Annihilation at Oubliette (room 2 → [4] Cradle).
    {
        let prog = runner.state_mut().dungeon_progress.entry(P0).or_default();
        prog.current_dungeon = Some(DungeonId::TombOfAnnihilation);
        prog.current_room = 2;
    }

    // Venture: advance into Cradle of the Death God (room 4); its token ability
    // goes on the stack.
    let venture_ability =
        ResolvedAbility::new(Effect::VentureIntoDungeon, vec![], ObjectId(99), P0);
    let mut events = Vec::new();
    venture::resolve(runner.state_mut(), &venture_ability, &mut events).unwrap();
    assert_eq!(
        runner.state().dungeon_progress[&P0].current_room,
        4,
        "venture must advance the marker into Cradle of the Death God (room 4)"
    );

    // Resolve the room ability — creates The Atropal on the battlefield.
    runner.resolve_top();

    let atropal = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .find(|obj| obj.is_token && obj.name == "The Atropal")
        .expect("Cradle of the Death God creates The Atropal token");

    assert!(
        atropal.keywords.contains(&Keyword::Deathtouch),
        "CR 702.2a: The Atropal enters with deathtouch (the old empty keyword \
         list dropped it), got keywords {:?}",
        atropal.keywords
    );
}
