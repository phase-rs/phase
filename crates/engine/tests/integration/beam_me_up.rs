//! Beam me up (TRK): Marooned's "can't be beamed up" prohibition (CR 101.2 +
//! CR 604.1 + CR 611.3a; the beam me up rule number needs manual
//! verification). Phase 1 rows; the cast-route rows land with the graveyard
//! cast route. Oracle text verbatim from MTGJSON / Scryfall.

use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::game_object::AttachTarget;
use engine::game::keywords::can_be_beamed_up;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{FilterProp, TargetFilter, TypedFilter};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;

const MAROONED: &str = "Enchant artifact or creature\nWhen this Aura enters, you may pay {3}. If you do, tap enchanted permanent.\nEnchanted permanent doesn't untap during its controller's untap step and can't be beamed up.";
const MALFUNCTION: &str = "Enchant artifact or creature\nWhen this Aura enters, tap enchanted permanent.\nEnchanted permanent doesn't untap during its controller's untap step.";

/// Stage an Aura on the battlefield with its Oracle text run through the
/// production parser (mirrors `attachment_host_reference_untargeted.rs`).
fn aura_on_battlefield(
    sc: &mut GameScenario,
    player: PlayerId,
    name: &str,
    text: &str,
) -> ObjectId {
    sc.add_creature(player, name, 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["enchant"], text)
        .id()
}

/// CR 303.4b: attach `attachment` to `host` and re-derive the board.
fn attach(runner: &mut GameRunner, attachment: ObjectId, host: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&attachment)
        .unwrap()
        .attached_to = Some(AttachTarget::Object(host));
    let host_obj = runner.state_mut().objects.get_mut(&host).unwrap();
    if !host_obj.attachments.contains(&attachment) {
        host_obj.attachments.push(attachment);
    }
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
}

/// Undo `attach`: the Aura is unattached and the host no longer lists it.
fn detach(runner: &mut GameRunner, attachment: ObjectId, host: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&attachment)
        .unwrap()
        .attached_to = None;
    runner
        .state_mut()
        .objects
        .get_mut(&host)
        .unwrap()
        .attachments
        .retain(|id| *id != attachment);
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
}

/// The beam me up eligibility probe: "creature … that can be beamed up".
fn beamable_creature() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::CanBeBeamedUp]))
}

fn probe_admits(runner: &GameRunner, source: ObjectId, id: ObjectId) -> bool {
    let state = runner.state();
    matches_target_filter(
        state,
        id,
        &beamable_creature(),
        &FilterContext::from_source(state, source),
    )
}

struct Board {
    runner: GameRunner,
    a: ObjectId,
    b: ObjectId,
    aura: ObjectId,
}

/// P0 controls Grizzly Bears (A) and Savannah Lions (B); `aura_controller`
/// controls the Aura (`aura_text`), staged unattached on the battlefield.
fn board(aura_controller: PlayerId, aura_name: &str, aura_text: &str) -> Board {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let a = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let b = sc.add_creature(P0, "Savannah Lions", 2, 1).id();
    let aura = aura_on_battlefield(&mut sc, aura_controller, aura_name, aura_text);
    let mut runner = sc.build();
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    Board { runner, a, b, aura }
}

fn has_static(runner: &GameRunner, id: ObjectId, mode: &StaticMode) -> bool {
    runner.state().objects[&id]
        .static_definitions
        .iter_all()
        .any(|def| def.mode == *mode)
}

/// R5: on a real Marooned board the reader excludes Marooned's host and admits
/// another creature. An opponent's (P1's) Marooned on P0's creature is the
/// realistic case. Reach control: before `attach`, the same board admits A, so
/// the exclusion comes from the host binding (`EnchantedBy`), not from
/// Marooned's mere presence.
#[test]
fn marooned_host_cannot_be_beamed_up_but_another_creature_can() {
    let Board {
        mut runner,
        a,
        b,
        aura,
    } = board(P1, "Marooned", MAROONED);
    assert!(
        has_static(&runner, aura, &StaticMode::CantBeBeamedUp),
        "REACH: Marooned must carry the parsed CantBeBeamedUp static"
    );

    // Reach control: unattached Marooned names no host.
    assert!(can_be_beamed_up(runner.state(), a));
    assert!(probe_admits(&runner, a, a));

    attach(&mut runner, aura, a);

    assert!(
        !can_be_beamed_up(runner.state(), a),
        "Marooned's host can't be beamed up"
    );
    assert!(
        !probe_admits(&runner, a, a),
        "FilterProp::CanBeBeamedUp must reject Marooned's host"
    );
    // Paired positive: the evaluator runs and answers true for another creature.
    assert!(can_be_beamed_up(runner.state(), b));
    assert!(probe_admits(&runner, a, b));
}

/// R6: the prohibition is keyed on the `CantBeBeamedUp` static, not on the
/// Aura's other static or its attachment. Malfunction carries the identical
/// `CantUntap` on the enchanted permanent but prints no beam me up clause.
#[test]
fn malfunction_host_can_still_be_beamed_up() {
    let Board {
        mut runner,
        a,
        b,
        aura,
    } = board(P1, "Malfunction", MALFUNCTION);
    assert!(
        has_static(&runner, aura, &StaticMode::CantUntap),
        "REACH: Malfunction must carry its parsed CantUntap static"
    );
    assert!(!has_static(&runner, aura, &StaticMode::CantBeBeamedUp));

    attach(&mut runner, aura, a);

    assert!(can_be_beamed_up(runner.state(), a));
    assert!(probe_admits(&runner, a, a));
    assert!(can_be_beamed_up(runner.state(), b));
}

/// R7a (CR 611.3a): the binding is live and source-relative. Moving Marooned
/// from A to B moves the prohibition with it; no latched id survives.
#[test]
fn moving_marooned_moves_the_prohibition() {
    let Board {
        mut runner,
        a,
        b,
        aura,
    } = board(P1, "Marooned", MAROONED);
    attach(&mut runner, aura, a);
    assert!(!can_be_beamed_up(runner.state(), a));
    assert!(can_be_beamed_up(runner.state(), b));

    detach(&mut runner, aura, a);
    attach(&mut runner, aura, b);

    assert!(
        can_be_beamed_up(runner.state(), a),
        "A is no longer enchanted by Marooned"
    );
    assert!(probe_admits(&runner, a, a));
    assert!(
        !can_be_beamed_up(runner.state(), b),
        "Marooned's new host can't be beamed up"
    );
    assert!(!probe_admits(&runner, a, b));
}

/// R7b: Marooned's text has no controller clause, so a Marooned controlled by
/// the host's own controller (P0) still blocks its host.
#[test]
fn own_marooned_still_blocks_its_host() {
    let Board {
        mut runner,
        a,
        b,
        aura,
    } = board(P0, "Marooned", MAROONED);
    attach(&mut runner, aura, a);

    assert!(!can_be_beamed_up(runner.state(), a));
    assert!(!probe_admits(&runner, a, a));
    assert!(can_be_beamed_up(runner.state(), b));
}
