//! Runtime regression for GitHub issue #4543 — Tamiyo, Inquisitive Student's
//! "draw your third card" transform-flip trigger.
//!
//! Front-face Oracle: "When you draw your third card in a turn, exile Tamiyo,
//! then return her to the battlefield transformed under her owner's control."
//!
//! The reported bug was runtime-visible: when the controller drew their third
//! card, Tamiyo exiled herself but never returned — she was stranded in exile.
//! Root cause was a parser anaphor mis-binding. Clause 1 ("exile Tamiyo")
//! correctly names the source via `~` → `SelfRef`, but clause 2's bare "her"
//! ("return her ... transformed") bound to `TriggeringSource`. The trigger
//! subject is the player ("you draw ..."), so the `CardDrawn` event carries no
//! resolvable source object — `TriggeringSource` resolved to nothing and the
//! return moved nothing. The SelfRef-anaphor rewrite that fixes this class was
//! previously gated on a *typed* trigger subject (Ajani's "one or more Cats
//! die"); the fix extends it to the transform-flip-from-exile class.
//!
//! This drives the REAL parse → draw-event → trigger → stack → zone-change
//! pipeline: Tamiyo is synthesized from Oracle text (so the trigger reflects
//! current parser source, not a stored/stale card-data AST), three draws are
//! resolved through the production `draw::resolve` seam, and the third queues
//! and resolves the trigger. The discriminating assertion is that Tamiyo is
//! back on the battlefield — with the fix reverted she stays in exile.
//!
//! CR 608.2c: "read the whole text ... apply the rules of English" — the
//! anaphor "her" binds to the named antecedent in the preceding clause.
//! CR 712.14a: a double-faced card put onto the battlefield transformed enters
//! with its back face up.
//! CR 603.2: the third-draw trigger fires only on the third draw of the turn.

use engine::game::effects::draw::resolve as resolve_draw;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::stack::resolve_top;
use engine::game::triggers::process_triggers;
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);

// Only the flip trigger line is needed to exercise the bug; `~` normalization
// keys off the card name, so "exile Tamiyo" resolves to SelfRef.
const TAMIYO: &str = "When you draw your third card in a turn, exile Tamiyo, then return her to the battlefield transformed under her owner's control.";

/// Resolve one draw for P0 through the production `draw::resolve` seam, then
/// process the resulting triggers. The third draw queues the flip trigger.
fn draw_one(runner: &mut GameRunner) {
    let ability = ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        Vec::new(),
        ObjectId(0),
        P0,
    );
    let mut events = Vec::new();
    resolve_draw(runner.state_mut(), &ability, &mut events).expect("draw resolves");
    process_triggers(runner.state_mut(), &events);
}

#[test]
fn tamiyo_third_draw_returns_transformed_not_stranded_in_exile() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Tamiyo on P0's battlefield, built from Oracle text through the real parse
    // + synthesis pipeline so the (fixed) NthDrawThisTurn trigger is installed.
    let tamiyo = scenario
        .add_creature_from_oracle(P0, "Tamiyo, Inquisitive Student", 0, 3, TAMIYO)
        .id();

    // Seed P0's library with cards to draw.
    for i in 0..4 {
        scenario.add_card_to_library_top(P0, &format!("Library Card {i}"));
    }

    let mut runner = scenario.build();

    // Precondition: Tamiyo is on the battlefield, front-face (not transformed).
    {
        let obj = runner.state().objects.get(&tamiyo).expect("Tamiyo exists");
        assert_eq!(
            obj.zone,
            Zone::Battlefield,
            "precondition: Tamiyo on battlefield"
        );
        assert!(!obj.transformed, "precondition: Tamiyo starts front-face");
    }

    // First draw of the turn: the third-draw trigger must NOT fire.
    draw_one(&mut runner);
    assert_eq!(
        runner.state().stack.len(),
        0,
        "CR 603.2: the first draw must not queue the third-draw trigger"
    );
    assert_eq!(
        runner.state().objects.get(&tamiyo).unwrap().zone,
        Zone::Battlefield,
        "after one draw Tamiyo is untouched"
    );

    // Second draw: still must NOT fire (discriminates the n=3 gate).
    draw_one(&mut runner);
    assert_eq!(
        runner.state().stack.len(),
        0,
        "CR 603.2: the second draw must not queue the third-draw trigger"
    );

    // Third draw: the trigger fires and goes on the stack.
    draw_one(&mut runner);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the third draw fires 'when you draw your third card in a turn'"
    );

    // Resolve the triggered ability — exile Tamiyo, then return her transformed.
    let mut events = Vec::new();
    resolve_top(runner.state_mut(), &mut events);

    // The fix's payload: Tamiyo is back on the battlefield, NOT stranded in
    // exile. With the parser fix reverted, clause 2's "her" binds to
    // `TriggeringSource`; the player-subject `CardDrawn` event has no source, so
    // the return moves nothing and this assertion fails (Tamiyo stays in Exile).
    let obj = runner
        .state()
        .objects
        .get(&tamiyo)
        .expect("Tamiyo object still exists");
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "CR 608.2c: 'return her transformed' must bind to the source (SelfRef) \
         named by clause 1's 'exile ~' — otherwise Tamiyo is stranded in exile, \
         got zone {:?}",
        obj.zone
    );
    // The return is the same object the exile named (CR 608.2c anaphor), not a
    // stranded husk left in exile.
    assert_ne!(
        obj.zone,
        Zone::Exile,
        "the source must not be left in exile after the return resolves"
    );
    // NB: this Oracle-text-synthesized Tamiyo has no printed back face, so the
    // `enter_transformed` flip has nothing to swap to and `obj.transformed`
    // stays false — that AST flag (ChangeZone { enter_transformed: true }) is
    // asserted directly by the parser-shape test
    // `tamiyo_flip_return_transformed_binds_to_self_ref` in oracle_trigger.rs.
    // What this runtime test discriminates is the binding bug: the return moves
    // the source back to the battlefield instead of fizzling on an unresolvable
    // `TriggeringSource`.
}
