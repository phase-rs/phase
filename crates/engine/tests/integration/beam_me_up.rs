//! Beam me up (TRK): Marooned's "can't be beamed up" prohibition (CR 101.2 +
//! CR 604.1 + CR 611.3a; the beam me up rule number needs manual
//! verification) and the beam me up keyword's inertness row. Phase 1 and 2
//! rows; the cast-route rows land with the graveyard cast route. Oracle text
//! verbatim from MTGJSON / Scryfall.

use engine::ai_support::legal_actions;
use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::game_object::AttachTarget;
use engine::game::keywords::can_be_beamed_up;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{FilterProp, TargetFilter, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

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

const OPEN_COMMUNICATIONS: &str = "Draw a card.\nBeam me up {2}{U} (You may cast this card from your graveyard for {2}{U} if you also return a creature you control to its owner's hand. Then exile this spell.)";
const WILD_RIDE: &str = "Target creature gets +3/+0 and gains haste until end of turn.\nHarmonize {4}{R} (You may cast this card from your graveyard for its harmonize cost. You may tap a creature you control to reduce that cost by {X}, where X is its power. Then exile this spell.)";

fn offers_cast_of(runner: &GameRunner, object: ObjectId) -> bool {
    legal_actions(runner.state()).iter().any(
        |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == object),
    )
}

/// Phase 2 inertness row: until the graveyard cast route lands, a card's
/// printed `Keyword::BeamMeUp` offers no cast from the graveyard, and the card
/// still casts from hand as printed (CR 601.3: only a permission makes a
/// graveyard cast legal; a hand cast is an ordinary sorcery that goes to its
/// owner's graveyard on resolution). The phase 3 cast route replaces this row.
#[test]
fn beam_me_up_is_inert_until_the_graveyard_cast_route_lands() {
    let open_communications_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 0,
    };
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    sc.add_creature(P0, "Grizzly Bears", 2, 2);
    let in_graveyard = sc
        .add_spell_to_graveyard(P0, "Open Communications", false)
        .with_mana_cost(open_communications_cost.clone())
        .from_oracle_text(OPEN_COMMUNICATIONS)
        .id();
    let in_hand = sc
        .add_spell_to_hand_from_oracle(P0, "Open Communications", false, OPEN_COMMUNICATIONS)
        .with_mana_cost(open_communications_cost)
        .id();
    let wild_ride = sc
        .add_spell_to_graveyard(P0, "Wild Ride", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .from_oracle_text(WILD_RIDE)
        .id();
    sc.add_card_to_library_top(P0, "Library Card");
    let mut pool = vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]); 3];
    pool.extend(vec![
        ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]);
        5
    ]);
    sc.with_mana_pool(P0, pool);
    let mut runner = sc.build();

    // (i) Reach guard: the real graveyard object carries the printed keyword.
    let printed = Keyword::BeamMeUp(ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 2,
    });
    assert!(
        runner.state().objects[&in_graveyard].has_keyword(&printed),
        "REACH: the graveyard Open Communications must carry Beam me up {{2}}{{U}}"
    );

    // Paired positives: the graveyard-cast offer path is reachable at this
    // priority with this pool (Wild Ride's harmonize), and the hand copy is
    // castable.
    assert!(
        offers_cast_of(&runner, wild_ride),
        "Wild Ride's harmonize cast must be offered from the graveyard"
    );
    assert!(
        offers_cast_of(&runner, in_hand),
        "the hand copy is castable"
    );
    // (ii) No beam me up cast is offered.
    assert!(
        !offers_cast_of(&runner, in_graveyard),
        "beam me up must offer no graveyard cast before its route lands"
    );

    // (iii) The hand copy casts and resolves as printed.
    let outcome = runner.cast(in_hand).resolve();
    assert_eq!(
        outcome.hand_drawn(P0),
        1,
        "Open Communications draws a card"
    );
    assert_eq!(
        outcome.zone_of(in_hand),
        Zone::Graveyard,
        "a hand cast goes to its owner's graveyard, not exile"
    );

    // (iv) Still nothing offered for a graveyard object carrying the keyword.
    let offered_beam = runner
        .state()
        .objects
        .values()
        .filter(|obj| {
            obj.zone == Zone::Graveyard
                && obj.owner == P0
                && obj
                    .keywords
                    .iter()
                    .any(|kw| matches!(kw, Keyword::BeamMeUp(_)))
        })
        .map(|obj| obj.id)
        .collect::<Vec<_>>();
    assert_eq!(
        offered_beam.len(),
        2,
        "both copies now sit in the graveyard"
    );
    for id in offered_beam {
        assert!(
            !offers_cast_of(&runner, id),
            "no beam me up cast after resolution"
        );
    }
}
